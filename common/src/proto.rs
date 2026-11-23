//! Game-message layouts the server reads and writes. Offsets come from
//! `notes/message-types.md`, the 2026-11-10 capture and the client handlers
//! named below. Anything marked [inferred] still needs live confirmation.

use crate::character::{Character, Fittings, ITEM_SLOTS};
use crate::ship::{ShipView, Vec3};
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

/// 0x08 location change (64 B; handler `0x9d30c6`). `sub` @1 is the contact
/// slot the ship appears at: the arrival gate number (1–4) for a jump, 6 for a
/// launch (verified against the capture and the extract's gate links); dest loc @0x2c.
pub const LOCCHG_LEN: usize = 64;
pub const LOCCHG_LOC_OFF: usize = 0x2c;
/// Launch tube (0–3) for sub 6: the client places the ship in that tube of the
/// station model (`0x9d34a3`; notes/launch.md). Captures: 2 at 118 and 109, 0 at 107.
pub const LOCCHG_TUBE_OFF: usize = 2;

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
            // Same shape as captured jumps: 0xCC fill, zeros @3..0x0b and @0x30.
            let mut m = vec![0xcc; LOCCHG_LEN];
            m[0] = 0x08;
            m[1] = sub;
            m[3..0x0b].fill(0);
            m[0x30..0x34].fill(0);
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
/// Text of a C→S 0x04 chat line (`[04][sub][u16 loc][u32 pid][callsign 11B @8]
/// [text @0x13]`), up to the NUL; 0xCC is the client's fill byte.
pub const CHAT_TEXT_OFF: usize = 0x13;
pub fn chat_text(msg: &[u8]) -> Option<String> {
    let t = msg.get(CHAT_TEXT_OFF..)?;
    let end = t.iter().position(|&b| b == 0 || b == 0xcc).unwrap_or(t.len());
    Some(String::from_utf8_lossy(&t[..end]).into_owned())
}

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

/// S→C 0x70 bot spawn order (6 B): `[70][u8 ai_type][u16 spawn#][cc cc]`,
/// handler `0x9cbac0`. ai_type = bot ship type − 219 (spawn_bot `0x9cbb60`
/// indexes `0xbeb63c + t·64` = the AI table row 219 + t; capture jgcap2:
/// ai_type 0x0e came back in botloc as ship type 233). spawn# becomes the bot
/// id in botloc @8 and C→S 0x44. No position or target: the receiving client
/// owns the bot, targets itself and picks the spawn point ~5 s later
/// (ai.md "Spawning"). The trailing two bytes were `cc cc` in every capture.
pub const BOT_SHIP_TYPES: std::ops::RangeInclusive<u16> = 220..=259;

pub fn bot_spawn(ship_type: u16, spawn_no: u16) -> Option<Vec<u8>> {
    if !BOT_SHIP_TYPES.contains(&ship_type) {
        return None;
    }
    let mut m = vec![0x70, (ship_type - 219) as u8];
    m.extend_from_slice(&spawn_no.to_le_bytes());
    m.extend_from_slice(&[0xcc, 0xcc]);
    Some(m)
}

/// Botloc 0x71 (52 B, same layout both ways): `@8` u16 bot id (spawn#),
/// `@0xa` u16 loc, `@0xc` u16 ship type, `@0x20` u32 controller player id,
/// `@0x24` u32 target player id, `@0x28` f32×3 position (ai.md "Spawning").
pub const BOTLOC_LEN: usize = 52;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Botloc {
    pub id: u16,
    pub loc: u16,
    pub ship_type: u16,
    pub controller: u32,
    pub target: u32,
}

impl Botloc {
    /// A bot entry rather than the all-zero "I control none" report.
    pub fn is_bot(&self) -> bool {
        (219..=259).contains(&self.ship_type) || self.ship_type == 0xffff
    }
}

pub fn parse_botloc(m: &[u8]) -> Option<Botloc> {
    (m.len() == BOTLOC_LEN && m[0] == 0x71).then(|| Botloc {
        id: u16_at(m, 8),
        loc: u16_at(m, 0xa),
        ship_type: u16_at(m, 0xc),
        controller: u32_at(m, 0x20),
        target: u32_at(m, 0x24),
    })
}

/// Everything a botloc carries, as the controller's sender `0x9c9d10` fills
/// it and the handler `0x9d6f25` reads it.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct BotState {
    /// pbotloc `+1`: AI state (0 attack, 1 home, 2 approach dock, 3 dock, 8 jink).
    pub state: u8,
    /// Shields and hull as fractions of their maxima (sent as whole percent).
    pub shields: f32,
    pub hull: f32,
    pub id: u16,
    pub loc: u16,
    pub ship_type: u16,
    pub vel: Vec3,
    pub fwd: Vec3,
    pub up: Vec3,
    pub controller: u32,
    pub target: u32,
    pub pos: Vec3,
}

fn i16_at(m: &[u8], o: usize) -> i16 {
    i16::from_le_bytes([m[o], m[o + 1]])
}
fn put_i16(m: &mut [u8], o: usize, v: f32) {
    m[o..o + 2].copy_from_slice(&(v.round().clamp(-32768.0, 32767.0) as i16).to_le_bytes());
}

/// Full botloc: `@1` state, `@2` shields %, `@3` hull %, `@4/@5` pbotloc
/// `+2/+3` and `@6` u16 `+6` (target slot; receivers recompute it, sent 0),
/// `@8` id, `@0xa` loc, `@0xc` type, `@0xe` velocity i16 × 10, `@0x14` fwd
/// i16 × 10⁴, `@0x1a` up i16 × 10⁴, `@0x20` controller, `@0x24` target,
/// `@0x28` position f32×3.
pub fn botloc(b: &BotState) -> Vec<u8> {
    let mut m = vec![0u8; BOTLOC_LEN];
    m[0] = 0x71;
    m[1] = b.state;
    m[2] = (b.shields * 100.0).round().clamp(0.0, 100.0) as u8;
    m[3] = (b.hull * 100.0).round().clamp(0.0, 100.0) as u8;
    put_u16(&mut m, 8, b.id);
    put_u16(&mut m, 0xa, b.loc);
    put_u16(&mut m, 0xc, b.ship_type);
    for k in 0..3 {
        put_i16(&mut m, 0xe + 2 * k, b.vel[k] * 10.0);
        put_i16(&mut m, 0x14 + 2 * k, b.fwd[k] * 10_000.0);
        put_i16(&mut m, 0x1a + 2 * k, b.up[k] * 10_000.0);
        put_f32(&mut m, 0x28 + 4 * k, b.pos[k]);
    }
    put_u32(&mut m, 0x20, b.controller);
    put_u32(&mut m, 0x24, b.target);
    m
}

