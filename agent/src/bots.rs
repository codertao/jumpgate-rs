//! Stock bots (conflux, faction defenders, turrets) as a stock client handles
//! them (ai.md §2 and "Spawning"; reversing-map "Conflux / bot AI"):
//!
//! - **Spawn order** S→C 0x70 → queued, spawned ~5 s later at a point the
//!   client picks; this client flies it, and it attacks this client. A full
//!   queue (40) or table (50) bounces the order with C→S 0x16.
//! - **Botloc** S→C 0x71 creates or updates a bot. The handler makes the
//!   bot's *target* its controller: target == me means I fly it now and answer
//!   with a botloc at once.
//! - **Flying** a bot: a port of `online_bot_ai` (`0x4338c0`): jink, obstacle
//!   avoidance, attack with lead, conflux hanging back, defenders going home to
//!   dock, plus the bot collision sweep (`0x40bf50`). A botloc goes out after
//!   each of our 0x05 reports.
//! - **Bot fire at our own ship** is resolved here, as the victim's client
//!   does (`0x43a4a0`): nothing goes on the wire until a death.
//! - **Hand-off**: for bots someone else flies, the client compares its own
//!   distance (projected over the bot's first gun's shot lifetime) with the
//!   controller's (`0x43648b`) and sends C→S 0x2C "bxr" when it is closer.
//!
//! Approximations (docs/agents.md "Stock bots"): bots don't collide with
//! ships, turrets aren't placed on player structures, and shots are spheres against our hull's largest extent.

use crate::collide::{self, Surroundings};
use crate::missiles::Missiles;
use crate::physics::{add, cross, dot, scale, sub, Flight};
use crate::sector::Sector;
use jumpgate_common::proto::{self, BotState};
use jumpgate_common::ship::{length, normalize, Hull, Vec3};
use jumpgate_common::world::{BotType, World};
use std::collections::HashMap;

/// `0xac2a2c`: queued spawn orders run this long after arriving.
const SPAWN_DELAY_S: f32 = 4.99;
const QUEUE_MAX: usize = 40;
const TABLE_MAX: usize = 50;
/// Forget someone else's bot after this long without a botloc [ours].
const STALE_S: f32 = 30.0;
/// The client checks each bot for hand-off about once a second (every tenth
/// frame per bot, rate-limited by a clock byte) [approximate].
const HANDOFF_EVERY_S: f32 = 1.0;
/// Jink: the timer is 4 s; we steer off for the first second of it [ours].
const JINK_S: f32 = 4.0;
const JINK_STEER_S: f32 = 1.0;
/// First remote ship slot: bots are named `0x8000 + slot` as killers.
const FIRST_SLOT: u16 = 10;

/// Our own ship, as the bots see it.
#[derive(Clone, Copy, Debug)]
pub struct Me {
    pub pid: u32,
    pub pos: Vec3,
    pub vel: Vec3,
    /// Radius for bot shots (half the hull's largest extent).
    pub radius: f32,
}

/// What a step produced.
#[derive(Default, Debug)]
pub struct Out {
    pub state: Vec<Vec<u8>>,
    pub space: Vec<Vec<u8>>,
    /// Damage our own ship took from bot shots, and the last shooter
    /// (`0x8000 + slot`, as the client records it in `0xbe5ca0`).
    pub hit_me: f32,
    pub attacker: Option<u32>,
}

struct Shot {
    pos: Vec3,
    vel: Vec3,
    life: f32,
    damage: f32,
    slot: u16,
}

/// A bot this client flies.
struct Flown {
    ty: BotType,
    flight: Flight,
    shields: f32,
    hull: f32,
    /// pbotloc `+1`: 0 attack, 1 head home, 2 approach dock, 3 dock, 8 jink.
    state: u8,
    jink_timer: f32,
    jink_dir: Vec3,
    /// Who it turned on after being shot (local aim only; the botloc keeps
    /// naming us).
    aggro: Option<u32>,
    /// (gun index, cooldown).
    guns: Vec<(usize, f32)>,
    range: f32,
    shot_speed: f32,
}

