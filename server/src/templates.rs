//! Response templates taken from a local `jgcap.bin` capture.
//!
//! Most server→client layouts are only partly decoded. Until each one has a
//! proper builder, the server answers with a captured copy and patches the
//! fields it understands (player id, callsign, location, space address). The
//! capture is read at startup from a local path and is never part of the repo.
//!
//! What is never taken from the capture:
//! - 0xFE (arbitrary client memory write): never sent.
//! - 0x81 MOTD and 0x04 chat: those are other people's words; we build our own.
//! - 0x08: its space-server address is always replaced (see `proto::location_change`).
//!
//! The store also learns the gate graph from the capture: each C→S 0x07 jump
//! `(from_loc, gate)` followed by an S→C 0x08 gives the destination sector.

use crate::proto::{self, ACT_JUMP};
use raknet::capture::{self, Dir};
use raknet::game;
use std::collections::HashMap;

/// Types the store refuses to hold.
const NEVER: &[u8] = &[0xfe, 0x81, 0x04];

#[derive(Default)]
pub struct Templates {
    by_type: HashMap<u8, Vec<Vec<u8>>>,
    /// (from_loc, gate) → the S→C 0x08 that followed (dest loc @0x2c).
    pub gates: HashMap<(u16, u32), Vec<u8>>,
    /// The 0x08 sent after a launch, if seen.
    pub launch_redirect: Option<Vec<u8>>,
}

impl Templates {
    pub fn from_capture_bytes(bytes: &[u8]) -> Self {
        let (records, _) = capture::parse_lenient(bytes);
        let mut t = Templates::default();
        let mut pending: Option<(u8, u16, u32)> = None;
        for r in &records {
            let Some(ty) = r.message_type() else { continue };
            match r.dir {
                Dir::ClientToServer => {
                    if let Some(tr) = proto::parse_travel(&r.data) {
                        pending = Some((tr.action, tr.from_loc, tr.arg));
                    }
                }
                Dir::ServerToClient => {
                    if ty == 0x08 {
                        if let Some((action, from, arg)) = pending.take() {
                            let mut m = r.data.clone();
                            game::rewrite_space_ip(&mut m, "0.0.0.0");
                            match action {
                                ACT_JUMP => {
                                    t.gates.entry((from, arg)).or_insert(m);
                                }
                                proto::ACT_LAUNCH => {
                                    t.launch_redirect.get_or_insert(m);
                                }
                                _ => {}
                            }
                        }
                    }
                    if !NEVER.contains(&ty) {
                        let mut m = r.data.clone();
                        game::rewrite_space_ip(&mut m, "0.0.0.0");
                        t.by_type.entry(ty).or_default().push(m);
                    }
                }
            }
        }
        t
    }

    /// First captured message of this type.
    pub fn first(&self, ty: u8) -> Option<&[u8]> {
        self.by_type.get(&ty)?.first().map(|v| v.as_slice())
    }

    /// The first `n` distinct captured messages of this type (e.g. 3 × 0x14).
    pub fn first_n(&self, ty: u8, n: usize) -> Vec<Vec<u8>> {
        let mut out: Vec<Vec<u8>> = Vec::new();
        for m in self.by_type.get(&ty).into_iter().flatten() {
            if out.len() >= n {
                break;
            }
            if !out.contains(m) {
                out.push(m.clone());
            }
        }
        out
    }

    pub fn len(&self) -> usize {
        self.by_type.len()
    }

    #[cfg(test)]
    pub fn has(&self, ty: u8) -> bool {
        self.by_type.contains_key(&ty)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rec(dir: u8, data: &[u8]) -> Vec<u8> {
        let mut v = vec![dir];
        v.extend_from_slice(&0u32.to_le_bytes());
        v.extend_from_slice(&(data.len() as u16).to_le_bytes());
        v.extend_from_slice(data);
        v
    }

    #[test]
    fn refuses_memory_patch_and_learns_gates() {
        let mut jump = vec![0x07, 3, 118, 0, 1, 0, 0, 0, 0xf9, 0x0e, 0, 0];
        let redirect = proto::location_change(None, 2, 241, proto::SpaceTarget::Ip("203.0.113.9"));
        let mut buf = Vec::new();
        buf.extend(rec(0, &[0xfe, 1, 2, 3]));
        buf.extend(rec(0, &[0x81, b'h', b'i', 0]));
        buf.extend(rec(1, &jump));
        buf.extend(rec(0, &redirect));
        jump[1] = 1; // a launch, no 0x08 after it
        buf.extend(rec(1, &jump));
        let t = Templates::from_capture_bytes(&buf);
        assert!(!t.has(0xfe));
        assert!(!t.has(0x81));
        let g = t.gates.get(&(118, 1)).expect("gate learned");
        assert_eq!(proto::u16_at(g, proto::LOCCHG_LOC_OFF), 241);
        // The captured address is not kept.
        assert_eq!(game::read_wstr(g, 0x0c, 32).unwrap(), "0.0.0.0");
        assert!(t.launch_redirect.is_none());
    }
}