pub fn parse_bot_state(m: &[u8]) -> Option<BotState> {
    (m.len() == BOTLOC_LEN && m[0] == 0x71).then(|| {
        let v = |o: usize, k: f32| [i16_at(m, o) as f32 * k, i16_at(m, o + 2) as f32 * k, i16_at(m, o + 4) as f32 * k];
        BotState {
            state: m[1],
            shields: m[2] as f32 * 0.01,
            hull: m[3] as f32 * 0.01,
            id: u16_at(m, 8),
            loc: u16_at(m, 0xa),
            ship_type: u16_at(m, 0xc),
            vel: v(0xe, 0.1),
            fwd: v(0x14, 1e-4),
            up: v(0x1a, 1e-4),
            controller: u32_at(m, 0x20),
            target: u32_at(m, 0x24),
            pos: [f32_at(m, 0x28), f32_at(m, 0x2c), f32_at(m, 0x30)],
        }
    })
}

/// A botloc handing the bot to `pid`: the receiving handler makes the bot's
/// **target** its controller (`0x9d7191`: target == me → I fly it and answer
/// with a botloc at once; otherwise the bot follows the target's ship, or is
/// removed if that player isn't in the sector). So both fields name `pid`.
pub fn botloc_handed_to(last: &[u8], pid: u32) -> Option<Vec<u8>> {
    let mut m = last.to_vec();
    (m.len() == BOTLOC_LEN && m[0] == 0x71).then(|| {
        put_u32(&mut m, 0x20, pid);
        put_u32(&mut m, 0x24, pid);
        m
    })
}

/// 0x32 missile (40 B, same layout both ways; C→S on space from the
/// launcher, S→C handler `0x9d4686`): `[32][u8 id @1][u8 type @2][? @3]
/// [u16 target bot id @4, 0xfe = not a bot][u16 @6 ?][i16×3 velocity ×10 @8]
/// [i16×3 fwd ×10⁴ @0xe][u32 target pid @0x14][u32 owner pid @0x18]
/// [f32×3 pos @0x1c]`. Type 0xfa removes the missile (the position is where
/// it ended). A receiver whose pid is the target (and @4 = 0xfe) hears the
/// lock warning; a target pid ≥ 0x8000 names a slot or object instead.
/// [static + capture jgcap2, 2026-11-20]
pub const MISSILE_LEN: usize = 40;
pub const MISSILE_GONE: u8 = 0xfa;

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MissileMsg {
    pub id: u8,
    pub ty: u8,
    pub target_bot: u16,
    pub vel: Vec3,
    pub fwd: Vec3,
    pub target: u32,
    pub owner: u32,
    pub pos: Vec3,
}

pub fn parse_missile(m: &[u8]) -> Option<MissileMsg> {
    (m.len() == MISSILE_LEN && m[0] == 0x32).then(|| {
        let v = |o: usize, k: f32| [i16_at(m, o) as f32 * k, i16_at(m, o + 2) as f32 * k, i16_at(m, o + 4) as f32 * k];
        MissileMsg {
            id: m[1],
            ty: m[2],
            target_bot: u16_at(m, 4),
            vel: v(8, 0.1),
            fwd: v(0xe, 1e-4),
            target: u32_at(m, 0x14),
            owner: u32_at(m, 0x18),
            pos: [f32_at(m, 0x1c), f32_at(m, 0x20), f32_at(m, 0x24)],
        }
    })
}

pub fn missile(mm: &MissileMsg) -> Vec<u8> {
    let mut m = vec![0u8; MISSILE_LEN];
    m[0] = 0x32;
    m[1] = mm.id;
    m[2] = mm.ty;
    put_u16(&mut m, 4, mm.target_bot);
    for k in 0..3 {
        put_i16(&mut m, 8 + 2 * k, mm.vel[k] * 10.0);
        put_i16(&mut m, 0xe + 2 * k, mm.fwd[k] * 10_000.0);
        put_f32(&mut m, 0x1c + 4 * k, mm.pos[k]);
    }
    put_u32(&mut m, 0x14, mm.target);
    put_u32(&mut m, 0x18, mm.owner);
    m
}

/// C→S 0x2C "bxr" hand-off request (12 B, space link; online_bot_ai
/// `0x436651`): `[2C][?][u16 bot id @2][u16 0 @4][u16 loc @6][u32 me @8]`.
/// @1 is never written (stack garbage).
pub const BXR_LEN: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Bxr {
    pub bot: u16,
    pub loc: u16,
    pub pid: u32,
}

pub fn bxr(bot: u16, loc: u16, me: u32) -> Vec<u8> {
    let mut m = vec![0u8; BXR_LEN];
    m[0] = 0x2c;
    put_u16(&mut m, 2, bot);
    put_u16(&mut m, 6, loc);
    put_u32(&mut m, 8, me);
    m
}

pub fn parse_bxr(m: &[u8]) -> Option<Bxr> {
    (m.len() == BXR_LEN && m[0] == 0x2c).then(|| Bxr { bot: u16_at(m, 2), loc: u16_at(m, 6), pid: u32_at(m, 8) })
}

/// C→S 0x16 (6 B, state link) has two senders with one layout,
/// `[16][u8][u16 id][u16 ?]`:
/// - the spawn queue bouncing a 0x70 (`0x9cb930`): `[16][ai_type][spawn#]`;
/// - `remove_bot` (`0x9cb9c0`), when the bot's controller drops it (despawn,
///   docked, target gone): `[16][slot +1 "dead" byte][bot id]`.
/// Either way the bot with that id is gone from the sender's client.
pub const BOT_GONE_LEN: usize = 6;

