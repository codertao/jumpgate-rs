//! What the stock client does to its own ship besides flying it, per frame
//! (physics.md §6, `0x40d630` and `0x42a110`):
//!
//! - **Static probe:** the centre and centre ± ½·extent along right/up/fwd,
//!   each swept by v·dt against the meshes of the station (contact slot 6),
//!   gates and beacon (slots 1–5) and world objects (asteroids, debris).
//!   - station: `pos += 0.1n; v = −0.8v + 5n`, damage `0.1·m·|v|²`;
//!   - slots 1–5: mass-ratio bounce `v' = v·e`, e = (m − m₂)/(m + m₂);
//!   - world objects: `v' = e·v + 5n`;
//!   - damage for both bounces is `0.1·m·|Δv|²`, shields then hull.
//! - **Soft push** from other ships within (size_a + size_b)·5/12: no bounce,
//!   no damage. Skipped in a frame with a slot 1–5 contact.
//! - **Jumpgate fields** (model 414): the pull toward the gate within 400, and
//!   the burn after loitering within 375 for 18 s.
//!
//! Steps are per client frame; one `DT` step stands for one frame.

use crate::physics::{add, cross, dot, scale, sub, Flight};
use crate::sector::Sector;
use jumpgate_common::mesh::{self, Model, Models, Pose};
use jumpgate_common::ship::{length, normalize, Vec3};
use jumpgate_common::world::{placed_mass, World};
use std::sync::Arc;

/// The active jumpgate model, whose field pulls and burns.
const PULL_GATE: u16 = 414;
const PULL_RANGE: f32 = 400.0;
const BURN_RANGE: f32 = 375.0;
const BURN_AFTER_S: f32 = 17.99;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Station,
    /// Contact slot 1–5 (gate or beacon).
    Contact,
    /// World-object table entry.
    Object,
}

pub struct Obstacle {
    pub kind: Kind,
    pub pose: Pose,
    pub geom: Arc<Model>,
    pub mass: f32,
    /// Bounding radius around `pose.pos`, for the broad phase.
    pub reach: f32,
}

/// The collidable sector, built once per sortie.
pub struct Surroundings {
    pub obstacles: Vec<Obstacle>,
    /// Positions of the sector's pull gates (model 414).
    pub pull_gates: Vec<Vec3>,
}

/// A model's size as the push test uses it: block 0's radius (`+0x2e0`),
/// which `0xa61f20` sets to the largest extent.
pub fn model_size(models: &Models, model: u16) -> f32 {
    models
        .volumes(model)
        .iter()
        .find(|(k, _)| *k == 0)
        .map_or(0.0, |(_, v)| v.ext.iter().fold(0.0f32, |a, &e| a.max(e.abs())))
}

impl Surroundings {
    pub fn new(world: &World, models: &Models, loc: u16) -> Surroundings {
        let mut obstacles = Vec::new();
        let mut pull_gates = Vec::new();
        for p in world.placed(loc) {
            if p.model == PULL_GATE {
                pull_gates.push(p.pos);
            }
            let geom = models.get(p.model);
            if geom.triangle_count() == 0 {
                continue;
            }
            let reach = models
                .volumes(p.model)
                .iter()
                .map(|(_, v)| length(v.centre) + v.ext.iter().fold(0.0f32, |a, &e| a.max(e.abs())) * 0.5)
                .fold(0.0, f32::max);
            let kind = match p.contact_slot {
                Some(6) => Kind::Station,
                Some(_) => Kind::Contact,
                None => Kind::Object,
            };
            obstacles.push(Obstacle { kind, pose: Pose::new(p.pos, p.fwd, p.up), geom, mass: placed_mass(p), reach });
        }
        Surroundings { obstacles, pull_gates }
    }

    #[cfg(test)]
    pub fn empty() -> Surroundings {
        Surroundings { obstacles: Vec::new(), pull_gates: Vec::new() }
    }
}

/// What one frame of collisions did to the ship.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Frame {
    /// Impact damage (shields first, then hull).
    pub impact: f32,
    /// Gate-burn damage.
    pub burn: f32,
    pub hit: Option<Kind>,
}

