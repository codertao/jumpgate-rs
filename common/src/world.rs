//! Static world data loaded from `notes/extracts/` (all extracted from the
//! client binary by `notes/tools/`, so server and client agree by construction).
//!
//! - `locations.csv`: location id → name.
//! - `sector_objects_all.csv`: the gate graph. Each jumpgate row has
//!   `gate_number` (1–4, also its contact slot), `gate_dest_loc` and
//!   `gate_dest_gate` (the arrival gate). From the client's link table at
//!   `0x146d7a0`, filled by `0x4a7460`. Verified 2026-11-17 against all three
//!   jumps in the capture: dest loc and the 0x08 sub byte (= arrival gate) match.
//! - `ship_types.csv`: hull stats (from `init_ship_types` `0xa08440`).
//! - `sector_objects_all.csv` also gives each sector's station (the object in
//!   contact slot 6) with its model, position and orientation.
//! - `station_points.csv`: per station model, the docking tube (`+0x7f4` →
//!   `+0x814`) and up to four launch tubes (inner end `+0x834 + 0x20k`, outer
//!   end `+0x8b4 + 0x20k`), in model space.
//! - `engines.csv`: engine thrust by index.
//! - `guns.csv`, `missiles.csv`, `shields.csv`: per-index weapon damage, gun
//!   magazine size, and shield regen/capacity (index = position in the
//!   category, the byte fittings and the 0x1D weapon field use).

use crate::ship::{Hull, Shield, Vec3};
use std::collections::HashMap;
use std::io;
use std::path::Path;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GateLink {
    pub dest_loc: u16,
    /// Gate number in the destination sector; the client spawns the ship at
    /// contact slot `dest_gate` (0x08 sub @1).
    pub dest_gate: u8,
}

#[derive(Default)]
pub struct World {
    pub names: HashMap<u16, String>,
    /// Location id → faction (`faction` column: 1 Solrain, 2 Octavius, 3 Quantar, …).
    pub factions: HashMap<u16, u8>,
    /// (loc, gate_number) → link.
    pub gates: HashMap<(u16, u8), GateLink>,
    /// Ship type id → hull stats.
    pub hulls: HashMap<u16, Hull>,
    /// Location id → the sector's station (station sectors only).
    pub stations: HashMap<u16, Station>,
    /// Station model id → docking and launch points (model space).
    pub station_points: HashMap<u16, StationPoints>,
    /// Engine index → thrust per engine.
    pub engine_thrust: Vec<f32>,
    /// Gun index → stats.
    pub guns: Vec<Gun>,
    /// Missile index → damage per direct hit.
    pub missile_damage: Vec<f32>,
    /// Missile index → flight and guidance stats (missiles.csv).
    pub missile_types: Vec<crate::missile::MissileType>,
    /// Shield index → stats.
    pub shields: Vec<Shield>,
    /// Location id → everything placed in the sector that can be collided
    /// with: contact slots 1–6 (gates, beacon, station) and world objects
    /// (asteroids, debris).
    pub objects: HashMap<u16, Vec<Placed>>,
    /// Bot ship type (220–259) → the client's AI-table row (ai_types.csv).
    pub bot_types: HashMap<u16, BotType>,
}

/// One row of the client's per-type bot table (`0xbe7f7c + type·0x40`,
/// ai.md "Per-type AI table").
#[derive(Clone, Debug, PartialEq, Default)]
pub struct BotType {
    pub type_id: u16,
    /// Thrust at full throttle.
    pub thrust: f32,
    pub mass: f32,
    pub hull: f32,
    pub shield_max: f32,
    /// Shield points per second.
    pub shield_regen: f32,
    /// The "bxr" hand-off range (`+0x14`).
    pub handoff_range: f32,
    /// Gun indices of the fitted weapon slots (empty slots left out; some
    /// slots the extractor couldn't resolve are missing too).
    pub weapons: Vec<u8>,
    /// 1–4: ≥ 3 dodges and flanks; even = full lead, odd = half lead.
    pub skill: u8,
}

impl BotType {
    /// Faction defenders (220–225), which go home and dock when their target
    /// is gone instead of despawning.
    pub fn is_defender(&self) -> bool {
        (220..=225).contains(&self.type_id)
    }
    /// Conflux (230–257), which hang around a target inside weapon range.
    pub fn is_conflux(&self) -> bool {
        (230..=257).contains(&self.type_id)
    }
    /// Turrets (258–259) never move.
    pub fn is_turret(&self) -> bool {
        self.thrust == 0.0
    }
}

