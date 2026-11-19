//! The client's own-ship flight model (physics.md §3–§4), so an agent moves
//! exactly as a real ship would and other clients' dead reckoning (§8, the
//! same thrust/drag law) tracks it without corrections.

use jumpgate_common::ship::{length, normalize, Hull, ShipView, Vec3};

/// Fixed simulation step. The client runs per frame; 30 Hz is close enough
/// for the drag law and keeps the turn steps small.
pub const DT: f32 = 1.0 / 30.0;

/// Mass the hull carries beyond its own (equipment, armour). The client sums
/// every fitted item's mass (physics.md §5); this approximates a starter
/// loadout until the agent tracks its fittings.
pub const EQUIPMENT_MASS: f32 = 5000.0;

#[derive(Clone, Debug)]
pub struct Flight {
    pub pos: Vec3,
    pub vel: Vec3,
    pub fwd: Vec3,
    pub up: Vec3,
    /// Throttle 0..1 (the client's 0..100 × 0.01).
    pub throttle: f32,
    /// Full thrust: engines × engine thrust.
    pub thrust_max: f32,
    pub mass: f32,
    pub drag: f32,
    /// Turn coefficients (rad/s ≈ 1000 × rate at full stick).
    pub pitch_rate: f32,
    pub yaw_rate: f32,
}

pub fn dot(a: Vec3, b: Vec3) -> f32 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
pub fn sub(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}
pub fn add(a: Vec3, b: Vec3) -> Vec3 {
    [a[0] + b[0], a[1] + b[1], a[2] + b[2]]
}
pub fn scale(a: Vec3, k: f32) -> Vec3 {
    [a[0] * k, a[1] * k, a[2] * k]
}
pub fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

impl Flight {
    pub fn new(hull: &Hull, engine_thrust: f32, pos: Vec3, fwd: Vec3) -> Self {
        let mut f = Flight {
            pos,
            vel: [0.0; 3],
            fwd: normalize(fwd),
            up: [0.0, 1.0, 0.0],
            throttle: 0.0,
            thrust_max: hull.engines.max(1) as f32 * engine_thrust,
            mass: hull.mass + EQUIPMENT_MASS,
            drag: hull.drag,
            pitch_rate: hull.pitch,
            yaw_rate: hull.yaw,
        };
        f.fix_up();
        f
    }

    pub fn speed(&self) -> f32 {
        length(self.vel)
    }

    /// √(thrust / drag): mass cancels out (physics.md §3).
    pub fn top_speed(&self) -> f32 {
        (self.thrust_max / self.drag).sqrt()
    }

    /// Throttle that holds `speed` against drag.
    pub fn throttle_for(&self, speed: f32) -> f32 {
        (self.drag * speed * speed / self.thrust_max).clamp(0.0, 1.0)
    }

    /// Effective thrust (what 0x05 @0xa8 / 0x33 @0x28 carry).
    pub fn thrust(&self) -> f32 {
        self.thrust_max * self.throttle
    }

    /// Re-orthogonalise `up` against `fwd`, keeping it near world up (no roll).
    fn fix_up(&mut self) {
        let world_up = [0.0, 1.0, 0.0];
        let base = if dot(self.fwd, world_up).abs() > 0.98 { self.up } else { world_up };
        let u = sub(base, scale(self.fwd, dot(base, self.fwd)));
        if length(u) > 1e-4 {
            self.up = normalize(u);
        }
    }

    /// Turn the nose toward `want` at the hull's rate, scaled by throttle as
    /// the client does: turnScale = (1.6 − thr) · 0.625 (physics.md §4).
    pub fn turn_toward(&mut self, want: Vec3, dt: f32) {
        let want = normalize(want);
        let rate = self.pitch_rate.min(self.yaw_rate) * 1000.0 * (1.6 - self.throttle) * 0.625;
        let max = rate * dt;
        let c = dot(self.fwd, want).clamp(-1.0, 1.0);
        let angle = c.acos();
        if angle < 1e-5 {
            return;
        }
        let perp = {
            let p = sub(want, scale(self.fwd, c));
            if length(p) < 1e-5 {
                self.up // straight behind: turn over the top
            } else {
                normalize(p)
            }
        };
        let a = angle.min(max);
        self.fwd = normalize(add(scale(self.fwd, a.cos()), scale(perp, a.sin())));
        self.fix_up();
    }

    /// Linear dynamics, `0x429bb0` without afterburner, boost or currents.
    pub fn step(&mut self, dt: f32) {
        let speed = self.speed();
        let mut drag_dec = speed * speed / self.mass * self.drag;
        // Brakes: on whenever the throttle is closed and the ship is moving.
        if self.throttle <= 0.0 && speed > 0.0 {
            drag_dec += self.thrust_max * 0.08 / self.mass;
        }
        if self.throttle > 0.0 || speed > 0.0 {
            if drag_dec * dt <= speed || self.throttle > 0.0 {
                let a = self.thrust_max * self.throttle / self.mass;
                let vhat = if speed > 0.0 { scale(self.vel, 1.0 / speed) } else { [0.0; 3] };
                if dt < 0.6 {
                    self.vel = add(self.vel, scale(sub(scale(self.fwd, a), scale(vhat, drag_dec)), dt));
                }
            } else {
                self.vel = [0.0; 3];
            }
        }
        self.pos = add(self.pos, scale(self.vel, dt));
    }

    pub fn view(&self, callsign: &str, loc: u16, hull: &Hull) -> ShipView {
        ShipView {
            loc,
            hull_type: hull.type_id,
            callsign: callsign.to_string(),
            mass: self.mass,
            pos: self.pos,
            vel: self.vel,
            fwd: self.fwd,
            up: self.up,
            thrust: self.thrust(),
            armour: 1.0,
            armour_max: hull.armour,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn storm() -> Hull {
        Hull {
            type_id: 90,
            name: "Storm".into(),
            mass: 8500.0,
            drag: 13.5,
            armour: 4000.0,
            pitch: 0.001047,
            yaw: 0.001571,
            roll: 0.001571,
            engines: 2,
        }
    }

    #[test]
    fn reaches_top_speed_and_brakes_to_a_stop() {
        let mut f = Flight::new(&storm(), 758_000.0, [0.0; 3], [0.0, 0.0, 1.0]);
        f.throttle = 1.0;
        for _ in 0..30 * 20 {
            f.step(DT);
        }
        assert!((f.speed() - f.top_speed()).abs() < 1.0, "{} vs {}", f.speed(), f.top_speed());
        assert!((f.top_speed() - 335.1).abs() < 0.5);
        f.throttle = 0.0;
        for _ in 0..30 * 60 {
            f.step(DT);
        }
        assert_eq!(f.speed(), 0.0);
    }

    #[test]
    fn turns_at_the_hull_rate() {
        let mut f = Flight::new(&storm(), 758_000.0, [0.0; 3], [0.0, 0.0, 1.0]);
        f.throttle = 0.0; // turnScale 1.0: 1.047 rad/s
        f.turn_toward([1.0, 0.0, 0.0], 1.0);
        let a = dot(f.fwd, [0.0, 0.0, 1.0]).acos();
        assert!((a - 1.047).abs() < 0.01, "{a}");
        assert!(dot(f.up, f.fwd).abs() < 1e-4);
        f.turn_toward([1.0, 0.0, 0.0], 1.0);
        assert!(dot(f.fwd, [1.0, 0.0, 0.0]) > 0.9999, "stops at the target");
    }
}
