//! Game-message layouts the server reads and writes. Offsets come from
//! `notes/message-types.md`, the 2026-11-10 capture and the client handlers
//! named below. Anything marked [inferred] still needs live confirmation.

use raknet::game;

// ---- C→S ----

/// Login `[05][callsign\0][password\0]…[version @0x16]`, 33 bytes, state conn.
/// The 176-byte location report shares type 0x05 but arrives on space.
pub const LOGIN_MAX_LEN: usize = 64;
pub const LOGIN_VERSION_OFF: usize = 0x16;

pub struct Login {
    pub callsign: String,
    /// Kept only for the account check, then dropped. Never logged.
    pub password: Vec<u8>,
    pub version: String,
}

fn cstr(b: &[u8]) -> &[u8] {
    let end = b.iter().position(|&c| c == 0).unwrap_or(b.len());
    &b[..end]
}

pub fn parse_login(msg: &[u8]) -> Option<Login> {
    if msg.first() != Some(&0x05) || msg.len() > LOGIN_MAX_LEN || msg.len() <= LOGIN_VERSION_OFF {
        return None;
    }
    let cs = cstr(&msg[1..LOGIN_VERSION_OFF]);
    let pw_start = 1 + cs.len() + 1;
    let password = if pw_start < LOGIN_VERSION_OFF {
        cstr(&msg[pw_start..LOGIN_VERSION_OFF]).to_vec()
    } else {
        Vec::new()
    };
    let callsign: String = cs
        .iter()
        .map(|&c| c as char)
        .filter(|c| c.is_ascii_graphic() || *c == ' ')
        .take(CALLSIGN_MAX)
        .collect();
    if callsign.is_empty() {
        return None;
    }
    let version = String::from_utf8_lossy(cstr(&msg[LOGIN_VERSION_OFF..])).into_owned();
    Some(Login { callsign, password, version })
}

/// 0x07 launch/dock/jump: `[07][action][u16 from_loc][u32 arg][u32 player_id]`.
/// For a jump, `arg` is the gate index in the current sector.
pub const ACT_LAUNCH: u8 = 1;
pub const ACT_DOCK: u8 = 2;
pub const ACT_JUMP: u8 = 3;

pub struct Travel {
    pub action: u8,
    pub from_loc: u16,
    pub arg: u32,
}

pub fn parse_travel(msg: &[u8]) -> Option<Travel> {
    if msg.len() < 12 || msg[0] != 0x07 {
        return None;
    }
    Some(Travel { action: msg[1], from_loc: u16_at(msg, 2), arg: u32_at(msg, 4) })
}

/// 176-byte location report (C→S on space). Relayed verbatim as S→C 0x05, which
/// the client's handler `0x9cf7c8` uses to create/update the sender's ship.
pub const LOC_REPORT_LEN: usize = 176;
pub const LOC_LOC_OFF: usize = 0x44; // u16 current location
pub const LOC_SHIP_OFF: usize = 0x46; // u16 ship type; 0xf9/0xfa = remove [inferred]
pub const LOC_PID_OFF: usize = 0x5c; // u32 player id
pub const SHIP_GONE: u16 = 0xfa;

/// Player id inside the fast position updates (same offsets S→C, physics.md §8).
pub fn fast_loc_pid(msg: &[u8]) -> Option<u32> {
    match msg.first()? {
        0x33 if msg.len() >= 0x1c => Some(u32_at(msg, 0x18)),
        0x34 if msg.len() >= 0x18 => Some(u32_at(msg, 0x14)),
        _ => None,
    }
}

/// S→C listing type for a C→S 0x09 command (replies come back in request order).
pub fn listing_for_command(cmd: u8) -> Option<u8> {
    match cmd {
        0x00 => Some(0x0a),
        0x01 => Some(0x0b),
        0x03 => Some(0x0d),
        0x5a..=0x60 => Some(0x50 + (cmd - 0x5a)),
        _ => None,
    }
}
pub const LISTING_LEN: usize = 896;

// ---- S→C builders / patchers ----

/// 0x06 character record (544 B; handler `0x9d205f`).
pub const CHAR_LEN: usize = 544;
pub const CHAR_CALLSIGN_OFF: usize = 3;
pub const CHAR_CALLSIGN_FIELD: usize = 11; // up to the u16 @0x0e [inferred]
pub const CALLSIGN_MAX: usize = CHAR_CALLSIGN_FIELD - 1;
pub const CHAR_MODE_OFF: usize = 0x45; // 1 = connect to the space IP @0x22
pub const CHAR_LOC_OFF: usize = 0x50;
pub const CHAR_PID_OFF: usize = 0x54;

