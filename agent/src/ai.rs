//! Station patrol, shaped like the stock defender AI (ai.md §2): launch, fly a
//! loop around the station, then dock the way the stock Auto-Dock does.
//!
//! Geometry comes from the static extract (`station_points.csv`): the docking
//! tube runs `dock_start` → `dock_end` in model space, and ships come in from
//! outside `dock_end`. The approach copies the client's Auto-Dock autopilot
//! (`0x42a110`, physics.md §4):
//! - the **docking beacon** sits on the tube axis, 1500 units out from
//!   `dock_end` (2550 at Quantar Core, loc 107); the autopilot engages within
//!   350 of it;
//! - state 1: bleed off speed to a stop (< 2 u/s);
//! - state 2: turn to face the beacon and creep onto it (< 25–30 u/s) until
//!   within 3 units;
//! - state 3: aim at `dock_start` and go in at < 75–90 u/s; the normal dock
//!   check (inside the tube, < 60 along the axis, < 100 u/s) then fires.

use crate::physics::{add, dot, scale, sub, Flight, Tube};
use jumpgate_common::ship::{length, normalize, Hull, Vec3};
use jumpgate_common::world::{Station, StationPoints};

/// Distance at which a patrol waypoint counts as reached.
const WAYPOINT_RADIUS: f32 = 300.0;
/// The client's Auto-Dock engages within this distance of the beacon.
const BEACON_ENGAGE: f32 = 350.0;
/// "Stopped" for the Auto-Dock's state 1.
const STOPPED: f32 = 2.0;
/// State 2: while lined up and under 25 u/s the client raises the throttle
/// to at most 22 %, cuts it above 30 u/s, and switches to state 3 within 3
/// units (where it zeroes the velocity). We ease off near the beacon instead
/// of zeroing velocity, and accept 10 units (or passing the beacon).
const CREEP_THROTTLE: f32 = 0.22;
const CREEP_SPEED: f32 = 25.0;
const BEACON_REACHED: f32 = 10.0;
/// Room on the axis between the end of the patrol ring and the beacon, to
/// settle onto the axis line before stopping.
const RUN_IN: f32 = 2500.0;
/// Top speed on the run-in along the axis.
const RUN_IN_SPEED: f32 = 150.0;
/// State 3: throttle up below 75, cut above 90.
const FINAL_SPEED: f32 = 70.0;
/// Dock when this close to the tube's outer end (the client allows < 60) and slow.
const DOCK_RADIUS: f32 = 50.0;
const DOCK_SPEED: f32 = 100.0;
/// Ring waypoints per lap.
const RING_POINTS: usize = 12;

/// Beacon distance out from `dock_end`: 1500, or 2550 at Quantar Core
/// (`0x42a1c5`: `cmp loc, 0x6b`).
pub fn beacon_distance(loc: u16) -> f32 {
    if loc == 107 {
        2550.0
    } else {
        1500.0
    }
}

#[derive(Clone, Debug)]
pub struct Route {
    /// Where the ship appears on launch (at rest, in the tube) and which way
    /// it faces (down the tube).
    pub launch: Vec3,
    pub launch_fwd: Vec3,
    pub tube: Tube,
    /// Point straight out from the launch, where the patrol starts.
    pub exit: Vec3,
    pub ring: Vec<Vec3>,
    /// Docking beacon (on the axis, outside the station).
    pub beacon: Vec3,
    /// Tube's outer end in sector coordinates, and the unit axis pointing out.
    pub dock: Vec3,
    pub axis_out: Vec3,
    pub radius: f32,
    #[cfg_attr(not(test), allow(dead_code))]
    pub center: Vec3,
}