pub fn bot_gone(first: u8, id: u16) -> Vec<u8> {
    let mut m = vec![0x16, first, 0, 0, 0, 0];
    put_u16(&mut m, 2, id);
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

// ---- server-driven ships (NPCs) ----
//
// The client can't tell an NPC from a remote player: both arrive as S→C 0x05
// (create/update) and 0x33 (fast position). Field map from the handlers
// `0x9cf7c8` (0x05) and `0x9d573c` (0x33), each read paired with the contact-slot
// field it is stored in [static, 2026-11-17]. Conditions the client checks:
// loc must equal its own current location, the 0x05 callsign must be ≥ 2
// chars, and the clock must be strictly newer than the slot's (else dropped).

pub const SMALL_LOC_LEN: usize = 56;

/// Fast location updates (0x33 small, 0x34), ours and relayed. The client sends
/// these UNRELIABLE_SEQUENCED and drops any whose clock isn't newer, so the
/// server sends them the same way. Full 0x05 reports stay reliable: they create
/// the contact and carry the callsign and hull.
pub fn is_position_update(msg: &[u8]) -> bool {
    matches!(msg.first(), Some(0x33 | 0x34))
}

/// Unit vector → i16 ×10000 (client scales by 1e-4 @0xb06918).
fn put_unit(b: &mut [u8], o: usize, v: Vec3) {
    for (k, x) in v.iter().enumerate() {
        let q = (x * 10000.0).round().clamp(-32767.0, 32767.0) as i16;
        b[o + 2 * k..o + 2 * k + 2].copy_from_slice(&q.to_le_bytes());
    }
}
/// Velocity → i16 ×10 (client scales by 0.1 @0xaec470).
fn put_vel(b: &mut [u8], o: usize, v: Vec3) {
    for (k, x) in v.iter().enumerate() {
        let q = (x * 10.0).round().clamp(-32767.0, 32767.0) as i16;
        b[o + 2 * k..o + 2 * k + 2].copy_from_slice(&q.to_le_bytes());
    }
}
pub fn put_f32(b: &mut [u8], o: usize, v: f32) {
    b[o..o + 4].copy_from_slice(&v.to_le_bytes());
}
fn put_vec(b: &mut [u8], o: usize, v: Vec3) {
    for (k, x) in v.iter().enumerate() {
        put_f32(b, o + 4 * k, *x);
    }
}

/// Engine-glow byte (slot +0x156d): 0x33's handler sets 0 or 50 from thrust.
fn glow(ship: &ShipView) -> u8 {
    if ship.thrust > 0.0 {
        50
    } else {
        0
    }
}

/// 0x05 @0x17: the ship tows a cargo pod (`cargo.x`, model 119), drawn and
/// collided behind it. The client's sender (`0x9cd6cd`) sets it exactly for
/// the "Cargo Tow" hulls: Pioneer (32), Condor (62) and Thunder (92).
pub const LOC_CARGO_POD_OFF: usize = 0x17;
pub const LOC_RANK_OFF: usize = 0x14;
/// 0x05 shields now, shields full, hull (= ship-type armour × 3000 when
/// undamaged), fuel (0–100). 0x33 carries current shields too.
pub const LOC_SHIELDS_OFF: usize = 0x9c;
pub const LOC_SHIELDS_MAX_OFF: usize = 0xa0;
pub const LOC_HULL_OFF: usize = 0xa4;
pub const LOC_FUEL_OFF: usize = 0xac;
pub const SMALL_LOC_SHIELDS_OFF: usize = 0x2c;

pub fn tows_cargo_pod(ship_type: u16) -> bool {
    matches!(ship_type, 32 | 62 | 92)
}

/// S→C 0x05 for a server-driven ship (176 B).
///
/// Unknown scalar bytes are copied from the user's own report (2026-11-10
/// capture) with the callsign and the uninitialised-memory tail removed: the
/// equipment array @4..0x14 (`19 19` then `fe` = empty), 0x16 and
/// 0x3e..0x43. Their meaning isn't traced. Known: 0x14 = pilot rank (the
/// sender copies 0x06 @2, `0x9d22cc`; shown as the level), 0x15 = fitted ECM
/// while it's switched on, else 0xfe (`0x9cd686`), 0x17 = cargo pod (see
/// [`tows_cargo_pod`]). The capture had rank 30 and a pod (a Thunder).
pub fn ship_report(ship: &ShipView, pid: u32, clock: f32) -> Vec<u8> {
    let mut m = vec![0u8; LOC_REPORT_LEN];
    m[0] = 0x05;
    m[1] = glow(ship);
    m[4..0x14].fill(0xfe);
    m[4] = 0x19;
    m[5] = 0x19;
    m[0x14..0x19].copy_from_slice(&[0x00, 0xfe, 0x01, 0x00, 0x0b]);
    m[LOC_RANK_OFF] = ship.rank;
    m[LOC_CARGO_POD_OFF] = tows_cargo_pod(ship.hull_type) as u8;
    put_cstr(&mut m[0x19..0x2f], &ship.callsign); // squad name @0x2f stays empty
    m[0x3e..0x43].copy_from_slice(&[0x04, 0x00, 0x7d, 0x05, 0x0d]);
    put_u16(&mut m, LOC_LOC_OFF, ship.loc);
    put_u16(&mut m, LOC_SHIP_OFF, ship.hull_type);
    put_unit(&mut m, 0x48, ship.fwd);
    put_unit(&mut m, 0x4e, ship.up);
    put_vel(&mut m, 0x54, ship.vel);
    put_u32(&mut m, LOC_PID_OFF, pid);
    put_f32(&mut m, 0x64, clock);
    put_u32(&mut m, 0x68, ship.mass.round() as u32); // slot mass +0x15c0 (int)
    put_vec(&mut m, 0x90, ship.pos);
    // Shields now/full and hull: the receiving slot's +0x15f8/+0x15fc/+0x1600.
    put_f32(&mut m, LOC_SHIELDS_OFF, ship.shields);
    put_f32(&mut m, LOC_SHIELDS_MAX_OFF, ship.shields_max);
    put_f32(&mut m, LOC_HULL_OFF, ship.hull);
    put_f32(&mut m, 0xa8, ship.thrust); // effective thrust, slot +0x1608
    put_f32(&mut m, 0xac, 100.0);
    m
}

/// S→C 0x33 "small loc" for a server-driven ship (56 B).
pub fn ship_small_loc(ship: &ShipView, pid: u32, clock: f32) -> Vec<u8> {
    let mut m = vec![0u8; SMALL_LOC_LEN];
    m[0] = 0x33;
    put_u16(&mut m, 4, ship.loc);
    put_unit(&mut m, 6, ship.fwd);
    put_unit(&mut m, 0x0c, ship.up);
    put_vel(&mut m, 0x12, ship.vel);
    put_u32(&mut m, 0x18, pid);
    put_vec(&mut m, 0x1c, ship.pos);
    put_f32(&mut m, 0x28, ship.thrust);
    put_f32(&mut m, SMALL_LOC_SHIELDS_OFF, ship.shields);
    put_f32(&mut m, 0x34, clock);
    m
}

/// 0x05 that removes a ship from clients' screens (ship type 0xfa).
pub fn ship_gone(ship: &ShipView, pid: u32, clock: f32) -> Vec<u8> {
    let mut m = ship_report(ship, pid, clock);
    put_u16(&mut m, LOC_SHIP_OFF, SHIP_GONE);
    m
}

// ---- combat ----

/// C→S 0x1D hit report (32 B, space link): one entry of the shooter's hit
/// queue, flushed by `0x43a3d0` each location tick (message-types "0x1D").
pub const HIT_LEN: usize = 32;
/// `kind` byte: what `weapon` indexes.
pub const HIT_KIND_GUN: u8 = 0;
pub const HIT_KIND_MISSILE: u8 = 1;
/// `class` byte.
pub const HIT_CLASS_SHIP: u8 = 0;
pub const HIT_CLASS_OBJECT: u8 = 1;
pub const HIT_CLASS_BEAM: u8 = 2;
pub const HIT_CLASS_SPECIAL: u8 = 3;
/// Bot id meaning "not a bot" (also S→C 0x03 @2 "damage is for me").
pub const NOT_A_BOT: u16 = 0xfe;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    pub shooter: u32,
    pub loc: u16,
    /// Victim's player id (ship slot +0x15b4); 0x7fff for an object hit.
    pub victim: u32,
    /// The victim's bot id, or [`NOT_A_BOT`].
    pub bot: u16,
    pub count: u16,
    pub kind: u8,
    /// Gun id (kind 0) or missile type (kind 1); 0xfe = unknown missile.
    pub weapon: u8,
    /// Damage the shooter already computed (splash, beams); 0 for direct
    /// gun and missile hits, which the server prices from the tables.
    pub value: f32,
    pub class: u8,
}