/// Patch a 0x06 template for this player. The space-IP field is cleared and
/// mode forced off it, so the record never triggers a space connection itself;
/// the server uses 0x08 for that.
pub fn patch_char_record(rec: &mut [u8], callsign: &str, pid: u32, loc: u16) {
    if rec.len() < CHAR_PID_OFF + 4 {
        return;
    }
    let f = &mut rec[CHAR_CALLSIGN_OFF..CHAR_CALLSIGN_OFF + CHAR_CALLSIGN_FIELD];
    f.fill(0);
    let cs = callsign.as_bytes();
    let n = cs.len().min(CALLSIGN_MAX);
    f[..n].copy_from_slice(&cs[..n]);
    if let Some(off) = game::space_ip_offset(0x06) {
        rec[off..off + game::SPACE_IP_FIELD_BYTES].fill(0);
    }
    if rec[CHAR_MODE_OFF] == 1 {
        rec[CHAR_MODE_OFF] = 0;
    }
    put_u16(rec, CHAR_LOC_OFF, loc);
    put_u32(rec, CHAR_PID_OFF, pid);
}

/// 0x08 location change (64 B; handler `0x9d30c6`). `sub` @1 differs per case
/// (launch 6, jumps 1–3 = arrival gate? [inferred]); dest loc @0x2c.
pub const LOCCHG_LEN: usize = 64;
pub const LOCCHG_LOC_OFF: usize = 0x2c;

pub enum SpaceTarget<'a> {
    /// Connect (or stay connected) to the space server at this IPv4 address.
    Ip(&'a str),
    /// Docked: drop the space connection.
    None,
}

/// Build or patch a 0x08. With a template, keeps its other bytes.
pub fn location_change(template: Option<&[u8]>, sub: u8, loc: u16, target: SpaceTarget) -> Vec<u8> {
    let mut m = match template {
        Some(t) if t.len() >= LOCCHG_LEN => t.to_vec(),
        _ => {
            let mut m = vec![0xcc; LOCCHG_LEN];
            m[0] = 0x08;
            m[1] = sub;
            m
        }
    };
    let off = game::space_ip_offset(0x08).unwrap();
    match target {
        SpaceTarget::Ip(ip) => {
            if !game::write_wstr(&mut m, off, game::SPACE_IP_FIELD_BYTES, ip) {
                game::write_space_ip_none(&mut m, off);
            }
        }
        SpaceTarget::None => {
            game::write_space_ip_none(&mut m, off);
        }
    }
    put_u16(&mut m, LOCCHG_LOC_OFF, loc);
    m
}

/// 0x81 message of the day (851 B): text @1, second text @0x1aa, 0xCC padding.
pub const MOTD_LEN: usize = 851;
const MOTD_SECOND_OFF: usize = 0x1aa;

pub fn motd(first: &str, second: &str) -> Vec<u8> {
    let mut m = vec![0xcc; MOTD_LEN];
    m[0] = 0x81;
    put_cstr(&mut m[1..MOTD_SECOND_OFF], first);
    put_cstr(&mut m[MOTD_SECOND_OFF..], second);
    m
}

/// 0x04 system/news chat line: `[04][08][u16 loc][cc×4][sender\0 @8, 11 B][text\0 @0x13]`.
pub fn system_chat(loc: u16, sender: &str, text: &str) -> Vec<u8> {
    let mut m = vec![0x04, 0x08];
    m.extend_from_slice(&loc.to_le_bytes());
    m.extend_from_slice(&[0xcc; 4]);
    let mut name = vec![0xcc; 11];
    put_cstr(&mut name, sender);
    m.extend_from_slice(&name);
    m.extend_from_slice(text.as_bytes());
    m.push(0);
    m
}

/// 0x61 beacon update (4 B): `[61][state][u16 loc]`. Handler `0x9d67d5` accepts
/// only loc 201..=999 (open-space sectors) and only if contact slot 5 holds the
/// beacon (model 2). Otherwise it prints "Received invalid beacon packet".
/// `state` goes to the location table (+0x3c) [inferred: tuned/owner state;
/// the capture sent 0].
pub const BEACON_LOCS: std::ops::RangeInclusive<u16> = 201..=999;