struct Bot {
    st: BotState,
    seen: f32,
    slot: u16,
    flown: Option<Flown>,
    next_check: f32,
}

#[derive(Default)]
pub struct Bots {
    bots: HashMap<u16, Bot>,
    /// (ai_type, spawn#, due time).
    queue: Vec<(u8, u16, f32)>,
    shots: Vec<Shot>,
    rng: u64,
}

enum Act {
    Fly { toward: Vec3, throttle: f32, fire: bool },
    Despawn,
}

impl Bots {
    pub fn new(seed: u64) -> Bots {
        Bots { rng: seed | 1, ..Default::default() }
    }

    fn rand(&mut self) -> u32 {
        self.rng ^= self.rng << 13;
        self.rng ^= self.rng >> 7;
        self.rng ^= self.rng << 17;
        (self.rng >> 33) as u32
    }

    /// `rand()` as the client's CRT gives it: 0..=32767.
    fn crt_rand(&mut self) -> f32 {
        (self.rand() % 32768) as f32
    }

    pub fn flown(&self) -> usize {
        self.bots.values().filter(|b| b.flown.is_some()).count()
    }

    #[cfg(test)]
    pub fn known(&self) -> usize {
        self.bots.len()
    }

    fn free_slot(&self) -> u16 {
        (FIRST_SLOT..).find(|s| !self.bots.values().any(|b| b.slot == *s)).unwrap()
    }