pub fn parse_hit(m: &[u8]) -> Option<Hit> {
    (m.len() >= 0x1d && m[0] == 0x1d).then(|| Hit {
        shooter: u32_at(m, 4),
        loc: u16_at(m, 8),
        victim: u32_at(m, 0xc),
        bot: u16_at(m, 0x10),
        count: u16_at(m, 0x12),
        kind: m[0x14],
        weapon: m[0x15],
        value: f32_at(m, 0x18),
        class: m[0x1c],
    })
}

/// Build a 0x1D as the client's flush does (for agents and tests).
pub fn hit(h: &Hit) -> Vec<u8> {
    let mut m = vec![0u8; HIT_LEN];
    m[0] = 0x1d;
    put_u32(&mut m, 4, h.shooter);
    put_u16(&mut m, 8, h.loc);
    put_u32(&mut m, 0xc, h.victim);
    put_u16(&mut m, 0x10, h.bot);
    put_u16(&mut m, 0x12, h.count);
    m[0x14] = h.kind;
    m[0x15] = h.weapon;
    put_f32(&mut m, 0x18, h.value);
    m[0x1c] = h.class;
    m
}

/// S→C 0x03 damage (12 B, handler `0x9d1463`): `[03][0][u16 bot id][u32
/// attacker][f32 damage]`. Bot id [`NOT_A_BOT`] = the receiver's own ship;
/// otherwise a bot the receiver controls, which also turns on the attacker.
pub const DAMAGE_LEN: usize = 12;

