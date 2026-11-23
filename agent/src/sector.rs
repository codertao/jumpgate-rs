//! Other ships in the agent's sector, as the stock client knows them: created
//! and updated from the relayed S→C 0x05 (handler `0x9cf7c8`) and the fast
//! 0x33/0x34, and dead-reckoned in between (physics.md §8). The agent needs
//! them for the soft push between ships (physics.md §6).

use jumpgate_common::proto::{self, u16_at, u32_at};
use jumpgate_common::ship::Vec3;
use std::collections::HashMap;

/// Forget a ship that hasn't reported for this long.
const STALE_S: f32 = 30.0;

#[derive(Clone, Debug, PartialEq)]
pub struct Other {
    pub pos: Vec3,
    pub vel: Vec3,
    /// Slot mass (0x05 @0x68, an integer on the wire).
    pub mass: f32,
    /// Ship type, which is also its model id.
    pub ship_type: u16,
    pub seen: f32,
}

#[derive(Default, Debug)]
pub struct Sector {
    pub ships: HashMap<u32, Other>,
}

fn f32_at(m: &[u8], o: usize) -> f32 {
    proto::f32_at(m, o)
}
fn vec_at(m: &[u8], o: usize) -> Vec3 {
    [f32_at(m, o), f32_at(m, o + 4), f32_at(m, o + 8)]
}
/// Velocity as three i16 × 10.
fn vel_at(m: &[u8], o: usize) -> Vec3 {
    let q = |k: usize| i16::from_le_bytes([m[o + 2 * k], m[o + 2 * k + 1]]) as f32 * 0.1;
    [q(0), q(1), q(2)]
}

impl Sector {
    pub fn clear(&mut self) {
        self.ships.clear();
    }

    /// Take a relayed report. `loc` and `me` are ours: other sectors' reports
    /// and our own echo are dropped, as the client drops them.
    pub fn on_message(&mut self, m: &[u8], loc: u16, me: u32, now: f32) {
        match m.first() {
            Some(0x05) if m.len() == proto::LOC_REPORT_LEN => {
                let pid = u32_at(m, proto::LOC_PID_OFF);
                if pid == me || u16_at(m, proto::LOC_LOC_OFF) != loc {
                    return;
                }
                let ship_type = u16_at(m, proto::LOC_SHIP_OFF);
                if ship_type == proto::SHIP_GONE || ship_type == 0xf9 {
                    self.ships.remove(&pid);
                    return;
                }
                self.ships.insert(
                    pid,
                    Other { pos: vec_at(m, 0x90), vel: vel_at(m, 0x54), mass: u32_at(m, 0x68) as f32, ship_type, seen: now },
                );
            }
            Some(0x33) if m.len() >= proto::SMALL_LOC_LEN => {
                let pid = u32_at(m, 0x18);
                if u16_at(m, 4) != loc {
                    return;
                }
                if let Some(o) = self.ships.get_mut(&pid) {
                    (o.pos, o.vel, o.seen) = (vec_at(m, 0x1c), vel_at(m, 0x12), now);
                }
            }
            Some(0x34) if m.len() >= 0x34 => {
                let pid = u32_at(m, 0x14);
                if u16_at(m, 4) != loc {
                    return;
                }
                if let Some(o) = self.ships.get_mut(&pid) {
                    (o.pos, o.seen) = (vec_at(m, 0x18), now);
                }
            }
            _ => {}
        }
    }

    /// Dead reckoning between reports (linear: enough for the push test), and
    /// expiry of ships that went quiet.
    pub fn advance(&mut self, dt: f32, now: f32) {
        self.ships.retain(|_, o| now - o.seen < STALE_S);
        for o in self.ships.values_mut() {
            for k in 0..3 {
                o.pos[k] += o.vel[k] * dt;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use jumpgate_common::ship::ShipView;

    fn view(pos: Vec3) -> ShipView {
        ShipView {
            loc: 107,
            hull_type: 90,
            callsign: "Other".into(),
            mass: 13500.0,
            pos,
            vel: [10.0, 0.0, 0.0],
            fwd: [1.0, 0.0, 0.0],
            up: [0.0, 1.0, 0.0],
            thrust: 0.0,
            shields: 1.0,
            shields_max: 1.0,
            hull: 1.0,
            rank: 0,
        }
    }

    #[test]
    fn tracks_reports_and_removals() {
        let mut s = Sector::default();
        s.on_message(&proto::ship_report(&view([1.0, 2.0, 3.0]), 5, 0.0), 107, 1, 0.0);
        s.on_message(&proto::ship_report(&view([1.0, 2.0, 3.0]), 1, 0.0), 107, 1, 0.0); // our own
        let mut elsewhere = view([0.0; 3]);
        elsewhere.loc = 108;
        s.on_message(&proto::ship_report(&elsewhere, 6, 0.0), 107, 1, 0.0);
        assert_eq!(s.ships.len(), 1);
        let o = &s.ships[&5];
        assert_eq!((o.pos, o.vel, o.mass, o.ship_type), ([1.0, 2.0, 3.0], [10.0, 0.0, 0.0], 13500.0, 90));
        s.on_message(&proto::ship_small_loc(&view([50.0, 2.0, 3.0]), 5, 1.0), 107, 1, 1.0);
        assert_eq!(s.ships[&5].pos, [50.0, 2.0, 3.0]);
        s.advance(0.5, 1.5);
        assert_eq!(s.ships[&5].pos, [55.0, 2.0, 3.0]);
        s.on_message(&proto::ship_gone(&view([0.0; 3]), 5, 2.0), 107, 1, 2.0);
        assert!(s.ships.is_empty());
        s.on_message(&proto::ship_report(&view([0.0; 3]), 5, 0.0), 107, 1, 0.0);
        s.advance(0.0, 31.0);
        assert!(s.ships.is_empty(), "went quiet");
    }
}
