//! Other pilots' missiles in our sector, as the stock client keeps them
//! (`0x14fd998` table, filled by the S→C 0x32 handler `0x9d4686`) and flies
//! them for display with `missile_update`. The agent needs them so that the
//! skill ≥ 3 bots it flies can dodge (ai.md "Mobile bots": jink when a
//! missile targeting the bot is within 3 s of the missile's top speed).
//! Hits are the shooter's business: they come back to us as S→C 0x03.

use crate::sector::Sector;
use jumpgate_common::missile::{Aim, Missile};
use jumpgate_common::proto::{self, MissileMsg};
use jumpgate_common::ship::{length, Vec3};
use jumpgate_common::world::World;
use std::collections::HashMap;

/// How close (in seconds at the missile's top speed) counts as a threat.
const THREAT_S: f32 = 3.0;

struct Tracked {
    m: Missile,
    target_pid: u32,
    target_bot: u16,
}

#[derive(Default)]
pub struct Missiles {
    /// (owner, owner's missile id) → missile.
    map: HashMap<(u32, u8), Tracked>,
    rng: u32,
}

impl Missiles {
    pub fn clear(&mut self) {
        self.map.clear();
    }

    #[cfg(test)]
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// A relayed 0x32. Returns true when it is a new missile locked on us
    /// (the client plays its lock warning then).
    pub fn on_message(&mut self, m: &[u8], me: u32) -> bool {
        let Some(MissileMsg { id, ty, target_bot, vel, fwd, target, owner, pos }) = proto::parse_missile(m) else { return false };
        if ty == proto::MISSILE_GONE {
            self.map.remove(&(owner, id));
            return false;
        }
        let fresh = !self.map.contains_key(&(owner, id));
        let mut missile = Missile::new(ty, pos, vel, fwd);
        if let Some(t) = self.map.get(&(owner, id)) {
            missile.age = t.m.age;
        }
        self.map.insert((owner, id), Tracked { m: missile, target_pid: target, target_bot });
        fresh && target == me && target_bot == proto::NOT_A_BOT
    }

    /// One frame of flight for every missile. `me` is our ship (pid, pos,
    /// vel); `bot` gives a bot's position and velocity by id.
    pub fn step(&mut self, dt: f32, world: &World, me: Option<(u32, Vec3, Vec3)>, sector: &Sector, bot: impl Fn(u16) -> Option<(Vec3, Vec3)>) {
        let rng = &mut self.rng;
        self.map.retain(|_, t| {
            let Some(ty) = world.missile_types.get(t.m.ty as usize) else { return false };
            let aim = if t.target_bot != proto::NOT_A_BOT {
                bot(t.target_bot)
            } else if me.is_some_and(|m| m.0 == t.target_pid) {
                me.map(|m| (m.1, m.2))
            } else {
                sector.ships.get(&t.target_pid).map(|o| (o.pos, o.vel))
            }
            .map(|(pos, vel)| Aim { pos, vel, ship: true });
            t.m.step(ty, aim, dt, || {
                *rng = rng.wrapping_mul(214013).wrapping_add(2531011);
                (*rng >> 16) & 0x7fff
            })
        });
    }

    /// Is a missile aimed at bot `id` within 3 s of its top speed of `pos`?
    pub fn threatens_bot(&self, id: u16, pos: Vec3, world: &World) -> bool {
        self.map.values().any(|t| {
            t.target_bot == id
                && world.missile_types.get(t.m.ty as usize).is_some_and(|ty| {
                    let d = [t.m.pos[0] - pos[0], t.m.pos[1] - pos[1], t.m.pos[2] - pos[2]];
                    length(d) < ty.top_speed() * THREAT_S
                })
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::DT;

    #[test]
    fn tracks_flies_and_removes_relayed_missiles() {
        let w = World::load(std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"))).unwrap();
        let mut ms = Missiles::default();
        let mm = MissileMsg {
            id: 3,
            ty: 2, // Rapier, guided
            target_bot: 9,
            vel: [0.0, 0.0, 100.0],
            fwd: [0.0, 0.0, 1.0],
            target: 0,
            owner: 77,
            pos: [0.0; 3],
        };
        assert!(!ms.on_message(&proto::missile(&mm), 1000), "aimed at a bot, not us");
        let bot = |id: u16| (id == 9).then_some(([300.0, 0.0, 4000.0], [0.0; 3]));
        assert!(!ms.threatens_bot(9, [300.0, 0.0, 4000.0], &w), "4 km off: more than 3 s at 701 u/s");
        let mut t = 0.0;
        while !ms.threatens_bot(9, [300.0, 0.0, 4000.0], &w) {
            ms.step(DT, &w, None, &Sector::default(), bot);
            t += DT;
            assert!(t < 6.0, "never closed in");
        }
        assert!(t > 2.0, "closed 1.9 km in {t:.1} s");
        assert!(!ms.threatens_bot(8, [300.0, 0.0, 4000.0], &w), "another bot");
        assert!(ms.on_message(&proto::missile(&MissileMsg { id: 4, target_bot: proto::NOT_A_BOT, target: 1000, ..mm }), 1000));
        ms.on_message(&proto::missile(&MissileMsg { ty: proto::MISSILE_GONE, ..mm }), 1000);
        assert_eq!(ms.len(), 1);
    }
}