pub fn damage(bot: u16, attacker: u32, amount: f32) -> Vec<u8> {
    let mut m = vec![0u8; DAMAGE_LEN];
    m[0] = 0x03;
    put_u16(&mut m, 2, bot);
    put_u32(&mut m, 4, attacker);
    put_f32(&mut m, 8, amount);
    m
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Damage {
    pub bot: u16,
    pub attacker: u32,
    pub amount: f32,
}

pub fn parse_damage(m: &[u8]) -> Option<Damage> {
    (m.len() >= DAMAGE_LEN && m[0] == 0x03).then(|| Damage {
        bot: u16_at(m, 2),
        attacker: u32_at(m, 4),
        amount: f32_at(m, 8),
    })
}

/// C→S 0x44 "who killed me" (16 B, state link, sender `0x9cab20`):
/// `[44][0][u16 bot id][u32 reporter][i32 killer][i32 extra]`. Own death: bot
/// id 0xfe, killer = attacker of the last 0x03, extra −2. A bot's death: its
/// bot id, extra 0. The killer is sign-extended from 16 bits by the client.
pub const KILL_LEN: usize = 16;

pub fn kill_report(bot: u16, reporter: u32, killer: u32, extra: i32) -> Vec<u8> {
    let mut m = vec![0u8; KILL_LEN];
    m[0] = 0x44;
    put_u16(&mut m, 2, bot);
    put_u32(&mut m, 4, reporter);
    put_u32(&mut m, 8, killer as u16 as i16 as i32 as u32);
    put_u32(&mut m, 0xc, extra as u32);
    m
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Kill {
    pub bot: u16,
    pub reporter: u32,
    pub killer: u32,
}

pub fn parse_kill(m: &[u8]) -> Option<Kill> {
    (m.len() >= 12 && m[0] == 0x44).then(|| Kill { bot: u16_at(m, 2), reporter: u32_at(m, 4), killer: u32_at(m, 8) })
}

/// C→S 0x0C "impact" (20 B, state link, `0x40ea4c`): sent when a collision
/// kills the ship, just before `player_death`. `[0C][FE][u16 @2][u32 pid @4]`
/// then three dwords from the collision code's locals (not identified; sent
/// as 0). @2 is `0xff2a92` [inferred: the location].
pub const IMPACT_LEN: usize = 20;

pub fn impact_death(loc: u16, pid: u32) -> Vec<u8> {
    let mut m = vec![0u8; IMPACT_LEN];
    m[0] = 0x0c;
    m[1] = 0xfe;
    put_u16(&mut m, 2, loc);
    put_u32(&mut m, 4, pid);
    m
}

/// The 0x07 a dying client sends (`player_death` `0x9cf168`): a dock whose
/// target is 0xfe instead of a station.
pub const DEATH_DOCK_ARG: u32 = 0xfe;

/// S→C 0xB3 own-ship state (44 B, handler `0x9d94ea`): i32 ammo for gun slots
/// 0–7 at 4, 8, 0x10, 0x14, 0x1c, 0x20, 0x24, 0x28; f32 hull @0xc; f32 fuel
/// @0x18. The client takes all of it as authoritative.
pub const OWN_STATE_LEN: usize = 44;
pub const OWN_STATE_AMMO_OFFS: [usize; 8] = [4, 8, 0x10, 0x14, 0x1c, 0x20, 0x24, 0x28];

pub fn own_state(ammo: [i32; 8], hull: f32, fuel: f32) -> Vec<u8> {
    let mut m = vec![0u8; OWN_STATE_LEN];
    m[0] = 0xb3;
    for (o, a) in OWN_STATE_AMMO_OFFS.iter().zip(ammo) {
        put_u32(&mut m, *o, a as u32);
    }
    put_f32(&mut m, 0xc, hull);
    put_f32(&mut m, 0x18, fuel);
    m
}

// ---- byte helpers ----

pub fn u16_at(b: &[u8], o: usize) -> u16 {
    u16::from_le_bytes([b[o], b[o + 1]])
}
pub fn u32_at(b: &[u8], o: usize) -> u32 {
    u32::from_le_bytes([b[o], b[o + 1], b[o + 2], b[o + 3]])
}
pub fn f32_at(b: &[u8], o: usize) -> f32 {
    f32::from_bits(u32_at(b, o))
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

// ---- client side (agents): C→S builders, S→C parsers ----

/// C→S 0x05 login (33 B): callsign\0 @1, password\0 after it, version @0x16.
/// The server must never log this message.
pub const LOGIN_LEN: usize = 33;
pub const CLIENT_VERSION: &str = "2.0127";

pub fn login(callsign: &str, password: &str) -> Vec<u8> {
    let mut m = vec![0u8; LOGIN_LEN];
    m[0] = 0x05;
    let cs = &callsign.as_bytes()[..callsign.len().min(CALLSIGN_MAX)];
    m[1..1 + cs.len()].copy_from_slice(cs);
    let p = 1 + cs.len() + 1;
    let room = LOGIN_VERSION_OFF.saturating_sub(p + 1);
    let pw = &password.as_bytes()[..password.len().min(room)];
    m[p..p + pw.len()].copy_from_slice(pw);
    let v = CLIENT_VERSION.as_bytes();
    m[LOGIN_VERSION_OFF..LOGIN_VERSION_OFF + v.len()].copy_from_slice(v);
    m
}

/// C→S 0x07 launch/dock/jump (12 B).
pub fn travel(action: u8, from_loc: u16, arg: u32, pid: u32) -> Vec<u8> {
    let mut m = vec![0x07, action];
    m.extend_from_slice(&from_loc.to_le_bytes());
    m.extend_from_slice(&arg.to_le_bytes());
    m.extend_from_slice(&pid.to_le_bytes());
    m
}

pub const KEEPALIVE: [u8; 1] = [0x8f];

/// What a client needs from the 0x06 character record.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CharSummary {
    pub pid: u32,
    pub loc: u16,
    pub faction: u8,
    pub rank: u8,
    pub ship_type: u8,
    pub credits: u64,
    pub fittings: [u8; Fittings::LEN],
}

/// The rejection code (0xc8..=0xd1, not 0xca) if this 0x06 refuses a login.
pub fn parse_login_rejection(m: &[u8]) -> Option<u8> {
    let code = *m.get(CHAR_SHIP_OFF)?;
    (m[0] == 0x06 && (0xc8..=0xd1).contains(&code) && code != 0xca).then_some(code)
}

pub fn parse_char_record(m: &[u8]) -> Option<CharSummary> {
    if m.len() < CHAR_LEN || m[0] != 0x06 || parse_login_rejection(m).is_some() {
        return None;
    }
    let mut fittings = [0u8; Fittings::LEN];
    fittings.copy_from_slice(&m[CHAR_FIT_OFF..CHAR_FIT_OFF + Fittings::LEN]);
    Some(CharSummary {
        pid: u32_at(m, CHAR_PID_OFF),
        loc: u16_at(m, CHAR_LOC_OFF),
        faction: m[CHAR_FACTION_OFF],
        rank: m[CHAR_RANK_OFF],
        ship_type: m[CHAR_SHIP_OFF],
        credits: u64::from_le_bytes(m[CHAR_CREDITS_OFF..CHAR_CREDITS_OFF + 8].try_into().unwrap()),
        fittings,
    })
}

/// S→C 0x08 as a client reads it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct LocationChange {
    /// Contact slot to appear at: gate 1–4, 6 = station launch point, 0xcc = docked.
    pub sub: u8,
    pub loc: u16,
    /// Space server to use; None = "none" (docked: drop space).
    pub space_ip: Option<String>,
    /// Launch tube, meaningful when `sub` is 6.
    pub tube: u8,
}

pub fn parse_location_change(m: &[u8]) -> Option<LocationChange> {
    if m.len() < LOCCHG_LEN || m[0] != 0x08 {
        return None;
    }
    let off = game::space_ip_offset(0x08)?;
    let space_ip = if game::is_space_ip_none(m, off) {
        None
    } else {
        game::read_wstr(m, off, game::SPACE_IP_FIELD_BYTES).filter(|s| !s.is_empty())
    };
    Some(LocationChange { sub: m[1], loc: u16_at(m, LOCCHG_LOC_OFF), space_ip, tube: m[LOCCHG_TUBE_OFF] })
}

/// C→S 0x0F cargo report (392 B, sender `0x439f16`): `[0f][u8 flag][cc cc]`,
/// then 48 item-list slots @4 and the u32 player id @0x184. A client sends one with
/// flag 0 just before docking.
pub const CARGO_REPORT_LEN: usize = 392;

pub fn cargo_report(c: &Character, pid: u32, flag: u8) -> Vec<u8> {
    let mut m = vec![0xcc; CARGO_REPORT_LEN];
    m[0] = 0x0f;
    m[1] = flag;
    put_items(&mut m, 4, c);
    put_u32(&mut m, 0x184, pid);
    m
}

/// C→S 0x17 companion report (10 B, state conn, 1:1 with each 0x05). The nine
/// bytes after the type aren't decoded [inferred: compact heading/input];
/// the server only uses it as a heartbeat (answers 0x42 + 0xB3).
pub fn companion_report() -> Vec<u8> {
    let mut m = vec![0u8; 10];
    m[0] = 0x17;
    m
}

// ---- economy ----

/// C→S 0x09 market command: `[09][cmd][u16 loc][u32 index @4][u32 pid @8][f32 qty @12]`.
pub struct MarketCmd {
    pub cmd: u8,
    pub index: u32,
    pub qty: f32,
}

pub fn parse_market_cmd(m: &[u8]) -> Option<MarketCmd> {
    (m.len() >= 16 && m[0] == 0x09).then(|| MarketCmd {
        cmd: m[1],
        index: u32_at(m, 4),
        qty: f32::from_le_bytes(m[12..16].try_into().unwrap()),
    })
}

/// 0x09 sell: index = slot in the 0x45 item list.
pub const CMD_SELL: u8 = 0x08;

/// Item category bought by a 0x09 buy sub-op (message-types "Station market").
pub fn buy_category(cmd: u8) -> Option<u8> {
    match cmd {
        0x04 => Some(0),
        0x05 => Some(1),
        0x07 => Some(3),
        0x0a..=0x10 => Some(5 + (cmd - 0x0a)),
        _ => None,
    }
}

/// Listing reply type and entry count per category.
pub fn listing_for_category(cat: u8) -> Option<(u8, usize)> {
    match cat {
        0 => Some((0x0a, 60)),
        1 => Some((0x0b, 24)),
        3 => Some((0x0d, 99)),
        5 => Some((0x50, 27)),
        6 => Some((0x51, 27)),
        7 => Some((0x52, 20)),
        8 => Some((0x53, 8)),
        9 => Some((0x54, 25)),
        10 => Some((0x55, 21)),
        11 => Some((0x56, 70)),
        _ => None,
    }
}

/// Category asked for by a 0x09 listing command.
pub fn listing_category(cmd: u8) -> Option<u8> {
    match cmd {
        0x00 => Some(0),
        0x01 => Some(1),
        0x03 => Some(3),
        0x5a..=0x60 => Some(5 + (cmd - 0x5a)),
        _ => None,
    }
}

/// Market listing (896 B): `[type][3 pad]` + N × (u32 stock, u32 price), and N
/// flag bytes at 0x31C (1 = no 2% surcharge; the commodity handler zeroes them).
pub const LISTING_FLAGS_OFF: usize = 0x31c;

/// The market list (`0x44f870`) hides an equipment entry unless its stock is
/// non-zero **and its price is above 200** (outside GM / "show all" modes).
/// Commodities aren't filtered by price.
pub const EQUIPMENT_MIN_PRICE: u32 = 201;

/// The price a listing can actually show for `cat`: `price`, raised to the
/// equipment floor for everything but commodities.
pub fn listed_price(cat: u8, price: u32) -> u32 {
    if cat == crate::character::CAT_COMMODITY {
        price
    } else {
        price.max(EQUIPMENT_MIN_PRICE)
    }
}

pub fn listing(cat: u8, stock: u32, price: u32) -> Option<Vec<u8>> {
    let (ty, n) = listing_for_category(cat)?;
    let price = listed_price(cat, price);
    let mut m = vec![0u8; LISTING_LEN];
    m[0] = ty;
    for i in 0..n {
        put_u32(&mut m, 4 + 8 * i, stock);
        put_u32(&mut m, 8 + 8 * i, price);
        m[LISTING_FLAGS_OFF + i] = 1;
    }
    Some(m)
}

/// C→S 0x66 fitting change: `[66][u8 item-list slot][u8 category][u8 ship slot][u8 op]`.
pub const FIT_UNFIT: u8 = 0;
pub const FIT_FIT: u8 = 1;
pub const FIT_MISSILE_FIRED: u8 = 2;

pub struct FitChange {
    pub list_slot: u8,
    pub cat: u8,
    pub ship_slot: u8,
    pub op: u8,
}

pub fn parse_fit_change(m: &[u8]) -> Option<FitChange> {
    (m.len() >= 5 && m[0] == 0x66).then(|| FitChange { list_slot: m[1], cat: m[2], ship_slot: m[3], op: m[4] })
}

/// C→S 0x08 change ship (8 B, docked): `[08][u8 ship type][2 junk][u32 pid]`.
pub fn parse_change_ship(m: &[u8]) -> Option<u8> {
    (m.len() >= 2 && m[0] == 0x08).then(|| m[1])
}

/// S→C 0x45 credits and item list (400 B): `[45][7 pad][u64 credits @8]` +
/// 48 × `[cat][idx][cc cc][f32 qty]` @0x10.
pub const CREDITS_ITEMS_LEN: usize = 400;

fn put_items(m: &mut [u8], off: usize, c: &Character) {
    for (k, it) in c.item_slots().iter().enumerate() {
        let o = off + 8 * k;
        if it.is_empty() {
            m[o..o + 8].copy_from_slice(&[0xfe, 0xfe, 0xcc, 0xcc, 0, 0, 0, 0]);
        } else {
            m[o..o + 4].copy_from_slice(&[it.cat, it.idx, 0xcc, 0xcc]);
            put_f32(m, o + 4, it.qty);
        }
    }
}

pub fn credits_items(c: &Character) -> Vec<u8> {
    let mut m = vec![0xcc; CREDITS_ITEMS_LEN];
    m[0] = 0x45;
    m[8..16].copy_from_slice(&c.credits.to_le_bytes());
    put_items(&mut m, 0x10, c);
    m
}

/// S→C 0x92 experience total (8 B).
pub fn experience(exp: u32) -> Vec<u8> {
    let mut m = vec![0x92, 0xcc, 0xcc, 0xcc];
    m.extend_from_slice(&exp.to_le_bytes());
    m
}

// 0x06 fields decoded from the handler `0x9d205f` [static 2026-11-19].
pub const CHAR_FACTION_OFF: usize = 1;
pub const CHAR_RANK_OFF: usize = 2;
pub const CHAR_SHIP_OFF: usize = 0x42;
pub const CHAR_PRICE_FACTOR_OFF: usize = 0x52; // i16; market factor 1 + v/10000
pub const CHAR_EXP_OFF: usize = 0x5c;
pub const CHAR_CREDITS_OFF: usize = 0x68;
pub const CHAR_FIT_OFF: usize = 0x70;
pub const CHAR_SHIPNAME_OFF: usize = 0x8f;
pub const CHAR_ITEMS_OFF: usize = 0x9c;

/// Why a login is refused. The client answers a 0x06 whose byte @0x42 (the
/// ship type in a real record) is one of these codes with its "Connection
/// Error!" panel, showing line N of its text table (`sampleslarge.txt`), and
/// drops its logged-in state (handler `0x9d207f`, panel `0x403010`; notes
/// message-types "0x06 login rejection"). 0xca is not a rejection code.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LoginRejection {
    /// 0xc8, line 120: incorrect version of Jumpgate.
    WrongVersion,
    /// 0xc9, line 121: "This username is not valid".
    InvalidUsername,
    /// 0xcb, line 119: pilot already connected.
    AlreadyConnected,
    /// 0xcc (0xcf shows the same line), line 111: account expired.
    Expired,
    /// 0xcd, line 118: account not enabled.
    NotEnabled,
    /// 0xce, line 117: "Password or username incorrect".
    BadPassword,
    /// 0xd1, line 122: temporarily disabled, plus "Remaining time: %.2f
    /// hours." from the f32 @0x60.
    Suspended { hours: f32 },
}

