//! Ship feed: turns server-driven ships into per-viewer wire messages.
//!
//! It knows nothing about where ships come from (NPC sim, or anything else) and
//! nothing about sessions: the caller passes who is viewing from which sector,
//! and which ships exist, every frame. The feed remembers what each viewer has
//! been shown and emits:
//! - S→C 0x05 the first time a viewer sees a ship, and every `FULL_EVERY`
//!   frames after (a real client sends its 0x05 every 5–13.6 s; it also
//!   recovers a viewer that missed the first one),
//! - S→C 0x33 on the other frames,
//! - a "gone" 0x05 (ship type 0xfa) when a ship leaves the viewer's sector or
//!   stops existing.
//!
//! Interest management and the 190-slot budget (reversing-map "Ship slot
//! allocation") belong here once there are more ships than a client can hold.

use crate::proto;
use crate::ship::ShipView;
use std::collections::HashMap;

pub type ViewerId = u32;
/// Player id the client sees for a ship (0x05 @0x5c, 0x33 @0x18).
pub type WirePid = u32;

/// Frames between full 0x05 reports per viewer (at 5 Hz: 5 s).
pub const FULL_EVERY: u64 = 25;

struct Shown {
    last_full: u64,
    last: ShipView,
}

#[derive(Default)]
pub struct Feed {
    shown: HashMap<(ViewerId, WirePid), Shown>,
    frame: u64,
}

impl Feed {
    pub fn new() -> Self {
        Self::default()
    }

    /// One frame. `clock` is seconds and must increase every call: the client
    /// drops 0x05/0x33 whose clock isn't newer than the last one for that ship.
    pub fn update(
        &mut self,
        viewers: &[(ViewerId, u16)],
        ships: &[(WirePid, &ShipView)],
        clock: f32,
    ) -> Vec<(ViewerId, Vec<u8>)> {
        self.frame += 1;
        let frame = self.frame;
        let mut out = Vec::new();

        // Forget viewers who have gone (left space, docked, logged out). Their
        // client drops its contact table on a sector change.
        self.shown.retain(|(v, _), _| viewers.iter().any(|(id, _)| id == v));

        for &(viewer, loc) in viewers {
            for &(pid, ship) in ships {
                if ship.loc != loc {
                    continue;
                }
                match self.shown.get_mut(&(viewer, pid)) {
                    None => {
                        out.push((viewer, proto::ship_report(ship, pid, clock)));
                        self.shown.insert((viewer, pid), Shown { last_full: frame, last: ship.clone() });
                    }
                    Some(s) => {
                        if frame - s.last_full >= FULL_EVERY {
                            out.push((viewer, proto::ship_report(ship, pid, clock)));
                            s.last_full = frame;
                        } else {
                            out.push((viewer, proto::ship_small_loc(ship, pid, clock)));
                        }
                        s.last = ship.clone();
                    }
                }
            }
            // Ships this viewer was shown that aren't here any more.
            let gone: Vec<WirePid> = self
                .shown
                .keys()
                .filter(|(v, p)| {
                    *v == viewer && !ships.iter().any(|(sp, s)| sp == p && s.loc == loc)
                })
                .map(|&(_, p)| p)
                .collect();
            for pid in gone {
                let s = self.shown.remove(&(viewer, pid)).unwrap();
                out.push((viewer, proto::ship_gone(&s.last, pid, clock)));
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ship(loc: u16) -> ShipView {
        ShipView {
            loc,
            hull_type: 104,
            callsign: "Hauler".into(),
            mass: 515000.0,
            pos: [1.0, 2.0, 3.0],
            vel: [0.0, 0.0, 80.0],
            fwd: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
            thrust: 736000.0,
            armour: 1.0,
            armour_max: 62500.0,
        }
    }

    #[test]
    fn full_first_then_fast_then_periodic_full() {
        let mut f = Feed::new();
        let s = ship(118);
        let types = |out: Vec<(u32, Vec<u8>)>| out.iter().map(|(_, m)| m[0]).collect::<Vec<_>>();
        assert_eq!(types(f.update(&[(1, 118)], &[(900, &s)], 1.0)), vec![0x05]);
        for i in 1..FULL_EVERY {
            assert_eq!(types(f.update(&[(1, 118)], &[(900, &s)], 1.0 + i as f32)), vec![0x33]);
        }
        assert_eq!(types(f.update(&[(1, 118)], &[(900, &s)], 100.0)), vec![0x05]);
    }

    #[test]
    fn only_viewers_in_the_sector_and_removal_on_leave() {
        let mut f = Feed::new();
        let here = ship(118);
        let out = f.update(&[(1, 118), (2, 241)], &[(900, &here)], 1.0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].0, 1);
        // The ship moves to another sector: viewer 1 gets a removal.
        let away = ship(241);
        let out = f.update(&[(1, 118), (2, 241)], &[(900, &away)], 2.0);
        let to1: Vec<_> = out.iter().filter(|(v, _)| *v == 1).collect();
        assert_eq!(to1.len(), 1);
        assert_eq!(proto::u16_at(&to1[0].1, proto::LOC_SHIP_OFF), proto::SHIP_GONE);
        // ...and viewer 2 now sees it.
        assert!(out.iter().any(|(v, m)| *v == 2 && m[0] == 0x05 && proto::u16_at(m, proto::LOC_SHIP_OFF) == 104));
    }

    #[test]
    fn viewer_that_leaves_gets_a_fresh_full_report_on_return() {
        let mut f = Feed::new();
        let s = ship(118);
        f.update(&[(1, 118)], &[(900, &s)], 1.0);
        f.update(&[], &[(900, &s)], 2.0);
        let out = f.update(&[(1, 118)], &[(900, &s)], 3.0);
        assert_eq!(out[0].1[0], 0x05);
    }
}
