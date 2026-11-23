//! Missile flight and guidance, a port of `missile_update` (`0x42ca30`) as
//! notes/ai.md §1 describes it. The same code flies a client's own missiles
//! and, for display, everyone else's (from S→C 0x32).
//!
//! - Drag law as for ships: `v += (thrust/m·fwd − |v|²·drag/m·v̂)·dt`.
//! - Guidance class 0 flies straight. Guided classes steer toward
//!   `d = 2·LOŜ − v̂`; classes 2 and 4 add the target's heading when it is a
//!   ship moving with vx + vy + vz ≥ 15 (a component sum, as the client
//!   does): `d = normalize(2·d + v̂_target)`.
//! - Steering: pitch toward d by atan(d·up / d·fwd), clamped to the pitch
//!   rate × dt (full rate when the target is behind), then yaw the same way
//!   with the right vector, only while the target is in front.
//! - Expiry after the type's lifetime; Grave Robber (type 16) misfires with a
//!   1-in-6000 chance per frame between 1.9 and 2.4 s of flight.
//!
//! Not ported: the Jammer ECM aim offsets, PWD pulses and cluster splits.

use crate::ship::{length, normalize, Vec3};

#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct MissileType {
    /// Turn rates in rad/s (table `+4`, `+8`).
    pub pitch: f32,
    pub yaw: f32,
    pub thrust: f32,
    pub drag: f32,
    pub mass: f32,
    pub damage: f32,
    pub lifetime: f32,
    /// Guidance class (`+0x22`): 0 unguided, 1 pure pursuit, 2/4 with lead,
    /// 3+ proximity fuse.
    pub guidance: u16,
}

impl MissileType {
    /// √(thrust / drag).
    pub fn top_speed(&self) -> f32 {
        if self.drag > 0.0 {
            (self.thrust / self.drag).sqrt()
        } else {
            0.0
        }
    }
}

/// Grave Robber, the misfiring missile.
pub const GRAVE_ROBBER: u8 = 16;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Missile {
    pub ty: u8,
    pub pos: Vec3,
    pub vel: Vec3,
    pub fwd: Vec3,
    pub up: Vec3,
    /// Seconds since launch.
    pub age: f32,
}

fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
fn scale(a: Vec3, k: f32) -> Vec3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

/// What the missile is chasing.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Aim {
    pub pos: Vec3,
    pub vel: Vec3,
    /// A ship (models 30–259), which classes 2 and 4 lead.
    pub ship: bool,
}

impl Missile {
    pub fn new(ty: u8, pos: Vec3, vel: Vec3, fwd: Vec3) -> Missile {
        let fwd = if length(fwd) > 0.0 { normalize(fwd) } else { [0.0, 0.0, 1.0] };
        let base = if dot(fwd, [0.0, 1.0, 0.0]).abs() > 0.98 { [1.0, 0.0, 0.0] } else { [0.0, 1.0, 0.0] };
        let up = normalize(sub(base, scale(fwd, dot(base, fwd))));
        Missile { ty, pos, vel, fwd, up, age: 0.0 }
    }

    /// Rotate fwd (and the axis it turns toward) by `a` radians toward `axis`.
    fn turn(&mut self, axis: Vec3, a: f32, pitch: bool) {
        let (c, s) = (a.cos(), a.sin());
        let f = normalize(add(scale(self.fwd, c), scale(axis, s)));
        if pitch {
            self.up = normalize(sub(scale(self.up, c), scale(self.fwd, s)));
        }
        self.fwd = f;
        // Keep up orthogonal to fwd.
        self.up = normalize(sub(self.up, scale(self.fwd, dot(self.up, self.fwd))));
    }