pub const CHAR_REJECT_HOURS_OFF: usize = 0x60;

/// A 0x06 that refuses the login. Only @0x42 (and @0x60 for a suspension) is
/// read before the handler bails, so the rest is zero.
pub fn login_rejection(why: LoginRejection) -> Vec<u8> {
    let mut m = vec![0u8; CHAR_LEN];
    m[0] = 0x06;
    m[CHAR_SHIP_OFF] = match why {
        LoginRejection::WrongVersion => 0xc8,
        LoginRejection::InvalidUsername => 0xc9,
        LoginRejection::AlreadyConnected => 0xcb,
        LoginRejection::Expired => 0xcc,
        LoginRejection::NotEnabled => 0xcd,
        LoginRejection::BadPassword => 0xce,
        LoginRejection::Suspended { hours } => {
            put_f32(&mut m, CHAR_REJECT_HOURS_OFF, hours);
            0xd1
        }
    };
    m
}

/// Build the 0x06 for `c`. Bytes not modelled are taken from `template` (a
/// captured record) when given, else from constants seen in the captures.
pub fn char_record(template: Option<&[u8]>, c: &Character, pid: u32, loc: u16) -> Vec<u8> {
    let mut m = match template {
        Some(t) if t.len() == CHAR_LEN && t[0] == 0x06 => t.to_vec(),
        _ => {
            let mut m = vec![0xcc; CHAR_LEN];
            m[0] = 0x06;
            m[0x0e..0x14].copy_from_slice(&[0x04, 0x00, 0x7d, 0x05, 0x0d, 0x00]);
            m[0x22..0x24].fill(0);
            m[0x43..0x4f].copy_from_slice(&[0xfe, 0x01, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]);
            m[0x56..0x68].fill(0);
            put_f32(&mut m, 0x60, 1.0);
            m[0x6c..0x70].fill(0);
            m[0x21c..0x220].fill(0);
            m
        }
    };
    patch_char_record(&mut m, &c.callsign, pid, loc);
    m[CHAR_FACTION_OFF] = c.faction;
    m[CHAR_RANK_OFF] = c.rank;
    m[CHAR_SHIP_OFF] = c.ship_type;
    put_u16(&mut m, CHAR_PRICE_FACTOR_OFF, 0);
    put_u32(&mut m, CHAR_EXP_OFF, c.experience);
    m[CHAR_CREDITS_OFF..CHAR_CREDITS_OFF + 8].copy_from_slice(&c.credits.to_le_bytes());
    m[CHAR_FIT_OFF..CHAR_FIT_OFF + Fittings::LEN].copy_from_slice(&c.fittings.to_bytes());
    let name = &mut m[CHAR_SHIPNAME_OFF..CHAR_ITEMS_OFF];
    name.fill(0xcc);
    put_cstr(name, "My Ship");
    put_items(&mut m, CHAR_ITEMS_OFF, c);
    debug_assert_eq!(CHAR_ITEMS_OFF + 8 * ITEM_SLOTS, 0x21c);
    m
}