    /// Leaving the sector, docking or dying: queued orders bounce
    /// (`0x9cb930`, `[16][ai_type][spawn#]`) and the bots we fly are removed
    /// (`0x9cb9c0`, `[16][dead][id]`). Returns state-link messages.
    pub fn clear(&mut self) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = self.queue.drain(..).map(|(t, n, _)| proto::bot_gone(t, n)).collect();
        for (id, b) in self.bots.drain() {
            if b.flown.is_some() {
                out.push(proto::bot_gone(0, id));
            }
        }
        self.shots.clear();
        out
    }

    /// S→C 0x70 `[70][ai_type][u16 spawn#]`. Returns the bounce if refused.
    pub fn on_spawn_order(&mut self, m: &[u8], loc: u16, now: f32) -> Option<Vec<u8>> {
        if m.len() < 4 {
            return None;
        }
        let (t, n) = (m[1], proto::u16_at(m, 2));
        // Defenders (ai_type < 11) are refused outside station space.
        if self.queue.len() >= QUEUE_MAX || self.bots.len() + self.queue.len() >= TABLE_MAX || (t < 11 && loc > 200) {
            return Some(proto::bot_gone(t, n));
        }
        self.queue.push((t, n, now + SPAWN_DELAY_S));
        None
    }

    /// S→C 0x71. Returns our botloc if the bot just became ours.
    pub fn on_botloc(&mut self, m: &[u8], me: u32, loc: u16, world: &World, now: f32) -> Option<Vec<u8>> {
        let st = proto::parse_bot_state(m)?;
        if st.loc != loc || !(proto::BOT_SHIP_TYPES.contains(&st.ship_type)) {
            return None;
        }
        let slot = match self.bots.get(&st.id) {
            Some(b) => b.slot,
            None if self.bots.len() >= TABLE_MAX => return None,
            None => self.free_slot(),
        };
        let b = self.bots.entry(st.id).or_insert(Bot { st, seen: now, slot, flown: None, next_check: now + HANDOFF_EVERY_S });
        if st.target == me {
            b.st.controller = me;
            b.st.target = me;
            b.seen = now;
            if b.flown.is_none() {
                b.flown = Flown::new(world, &st);
                b.st = st;
                let f = b.flown.as_ref()?;
                return Some(proto::botloc(&f.state(&b.st, me, loc)));
            }
            return None;
        }
        // Someone else's (perhaps no longer ours).
        b.flown = None;
        b.st = st;
        b.seen = now;
        None
    }

    /// S→C 0x03 for a bot (`0x9d1463`): only one we fly takes damage. It
    /// turns on the attacker; at zero hull it dies: 0x44 then the removal.
    pub fn on_damage(&mut self, bot: u16, attacker: u32, amount: f32, me: u32) -> Vec<Vec<u8>> {
        let Some(b) = self.bots.get_mut(&bot) else { return Vec::new() };
        let Some(f) = b.flown.as_mut() else { return Vec::new() };
        if attacker != me {
            f.aggro = Some(attacker);
        }
        if f.damage(amount) {
            self.bots.remove(&bot);
            return vec![proto::kill_report(bot, me, attacker, 0), proto::bot_gone(1, bot)];
        }
        Vec::new()
    }

    /// Botlocs for the bots we fly (sent after each 0x05).
    pub fn botlocs(&self, me: u32, loc: u16) -> Vec<Vec<u8>> {
        self.bots.values().filter_map(|b| b.flown.as_ref().map(|f| proto::botloc(&f.state(&b.st, me, loc)))).collect()
    }

    /// One client frame for every bot. `me` is None when we aren't flying.
    /// A bot's position and velocity (for missiles chasing it).
    pub fn motion(&self, id: u16) -> Option<(Vec3, Vec3)> {
        self.bots.get(&id).map(|b| match &b.flown {
            Some(f) => (f.flight.pos, f.flight.vel),
            None => (b.st.pos, b.st.vel),
        })
    }

    #[allow(clippy::too_many_arguments)]
    pub fn step(&mut self, dt: f32, now: f32, me: Option<Me>, loc: u16, world: &World, s: &Surroundings, sector: &Sector, missiles: &Missiles) -> Out {
        let mut out = Out::default();
        let Some(me) = me else { return out };
        self.spawn_due(now, me, loc, world, &mut out);

        let dock = world.station(loc).map(|(st, sp)| {
            let (a, b) = (st.to_world(sp.dock_start), st.to_world(sp.dock_end));
            let axis = normalize(sub(b, a));
            (add(b, scale(axis, 300.0)), a)
        });
        let mut dead = Vec::new();
        let mut gone = Vec::new();
        let ids: Vec<u16> = self.bots.keys().copied().collect();
        for id in ids {
            let r = self.rand();
            let b = self.bots.get_mut(&id).unwrap();
            let Some(f) = b.flown.as_mut() else {
                // Someone else's: dead-reckon; forget if silent.
                b.st.pos = add(b.st.pos, scale(b.st.vel, dt));
                if now - b.seen > STALE_S {
                    gone.push((id, false));
                }
                continue;
            };
            let target = match f.aggro.and_then(|p| sector.ships.get(&p)) {
                Some(o) => Some((o.pos, o.vel, false)),
                None => {
                    f.aggro = None;
                    Some((me.pos, me.vel, true))
                }
            };
            let threat = f.ty.skill >= 3 && missiles.threatens_bot(id, f.flight.pos, world);
            match f.think(target.map(|t| (t.0, t.1)), s, dock, dt, r, threat) {
                Act::Despawn => gone.push((id, true)),
                Act::Fly { toward, throttle, fire } => {
                    f.flight.throttle = throttle;
                    f.flight.turn_toward(toward, dt);
                    f.flight.dynamics(dt);
                    let impact = collide::bot_sweep(&mut f.flight, s, dt, f.state == 3);
                    f.flight.integrate(dt);
                    f.shields = (f.shields + f.ty.shield_regen * dt).min(f.ty.shield_max);
                    for g in &mut f.guns {
                        g.1 -= dt;
                    }
                    // Only shots at our own ship matter here: every other
                    // client runs the same AI and resolves its own.
                    if fire && target.is_some_and(|t| t.2) {
                        for g in &mut f.guns {
                            if g.1 <= 0.0 {
                                let gun = world.guns[g.0];
                                g.1 = gun.refire.max(0.05);
                                self.shots.push(Shot {
                                    pos: f.flight.pos,
                                    vel: add(scale(f.flight.fwd, gun.speed), f.flight.vel),
                                    life: gun.lifetime,
                                    damage: gun.damage,
                                    slot: b.slot,
                                });
                            }
                        }
                    }
                    if impact > 0.0 && f.damage(impact) {
                        dead.push(id);
                    }
                }
            }
            b.st.pos = f.flight.pos;
        }
        for id in dead {
            self.bots.remove(&id);
            out.state.push(proto::kill_report(id, me.pid, me.pid, 0));
            out.state.push(proto::bot_gone(1, id));
        }
        for (id, ours) in gone {
            self.bots.remove(&id);
            if ours {
                out.state.push(proto::bot_gone(0, id));
            }
        }
        self.fly_shots(dt, me, &mut out);
        self.handoff(now, me, loc, world, sector, &mut out);
        out
    }

    fn spawn_due(&mut self, now: f32, me: Me, loc: u16, world: &World, out: &mut Out) {
        let due: Vec<(u8, u16, f32)> = self.queue.iter().copied().filter(|q| q.2 <= now).collect();
        self.queue.retain(|q| q.2 > now);
        for (t, n, _) in due {
            let ship_type = 219 + t as u16;
            if self.bots.len() >= TABLE_MAX || world.bot_types.get(&ship_type).is_none() {
                out.state.push(proto::bot_gone(t, n));
                continue;
            }
            let (pos, vel, fwd) = self.spawn_point(t, loc, world, me.pos);
            let st = BotState {
                shields: 1.0,
                hull: 1.0,
                id: n,
                loc,
                ship_type,
                vel,
                fwd,
                up: [0.0, 1.0, 0.0],
                controller: me.pid,
                target: me.pid,
                pos,
                ..Default::default()
            };
            let slot = self.free_slot();
            let flown = Flown::new(world, &st);
            if let Some(f) = &flown {
                out.space.push(proto::botloc(&f.state(&st, me.pid, loc)));
            }
            self.bots.insert(n, Bot { st, seen: now, slot, flown, next_check: now });
        }
    }

    /// Where `spawn_bot` (`0x9cbb60`) puts a new bot (ai.md "Spawning").
    fn spawn_point(&mut self, ai_type: u8, loc: u16, world: &World, me: Vec3) -> (Vec3, Vec3, Vec3) {
        // Defenders: out of the station's launch tube 0, 50 u along it at 50 u/s.
        if ai_type < 11 {
            if let Some((st, sp)) = world.station(loc) {
                if let Some(t) = sp.tubes.iter().find(|t| t.exists()) {
                    let (a, b) = (st.to_world(t.inner), st.to_world(t.outer));
                    let axis = normalize(sub(b, a));
                    return (add(a, scale(axis, 50.0)), scale(axis, 50.0), axis);
                }
            }
        }
        // Near a nav marker (model 304) in a gate slot, below loc 310.
        if loc < 310 {
            if let Some(p) = world.placed(loc).iter().find(|p| p.model == 304 && matches!(p.contact_slot, Some(1..=4))) {
                let mut q = p.pos;
                let mut v = [0.0; 3];
                for k in 0..3 {
                    q[k] += self.crt_rand() % 301.0 - 150.0;
                    v[k] = self.crt_rand() % 150.0;
                }
                let fwd = if length(v) > 0.0 { normalize(v) } else { [0.0, 0.0, 1.0] };
                return (q, v, fwd);
            }
        }
        // Anywhere within about ±16,000 u of us (rand() − 16000 per axis).
        let mut q = me;
        for k in 0..3 {
            q[k] += self.crt_rand() - 16_000.0;
        }
        let fwd = if length(sub(me, q)) > 0.0 { normalize(sub(me, q)) } else { [0.0, 0.0, 1.0] };
        (q, [0.0; 3], fwd)
    }

    fn fly_shots(&mut self, dt: f32, me: Me, out: &mut Out) {
        self.shots.retain_mut(|s| {
            let d = scale(s.vel, dt);
            // Closest approach of this frame's segment to our centre.
            let rel = sub(me.pos, s.pos);
            let len2 = dot(d, d);
            let t = if len2 > 0.0 { (dot(rel, d) / len2).clamp(0.0, 1.0) } else { 0.0 };
            if length(sub(rel, scale(d, t))) <= me.radius {
                out.hit_me += s.damage;
                out.attacker = Some(0x8000 + s.slot as u32);
                return false;
            }
            s.pos = add(s.pos, d);
            s.life -= dt;
            s.life > 0.0
        });
    }

    /// The bxr test (`0x43648b`), for bots another pilot flies: score =
    /// distance − (v · (pos − bot) / distance) · T, T = the lifetime of the
    /// bot's first gun's shot. Ask when our score is inside the type's
    /// hand-off range and the controller's is worse. (The sign means moving
    /// *away* lowers a score; that is what the client computes.)
    fn handoff(&mut self, now: f32, me: Me, loc: u16, world: &World, sector: &Sector, out: &mut Out) {
        for (&id, b) in self.bots.iter_mut() {
            if b.flown.is_some() || now < b.next_check {
                continue;
            }
            b.next_check = now + HANDOFF_EVERY_S;
            let Some(ty) = world.bot_types.get(&b.st.ship_type) else { continue };
            let Some(ctl) = sector.ships.get(&b.st.controller) else { continue };
            let t = ty.weapons.first().and_then(|&g| world.guns.get(g as usize)).map_or(0.0, |g| g.lifetime);
            let score = |p: Vec3, v: Vec3| {
                let d = sub(p, b.st.pos);
                let dist = length(d).max(1e-3);
                dist - dot(v, d) / dist * t
            };
            let mine = score(me.pos, me.vel);
            if mine < ty.handoff_range && score(ctl.pos, ctl.vel) > mine {
                out.space.push(proto::bxr(id, loc, me.pid));
            }
        }
    }
}