/// One placed object (sector_objects_all.csv).
#[derive(Clone, Debug, PartialEq)]
pub struct Placed {
    /// Contact slot 1–6 (`nav` 1), or None for a world-object table entry.
    pub contact_slot: Option<u8>,
    pub model: u16,
    pub pos: Vec3,
    pub fwd: Vec3,
    pub up: Vec3,
}

/// Masses the client gives placed objects (`0x4ba420`): gate model 11 weighs
/// 50,000; beacons, asteroids (15–29) and huge rocks (305–319) 500,000, and
/// so does every world-object table entry. Other contact models get none (0).
pub fn placed_mass(p: &Placed) -> f32 {
    match (p.contact_slot, p.model) {
        (None, _) => 500_000.0,
        (Some(_), 11) => 50_000.0,
        (Some(_), 2 | 15..=29 | 305..=319) => 500_000.0,
        _ => 0.0,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct Gun {
    /// Shield/hull points per hit (weapon +0x50).
    pub damage: f32,
    /// Magazine size; 0 = energy weapon.
    pub ammo: i32,
    /// Projectile lifetime in seconds (`+0x58`), projectile speed (`+0x5c`)
    /// and refire interval (`+0x60`).
    pub lifetime: f32,
    pub speed: f32,
    pub refire: f32,
}

impl Gun {
    /// How far a shot carries: speed × lifetime.
    pub fn range(&self) -> f32 {
        self.speed * self.lifetime
    }
}

/// A station placed in a sector.
#[derive(Clone, Debug, PartialEq)]
pub struct Station {
    pub model: u16,
    /// Bounding radius of the model (sector_objects_all.csv `model_radius`).
    pub radius: f32,
    pub pos: Vec3,
    pub fwd: Vec3,
    pub up: Vec3,
}

/// Points hard-coded per station model in the client's model loaders.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct StationPoints {
    /// Docking tube, from `+0x7f4` to `+0x814`. A docking ship comes in from
    /// beyond `dock_end` (captured dock at Quantar Core: just past the end).
    pub dock_start: Vec3,
    pub dock_end: Vec3,
    /// Launch tubes 0–3 (S→C 0x08 @2 picks one). A tube whose ends coincide
    /// doesn't exist on that model.
    pub tubes: [LaunchTube; 4],
}

/// A launch tube, inner end → outer end (model space).
#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct LaunchTube {
    pub inner: Vec3,
    pub outer: Vec3,
}

impl LaunchTube {
    pub fn exists(&self) -> bool {
        self.inner != self.outer
    }
}

impl StationPoints {
    /// Numbers of the tubes this model has.
    pub fn launch_tubes(&self) -> Vec<u8> {
        (0..4u8).filter(|&k| self.tubes[k as usize].exists()).collect()
    }
}

impl Station {
    /// Model-space point → sector coordinates (right = up × fwd).
    pub fn to_world(&self, p: Vec3) -> Vec3 {
        let r = cross(self.up, self.fwd);
        [
            self.pos[0] + r[0] * p[0] + self.up[0] * p[1] + self.fwd[0] * p[2],
            self.pos[1] + r[1] * p[0] + self.up[1] * p[1] + self.fwd[1] * p[2],
            self.pos[2] + r[2] * p[0] + self.up[2] * p[1] + self.fwd[2] * p[2],
        ]
    }
}

fn cross(a: Vec3, b: Vec3) -> Vec3 {
    [a[1] * b[2] - a[2] * b[1], a[2] * b[0] - a[0] * b[2], a[0] * b[1] - a[1] * b[0]]
}