#[cfg(test)]
mod tests {
    #[test]
    fn botloc_round_trips_and_hands_over() {
        let b = super::BotState {
            state: 0,
            shields: 0.5,
            hull: 0.25,
            id: 7,
            loc: 118,
            ship_type: 233,
            vel: [10.0, -20.0, 0.5],
            fwd: [0.0, 0.0, 1.0],
            up: [0.0, 1.0, 0.0],
            controller: 1000,
            target: 1000,
            pos: [1.0, 2.0, 3.0],
        };
        let m = super::botloc(&b);
        assert_eq!(super::parse_bot_state(&m), Some(b));
        let p = super::parse_botloc(&m).unwrap();
        assert_eq!((p.id, p.loc, p.ship_type, p.controller), (7, 118, 233, 1000));
        let h = super::parse_botloc(&super::botloc_handed_to(&m, 1001).unwrap()).unwrap();
        assert_eq!((h.controller, h.target), (1001, 1001));
        assert_eq!(super::parse_bxr(&super::bxr(7, 118, 1001)), Some(super::Bxr { bot: 7, loc: 118, pid: 1001 }));
        assert_eq!(super::bot_gone(1, 7), vec![0x16, 1, 7, 0, 0, 0]);
        // The captured C→S 0x32 (jgcap2): Purgatory (7) at conflux #1.
        let cap: Vec<u8> = "32 01 07 00 01 00 01 01 43 fe 95 fb 8b 01 0c 0d 76 22 0a f3 f9 0e 00 00 f9 0e 00 00 44 42 69 c5 69 b1 c4 45 e5 7b 5d 45"
            .split(' ')
            .map(|h| u8::from_str_radix(h, 16).unwrap())
            .collect();
        let mm = super::parse_missile(&cap).unwrap();
        assert_eq!((mm.id, mm.ty, mm.target_bot, mm.owner), (1, 7, 1, 3833));
        assert!((mm.vel[0] + 44.5).abs() < 1e-3 && (crate::ship::length(mm.fwd) - 1.0).abs() < 1e-3);
        assert_eq!(super::parse_missile(&super::missile(&mm)).unwrap().pos, mm.pos);
    }

    #[test]
    fn combat_messages_round_trip() {
        let h = Hit {
            shooter: 3,
            loc: 118,
            victim: 4,
            bot: NOT_A_BOT,
            count: 2,
            kind: HIT_KIND_MISSILE,
            weapon: 5,
            value: 0.0,
            class: HIT_CLASS_SHIP,
        };
        let m = hit(&h);
        assert_eq!(m.len(), HIT_LEN);
        assert_eq!(parse_hit(&m), Some(h));
        let d = damage(7, 3, 1.5e6);
        assert_eq!(&d[..4], &[0x03, 0, 7, 0]);
        assert_eq!(parse_damage(&d), Some(Damage { bot: 7, attacker: 3, amount: 1.5e6 }));
        // 0x44: reporter @4, killer @8 sign-extended from 16 bits, extra @0xc.
        let k = kill_report(NOT_A_BOT, 3, 0x1_0005, -2);
        assert_eq!(k, vec![0x44, 0, 0xfe, 0, 3, 0, 0, 0, 5, 0, 0, 0, 0xfe, 0xff, 0xff, 0xff]);
        assert_eq!(parse_kill(&k), Some(Kill { bot: NOT_A_BOT, reporter: 3, killer: 5 }));
        let b3 = own_state([1, 2, 3, 4, 5, 6, 7, 8], 12.0e6, 87.0);
        assert_eq!((u32_at(&b3, 0x10), u32_at(&b3, 0x28), f32_at(&b3, 0xc), f32_at(&b3, 0x18)), (3, 8, 12.0e6, 87.0));
    }

    #[test]
    fn reports_carry_real_shields_and_hull() {
        let mut v = npc();
        (v.shields, v.shields_max, v.hull) = (1.0e6, 7.0e6, 9.0e6);
        let r = ship_report(&v, 1, 0.0);
        assert_eq!((f32_at(&r, 0x9c), f32_at(&r, 0xa0), f32_at(&r, 0xa4)), (1.0e6, 7.0e6, 9.0e6));
        assert_eq!(f32_at(&ship_small_loc(&v, 1, 0.0), 0x2c), 1.0e6);
    }

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

    fn npc() -> ShipView {
        ShipView {
            loc: 118,
            hull_type: 104,
            callsign: "Hauler".into(),
            mass: 515000.0,
            pos: [2500.0, 400.0, -12.5],
            vel: [-3.2, 0.0, 79.9],
            fwd: [-0.04, 0.0, 0.9992],
            up: [0.0, 1.0, 0.0],
            thrust: 736000.0,
            shields: 62.5e6,
            shields_max: 62.5e6,
            hull: 187.5e6,
            rank: 0,
        }
    }

    fn i16s(m: &[u8], o: usize, scale: f32) -> [f32; 3] {
        [0, 1, 2].map(|k| i16::from_le_bytes([m[o + 2 * k], m[o + 2 * k + 1]]) as f32 * scale)
    }
    fn f32s(m: &[u8], o: usize) -> [f32; 3] {
        [0, 1, 2].map(|k| f32::from_le_bytes(m[o + 4 * k..o + 4 * k + 4].try_into().unwrap()))
    }
    fn f1(m: &[u8], o: usize) -> f32 {
        f32::from_le_bytes(m[o..o + 4].try_into().unwrap())
    }
    fn close(a: [f32; 3], b: [f32; 3], eps: f32) -> bool {
        a.iter().zip(b).all(|(x, y)| (x - y).abs() <= eps)
    }

    /// Decode with the client handler's offsets and scales (0x9cf7c8).
    #[test]
    fn ship_report_decodes_as_the_client_reads_it() {
        let s = npc();
        let m = ship_report(&s, 0x4000_0001, 12.5);
        assert_eq!(m.len(), LOC_REPORT_LEN);
        assert_eq!(m[0], 0x05);
        assert_eq!(u16_at(&m, LOC_LOC_OFF), 118);
        assert_eq!(u16_at(&m, LOC_SHIP_OFF), 104);
        assert_eq!(u32_at(&m, LOC_PID_OFF), 0x4000_0001);
        // Callsign must be ≥ 2 chars or the client ignores the message.
        assert_eq!(&m[0x19..0x20], b"Hauler\0");
        assert!(m[0x2f..0x3e].iter().all(|&b| b == 0), "no squad / no leaked bytes");
        assert!(close(i16s(&m, 0x48, 1e-4), s.fwd, 1e-4));
        assert!(close(i16s(&m, 0x4e, 1e-4), s.up, 1e-4));
        assert!(close(i16s(&m, 0x54, 0.1), s.vel, 0.05));
        assert_eq!(f32s(&m, 0x90), s.pos);
        assert_eq!(f1(&m, 0x64), 12.5);
        assert_eq!(u32_at(&m, 0x68), 515000);
        assert_eq!(f1(&m, 0xa8), 736000.0);
        assert_eq!(m[1], 50, "engine glow on while thrusting");
        assert_eq!(u16_at(&ship_gone(&s, 1, 13.0), LOC_SHIP_OFF), SHIP_GONE);
    }