impl Flown {
    fn new(world: &World, st: &BotState) -> Option<Flown> {
        let ty = world.bot_types.get(&st.ship_type)?.clone();
        let hull = world.hull(st.ship_type).cloned().unwrap_or_else(|| Hull {
            type_id: st.ship_type,
            mass: ty.mass,
            drag: 12.0,
            pitch: 0.0006,
            yaw: 0.0006,
            roll: 0.0006,
            engines: 1,
            ..Default::default()
        });
        let fwd = if length(st.fwd) > 0.5 { st.fwd } else { [0.0, 0.0, 1.0] };
        let mut flight = Flight::new(&hull, ty.thrust, st.pos, fwd);
        flight.thrust_max = ty.thrust;
        flight.mass = ty.mass.max(1.0);
        flight.vel = st.vel;
        if length(st.up) > 0.5 {
            flight.up = st.up;
        }
        let guns: Vec<(usize, f32)> = ty.weapons.iter().map(|&g| g as usize).filter(|&g| g < world.guns.len()).map(|g| (g, 0.0)).collect();
        let range = guns.iter().map(|&(g, _)| world.guns[g].range()).fold(0.0, f32::max);
        let speeds: Vec<f32> = guns.iter().map(|&(g, _)| world.guns[g].speed).filter(|&v| v > 0.0).collect();
        Some(Flown {
            shields: ty.shield_max * st.shields.clamp(0.0, 1.0),
            hull: ty.hull * if st.hull > 0.0 { st.hull.min(1.0) } else { 1.0 },
            ty,
            flight,
            // Carry on where the previous controller left off.
            state: if matches!(st.state, 0..=3) { st.state } else { 0 },
            jink_timer: 0.0,
            jink_dir: fwd,
            aggro: None,
            guns,
            range: if range > 0.0 { range } else { 1000.0 },
            shot_speed: if speeds.is_empty() { 10_000.0 } else { speeds.iter().sum::<f32>() / speeds.len() as f32 },
        })
    }