impl World {
    pub fn load(dir: &Path) -> io::Result<World> {
        let mut w = World::default();
        let locs = std::fs::read_to_string(dir.join("locations.csv"))?;
        let mut csv = Csv::new(&locs);
        while let Some(row) = csv.next_row() {
            if let (Some(id), Some(name)) = (row.get("loc_id"), row.get("name")) {
                if let Ok(id) = id.parse() {
                    w.names.insert(id, name.to_string());
                    if let Some(f) = row.get("faction").and_then(|f| f.parse().ok()) {
                        w.factions.insert(id, f);
                    }
                }
            }
        }
        let objs = std::fs::read_to_string(dir.join("sector_objects_all.csv"))?;
        w.load_gates(&objs);
        w.load_stations(&objs);
        w.load_placed(&objs);
        let ships = std::fs::read_to_string(dir.join("ship_types.csv"))?;
        w.load_hulls(&ships);
        // Optional for older extract sets.
        if let Ok(t) = std::fs::read_to_string(dir.join("station_points.csv")) {
            w.load_station_points(&t);
        }
        if let Ok(t) = std::fs::read_to_string(dir.join("engines.csv")) {
            let mut csv = Csv::new(&t);
            while let Some(row) = csv.next_row() {
                if let (Some(i), Some(th)) = (
                    row.get("idx").and_then(|v| v.parse::<usize>().ok()),
                    row.get("thrust").and_then(|v| v.parse::<f32>().ok()),
                ) {
                    if w.engine_thrust.len() <= i {
                        w.engine_thrust.resize(i + 1, 0.0);
                    }
                    w.engine_thrust[i] = th;
                }
            }
        }
        if let Ok(t) = std::fs::read_to_string(dir.join("guns.csv")) {
            w.guns = indexed(&t, "gun", |r| {
                let f = |k: &str| r.get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                Gun {
                    damage: f("damage"),
                    ammo: r.get("ammo").and_then(|v| v.parse().ok()).unwrap_or(0),
                    lifetime: f("lifetime_s"),
                    speed: f("speed"),
                    refire: f("refire_s"),
                }
            });
        }
        if let Ok(t) = std::fs::read_to_string(dir.join("ai_types.csv")) {
            let mut csv = Csv::new(&t);
            while let Some(r) = csv.next_row() {
                let f = |k: &str| r.get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                let Some(type_id) = r.get("type").and_then(|v| v.parse().ok()) else { continue };
                let weapons = r.get("weapons").unwrap_or("").split_whitespace().filter_map(|w| w.parse().ok()).collect();
                let skill = r.get("skill").and_then(|v| v.parse().ok()).unwrap_or(1);
                w.bot_types.insert(
                    type_id,
                    BotType {
                        type_id,
                        thrust: f("thrust"),
                        mass: f("mass"),
                        hull: f("hull"),
                        shield_max: f("shield_max"),
                        shield_regen: f("shield_regen"),
                        handoff_range: f("handoff_range"),
                        weapons,
                        skill,
                    },
                );
            }
        }
        if let Ok(t) = std::fs::read_to_string(dir.join("missiles.csv")) {
            w.missile_damage = indexed(&t, "idx", |r| r.get("damage").and_then(|v| v.parse().ok()).unwrap_or(0.0));
            w.missile_types = indexed(&t, "idx", |r| {
                let f = |k: &str| r.get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                crate::missile::MissileType {
                    pitch: f("pitch_rate"),
                    yaw: f("yaw_rate"),
                    thrust: f("thrust"),
                    drag: f("drag"),
                    mass: f("mass"),
                    damage: f("damage"),
                    lifetime: f("lifetime_s"),
                    guidance: r.get("guidance").and_then(|v| v.parse().ok()).unwrap_or(0),
                }
            });
        }
        if let Ok(t) = std::fs::read_to_string(dir.join("shields.csv")) {
            // Item ids 106–130; the category index is the row order.
            let mut csv = Csv::new(&t);
            while let Some(r) = csv.next_row() {
                let f = |k: &str| r.get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                w.shields.push(Shield { regen: f("regen"), capacity: f("capacity") });
            }
        }
        Ok(w)
    }

    /// Damage of one direct hit by gun (kind 0) or missile (kind 1) `idx`.
    pub fn weapon_damage(&self, kind: u8, idx: u8) -> Option<f32> {
        match kind {
            0 => self.guns.get(idx as usize).map(|g| g.damage),
            1 => self.missile_damage.get(idx as usize).copied(),
            _ => None,
        }
    }

    pub fn shield(&self, idx: u8) -> Option<Shield> {
        self.shields.get(idx as usize).copied()
    }

