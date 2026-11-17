//! Jumpgate's own message framing, which sits *inside* a RakNet internal
//! packet's payload (above the RakNet transport):
//!
//! ```text
//! C → S :  [0x51][obfuscated]
//! S → C :  [0x1A][u32 time][0x51][obfuscated]      (0x1A = ID_TIMESTAMP)
//! ```
//!
//! The obfuscation is a reversible salt + running-sum scramble (not encryption),
//! ported from `0x9c8ca0` / `0x9c8d30` (and `tools/jgdecode.py`). Used only to
//! decode messages for logging; the proxy forwards payloads verbatim.

/// MSVC `rand()`-derived key byte for a given salt (verified in the binary).
fn obfuscation_k(salt: u8) -> u8 {
    let s = (salt as u32).wrapping_mul(0x89);
    let s = s.wrapping_mul(0x0003_43fd).wrapping_add(0x0026_9ec3); // one MSVC rand() step
    let r = (s >> 16) & 0x7fff;
    // 254/32767 as f32 (bytes fc 01 fe 3b); `_ftol` truncates toward zero.
    let f = f32::from_le_bytes([0xfc, 0x01, 0xfe, 0x3b]);
    ((r as f32 * f) as u32 & 0xff) as u8
}

/// Reverse the obfuscation. `enc[0]` is the salt; returns `enc.len()-1` bytes.
pub fn deobfuscate(enc: &[u8]) -> Vec<u8> {
    if enc.len() < 2 {
        return Vec::new();
    }
    let k = obfuscation_k(enc[0]);
    let mut out = Vec::with_capacity(enc.len() - 1);
    out.push(enc[1].wrapping_sub(k));
    for i in 2..enc.len() {
        out.push(enc[i].wrapping_sub(enc[i - 1]));
    }
    out
}

/// Apply the obfuscation to a plaintext message with the given salt.
pub fn obfuscate(msg: &[u8], salt: u8) -> Vec<u8> {
    let k = obfuscation_k(salt);
    let mut out = Vec::with_capacity(msg.len() + 1);
    out.push(salt);
    if msg.is_empty() {
        return out;
    }
    out.push(msg[0].wrapping_add(k));
    for &b in &msg[1..] {
        let prev = *out.last().unwrap();
        out.push(prev.wrapping_add(b));
    }
    out
}

/// A decoded game-layer message extracted from a RakNet packet payload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GameMessage {
    /// Present for server→client messages (ID_TIMESTAMP wrapper).
    pub timestamp: Option<u32>,
    /// Deobfuscated message bytes; `body[0]` is the game message type.
    pub body: Vec<u8>,
}

impl GameMessage {
    pub fn message_type(&self) -> Option<u8> {
        self.body.first().copied()
    }
}

/// Decode a RakNet packet payload (`packet.data`) into a game message, peeling
/// the optional 0x1A timestamp and the 0x51 frame. Returns `None` if the payload
/// is not a 0x51-framed game message (e.g. a pure transport ID).
pub fn decode_payload(payload: &[u8]) -> Option<GameMessage> {
    let mut p = payload;
    let mut timestamp = None;
    if p.first() == Some(&0x1A) && p.len() >= 5 {
        timestamp = Some(u32::from_le_bytes([p[1], p[2], p[3], p[4]]));
        p = &p[5..];
    }
    if p.first() != Some(&0x51) {
        return None;
    }
    Some(GameMessage {
        timestamp,
        body: deobfuscate(&p[1..]),
    })
}


// ---- Space-server address fields (S→C 0x06 and 0x08) ----
//
// Two server→client messages carry the address of the space server as a
// UTF-16LE, NUL-terminated string. When it differs from the current one the
// client drops its space connection and opens a new one to `<ip>:5151`
// (`0x9c7190`). The "none" sentinel means stay disconnected (used on dock).
// Quirk: the compare is wide (`wcscmp` 0xaabf39) but the constant at `0xb05afc`
// is the *narrow* bytes "none\0", and the server sends the same narrow bytes.
// They match byte-for-byte, so "none" must be written as ASCII, not UTF-16.
//   0x08 location change: string @0x0c (handler 0x9d30c6; next field read is the loc @0x2c)
//   0x06 character record: string @0x22, acted on only when mode @0x45 == 1 (handler 0x9d2e53)
// A captured message holds the *capture's* server address, so replaying one
// unmodified would send the client's space connection to that server.
// Anything that sends these must go through [`rewrite_space_ip`].

/// Room for each address field: 16 UTF-16 code units, including the NUL.
pub const SPACE_IP_FIELD_BYTES: usize = 32;
/// The "no space server" sentinel exactly as the client compares it
/// (`0xb05afc`): narrow ASCII bytes plus a 16-bit NUL.
pub const SPACE_IP_NONE: &[u8] = b"none\0\0";

/// True if the field at `off` holds the narrow "none" sentinel.
pub fn is_space_ip_none(msg: &[u8], off: usize) -> bool {
    msg.get(off..off + SPACE_IP_NONE.len()) == Some(SPACE_IP_NONE)
}

/// Write the "none" sentinel into the field at `off`, zero-filling the rest.
pub fn write_space_ip_none(msg: &mut [u8], off: usize) -> bool {
    if off + SPACE_IP_FIELD_BYTES > msg.len() {
        return false;
    }
    msg[off..off + SPACE_IP_FIELD_BYTES].fill(0);
    msg[off..off + SPACE_IP_NONE.len()].copy_from_slice(SPACE_IP_NONE);
    true
}

/// Offset of the space-server address string, for message types that carry one.
pub fn space_ip_offset(msg_type: u8) -> Option<usize> {
    match msg_type {
        0x08 => Some(0x0c),
        0x06 => Some(0x22),
        _ => None,
    }
}