impl Route {
    /// A loop of `laps` around the station at a radius that clears it and the
    /// beacon, launching from tube `tube` (S→C 0x08 @2) as the client does
    /// (`0x9d34c8`): at rest at the tube's inner end + dir × launch depth,
    /// nose down the tube.
    pub fn around(st: &Station, sp: &StationPoints, loc: u16, laps: usize, tube: u8, hull: &Hull) -> Route {
        let t = sp
            .tubes
            .get(tube as usize)
            .filter(|t| t.exists())
            .or_else(|| sp.tubes.iter().find(|t| t.exists()))
            .copied()
            .unwrap_or_default();
        let (t_in, t_out) = (st.to_world(t.inner), st.to_world(t.outer));
        let tube_dir = normalize(sub(t_out, t_in));
        let launch = add(t_in, scale(tube_dir, hull.launch_depth()));
        let tube = Tube { start: t_in, dir: tube_dir, ship_size: hull.size };
        let dock = st.to_world(sp.dock_end);
        let dock_start = st.to_world(sp.dock_start);
        let axis_out = normalize(sub(dock, dock_start));
        let beacon = add(dock, scale(axis_out, beacon_distance(loc)));
        let up = normalize(st.up);
        // Ring plane: through the dock point, perpendicular to the station's up.
        let center = add(st.pos, scale(up, dot(sub(dock, st.pos), up)));
        let flat = |v: Vec3| normalize(sub(v, scale(up, dot(v, up))));
        // Out of the tube, keep going straight until clear of the station's
        // sphere (a tube pointing inwards would be an extract error).
        let launch_out = if dot(tube_dir, sub(launch, st.pos)) > 0.0 { tube_dir } else { flat(sub(launch, center)) };
        let radius = (2.5 * length(sub(launch, st.pos)))
            .max(1.5 * st.radius)
            .max(length(sub(beacon, center)) + RUN_IN)
            .max(3000.0);
        let exit = {
            // Smallest k ≥ 800 with |launch + k·out − pos| ≥ station radius + 300.
            let d = sub(launch, st.pos);
            let (bq, cq) = (dot(d, launch_out), dot(d, d) - (st.radius + 300.0).powi(2));
            let k = -bq + (bq * bq - cq).max(0.0).sqrt();
            add(launch, scale(launch_out, k.max(800.0)))
        };

        // Ring from the exit's bearing, counter-clockwise about `up`, ending
        // on the docking axis's bearing after `laps` full turns.
        let e1 = flat(axis_out);
        let e2 = normalize(crate::physics::cross(up, e1));
        let bearing = |v: Vec3| {
            let f = flat(sub(v, center));
            dot(f, e2).atan2(dot(f, e1))
        };
        let start = bearing(exit).rem_euclid(std::f32::consts::TAU);
        let step = std::f32::consts::TAU / RING_POINTS as f32;
        let first = (start / step).ceil() as usize + 1;
        let last = RING_POINTS * laps.max(1) + RING_POINTS;
        let ring = (first..=last)
            .map(|k| {
                let a = k as f32 * step;
                add(center, add(scale(e1, radius * a.cos()), scale(e2, radius * a.sin())))
            })
            .collect();
        Route { launch, launch_fwd: tube_dir, tube, exit, ring, beacon, dock, axis_out, radius, center }
    }