    fn load_gates(&mut self, text: &str) {
        let mut csv = Csv::new(text);
        while let Some(row) = csv.next_row() {
            if !row.get("category").is_some_and(|c| c.starts_with("jumpgate")) {
                continue;
            }
            let num = |k: &str| row.get(k).and_then(|v| v.parse::<u16>().ok());
            if let (Some(loc), Some(gate), Some(dest_loc), Some(dest_gate)) =
                (num("loc_id"), num("gate_number"), num("gate_dest_loc"), num("gate_dest_gate"))
            {
                self.gates.insert((loc, gate as u8), GateLink { dest_loc, dest_gate: dest_gate as u8 });
            }
        }
    }

    fn load_stations(&mut self, text: &str) {
        let mut csv = Csv::new(text);
        while let Some(row) = csv.next_row() {
            if row.get("category") != Some("station") || row.get("object_id") != Some("6") {
                continue;
            }
            let f = |k: &str| row.get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            if let (Some(loc), Some(model)) = (
                row.get("loc_id").and_then(|v| v.parse::<u16>().ok()),
                row.get("type").and_then(|v| v.parse::<u16>().ok()),
            ) {
                let st = Station {
                    model,
                    radius: f("model_radius"),
                    pos: [f("x"), f("y"), f("z")],
                    fwd: [f("fwd_x"), f("fwd_y"), f("fwd_z")],
                    up: [f("up_x"), f("up_y"), f("up_z")],
                };
                self.stations.insert(loc, st);
            }
        }
    }

    fn load_placed(&mut self, text: &str) {
        let mut csv = Csv::new(text);
        while let Some(row) = csv.next_row() {
            let f = |k: &str| row.get(k).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
            let (Some(loc), Some(model)) =
                (row.get("loc_id").and_then(|v| v.parse::<u16>().ok()), row.get("type").and_then(|v| v.parse::<u16>().ok()))
            else {
                continue;
            };
            // 339: invisible placeholders with no mesh.
            if model == 339 || row.get("model").is_none_or(|m| m.is_empty()) {
                continue;
            }
            let contact_slot = if row.get("nav") == Some("1") {
                match row.get("object_id").and_then(|v| v.parse::<u8>().ok()) {
                    Some(s @ 1..=6) => Some(s),
                    _ => continue, // parked ship models in contact slots 60+
                }
            } else {
                None
            };
            self.objects.entry(loc).or_default().push(Placed {
                contact_slot,
                model,
                pos: [f("x"), f("y"), f("z")],
                fwd: [f("fwd_x"), f("fwd_y"), f("fwd_z")],
                up: [f("up_x"), f("up_y"), f("up_z")],
            });
        }
    }

    /// Everything collidable in `loc`.
    pub fn placed(&self, loc: u16) -> &[Placed] {
        self.objects.get(&loc).map(Vec::as_slice).unwrap_or(&[])
    }

    fn load_station_points(&mut self, text: &str) {
        let mut csv = Csv::new(text);
        while let Some(row) = csv.next_row() {
            let v = |p: &str| {
                let f = |a: &str| row.get(&format!("{p}_{a}")).and_then(|v| v.parse::<f32>().ok()).unwrap_or(0.0);
                [f("x"), f("y"), f("z")]
            };
            if let Some(model) = row.get("model").and_then(|v| v.parse::<u16>().ok()) {
                let sp = StationPoints {
                    dock_start: v("dock_start"),
                    dock_end: v("dock_end"),
                    tubes: [0, 1, 2, 3].map(|k| LaunchTube {
                        inner: v(&format!("tube{k}_in")),
                        outer: v(&format!("tube{k}_out")),
                    }),
                };
                self.station_points.insert(model, sp);
            }
        }
    }

    /// The station in `loc` and its model's points, if both are known.
    pub fn station(&self, loc: u16) -> Option<(&Station, &StationPoints)> {
        let st = self.stations.get(&loc)?;
        Some((st, self.station_points.get(&st.model)?))
    }