    fn state(&self, st: &BotState, me: u32, loc: u16) -> BotState {
        let frac = |v: f32, max: f32| if max > 0.0 { (v / max).clamp(0.0, 1.0) } else { 0.0 };
        BotState {
            state: self.state,
            shields: frac(self.shields, self.ty.shield_max),
            hull: frac(self.hull, self.ty.hull),
            id: st.id,
            loc,
            ship_type: st.ship_type,
            vel: self.flight.vel,
            fwd: self.flight.fwd,
            up: self.flight.up,
            controller: me,
            target: me,
            pos: self.flight.pos,
        }
    }

    /// Shields first, then hull. True when this kills it.
    fn damage(&mut self, amount: f32) -> bool {
        if !(amount > 0.0) || self.hull <= 0.0 {
            return false;
        }
        self.shields -= amount;
        if self.shields < 0.0 {
            self.hull += self.shields;
            self.shields = 0.0;
        }
        self.hull <= 0.0
    }

    /// One tick of the state machine (ai.md "Mobile bots").
    /// `missile_near`: a missile aimed at this bot is within 3 s of it (only
    /// asked for skill ≥ 3).
    fn think(&mut self, target: Option<(Vec3, Vec3)>, s: &Surroundings, dock: Option<(Vec3, Vec3)>, dt: f32, r: u32, missile_near: bool) -> Act {
        let f = &self.flight;
        let right = cross(f.up, f.fwd);
        self.jink_timer -= dt;
        if self.ty.is_turret() {
            // Turrets only slew and shoot.
            let Some((tp, tv)) = target else { return Act::Despawn };
            let (aim, dist) = self.lead(tp, tv);
            let fire = dist < self.range && dot(f.fwd, normalize(sub(aim, f.pos))) > 0.99;
            return Act::Fly { toward: sub(aim, f.pos), throttle: 0.0, fire };
        }
        if self.state == 8 {
            self.state = 0;
        }
        if self.state == 0 && target.is_none() {
            if self.ty.is_defender() && dock.is_some() {
                self.state = 1;
            } else {
                return Act::Despawn;
            }
        }
        match self.state {
            1 => {
                // Coast to a stop, then head for the dock.
                if f.speed() < 1.0 {
                    self.state = 2;
                }
                Act::Fly { toward: f.fwd, throttle: 0.0, fire: false }
            }
            2 | 3 => {
                let Some((approach, inside)) = dock else { return Act::Despawn };
                let goal = if self.state == 2 { approach } else { inside };
                let to = sub(goal, f.pos);
                let dist = length(to);
                if self.state == 2 && dist < 100.0 {
                    self.state = 3;
                }
                if self.state == 3 && dist < 60.0 {
                    return Act::Despawn;
                }
                let want = (dist * 0.25).clamp(20.0, f.top_speed().max(20.0));
                let throttle = if dot(f.fwd, normalize(to)) > 0.9 { f.throttle_for(want) } else { 0.0 };
                Act::Fly { toward: to, throttle, fire: false }
            }
            _ => {
                let (tp, tv) = target.expect("checked above");
                // 1. Jink when shields are low (by 0.3), or, at skill ≥ 3, when
                //    a missile closes in (by 0.4).
                let low = self.shields < 0.7 * self.ty.shield_max;
                if self.jink_timer <= 0.0 && (low || missile_near) {
                    let axis = match r & 3 {
                        0 => f.up,
                        1 => scale(f.up, -1.0),
                        2 => right,
                        _ => scale(right, -1.0),
                    };
                    let k = if missile_near { 0.4 } else { 0.3 };
                    self.jink_dir = normalize(add(f.fwd, scale(axis, k)));
                    self.jink_timer = JINK_S;
                }
                if self.jink_timer > JINK_S - JINK_STEER_S {
                    return Act::Fly { toward: self.jink_dir, throttle: 1.2, fire: false };
                }
                let throttle_boost = if self.jink_timer > 0.0 { 1.2 } else { 1.0 };
                // 2. Obstacle ahead: turn ~26.6° away on the dominant axis.
                let look = if f.speed() < 100.0 { 6.0 } else { 3.0 };
                if f.speed() > 1.0 {
                    if let Some((p, _, _)) = collide::first_contact(s, f.pos, scale(f.vel, look), false) {
                        let dir = normalize(sub(p, f.pos));
                        if dot(dir, normalize(f.vel)) > 0.1 {
                            let (du, dr) = (dot(f.up, dir), dot(right, dir));
                            let away = if du.abs() >= dr.abs() { scale(f.up, -du.signum() * 0.5) } else { scale(right, -dr.signum() * 0.5) };
                            return Act::Fly { toward: add(f.fwd, away), throttle: 1.0, fire: false };
                        }
                    }
                }
                // 3. Attack.
                let (mut aim, dist) = self.lead(tp, tv);
                let mut throttle = throttle_boost;
                if dist >= self.range && self.ty.skill >= 3 {
                    // Flank: 120 u off the target, the axis picked as for jinks.
                    let side = match r & 3 {
                        0 => [0.0, 1.0, 0.0],
                        1 => [0.0, -1.0, 0.0],
                        2 => [1.0, 0.0, 0.0],
                        _ => [-1.0, 0.0, 0.0],
                    };
                    aim = add(tp, scale(side, 120.0));
                } else if dist < self.range && self.ty.is_conflux() {
                    throttle *= if length(tv) < 10.0 { 0.25 } else { 0.5 };
                }
                let to = sub(aim, f.pos);
                let fire = dist < self.range && length(to) > 0.0 && dot(f.fwd, normalize(to)) > 0.99;
                Act::Fly { toward: to, throttle, fire }
            }
        }
    }

