//! Station patrol, shaped like the stock defender AI (ai.md §2): launch, fly a
//! loop around the station, come back down the docking axis and dock.
//!
//! Geometry comes from the static extract (`station_points.csv`): the docking
//! tube runs `dock_start` → `dock_end` in model space, and a ship docks just
//! outside `dock_end` (within 60 units of it, below 100 u/s; physics.md §6;
//! the captured dock at Quantar Core ended 59 units past `dock_end`). So the
//! approach comes in along the tube axis from outside.

use crate::physics::{add, dot, scale, sub, Flight};
use jumpgate_common::ship::{length, normalize, Vec3};
use jumpgate_common::world::{Station, StationPoints};

/// Distance at which a waypoint counts as reached.
const WAYPOINT_RADIUS: f32 = 300.0;
/// Dock when this close to the dock point (the client allows < 60) and slow.
const DOCK_RADIUS: f32 = 50.0;
const DOCK_SPEED: f32 = 100.0;
/// Final-approach speed limit, as the Auto-Dock autopilot's state 3 (< 75).
const APPROACH_SPEED: f32 = 70.0;
/// Ring waypoints per lap.
const RING_POINTS: usize = 12;

#[derive(Clone, Debug)]
pub struct Route {
    /// Where the ship appears on launch and which way it faces.
    pub launch: Vec3,
    pub launch_fwd: Vec3,
    /// Point straight out from the launch, where the patrol starts.
    pub exit: Vec3,
    pub ring: Vec<Vec3>,
    /// On the docking axis, `entry_dist` out from the dock point.
    pub entry: Vec3,
    pub dock: Vec3,
    pub radius: f32,
    #[cfg_attr(not(test), allow(dead_code))]
    pub center: Vec3,
}

impl Route {
    /// A loop of `laps` around the station at a radius that clears it.
    pub fn around(st: &Station, sp: &StationPoints, laps: usize) -> Route {
        let launch = st.to_world(sp.launch);
        let dock = st.to_world(sp.dock_end);
        let axis_out = normalize(sub(dock, st.to_world(sp.dock_start)));
        let up = normalize(st.up);
        // Ring plane: through the dock point, perpendicular to the station's up.
        let center = add(st.pos, scale(up, dot(sub(dock, st.pos), up)));
        let flat = |v: Vec3| normalize(sub(v, scale(up, dot(v, up))));
        let launch_out = flat(sub(launch, center));
        let radius = (2.5 * length(sub(launch, st.pos))).max(3000.0);
        let exit = add(launch, scale(launch_out, 800.0));
        let entry = add(dock, scale(axis_out, radius));

        // Ring from the exit's bearing, counter-clockwise about `up`, ending
        // on the entry's bearing after `laps` full turns.
        let e1 = flat(sub(entry, center));
        let e2 = normalize(crate::physics::cross(up, e1));
        let bearing = |v: Vec3| {
            let f = flat(sub(v, center));
            dot(f, e2).atan2(dot(f, e1))
        };
        let start = bearing(exit).rem_euclid(std::f32::consts::TAU);
        let step = std::f32::consts::TAU / RING_POINTS as f32;
        let first = (start / step).ceil() as usize + 1;
        let last = RING_POINTS * laps.max(1) + RING_POINTS;
        let ring = (first..last)
            .map(|k| {
                let a = k as f32 * step;
                add(center, add(scale(e1, radius * a.cos()), scale(e2, radius * a.sin())))
            })
            .collect();
        Route { launch, launch_fwd: launch_out, exit, ring, entry, dock, radius, center }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Launch,
    Patrol(usize),
    Entry,
    Approach,
    Docked,
}

pub struct Patrol {
    pub route: Route,
    pub phase: Phase,
}

/// What the AI wants this step.
pub enum Order {
    Fly { toward: Vec3, throttle: f32 },
    /// Conditions met: send the dock messages.
    Dock,
}

impl Patrol {
    pub fn new(route: Route) -> Self {
        Patrol { route, phase: Phase::Launch }
    }

    pub fn think(&mut self, f: &Flight) -> Order {
        let r = &self.route;
        let cruise = f.top_speed();
        let near = |p: Vec3| length(sub(p, f.pos)) < WAYPOINT_RADIUS;
        loop {
            match self.phase {
                Phase::Launch if near(r.exit) => self.phase = Phase::Patrol(0),
                Phase::Patrol(i) if i >= r.ring.len() => self.phase = Phase::Entry,
                Phase::Patrol(i) if near(r.ring[i]) => self.phase = Phase::Patrol(i + 1),
                Phase::Entry if near(r.entry) => self.phase = Phase::Approach,
                _ => break,
            }
        }
        let fly = |toward: Vec3, speed: f32| {
            // Closed throttle (brakes) when well over the wanted speed.
            let throttle = if f.speed() > speed + 15.0 { 0.0 } else { f.throttle_for(speed.min(cruise)).max(0.05) };
            Order::Fly { toward: sub(toward, f.pos), throttle }
        };
        match self.phase {
            Phase::Launch => fly(r.exit, cruise * 0.5),
            Phase::Patrol(i) => fly(r.ring[i], cruise),
            Phase::Entry => {
                // Arrive at the entry slow enough to line up on the axis.
                let d = length(sub(r.entry, f.pos));
                fly(r.entry, (d * 0.4).clamp(120.0, cruise))
            }
            Phase::Approach => {
                let d = length(sub(r.dock, f.pos));
                if d < DOCK_RADIUS && f.speed() < DOCK_SPEED {
                    self.phase = Phase::Docked;
                    return Order::Dock;
                }
                fly(r.dock, (d * 0.25).clamp(30.0, APPROACH_SPEED))
            }
            Phase::Docked => Order::Fly { toward: f.fwd, throttle: 0.0 },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::DT;
    use jumpgate_common::ship::Hull;

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

    /// Fly the whole patrol at the real Quantar Core geometry: it must dock
    /// within ten minutes and stay near the ring.
    fn fly_patrol(loc: u16) -> (f32, f32, f32) {
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"));
        let w = jumpgate_common::world::World::load(dir).unwrap();
        let (st, sp) = w.station(loc).unwrap();
        let route = Route::around(st, sp, 1);
        let mut f = Flight::new(&storm(), 758_000.0, route.launch, route.launch_fwd);
        let mut ai = Patrol::new(route.clone());
        let mut t = 0.0;
        let mut far = 0.0f32;
        while t < 600.0 {
            match ai.think(&f) {
                Order::Dock => return (t, far, route.radius),
                Order::Fly { toward, throttle } => {
                    f.throttle = throttle;
                    f.turn_toward(toward, DT);
                }
            }
            f.step(DT);
            far = far.max(length(sub(f.pos, route.center)));
            t += DT;
        }
        panic!("no dock after {t}s, phase {:?}, at {:?}", ai.phase, f.pos);
    }

    #[test]
    fn patrols_and_docks_at_quantar_core() {
        let (t, far, radius) = fly_patrol(107);
        eprintln!("docked after {t:.0} s, furthest {far:.0} u, ring {radius:.0} u");
        assert!(t > 30.0, "{t}");
        assert!(far < radius + 1500.0, "{far} vs ring {radius}");
    }

    #[test]
    fn patrols_and_docks_at_quantar_depot() {
        fly_patrol(118);
    }
}