    /// Distance from `p` to the docking axis line.
    pub fn off_axis(&self, p: Vec3) -> f32 {
        let d = sub(p, self.dock);
        length(sub(d, scale(self.axis_out, dot(d, self.axis_out))))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Launch,
    Patrol(usize),
    /// Fly in along the axis towards the beacon, slowing down.
    ToBeacon,
    /// Auto-Dock state 1: stop.
    Stop,
    /// Auto-Dock state 2: face the beacon and creep onto it.
    Creep,
    /// Auto-Dock state 3: down the tube.
    Final,
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
        let dist = |p: Vec3| length(sub(p, f.pos));
        let near = |p: Vec3| dist(p) < WAYPOINT_RADIUS;
        loop {
            match self.phase {
                Phase::Launch if near(r.exit) => self.phase = Phase::Patrol(0),
                Phase::Patrol(i) if i >= r.ring.len() => self.phase = Phase::ToBeacon,
                Phase::Patrol(i) if near(r.ring[i]) => self.phase = Phase::Patrol(i + 1),
                Phase::ToBeacon if dist(r.beacon) < BEACON_ENGAGE => self.phase = Phase::Stop,
                Phase::Stop if f.speed() < STOPPED => self.phase = Phase::Creep,
                Phase::Creep
                    if dist(r.beacon) < BEACON_REACHED
                        || (dist(r.beacon) < 3.0 * BEACON_REACHED && dot(sub(r.beacon, f.pos), f.fwd) < 0.0) =>
                {
                    self.phase = Phase::Final
                }
                _ => break,
            }
        }
        // Throttle for a wanted speed: closed (brakes) when well over it.
        let throttle = |speed: f32| {
            if f.speed() > speed + 10.0 {
                0.0
            } else {
                f.throttle_for(speed.min(cruise)).max(0.02)
            }
        };
        // Turning tightens as the throttle closes (turnScale, physics.md §4):
        // full throttle turns a Storm on an ~850 u radius, so ease off when
        // the target is well off the nose rather than orbit it.
        let fly = |toward: Vec3, speed: f32| {
            let to = sub(toward, f.pos);
            let speed = if dot(normalize(to), f.fwd) < 0.85 { speed.min(150.0) } else { speed };
            Order::Fly { toward: to, throttle: throttle(speed) }
        };
        match self.phase {
            // In the tube: hold the nose straight (steering would take the
            // ship out of the tube and off the push). Then on to the exit
            // point at cruise, keeping the catapult's speed.
            Phase::Launch if f.tube.is_some() => Order::Fly { toward: r.launch_fwd, throttle: throttle(cruise) },
            Phase::Launch => fly(r.exit, cruise),
            // Slow into the last ring point, where the 90° turn onto the
            // docking axis happens, so the turn stays tight.
            Phase::Patrol(i) if i + 1 == r.ring.len() => fly(r.ring[i], (dist(r.ring[i]) * 0.15).clamp(120.0, cruise)),
            Phase::Patrol(i) => fly(r.ring[i], cruise),
            Phase::ToBeacon => {
                let mut speed = (dist(r.beacon) * 0.2).clamp(40.0, RUN_IN_SPEED);
                if r.off_axis(f.pos) > 100.0 {
                    speed = speed.min(100.0);
                }
                follow_axis(r, f, speed, throttle(speed))
            }
            Phase::Stop => Order::Fly { toward: sub(r.beacon, f.pos), throttle: 0.0 },
            Phase::Creep => {
                let to = sub(r.beacon, f.pos);
                let aligned = dot(normalize(to), f.fwd) > 0.9999;
                let want = (dist(r.beacon) * 0.6).clamp(3.0, CREEP_SPEED);
                let thr = if !aligned || f.speed() > want + 5.0 {
                    0.0
                } else if f.speed() < want {
                    CREEP_THROTTLE
                } else {
                    f.throttle_for(want)
                };
                Order::Fly { toward: to, throttle: thr }
            }
            Phase::Final => {
                if dist(r.dock) < DOCK_RADIUS && f.speed() < DOCK_SPEED {
                    self.phase = Phase::Docked;
                    return Order::Dock;
                }
                // Down the tube towards dock_start, holding the axis line.
                // No thrust until roughly lined up, so a turn-around at the
                // beacon doesn't drift us off it.
                let aligned = dot(f.fwd, scale(r.axis_out, -1.0)) > 0.99;
                let thr = if f.speed() > 90.0 || !aligned { 0.0 } else { f.throttle_for(FINAL_SPEED) };
                follow_axis(r, f, FINAL_SPEED, thr)
            }
            Phase::Docked => Order::Fly { toward: f.fwd, throttle: 0.0 },
        }
    }
}