pub fn beacon_update(loc: u16, state: u8) -> Option<Vec<u8>> {
    if !BEACON_LOCS.contains(&loc) {
        return None;
    }
    let mut m = vec![0x61, state];
    m.extend_from_slice(&loc.to_le_bytes());
    Some(m)
}

/// A zero-filled table of a fixed size, used when no template is loaded.
pub fn empty_table(msg_type: u8, len: usize) -> Vec<u8> {
    let mut m = vec![0u8; len];
    m[0] = msg_type;
    m
}

// ---- byte helpers ----

pub fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
pub fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
pub fn put_u16(b: &mut [u8], o: usize, v: u16) {
    b[o..o + 2].copy_from_slice(&v.to_le_bytes());
}
pub fn put_u32(b: &mut [u8], o: usize, v: u32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
/// Write `s` NUL-terminated into `dst`, truncating to fit.
fn put_cstr(dst: &mut [u8], s: &str) {
    if dst.is_empty() {
        return;
    }
    let n = s.len().min(dst.len() - 1);
    dst[..n].copy_from_slice(&s.as_bytes()[..n]);
    dst[n] = 0;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn login_bytes(cs: &str, pw: &str) -> Vec<u8> {
        let mut m = vec![0u8; 33];
        m[0] = 0x05;
        m[1..1 + cs.len()].copy_from_slice(cs.as_bytes());
        let p = 1 + cs.len() + 1;
        m[p..p + pw.len()].copy_from_slice(pw.as_bytes());
        m[LOGIN_VERSION_OFF..LOGIN_VERSION_OFF + 6].copy_from_slice(b"2.0127");
        m
    }

    #[test]
    fn login_parses_fields() {
        let l = parse_login(&login_bytes("pilot", "hunter2")).unwrap();
        assert_eq!(l.callsign, "pilot");
        assert_eq!(l.password, b"hunter2");
        assert_eq!(l.version, "2.0127");
    }

    #[test]
    fn listing_map_matches_capture_order() {
        // Capture: 09 cmds 03,00,01,60..5a → 0d,0a,0b,56..50.
        let cmds = [0x03, 0x00, 0x01, 0x60, 0x5f, 0x5e, 0x5d, 0x5c, 0x5b, 0x5a];
        let want = [0x0d, 0x0a, 0x0b, 0x56, 0x55, 0x54, 0x53, 0x52, 0x51, 0x50];
        for (c, w) in cmds.iter().zip(want) {
            assert_eq!(listing_for_command(*c), Some(w));
        }
    }

    #[test]
    fn location_change_targets() {
        let m = location_change(None, 6, 118, SpaceTarget::Ip("127.0.0.1"));
        assert_eq!(m.len(), LOCCHG_LEN);
        assert_eq!(game::read_wstr(&m, 0x0c, 32).unwrap(), "127.0.0.1");
        assert_eq!(u16_at(&m, LOCCHG_LOC_OFF), 118);
        let d = location_change(None, 0xcc, 8, SpaceTarget::None);
        assert!(game::is_space_ip_none(&d, 0x0c));
    }

    #[test]
    fn char_record_patch() {
        let mut r = vec![0xcc; CHAR_LEN];
        r[0] = 0x06;
        r[CHAR_MODE_OFF] = 1;
        patch_char_record(&mut r, "averyverylongname", 4242, 18);
        assert_eq!(&r[3..13], b"averyveryl");
        assert_eq!(r[13], 0);
        assert_eq!(u32_at(&r, CHAR_PID_OFF), 4242);
        assert_eq!(u16_at(&r, CHAR_LOC_OFF), 18);
        assert_eq!(r[CHAR_MODE_OFF], 0);
        assert_eq!(&r[0x22..0x24], &[0, 0]);
    }

    #[test]
    fn beacon_only_in_open_space() {
        assert_eq!(beacon_update(241, 0), Some(vec![0x61, 0, 0xf1, 0]));
        assert_eq!(beacon_update(118, 0), None, "station sectors have no beacon");
        assert_eq!(beacon_update(1000, 0), None);
    }

    #[test]
    fn system_chat_layout() {
        let m = system_chat(18, "Server", "hi");
        assert_eq!(&m[..4], &[0x04, 0x08, 18, 0]);
        assert_eq!(&m[8..15], b"Server\0");
        assert_eq!(&m[0x13..], b"hi\0");
    }
}