    fn load_hulls(&mut self, text: &str) {
        let mut csv = Csv::new(text);
        while let Some(row) = csv.next_row() {
            let f = |k: &str| row.get(k).and_then(|v| v.parse::<f32>().ok());
            if let (Some(id), Some(mass), Some(drag), Some(armour)) =
                (row.get("type").and_then(|v| v.parse().ok()), f("mass"), f("drag"), f("armour"))
            {
                let name = row.get("name").unwrap_or("?").to_string();
                let hull = Hull {
                    type_id: id,
                    name,
                    mass,
                    drag,
                    armour,
                    pitch: f("pitch").unwrap_or(0.001),
                    yaw: f("yaw").unwrap_or(0.001),
                    roll: f("roll").unwrap_or(0.001),
                    engines: row.get("engines").and_then(|v| v.parse().ok()).unwrap_or(1),
                    tube_offset: f("f142").unwrap_or(0.0),
                    size: f("size").unwrap_or(0.0),
                    ext: [f("ext_right").unwrap_or(0.0), f("ext_up").unwrap_or(0.0), f("ext_fwd").unwrap_or(0.0)],
                };
                self.hulls.insert(id, hull);
            }
        }
    }

    pub fn hull(&self, type_id: u16) -> Option<&Hull> {
        self.hulls.get(&type_id)
    }

    /// Where gate `gate_number` in `loc` leads.
    pub fn gate(&self, loc: u16, gate_number: u8) -> Option<GateLink> {
        self.gates.get(&(loc, gate_number)).copied()
    }

    pub fn name(&self, loc: u16) -> &str {
        self.names.get(&loc).map(|s| s.as_str()).unwrap_or("?")
    }
}

/// Rows keyed by an integer index column, as a dense vector.
fn indexed<T: Default + Clone>(text: &str, key: &str, f: impl Fn(&Row<'_>) -> T) -> Vec<T> {
    let mut out: Vec<T> = Vec::new();
    let mut csv = Csv::new(text);
    while let Some(r) = csv.next_row() {
        if let Some(i) = r.get(key).and_then(|v| v.parse::<usize>().ok()) {
            if out.len() <= i {
                out.resize(i + 1, T::default());
            }
            out[i] = f(&r);
        }
    }
    out
}

/// Minimal CSV reader: header row, comma separated, double-quoted fields with
/// `""` escapes. Enough for the files `notes/tools/` writes.
struct Csv<'a> {
    header: HashMap<String, usize>,
    lines: std::str::Lines<'a>,
}

impl<'a> Csv<'a> {
    fn new(text: &'a str) -> Self {
        let mut lines = text.lines();
        let header = lines
            .next()
            .map(split_csv)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(i, h)| (h, i))
            .collect();
        Csv { header, lines }
    }

    /// Next row, with a column getter (empty fields read as `None`).
    fn next_row(&mut self) -> Option<Row<'_>> {
        let line = self.lines.next()?;
        Some(Row { header: &self.header, fields: split_csv(line) })
    }
}

struct Row<'h> {
    header: &'h HashMap<String, usize>,
    fields: Vec<String>,
}

impl Row<'_> {
    fn get(&self, key: &str) -> Option<&str> {
        let i = *self.header.get(key)?;
        self.fields.get(i).map(|s| s.as_str()).filter(|s| !s.is_empty())
    }
}

