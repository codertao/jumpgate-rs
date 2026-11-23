//! Stock bots (conflux, faction defenders, turrets) in the server's books.
//!
//! The classic design (ai.md "Spawning", reversing-map "Conflux / bot AI"):
//! the server only decides which bots exist and which client flies each one.
//! That client runs the AI and physics and reports botlocs, which the server
//! relays to the sector. A bot attacks the client that flies it; ownership
//! moves when another client in the sector sends a 0x2C "bxr" because it is
//! closer (by the client's own measure). See docs/server.md "Stock bots".

use crate::game::PlayerId;
use jumpgate_common::ship::Vec3;
use std::collections::HashMap;

/// A spawn order whose bot hasn't reported within this long is forgotten
/// (the client spawns ~5 s after the 0x70).
const PENDING_S: f32 = 30.0;
/// The client's pbotloc table holds 50 bots; leave room for `!ai` and
/// hand-offs from elsewhere.
pub const MAX_PER_SECTOR: usize = 40;
/// How often a population rule may add one bot.
const SPAWN_EVERY_S: f32 = 20.0;

#[derive(Debug, Clone, PartialEq)]
pub struct Bot {
    pub ship_type: u16,
    pub controller: PlayerId,
    /// The controller's latest botloc (None until the first one arrives).
    pub last: Option<Vec<u8>>,
    pub pos: Vec3,
    /// Server time of the spawn order or the latest botloc.
    pub seen: f32,
}

/// "Keep `count` bots of `ship_type` in `loc` while someone is there."
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rule {
    pub loc: u16,
    pub ship_type: u16,
    pub count: usize,
}

#[derive(Default)]
pub struct Bots {
    /// (loc, bot id) → bot.
    pub map: HashMap<(u16, u16), Bot>,
    next_spawn: u16,
    pub rules: Vec<Rule>,
    /// Per rule: time of the last spawn order.
    last_spawn: Vec<f32>,
}

/// Parse a population file: one rule per line, `loc ship_type count`; `#`
/// starts a comment.
pub fn parse_rules(text: &str) -> Result<Vec<Rule>, String> {
    let mut rules = Vec::new();
    for (n, line) in text.lines().enumerate() {
        let line = line.split('#').next().unwrap_or("").trim();
        if line.is_empty() {
            continue;
        }
        let f: Vec<&str> = line.split_whitespace().collect();
        let bad = || format!("line {}: expected `loc ship_type count`, got {line:?}", n + 1);
        let [loc, ty, count] = f[..] else { return Err(bad()) };
        let rule = Rule {
            loc: loc.parse().map_err(|_| bad())?,
            ship_type: ty.parse().map_err(|_| bad())?,
            count: count.parse().map_err(|_| bad())?,
        };
        if !jumpgate_common::proto::BOT_SHIP_TYPES.contains(&rule.ship_type) {
            return Err(format!("line {}: ship type {} is not a bot (220–259)", n + 1, rule.ship_type));
        }
        rules.push(rule);
    }
    Ok(rules)
}

impl Bots {
    pub fn with_rules(rules: Vec<Rule>) -> Bots {
        let last_spawn = vec![f32::NEG_INFINITY; rules.len()];
        Bots { rules, last_spawn, ..Default::default() }
    }

    /// Take the next spawn number (the bot id in botloc and 0x44).
    pub fn next_id(&mut self) -> u16 {
        // Skip ids still in use anywhere: ids are per sector on the client,
        // but unique ones keep the logs readable.
        loop {
            let n = self.next_spawn;
            self.next_spawn = self.next_spawn.wrapping_add(1);
            if n != jumpgate_common::proto::NOT_A_BOT && !self.map.keys().any(|&(_, id)| id == n) {
                return n;
            }
        }
    }

    /// Record a spawn order sent to `controller`.
    pub fn ordered(&mut self, loc: u16, id: u16, ship_type: u16, controller: PlayerId, now: f32) {
        self.map.insert((loc, id), Bot { ship_type, controller, last: None, pos: [0.0; 3], seen: now });
    }

    pub fn in_sector(&self, loc: u16) -> usize {
        self.map.keys().filter(|&&(l, _)| l == loc).count()
    }

