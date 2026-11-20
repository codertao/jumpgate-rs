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

use crate::ship::{Hull, Vec3};
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
        Ok(w)
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