fn split_csv(line: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match (c, quoted) {
            ('"', true) if chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            ('"', _) => quoted = !quoted,
            (',', false) => out.push(std::mem::take(&mut cur)),
            _ => cur.push(c),
        }
    }
    out.push(cur);
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn csv_quotes() {
        assert_eq!(split_csv(r#"1,"a, b","say ""hi""",,x"#), vec!["1", "a, b", r#"say "hi""#, "", "x"]);
    }

    #[test]
    fn gates_from_rows() {
        let csv = "loc_id,category,gate_number,gate_dest_loc,gate_dest_gate\n\
                   118,jumpgate,2,241,2\n\
                   241,jumpgate (x_ai_gate model),3,249,3\n\
                   0,jumpgate,1,,\n\
                   118,asteroid,,,\n";
        let mut w = World::default();
        w.load_gates(csv);
        assert_eq!(w.gate(118, 2), Some(GateLink { dest_loc: 241, dest_gate: 2 }));
        assert_eq!(w.gate(241, 3), Some(GateLink { dest_loc: 249, dest_gate: 3 }));
        assert_eq!(w.gates.len(), 2);
    }

    /// Every captured launch (S→C 0x08 sub 6, tube @2) is followed by a
    /// report at rest at tube inner end + dir × launch depth (notes/launch.md).
    #[test]
    fn real_extract_placed_objects() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"));
        let w = World::load(dir).unwrap();
        let p = w.placed(107);
        let station = p.iter().find(|o| o.contact_slot == Some(6)).unwrap();
        assert_eq!((station.model, station.pos), (323, [-23.37, 0.0, 0.0]));
        assert_eq!(p.iter().filter(|o| o.contact_slot.is_some_and(|s| s <= 4)).count(), 4);
        assert!(p.iter().filter(|o| o.contact_slot.is_none()).count() > 100, "asteroids");
        assert_eq!(placed_mass(station), 0.0);
        assert_eq!(w.hull(90).unwrap().ext, [11.686, 3.007, 9.632]);
    }

    #[test]
    fn real_extract_weapons_and_shields() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"));
        let w = World::load(dir).unwrap();
        assert_eq!(w.weapon_damage(0, 0), Some(251000.0)); // Liar
        assert_eq!(w.weapon_damage(1, 0), Some(6.0e6)); // Spear
        assert_eq!(w.weapon_damage(2, 0), None);
        assert_eq!(w.shield(0), Some(Shield { regen: 73500.0, capacity: 7.0e6 })); // Flatiron
        assert_eq!(w.shields.len(), 25);
        assert_eq!(w.guns[0].range(), 1000.0);
        assert_eq!(w.missile_types[2].guidance, 2); // Rapier
        assert_eq!(w.missile_types[2].top_speed().round(), 701.0);
        let jelly = &w.bot_types[&230];
        assert_eq!((jelly.thrust, jelly.mass, jelly.handoff_range, jelly.skill), (594000.0, 15000.0, 50000.0, 1));
        assert_eq!(w.bot_types[&236].weapons, vec![57, 57]);
        assert!(w.bot_types[&220].is_defender() && w.bot_types[&259].is_turret());
    }

    #[test]
    fn real_extract_station_points() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"));
        let w = World::load(dir).unwrap();
        let (st, sp) = w.station(107).expect("Quantar Core station");
        assert_eq!(st.model, 323);
        assert_eq!(sp.launch_tubes(), vec![0, 1, 2, 3]);
        // (loc, tube, ship type, first reported position)
        for (loc, tube, ship, seen) in [
            (118, 2, 92, [151.6, 611.3, -98.0]),
            (109, 2, 91, [1037.8, -30.8, 919.2]),
            (107, 0, 94, [-537.7, 155.3, -1461.1]),
        ] {
            let (st, sp) = w.station(loc).unwrap();
            let t = sp.tubes[tube];
            let d: Vec3 = std::array::from_fn(|i| t.outer[i] - t.inner[i]);
            let dir = crate::ship::normalize(d);
            let depth = w.hull(ship).unwrap().launch_depth();
            let p = st.to_world(std::array::from_fn(|i| t.inner[i] + dir[i] * depth));
            let err = crate::ship::length(std::array::from_fn(|i| p[i] - seen[i]));
            assert!(err < 1.0, "loc {loc} tube {tube}: predicted {p:?}, captured {seen:?}");
        }
        let storm = w.hull(90).unwrap();
        assert_eq!((storm.name.as_str(), storm.engines), ("Storm", 2));
        assert_eq!(w.engine_thrust[0], 758000.0);
    }

    /// The real extract: the flown route from the capture must resolve.
    #[test]
    fn real_extract_matches_capture_route() {
        let dir = Path::new(concat!(env!("CARGO_MANIFEST_DIR"), "/../notes/extracts"));
        let w = World::load(dir).expect("notes/extracts present in the repo");
        assert!(w.gates.len() >= 400, "only {} gates", w.gates.len());
        let chinook = w.hull(104).unwrap();
        assert_eq!((chinook.name.as_str(), chinook.mass, chinook.drag, chinook.armour), ("Chinook", 515000.0, 115.0, 62500.0));
        // Capture jumps: (from, 0x07 arg) → (dest, 0x08 sub). gate_number = arg + 1.
        for (from, arg, dest, sub) in [(118, 1, 241, 2), (241, 2, 249, 3), (249, 1, 108, 1)] {
            let g = w.gate(from, arg + 1).unwrap();
            assert_eq!((g.dest_loc, g.dest_gate), (dest, sub), "from {from} arg {arg}");
        }
        assert_eq!(w.name(118), "Quantar Depot");
    }
}