/// Per-ship state the fields keep between frames.
#[derive(Clone, Copy, Debug, Default)]
pub struct FieldState {
    /// Seconds spent within burn range of a pull gate.
    pub loiter: f32,
}

/// The probe points: centre, and centre ± ½·extent on each axis.
fn probe_points(f: &Flight, ext: Vec3) -> [Vec3; 7] {
    let right = cross(f.up, f.fwd);
    let mut pts = [f.pos; 7];
    for (k, axis) in [right, f.up, f.fwd].into_iter().enumerate() {
        let h = scale(axis, ext[k] * 0.5);
        pts[1 + 2 * k] = add(f.pos, h);
        pts[2 + 2 * k] = sub(f.pos, h);
    }
    pts
}

/// Nearest contact of any probe point against obstacles of one kind.
fn nearest(f: &Flight, pts: &[Vec3; 7], d: Vec3, s: &Surroundings, kind: Kind, slack: f32) -> Option<(Vec3, f32)> {
    let mut best: Option<(Vec3, f32, f32)> = None;
    let step = length(d);
    for o in s.obstacles.iter().filter(|o| o.kind == kind) {
        if length(sub(f.pos, o.pose.pos)) > o.reach + step + slack {
            continue;
        }
        for &p in pts {
            if let Some(c) = mesh::sweep(&o.geom, &o.pose, p, d) {
                if best.is_none_or(|b| c.t < b.1) {
                    best = Some((c.point, c.t, o.mass));
                }
            }
        }
    }
    best.map(|(p, _, m)| (p, m))
}

/// Nearest contact of the segment `p → p + d` with anything collidable:
/// (point, mass, kind). `skip_station` leaves the station out.
pub fn first_contact(s: &Surroundings, p: Vec3, d: Vec3, skip_station: bool) -> Option<(Vec3, f32, Kind)> {
    let step = length(d);
    let mut best: Option<(f32, Vec3, f32, Kind)> = None;
    for o in &s.obstacles {
        if skip_station && o.kind == Kind::Station || length(sub(p, o.pose.pos)) > o.reach + step {
            continue;
        }
        if let Some(c) = mesh::sweep(&o.geom, &o.pose, p, d) {
            if best.is_none_or(|b| c.t < b.0) {
                best = Some((c.t, c.point, o.mass, o.kind));
            }
        }
    }
    best.map(|(_, p, m, k)| (p, m, k))
}

/// The sweep the client runs for each bot it controls (`0x40bf50`,
/// physics.md §6 "Bots this client controls"): the centre only, first
/// contact, `v' = v·e` with e = (m − m₂)/(m + m₂) against a body at rest,
/// damage `0.1·m·|Δv|²`. The station's slot mass is 0 (e = 1: no bounce),
/// and a docking bot skips it. Bot-vs-ship contacts aren't modelled here.
/// Returns the impact damage.
pub fn bot_sweep(f: &mut Flight, s: &Surroundings, dt: f32, docking: bool) -> f32 {
    let Some((_, m2, _)) = first_contact(s, f.pos, scale(f.vel, dt), docking) else { return 0.0 };
    let m = f.mass;
    let e = (m - m2) / (m + m2);
    let v2 = scale(f.vel, e);
    let dv = sub(v2, f.vel);
    f.vel = v2;
    0.1 * m * dot(dv, dv)
}