    /// Decode with the 0x33 handler's offsets (0x9d573c).
    #[test]
    fn ship_small_loc_decodes_as_the_client_reads_it() {
        let s = npc();
        let m = ship_small_loc(&s, 0x4000_0001, 12.7);
        assert_eq!(m.len(), SMALL_LOC_LEN);
        assert_eq!(m[0], 0x33);
        assert_eq!(u16_at(&m, 4), 118, "client drops 0x33 for another location");
        assert_eq!(fast_loc_pid(&m), Some(0x4000_0001));
        assert!(close(i16s(&m, 6, 1e-4), s.fwd, 1e-4));
        assert!(close(i16s(&m, 0x0c, 1e-4), s.up, 1e-4));
        assert!(close(i16s(&m, 0x12, 0.1), s.vel, 0.05));
        assert_eq!(f32s(&m, 0x1c), s.pos);
        assert_eq!(f1(&m, 0x28), 736000.0);
        assert_eq!(f1(&m, 0x34), 12.7);
        // Same slot field as 0x05 @0x9c, so the two must agree.
        assert_eq!(f1(&m, 0x2c), f32s(&ship_report(&s, 1, 0.0), 0x9c)[0]);
    }

    #[test]
    fn login_builder_round_trips() {
        let m = login("agent01", "pw");
        assert_eq!(m.len(), LOGIN_LEN);
        let l = parse_login(&m).unwrap();
        assert_eq!((l.callsign.as_str(), l.password.as_slice(), l.version.as_str()), ("agent01", &b"pw"[..], "2.0127"));
    }

    fn pilot() -> Character {
        let mut c = Character {
            callsign: "Newbie".into(),
            faction: 3,
            rank: 0,
            ship_type: 90,
            fittings: Fittings::default(),
            items: Vec::new(),
            credits: 200_000,
            experience: 12,
        };
        c.fittings.engine = 0;
        c.add_item(3, 37, 2.5);
        c
    }

    #[test]
    fn char_record_fields() {
        let c = pilot();
        let m = char_record(None, &c, 1001, 7);
        assert_eq!(m.len(), CHAR_LEN);
        let s = parse_char_record(&m).unwrap();
        assert_eq!((s.pid, s.loc, s.faction, s.ship_type, s.credits), (1001, 7, 3, 90, 200_000));
        assert_eq!(s.fittings[24], 0);
        assert_eq!(&m[3..10], b"Newbie\0");
        assert_eq!(u32_at(&m, CHAR_EXP_OFF), 12);
        assert_eq!(&m[CHAR_ITEMS_OFF..CHAR_ITEMS_OFF + 4], &[3, 37, 0xcc, 0xcc]);
        assert_eq!(&m[CHAR_SHIPNAME_OFF..CHAR_SHIPNAME_OFF + 8], b"My Ship\0");
        assert_eq!((m[0x44], m[CHAR_MODE_OFF]), (1, 0), "as captured; mode 0 = no space connect");
        // 0x45 carries the same list.
        let ci = credits_items(&c);
        assert_eq!(ci.len(), CREDITS_ITEMS_LEN);
        assert_eq!(&ci[0x10..0x14], &[3, 37, 0xcc, 0xcc]);
        assert_eq!(&ci[0x18..0x1a], &[0xfe, 0xfe]);
    }

    #[test]
    fn listings_and_commands() {
        let m = listing(3, 99, 99).unwrap();
        assert_eq!((m[0], m.len(), u32_at(&m, 4), u32_at(&m, 8 + 8 * 98), m[LISTING_FLAGS_OFF + 98]), (0x0d, 896, 99, 99, 1));
        assert_eq!(m[LISTING_FLAGS_OFF + 99], 0);
        for cmd in [0x00, 0x01, 0x03, 0x5a, 0x60] {
            let cat = listing_category(cmd).unwrap();
            assert_eq!(listing_for_category(cat).map(|l| l.0), listing_for_command(cmd));
        }
        assert_eq!((buy_category(0x07), buy_category(0x0a), buy_category(0x10), buy_category(0x08)), (Some(3), Some(5), Some(11), None));
    }

    #[test]
    fn location_change_round_trips() {
        let mut m = location_change(None, 6, 107, SpaceTarget::Ip("127.0.0.1"));
        m[LOCCHG_TUBE_OFF] = 3;
        assert_eq!(
            parse_location_change(&m),
            Some(LocationChange { sub: 6, loc: 107, space_ip: Some("127.0.0.1".into()), tube: 3 })
        );
        let m = location_change(None, 0xcc, 7, SpaceTarget::None);
        assert_eq!(parse_location_change(&m).unwrap().space_ip, None);
    }

    #[test]
    fn report_rank_and_cargo_pod_follow_the_ship() {
        let mut v = npc();
        v.hull_type = 90;
        v.rank = 0;
        let m = ship_report(&v, 1, 1.0);
        assert_eq!((m[LOC_RANK_OFF], m[LOC_CARGO_POD_OFF]), (0, 0), "Storm, rank 0: no pod");
        v.hull_type = 92;
        v.rank = 12;
        let m = ship_report(&v, 1, 1.0);
        assert_eq!((m[LOC_RANK_OFF], m[LOC_CARGO_POD_OFF]), (12, 1), "Thunder tows a pod");
    }

    #[test]
    fn equipment_listings_respect_the_display_floor() {
        assert_eq!(u32_at(&listing(7, 99, 99).unwrap(), 8), EQUIPMENT_MIN_PRICE);
        assert_eq!(u32_at(&listing(3, 99, 99).unwrap(), 8), 99);
        assert_eq!(u32_at(&listing(0, 99, 5000).unwrap(), 8), 5000);
    }

    #[test]
    fn login_rejection_sets_the_code_at_the_ship_type() {
        let m = login_rejection(LoginRejection::BadPassword);
        assert_eq!((m.len(), m[0], m[CHAR_SHIP_OFF]), (CHAR_LEN, 0x06, 0xce));
        let m = login_rejection(LoginRejection::Suspended { hours: 2.5 });
        assert_eq!(m[CHAR_SHIP_OFF], 0xd1);
        assert_eq!(parse_login_rejection(&m), Some(0xd1));
        assert!(parse_char_record(&m).is_none());
        assert_eq!(f32::from_le_bytes(m[CHAR_REJECT_HOURS_OFF..][..4].try_into().unwrap()), 2.5);
    }
}