    /// A botloc from `pid`. True if it should be relayed: the bot is unknown
    /// (spawned with `!ai` or by an earlier server run) and gets adopted, or
    /// `pid` is its controller. Stale botlocs from a previous controller are
    /// dropped.
    pub fn report(&mut self, loc: u16, id: u16, ship_type: u16, pid: PlayerId, msg: &[u8], pos: Vec3, now: f32) -> bool {
        let bot = self.map.entry((loc, id)).or_insert(Bot { ship_type, controller: pid, last: None, pos, seen: now });
        if bot.controller != pid {
            return false;
        }
        (bot.ship_type, bot.last, bot.pos, bot.seen) = (ship_type, Some(msg.to_vec()), pos, now);
        true
    }

    /// Give the bot to `to`; returns the botloc to broadcast (None if the
    /// bot has never reported, so there's nothing to hand over).
    pub fn hand_over(&mut self, loc: u16, id: u16, to: PlayerId) -> Option<Vec<u8>> {
        let bot = self.map.get_mut(&(loc, id))?;
        let m = jumpgate_common::proto::botloc_handed_to(bot.last.as_ref()?, to)?;
        bot.controller = to;
        bot.last = Some(m.clone());
        Some(m)
    }

    /// The bot left `pid`'s client (0x16 removal or spawn bounce, or 0x44).
    pub fn gone(&mut self, loc: u16, id: u16, pid: PlayerId) -> bool {
        match self.map.get(&(loc, id)) {
            Some(b) if b.controller == pid => {
                self.map.remove(&(loc, id));
                true
            }
            _ => false,
        }
    }

    /// Forget spawn orders that never turned into a bot.
    pub fn expire(&mut self, now: f32) {
        self.map.retain(|_, b| b.last.is_some() || now - b.seen < PENDING_S);
    }

    /// Rules due a new bot: (rule index, rule).
    pub fn due(&self, now: f32) -> Vec<(usize, Rule)> {
        self.rules
            .iter()
            .enumerate()
            .filter(|&(i, r)| {
                let have = self.map.iter().filter(|&(&(l, _), b)| l == r.loc && b.ship_type == r.ship_type).count();
                have < r.count && self.in_sector(r.loc) < MAX_PER_SECTOR && now - self.last_spawn[i] >= SPAWN_EVERY_S
            })
            .map(|(i, &r)| (i, r))
            .collect()
    }

    pub fn spawned_for(&mut self, rule: usize, now: f32) {
        self.last_spawn[rule] = now;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jumpgate_common::proto;

    fn botloc(id: u16, ctl: u32) -> Vec<u8> {
        proto::botloc(&proto::BotState { id, loc: 118, ship_type: 233, controller: ctl, target: ctl, ..Default::default() })
    }

    #[test]
    fn control_follows_hand_overs_only() {
        let mut b = Bots::default();
        let id = b.next_id();
        b.ordered(118, id, 233, 1, 0.0);
        assert!(b.hand_over(118, id, 2).is_none(), "nothing to hand over before the first botloc");
        assert!(b.report(118, id, 233, 1, &botloc(id, 1), [1.0; 3], 6.0));
        assert!(!b.report(118, id, 233, 2, &botloc(id, 2), [1.0; 3], 6.0), "not the controller");
        let m = b.hand_over(118, id, 2).unwrap();
        assert_eq!(proto::parse_botloc(&m).unwrap().controller, 2);
        assert!(!b.report(118, id, 233, 1, &botloc(id, 1), [1.0; 3], 7.0), "stale botloc from the old controller");
        assert!(b.report(118, id, 233, 2, &botloc(id, 2), [1.0; 3], 7.0));
        assert!(!b.gone(118, id, 1));
        assert!(b.gone(118, id, 2));
        assert!(b.map.is_empty());
    }

    #[test]
    fn rules_parse_and_pace_spawns() {
        let rules = parse_rules("# conflux at Quantar Depot\n118 233 2\n\n107 220 1 # defender\n").unwrap();
        assert_eq!(rules, vec![Rule { loc: 118, ship_type: 233, count: 2 }, Rule { loc: 107, ship_type: 220, count: 1 }]);
        assert!(parse_rules("118 90 2").is_err());
        assert!(parse_rules("118 233").is_err());
        let mut b = Bots::with_rules(rules);
        assert_eq!(b.due(0.0).len(), 2);
        b.ordered(118, 0, 233, 1, 0.0);
        b.spawned_for(0, 0.0);
        assert_eq!(b.due(1.0).iter().map(|d| d.0).collect::<Vec<_>>(), vec![1], "rule 0 waits");
        assert_eq!(b.due(21.0).len(), 2, "one more conflux due");
        b.expire(31.0);
        assert!(b.map.is_empty(), "the order never produced a bot");
    }
}