    /// Lead aim at a target: t = flight time of a shot, ×½ for odd skill;
    /// minus our own sideways drift. Returns (aim point, distance).
    fn lead(&self, tp: Vec3, tv: Vec3) -> (Vec3, f32) {
        let f = &self.flight;
        let to = sub(tp, f.pos);
        let dist = length(to);
        if dist >= self.range {
            return (tp, dist);
        }
        let mut t = dist / self.shot_speed;
        if self.ty.skill % 2 == 1 {
            t *= 0.5;
        }
        let los = if dist > 0.0 { scale(to, 1.0 / dist) } else { f.fwd };
        let side = sub(f.vel, scale(los, dot(f.vel, los)));
        (sub(add(tp, scale(tv, t)), scale(side, t)), dist)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::DT;
    use std::path::Path;

    fn world() -> World {
        World::load(Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"))).unwrap()
    }

    fn me(pos: Vec3) -> Me {
        Me { pid: 1000, pos, vel: [0.0; 3], radius: 6.0 }
    }

    #[test]
    fn spawn_order_becomes_a_bot_that_hunts_us() {
        let w = world();
        let mut b = Bots::new(7);
        let order = proto::bot_spawn(236, 4).unwrap();
        assert!(b.on_spawn_order(&order, 118, 0.0).is_none());
        let s = Surroundings::empty();
        let sector = Sector::default();
        let me = me([0.0; 3]);
        assert!(b.step(DT, 1.0, Some(me), 118, &w, &s, &sector, &Missiles::default()).space.is_empty(), "not yet");
        let out = b.step(DT, 5.0, Some(me), 118, &w, &s, &sector, &Missiles::default());
        let st = proto::parse_bot_state(&out.space[0]).unwrap();
        assert_eq!((st.id, st.ship_type, st.controller, st.target), (4, 236, 1000, 1000));
        assert!(length(st.pos) < 30_000.0);
        // Fly it for a while: it closes in and shoots us.
        let mut hurt = 0.0;
        let mut t = 5.0;
        while t < 300.0 && hurt == 0.0 {
            t += DT;
            hurt += b.step(DT, t, Some(me), 118, &w, &s, &sector, &Missiles::default()).hit_me;
        }
        assert!(hurt > 0.0, "never hit us in {t:.0} s");
        let st = proto::parse_bot_state(&b.botlocs(1000, 118)[0]).unwrap();
        assert!(length(st.pos) < w.guns[57].range(), "within its gun's range: {:?}", st.pos);
    }

    #[test]
    fn defenders_launch_from_the_station_and_dock_without_a_target() {
        let w = world();
        let mut b = Bots::new(3);
        b.on_spawn_order(&proto::bot_spawn(220, 1).unwrap(), 107, 0.0);
        let s = Surroundings::empty();
        let out = b.step(DT, 5.0, Some(me([0.0; 3])), 107, &w, &s, &Sector::default(), &Missiles::default());
        let st = proto::parse_bot_state(&out.space[0]).unwrap();
        assert!((length(st.vel) - 50.0).abs() < 0.5, "launched at 50 u/s: {:?}", st.vel);
        // Once we're gone (target lost) it heads home and despawns in the dock.
        let f = b.bots.get_mut(&1).unwrap().flown.as_mut().unwrap();
        let mut t = 5.0;
        loop {
            t += DT;
            match f.think(None, &s, w.station(107).map(|(st, sp)| {
                let (a, e) = (st.to_world(sp.dock_start), st.to_world(sp.dock_end));
                (add(e, scale(normalize(sub(e, a)), 300.0)), a)
            }), DT, 0, false)
            {
                Act::Despawn => break,
                Act::Fly { toward, throttle, .. } => {
                    f.flight.throttle = throttle;
                    f.flight.turn_toward(toward, DT);
                    f.flight.step(DT);
                }
            }
            assert!(t < 600.0, "never docked; state {} at {:?}", f.state, f.flight.pos);
        }
    }

    #[test]
    fn botlocs_hand_control_back_and_forth() {
        let w = world();
        let mut b = Bots::new(1);
        let theirs = BotState { id: 9, loc: 118, ship_type: 233, controller: 5, target: 5, pos: [100.0, 0.0, 0.0], ..Default::default() };
        assert!(b.on_botloc(&proto::botloc(&theirs), 1000, 118, &w, 0.0).is_none());
        assert_eq!((b.known(), b.flown()), (1, 0));
        let mine = BotState { controller: 1000, target: 1000, ..theirs };
        let reply = b.on_botloc(&proto::botloc(&mine), 1000, 118, &w, 1.0).expect("answers at once");
        assert_eq!(proto::parse_botloc(&reply).unwrap().controller, 1000);
        assert_eq!(b.flown(), 1);
        assert!(b.on_botloc(&proto::botloc(&theirs), 1000, 118, &w, 2.0).is_none());
        assert_eq!(b.flown(), 0, "handed away");
        assert!(b.on_botloc(&proto::botloc(&BotState { loc: 241, ..mine }), 1000, 118, &w, 3.0).is_none(), "other sector");
    }

    #[test]
    fn closer_than_the_controller_asks_for_the_bot() {
        let w = world();
        let mut b = Bots::new(1);
        let theirs = BotState { id: 9, loc: 118, ship_type: 233, controller: 5, target: 5, pos: [0.0; 3], ..Default::default() };
        b.on_botloc(&proto::botloc(&theirs), 1000, 118, &w, 0.0);
        let mut sector = Sector::default();
        sector.ships.insert(5, crate::sector::Other { pos: [3000.0, 0.0, 0.0], vel: [0.0; 3], mass: 1.0, ship_type: 90, seen: 0.0 });
        let s = Surroundings::empty();
        let out = b.step(DT, 2.0, Some(me([500.0, 0.0, 0.0])), 118, &w, &s, &sector, &Missiles::default());
        assert_eq!(out.space, vec![proto::bxr(9, 118, 1000)]);
        // Further away than the controller: no request.
        let mut b2 = Bots::new(1);
        b2.on_botloc(&proto::botloc(&theirs), 1000, 118, &w, 0.0);
        assert!(b2.step(DT, 2.0, Some(me([5000.0, 0.0, 0.0])), 118, &w, &s, &sector, &Missiles::default()).space.is_empty());
    }

    #[test]
    fn damage_kills_a_flown_bot_with_a_kill_report() {
        let w = world();
        let mut b = Bots::new(1);
        let mine = BotState { id: 2, loc: 118, ship_type: 230, controller: 1000, target: 1000, shields: 1.0, hull: 1.0, ..Default::default() };
        b.on_botloc(&proto::botloc(&mine), 1000, 118, &w, 0.0);
        assert!(b.on_damage(2, 77, 1.0e6, 1000).is_empty(), "shields absorb it");
        let out = b.on_damage(2, 77, 1.0e7, 1000);
        let k = proto::parse_kill(&out[0]).unwrap();
        assert_eq!((k.bot, k.killer), (2, 77));
        assert_eq!(out[1], proto::bot_gone(1, 2));
        assert_eq!(b.known(), 0);
        // Leaving the sector bounces queued orders.
        b.on_spawn_order(&proto::bot_spawn(233, 8).unwrap(), 118, 0.0);
        assert_eq!(b.clear(), vec![proto::bot_gone(233 - 219, 8)]);
    }
}