/// Read a NUL-terminated UTF-16LE string of at most `cap` bytes at `off`.
pub fn read_wstr(msg: &[u8], off: usize, cap: usize) -> Option<String> {
    let end = off.checked_add(cap)?.min(msg.len());
    if off >= end {
        return None;
    }
    let units: Vec<u16> = msg[off..end]
        .chunks_exact(2)
        .map(|c| u16::from_le_bytes([c[0], c[1]]))
        .take_while(|&u| u != 0)
        .collect();
    Some(String::from_utf16_lossy(&units))
}

/// Write `s` as NUL-terminated UTF-16LE into `cap` bytes at `off`, zero-filling
/// the rest. Fails (writes nothing) if it doesn't fit.
pub fn write_wstr(msg: &mut [u8], off: usize, cap: usize, s: &str) -> bool {
    let units: Vec<u16> = s.encode_utf16().collect();
    if (units.len() + 1) * 2 > cap || off + cap > msg.len() {
        return false;
    }
    msg[off..off + cap].fill(0);
    for (i, u) in units.iter().enumerate() {
        msg[off + 2 * i..off + 2 * i + 2].copy_from_slice(&u.to_le_bytes());
    }
    true
}

/// Outcome of [`rewrite_space_ip`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpaceIpRewrite {
    /// Not a 0x06/0x08, or the field is past the end of the message.
    NotApplicable,
    /// Empty or "none": left alone, since neither triggers a connection.
    Kept,
    /// Replaced with the given address.
    Rewritten,
    /// The address doesn't fit; the field was set to "none" instead so the
    /// client can't connect anywhere unintended.
    Neutralized,
}

/// Point any space-server address in a 0x06/0x08 at `ip` (our own server).
/// Empty and "none" are kept as-is. Every other value is replaced.
pub fn rewrite_space_ip(msg: &mut [u8], ip: &str) -> SpaceIpRewrite {
    let Some(off) = msg.first().copied().and_then(space_ip_offset) else {
        return SpaceIpRewrite::NotApplicable;
    };
    if off + SPACE_IP_FIELD_BYTES > msg.len() {
        return SpaceIpRewrite::NotApplicable;
    }
    if is_space_ip_none(msg, off) || msg[off..off + 2] == [0, 0] {
        return SpaceIpRewrite::Kept;
    }
    if write_wstr(msg, off, SPACE_IP_FIELD_BYTES, ip) {
        SpaceIpRewrite::Rewritten
    } else {
        write_space_ip_none(msg, off);
        SpaceIpRewrite::Neutralized
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn obfuscation_round_trips_all_salts() {
        let msg = vec![0x09, 0x03, 0x12, 0x00, 0x00, 0xf9, 0x0e];
        for salt in 0u8..=254 {
            let enc = obfuscate(&msg, salt);
            assert_eq!(enc[0], salt);
            assert_eq!(deobfuscate(&enc), msg, "salt {salt}");
        }
    }

    #[test]
    fn decode_client_and_server_frames() {
        let body = vec![0x09, 0xAB, 0xCD];
        let framed_c = {
            let mut v = vec![0x51];
            v.extend(obfuscate(&body, 42));
            v
        };
        let g = decode_payload(&framed_c).unwrap();
        assert_eq!(g.timestamp, None);
        assert_eq!(g.body, body);

        let framed_s = {
            let mut v = vec![0x1A, 0x44, 0x33, 0x22, 0x11, 0x51];
            v.extend(obfuscate(&body, 7));
            v
        };
        let g = decode_payload(&framed_s).unwrap();
        assert_eq!(g.timestamp, Some(0x1122_3344));
        assert_eq!(g.body, body);
        assert_eq!(g.message_type(), Some(0x09));
    }

    fn redirect_msg(ip: &str) -> Vec<u8> {
        let mut m = vec![0xCC; 0x36];
        m[0] = 0x08;
        if ip == "none" {
            assert!(write_space_ip_none(&mut m, 0x0c));
        } else {
            assert!(write_wstr(&mut m, 0x0c, SPACE_IP_FIELD_BYTES, ip));
        }
        m[0x2c] = 0xe9;
        m[0x2d] = 0x03;
        m
    }

    #[test]
    fn space_ip_is_rewritten_to_ours() {
        let mut m = redirect_msg("203.0.113.77");
        assert_eq!(rewrite_space_ip(&mut m, "127.0.0.1"), SpaceIpRewrite::Rewritten);
        assert_eq!(read_wstr(&m, 0x0c, SPACE_IP_FIELD_BYTES).unwrap(), "127.0.0.1");
        assert_eq!(&m[0x2c..0x2e], &[0xe9, 0x03], "loc after the field untouched");
    }

    #[test]
    fn space_ip_none_and_empty_are_kept() {
        let mut m = redirect_msg("none");
        assert_eq!(&m[0x0c..0x12], b"none\0\0", "narrow bytes, as captured on dock");
        assert_eq!(rewrite_space_ip(&mut m, "127.0.0.1"), SpaceIpRewrite::Kept);
        let mut m = redirect_msg("");
        assert_eq!(rewrite_space_ip(&mut m, "127.0.0.1"), SpaceIpRewrite::Kept);
        let mut other = vec![0x81, 1, 2, 3];
        assert_eq!(rewrite_space_ip(&mut other, "127.0.0.1"), SpaceIpRewrite::NotApplicable);
    }

    #[test]
    fn space_ip_too_long_is_neutralized() {
        let mut m = redirect_msg("203.0.113.77");
        let long = "a-very-long-hostname.example";
        assert_eq!(rewrite_space_ip(&mut m, long), SpaceIpRewrite::Neutralized);
        assert!(is_space_ip_none(&m, 0x0c));
    }
}