/// Fly inward along the docking axis at `speed`, converging onto the line.
/// Wanted velocity = along the axis plus a sideways component proportional to
/// the error; the nose points at that plus a correction for the current
/// velocity error, which damps the swing across the line.
fn follow_axis(r: &Route, f: &Flight, speed: f32, throttle: f32) -> Order {
    let rel = sub(f.pos, r.dock);
    let lat = sub(rel, scale(r.axis_out, dot(rel, r.axis_out)));
    let off = length(lat);
    let side = scale(lat, -(0.5f32).min(0.7 * speed / off.max(1.0)));
    let want = add(scale(r.axis_out, -speed), side);
    let heading = add(want, scale(sub(want, f.vel), 1.5));
    Order::Fly { toward: heading, throttle }
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
            tube_offset: 15.0,
            size: 5.74,
        }
    }

    /// Fly the whole patrol at the real Quantar Core geometry: it must dock
    /// within ten minutes and stay near the ring.
    fn fly_patrol(loc: u16, tube: u8) -> (f32, f32, f32) {
        let dir = std::path::Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"));
        let w = jumpgate_common::world::World::load(dir).unwrap();
        let (st, sp) = w.station(loc).unwrap();
        let route = Route::around(st, sp, loc, 1, tube, &storm());
        let mut f = Flight::new(&storm(), 758_000.0, route.launch, route.launch_fwd);
        f.tube = Some(route.tube);
        let mut left_tube = None;
        let mut ai = Patrol::new(route.clone());
        let mut t = 0.0;
        let mut far = 0.0f32;
        while t < 900.0 {
            // Never inside the station's bounding sphere except in the tube
            // (launch excepted: the launch point itself is inside it).
            if ai.phase != Phase::Launch && length(sub(f.pos, st.pos)) < st.radius {
                assert!(route.off_axis(f.pos) < 30.0, "inside station off-axis at {:?} ({:?}), {:.0} off", f.pos, ai.phase, route.off_axis(f.pos));
            }
            if ai.phase == Phase::Final {
                assert!(route.off_axis(f.pos) < 30.0, "final approach {:.0} off the axis at {t:.1}s, speed {:.1}", route.off_axis(f.pos), f.speed());
            }
            if ai.phase == Phase::ToBeacon && ((t / DT).round() as i64) % 30 == 0 && std::env::var_os("AI_TRACE").is_some() {
                let along = dot(sub(f.pos, route.dock), route.axis_out);
                eprintln!("  t={t:6.1} along {along:6.0} off {:5.0} speed {:4.0} fwd·axis {:.2}", route.off_axis(f.pos), f.speed(), dot(f.fwd, route.axis_out));
            }
            let before = ai.phase;
            let order = ai.think(&f);
            if ai.phase != before && !matches!(ai.phase, Phase::Patrol(_)) {
                eprintln!(
                    "{loc} t={t:6.1} {before:?} → {:?}: speed {:.0}, {:.0} from beacon, {:.0} off axis",
                    ai.phase,
                    f.speed(),
                    length(sub(f.pos, route.beacon)),
                    route.off_axis(f.pos)
                );
            }
            match order {
                Order::Dock => return (t, far, route.radius),
                Order::Fly { toward, throttle } => {
                    f.throttle = throttle;
                    f.turn_toward(toward, DT);
                }
            }
            f.step(DT);
            if left_tube.is_none() && f.tube.is_none() {
                left_tube = Some(f.speed());
                eprintln!("{loc} tube {tube}: out of the tube at t={t:.2}s, {:.0} u/s", f.speed());
                assert!(f.speed() > 150.0, "catapulted out at only {:.0} u/s", f.speed());
            }
            far = far.max(length(sub(f.pos, route.center)));
            t += DT;
        }
        panic!("no dock after {t}s, phase {:?}, at {:?}", ai.phase, f.pos);
    }

    #[test]
    fn patrols_and_docks_at_quantar_core() {
        for tube in 0..4 {
            let (t, far, radius) = fly_patrol(107, tube);
            eprintln!("tube {tube}: docked after {t:.0} s, furthest {far:.0} u, ring {radius:.0} u");
            assert!(t > 30.0, "{t}");
            assert!(far < radius + 1500.0, "{far} vs ring {radius}");
        }
    }

    #[test]
    fn patrols_and_docks_at_quantar_depot() {
        for tube in 0..4 {
            fly_patrol(118, tube);
        }
    }
}
