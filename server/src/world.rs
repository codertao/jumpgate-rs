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

use crate::ship::Hull;
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
    /// (loc, gate_number) → link.
    pub gates: HashMap<(u16, u8), GateLink>,
    /// Ship type id → hull stats.
    pub hulls: HashMap<u16, Hull>,
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
                }
            }
        }
        let objs = std::fs::read_to_string(dir.join("sector_objects_all.csv"))?;
        w.load_gates(&objs);
        let ships = std::fs::read_to_string(dir.join("ship_types.csv"))?;
        w.load_hulls(&ships);
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

    fn load_hulls(&mut self, text: &str) {
        let mut csv = Csv::new(text);
        while let Some(row) = csv.next_row() {
            let f = |k: &str| row.get(k).and_then(|v| v.parse::<f32>().ok());
            if let (Some(id), Some(mass), Some(drag), Some(armour)) =
                (row.get("type").and_then(|v| v.parse().ok()), f("mass"), f("drag"), f("armour"))
            {
                let name = row.get("name").unwrap_or("?").to_string();
                self.hulls.insert(id, Hull { type_id: id, name, mass, drag, armour });
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