/// One frame of the client's collision and field code for its own ship.
/// `ext` is the hull's collision extents; `size` its model size (push test).
pub fn step(f: &mut Flight, ext: Vec3, size: f32, s: &Surroundings, others: &Sector, models: &Models, fs: &mut FieldState, dt: f32) -> Frame {
    let mut out = Frame::default();
    let slack = ext.iter().fold(0.0f32, |a, &e| a.max(e)) * 0.5;
    let pts = probe_points(f, ext);
    let d = scale(f.vel, dt);
    let m = f.mass;

    // Station: hard bounce.
    if let Some((contact, _)) = nearest(f, &pts, d, s, Kind::Station, slack) {
        let n = normalize(sub(f.pos, contact));
        f.pos = add(f.pos, scale(n, 0.1));
        out.impact += 0.1 * m * dot(f.vel, f.vel);
        f.vel = add(scale(f.vel, -0.8), scale(n, 5.0));
        out.hit = Some(Kind::Station);
    }
    // Gates and beacon: mass-ratio bounce off a body at rest.
    let mut contact15 = false;
    if let Some((contact, m2)) = nearest(f, &pts, d, s, Kind::Contact, slack) {
        let n = normalize(sub(f.pos, contact));
        let e = (m - m2) / (m + m2);
        let v2 = scale(f.vel, e);
        out.impact += 0.1 * m * dot(sub(v2, f.vel), sub(v2, f.vel));
        f.vel = v2;
        f.pos = add(f.pos, scale(n, 0.1));
        contact15 = true;
        out.hit.get_or_insert(Kind::Contact);
    }
    // World objects: mass-ratio bounce plus a 5 u/s kick.
    if let Some((contact, m2)) = nearest(f, &pts, d, s, Kind::Object, slack) {
        let n = normalize(sub(f.pos, contact));
        let e = (m - m2) / (m + m2);
        let v2 = add(scale(f.vel, e), scale(n, 5.0));
        out.impact += 0.1 * m * dot(sub(v2, f.vel), sub(v2, f.vel));
        f.vel = v2;
        out.hit.get_or_insert(Kind::Object);
    }
    // Soft push from other ships (players and bots alike).
    if !contact15 {
        for o in others.ships.values() {
            let reach = (size + model_size(models, o.ship_type)) * 5.0 / 12.0;
            let rel = sub(o.pos, f.pos);
            let dist = length(rel);
            if o.ship_type < 0x154 && dist < reach && dist > 0.0 {
                let n = scale(rel, 1.0 / dist);
                f.vel = sub(f.vel, scale(n, 4.0 * dt * o.mass / (m + o.mass)));
                f.pos = sub(f.pos, scale(n, 0.05));
            }
        }
    }
    // Jumpgate pull (per frame, not × dt) and burn.
    let mut near_gate = None::<f32>;
    for &g in &s.pull_gates {
        let r = sub(f.pos, g);
        let dist = length(r);
        if dist > 1.0 && dist < PULL_RANGE {
            let k = (10_000.0 / m) * (1.0 - (dist * std::f32::consts::PI / 200.0).cos());
            f.vel = sub(f.vel, scale(r, k / dist));
        }
        if dist < BURN_RANGE {
            near_gate = Some(near_gate.map_or(dist, |n: f32| n.min(dist)));
        }
    }
    match near_gate {
        Some(dist) => {
            fs.loiter += dt;
            let t = fs.loiter - BURN_AFTER_S;
            if t > 0.0 && t < 110.0 {
                out.burn = (400.0 - dist) * 0.0025 * ((1.0 + t).ln() + t) * dt * 1e7 / (110.0 - t);
            }
        }
        None => fs.loiter = 0.0,
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::physics::DT;
    use jumpgate_common::mesh::{Tri, Volume};
    use jumpgate_common::ship::Hull;
    use std::path::Path;

    fn storm() -> Hull {
        Hull { type_id: 90, mass: 8500.0, drag: 13.5, armour: 2000.0, engines: 2, pitch: 0.001, yaw: 0.001, ext: [11.686, 3.007, 9.632], ..Default::default() }
    }

    /// A wall facing −z at z = 50 (two triangles, wound to face the mover).
    fn wall(kind: Kind, mass: f32) -> Surroundings {
        let tris = [
            Tri([[-100.0, -100.0, 50.0], [-100.0, 100.0, 50.0], [100.0, -100.0, 50.0]]),
            Tri([[100.0, -100.0, 50.0], [-100.0, 100.0, 50.0], [100.0, 100.0, 50.0]]),
        ];
        let vol = Volume::new([1.0, 0.0, 0.0], [0.0, 1.0, 0.0], [400.0; 3], [0.0; 3]);
        let geom = Arc::new(Model::build(&[(0, vol)], &tris));
        Surroundings { obstacles: vec![Obstacle { kind, pose: Pose::at([0.0; 3]), geom, mass, reach: 400.0 }], pull_gates: Vec::new() }
    }

    fn no_models() -> Models {
        Models::load(Path::new("/nonexistent"), None).unwrap()
    }

    fn fly_into(s: &Surroundings) -> (Flight, f32) {
        let mut f = Flight::new(&storm(), 758_000.0, [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]);
        f.vel = [0.0, 0.0, 100.0];
        let (others, models, mut fs) = (Sector::default(), no_models(), FieldState::default());
        let mut damage = 0.0;
        for _ in 0..60 {
            let fr = step(&mut f, storm().ext, 10.0, s, &others, &models, &mut fs, DT);
            damage += fr.impact;
            f.pos = add(f.pos, scale(f.vel, DT));
        }
        (f, damage)
    }

    #[test]
    fn station_bounces_hard_and_hurts() {
        let (f, damage) = fly_into(&wall(Kind::Station, 0.0));
        // −0.8 v + 5 n: back out at 80 + 5 u/s, never through the wall.
        assert!((f.vel[2] + 85.0).abs() < 0.01, "{:?}", f.vel);
        assert!(f.pos[2] < 50.0);
        assert!((damage / (0.1 * f.mass * 100.0 * 100.0) - 1.0).abs() < 1e-4, "{damage}");
    }

    #[test]
    fn gate_bounce_follows_the_mass_ratio() {
        let (f, damage) = fly_into(&wall(Kind::Contact, 50_000.0));
        let e = (f.mass - 50_000.0) / (f.mass + 50_000.0);
        assert!((f.vel[2] - 100.0 * e).abs() < 0.01, "{:?} e={e}", f.vel);
        let dv = 100.0 * (1.0 - e);
        let want = 0.1 * f.mass * dv * dv;
        assert!((damage / want - 1.0).abs() < 1e-4, "{damage} vs {want}");
    }

    #[test]
    fn other_ships_only_nudge() {
        let mut f = Flight::new(&storm(), 758_000.0, [0.0; 3], [0.0, 0.0, 1.0]);
        let mut others = Sector::default();
        others.ships.insert(9, crate::sector::Other { pos: [0.0, 0.0, 3.0], vel: [0.0; 3], mass: 13500.0, ship_type: 90, seen: 0.0 });
        let fr = step(&mut f, storm().ext, 10.0, &Surroundings::empty(), &others, &no_models(), &mut FieldState::default(), DT);
        assert_eq!(fr, Frame::default(), "no damage");
        let m = f.mass;
        assert!((f.vel[2] + 4.0 * DT * 13500.0 / (m + 13500.0)).abs() < 1e-5, "{:?}", f.vel);
        assert!((f.pos[2] + 0.05).abs() < 1e-6);
        // Out of reach: (10 + 0) · 5/12 ≈ 4.2 (no volume for model 90 here).
        others.ships.get_mut(&9).unwrap().pos = [0.0, 0.0, 5.0];
        let before = f.vel;
        step(&mut f, storm().ext, 10.0, &Surroundings::empty(), &others, &no_models(), &mut FieldState::default(), DT);
        assert_eq!(f.vel, before);
    }

    #[test]
    fn pull_gate_draws_in_and_burns_loiterers() {
        let s = Surroundings { obstacles: Vec::new(), pull_gates: vec![[0.0, 0.0, 300.0]] };
        let mut f = Flight::new(&storm(), 758_000.0, [0.0; 3], [0.0, 0.0, 1.0]);
        let (others, models, mut fs) = (Sector::default(), no_models(), FieldState::default());
        step(&mut f, storm().ext, 10.0, &s, &others, &models, &mut fs, DT);
        let k = 10_000.0 / f.mass * (1.0 - (300.0 * std::f32::consts::PI / 200.0).cos());
        assert!((f.vel[2] - k).abs() < 1e-4, "pulled toward the gate: {:?}", f.vel);
        // Hold position within 375 for 20 s: burning after 17.99 s.
        let mut burn = 0.0;
        for _ in 0..(20.0 / DT) as usize {
            f.vel = [0.0; 3];
            burn += step(&mut f, storm().ext, 10.0, &s, &others, &models, &mut fs, DT).burn;
        }
        assert!(burn > 0.0 && fs.loiter > 19.9);
    }
}