    /// One frame. `rand` is the CRT's rand() for the misfire roll. Returns
    /// false when the missile is gone (expired or misfired).
    pub fn step(&mut self, t: &MissileType, aim: Option<Aim>, dt: f32, rand: impl FnOnce() -> u32) -> bool {
        if t.guidance != 0 {
            if let Some(a) = aim {
                let to = sub(a.pos, self.pos);
                if length(to) > 0.0 {
                    let los = normalize(to);
                    let vhat = if length(self.vel) > 0.0 { normalize(self.vel) } else { self.fwd };
                    let mut d = sub(scale(los, 2.0), vhat);
                    if matches!(t.guidance, 2 | 4) && a.ship && a.vel[0] + a.vel[1] + a.vel[2] >= 15.0 {
                        d = normalize(add(scale(d, 2.0), normalize(a.vel)));
                    }
                    let df = dot(d, self.fwd);
                    // Pitch toward d (full rate when it is behind).
                    let du = dot(d, self.up);
                    let max = t.pitch * dt;
                    let p = if df > 0.0 { (du / df).atan().clamp(-max, max) } else { max.copysign(du) };
                    let up = self.up;
                    self.turn(up, p, true);
                    // Yaw, only while the target is in front.
                    let df = dot(d, self.fwd);
                    if df > 0.0 {
                        let right = cross(self.up, self.fwd);
                        let max = t.yaw * dt;
                        let y = (dot(d, right) / df).atan().clamp(-max, max);
                        self.turn(right, y, false);
                    }
                }
            }
        }
        let speed = length(self.vel);
        let m = t.mass.max(1.0);
        let vhat = if speed > 0.0 { scale(self.vel, 1.0 / speed) } else { [0.0; 3] };
        let acc = sub(scale(self.fwd, t.thrust / m), scale(vhat, speed * speed * t.drag / m));
        self.vel = add(self.vel, scale(acc, dt));
        self.pos = add(self.pos, scale(self.vel, dt));
        self.age += dt;
        if self.ty == GRAVE_ROBBER && self.age > 1.9 && self.age < 2.4 && rand() % 6000 == 0 {
            return false;
        }
        self.age < t.lifetime
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DT: f32 = 1.0 / 30.0;

    fn spear() -> MissileType {
        MissileType { thrust: 100_000.0, drag: 0.1, mass: 180.0, damage: 6.0e6, lifetime: 8.0, ..Default::default() }
    }
    fn rapier() -> MissileType {
        MissileType { pitch: 1.326, yaw: 1.326, thrust: 90_000.0, drag: 0.183, mass: 150.0, damage: 2.65e6, lifetime: 22.0, guidance: 2 }
    }

    #[test]
    fn unguided_flies_straight_to_top_speed() {
        let t = spear();
        let mut m = Missile::new(0, [0.0; 3], [0.0; 3], [0.0, 0.0, 1.0]);
        let aim = Some(Aim { pos: [500.0, 0.0, 0.0], vel: [0.0; 3], ship: true });
        let mut n = 0;
        while m.step(&t, aim, DT, || 1) {
            n += 1;
        }
        assert!((n as f32 * DT - 8.0).abs() < 0.1, "lives 8 s");
        assert!(m.pos[0].abs() < 1e-3 && m.pos[1].abs() < 1e-3);
        assert!((length(m.vel) - t.top_speed()).abs() < 5.0, "{} vs {}", length(m.vel), t.top_speed());
    }

    #[test]
    fn guided_turns_onto_an_off_axis_target_and_reaches_it() {
        let t = rapier();
        let mut m = Missile::new(2, [0.0; 3], [0.0; 3], [0.0, 0.0, 1.0]);
        let target = [1500.0, 300.0, 1500.0];
        let mut closest = f32::MAX;
        for _ in 0..(10.0 / DT) as usize {
            let aim = Some(Aim { pos: target, vel: [0.0; 3], ship: true });
            assert!(m.step(&t, aim, DT, || 1));
            closest = closest.min(length(sub(target, m.pos)));
        }
        assert!(closest < 20.0, "missed by {closest:.0}");
    }

    #[test]
    fn grave_robber_can_misfire_only_in_its_window() {
        let t = MissileType { lifetime: 10.0, ..spear() };
        let mut m = Missile::new(GRAVE_ROBBER, [0.0; 3], [0.0; 3], [0.0, 0.0, 1.0]);
        let mut alive = true;
        while alive && m.age + DT < 1.9 {
            alive = m.step(&t, None, DT, || 0);
        }
        assert!(alive, "no misfire before 1.9 s");
        assert!(!m.step(&t, None, DT, || 0), "rand() % 6000 == 0 inside the window");
    }
}
