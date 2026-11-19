//! Fly a horizontal circle at constant speed.

use super::Behaviour;
use crate::ship::{normalize, Hull, ShipView, Vec3};

pub struct Orbit {
    pub center: Vec3,
    pub radius: f32,
    /// Units per second along the circle.
    pub speed: f32,
    /// Current angle (radians); advances by speed/radius per second.
    pub angle: f32,
}

impl Orbit {
    pub fn new(center: Vec3, radius: f32, speed: f32) -> Self {
        Orbit { center, radius, speed, angle: 0.0 }
    }
}

impl Behaviour for Orbit {
    fn step(&mut self, ship: &mut ShipView, hull: &Hull, dt: f32) {
        let w = self.speed / self.radius;
        self.angle = (self.angle + w * dt) % std::f32::consts::TAU;
        let (s, c) = self.angle.sin_cos();
        // Counter-clockwise seen from above (+Y up).
        ship.pos = [self.center[0] + self.radius * c, self.center[1], self.center[2] + self.radius * s];
        let dir = normalize([-s, 0.0, c]);
        ship.vel = [dir[0] * self.speed, 0.0, dir[2] * self.speed];
        ship.fwd = dir;
        ship.up = [0.0, 1.0, 0.0];
        ship.thrust = hull.cruise_thrust(self.speed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ship::length;

    fn hull() -> Hull {
        Hull { type_id: 104, name: "Chinook".into(), mass: 515000.0, drag: 115.0, armour: 62500.0, ..Default::default() }
    }

    fn view() -> ShipView {
        ShipView {
            loc: 118,
            hull_type: 104,
            callsign: "t".into(),
            mass: 515000.0,
            pos: [0.0; 3],
            vel: [0.0; 3],
            fwd: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
            thrust: 0.0,
            armour: 1.0,
            armour_max: 62500.0,
        }
    }

    #[test]
    fn stays_on_circle_at_speed() {
        let mut o = Orbit::new([0.0, 300.0, 0.0], 2500.0, 80.0);
        let mut s = view();
        for _ in 0..1000 {
            o.step(&mut s, &hull(), 0.1);
            let r = length([s.pos[0], 0.0, s.pos[2]]);
            assert!((r - 2500.0).abs() < 0.01);
            assert!((length(s.vel) - 80.0).abs() < 0.01);
            assert_eq!(s.pos[1], 300.0);
        }
        assert_eq!(s.thrust, 115.0 * 80.0 * 80.0);
    }

    /// Re-create the client's dead reckoning for a remote ship (physics.md §8:
    /// thrust along fwd, quadratic drag, pos += v·dt) and the 0x33 correction
    /// (velocity overwritten; errors < 200 u become a 0.1·Δ velocity nudge).
    /// Updates at 5 Hz must keep the drawn ship within a few units of the truth.
    #[test]
    fn client_dead_reckoning_tracks_the_orbit() {
        let h = hull();
        let mut o = Orbit::new([0.0, 300.0, 0.0], 2500.0, 80.0);
        let mut truth = view();
        o.step(&mut truth, &h, 0.0);
        let (mut pos, mut vel) = (truth.pos, truth.vel);
        let frame = 1.0 / 60.0;
        let mut worst: f32 = 0.0;
        for tick in 0..(60 * 120) {
            o.step(&mut truth, &h, frame);
            // Client frame.
            let speed = length(vel);
            let drag = speed * speed / truth.mass * h.drag;
            let vhat = normalize(vel);
            let a = truth.thrust / truth.mass;
            for k in 0..3 {
                vel[k] += (-drag * vhat[k] + a * truth.fwd[k]) * frame;
                pos[k] += vel[k] * frame;
            }
            // 0x33 every 12 frames (5 Hz).
            if tick % 12 == 0 {
                let d: Vec3 = [truth.pos[0] - pos[0], truth.pos[1] - pos[1], truth.pos[2] - pos[2]];
                vel = truth.vel;
                if length(d) < 200.0 {
                    for k in 0..3 {
                        vel[k] += 0.1 * d[k];
                    }
                } else {
                    pos = truth.pos;
                }
            }
            if tick > 60 * 10 {
                let d = [truth.pos[0] - pos[0], truth.pos[1] - pos[1], truth.pos[2] - pos[2]];
                worst = worst.max(length(d));
            }
        }
        assert!(worst < 5.0, "drawn ship strays {worst} u from the orbit");
    }
}
