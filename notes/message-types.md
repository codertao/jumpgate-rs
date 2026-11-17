# Jumpgate — game message types & field layouts

Type = first decoded byte. Direction: **C→S** client→server, **S→C** server→client.
Lengths are decoded payload bytes (incl. the type byte). The type-number space is
largely shared across directions. Evidence: **[static]** from the send-site /
handler code, **[capture]** seen in the 2026-11-10 session.

Client send sites were recovered by tracing the 260 calls to `SendGameMessage`
(`0x9c9c10`); see `reversing-map.md` and `tools/` for the method. Confidence
varies — treat single-byte offsets as solid, deeper structure as working theory.

Common sub-fields seen across many messages:
- `u16 loc` at a fixed offset — current sector/location id (global `0xbe5b3e`).
- `u32 player_id` — player/session id (global `0xbe5da0`).

---

## Client → server (confirmed by capture)

> Which **server** (state 5150 vs space 5151) each C→S type goes to: see
> network-protocol.md §1.1a. In short, positions/bots/shots/kills go to space and everything else to state.

| Type | Len | Name / meaning | Layout (offsets in bytes) |
|---|---|---|---|
| 0x05 | 33 | **Login** | `[05]` + callsign`\0` @1 + password`\0` (ends before +0x16) + version ("2.0127") at +0x16. **Cleartext password.** [capture, 2026-11-17] |
| 0x05 | 176 | Ship / loadout state (periodic, ~13 s) | `[05][u16?]` + callsign + slot list + floats; also contains some uninitialized client memory (minor info leak) |
| 0x07 | 12 | **Launch / Dock / Jump** | `[07][action][u16 from_loc][u32 to_loc][u32 player_id]`; action 1=launch, 2=dock, 3=jump |
| 0x09 | 16 | **Command / listing request** | `[09][cmd][u16 loc][u32 0][u32 player_id][u32]`; ~25 cmd values. Replies come back as the 896-byte listing tables (see below). |
| 0x0E | 16 | Consumables update | `[0E]...` |
| 0x04 | 27 | Chat line | `[04][05][u16 loc][u32 player_id][callsign\0][text...]` |
| 0x65 | 36 | Join chat channel ("chat") | `[65][u16 loc?][u32 player_id]["chat"\0]...` |
| 0x6A | 8 | Player-list / player request | `[6A][...][u32 player_id]` |
| 0x71 | 52 | Bot state report "botloc" (see Space section; 1000 = offline-sim variant) | `[71]...` |
| 0x8F | 1 | **Keepalive** | `[8F]` — one per connection every ~16 s |
| 0xC1 | 12 | Job-list request | `[C1][sub][u16 loc][u32 player_id][u32]` |
| 0xC8 | 8 | Logout / session end | `[C8][u16 loc?][u32 player_id]` |

Other C→S types mapped **[static]** from send sites (names from the client's
"Error Sending ..." strings; see `captures/send_types.txt`): 0x08 ship, 0x0C
impact, 0x0F building(392B), 0x10 prodboost, 0x16 reject-spawn, 0x36 wingman,
0x37–0x41 ("packet 55"–"65" in the strings = decimal), 0x43 hits-counter,
0x44 who-killed-me, 0x4A trade-ack(sub@1), 0x4C mission, 0x4D modx, 0x4E mentor,
0x4F teleport, 0x59 home, 0x74 asteroid-amount, 0x7B registry, 0x83 list-channels,
0x8A weather-request(1B), 0x90 findPOSowner, 0x91 missions, 0x93 mode, 0x96
volunteer, 0x9C event/query (sub = u16 @ off 8: 0xEC/0xED...), 0x9D vouch,
0xAA/0xAB invite, 0xAC shipname, 0xBB home-off, 0xC3 "util" (sub@1: 0xF2 miltour,
0xF3 util-243, 0xF4 insurance, ...), 0xC5 commission/away.

---

## Server → client (decoded from capture)

| Type | Len | Name / meaning | Layout |
|---|---|---|---|
| 0x06 | 544 | Character / ship record (handler `0x9d205f`) | `[06][03][1e]` + callsign`\0` @3 … **space-server address @0x22** (UTF-16, same rule as 0x08, acted on only if **mode @0x45 == 1**) … **u16 loc @0x50**, **u32 player id @0x54** (→ `0xbe5da0`) … "My Ship" + 8-byte cargo/equip slots (`fe fe` = empty slot). [static + capture, 2026-11-17] |
| 0x81 | ~850 | Message of the day | `[81]` + ASCII ("Welcome back! ...") |
| 0xFE | 321 | **Memory patch** | `[FE]` then records `[01][u32 addr][u16 len][bytes]`. In capture: only award/medal name strings. See network-protocol.md §3. |
| 0x0A/0x0B/0x0D, 0x50–0x56 | 896 | **Station listing tables** (one per category) | `[type][3 pad][N × (u32,u32)]` + `N` bytes at +0x31C. Each answers one 0x09 Command request, in request order (cmd 0x03 "commodity"→0x0D, 0x01 "missile"→0x0B, 0x5A–0x60→0x50–0x56). Pairs look like price/quantity [inferred]. |
| 0x08 | 64 | **Location change / space-server redirect** (handler `0x9d30c6`) | `[08][sub]` … space-server address @0x0c (UTF-16LE, ≤16 units incl. NUL, field ends at 0x2c) … **u16 dest loc @0x2c** (checked 1..1005) … @0x34. If the address differs from the current one (`0xba79a0`), the client drops space and connects to `<addr>:5151` (`0x9c7190`). Dock sends **"none" as narrow ASCII bytes**: the compare is `wcscmp` (0xaabf39) against a narrow literal at `0xb05afc`, which only works because both are the same raw bytes. **sub @1 = contact slot to appear at**: the arrival gate number (= extract `gate_dest_gate`) for a jump, 6 for a launch, 0xcc on dock (the handler uses `slot = msg[1]`, position = slot pos + random offset). Verified against all captured jumps. [static + capture, 2026-11-17] |
| 0x04 | 20 / 69 | Chat line (e.g. "KTRI" news bot) | `[04][sub][u16 loc][u32 player_id][callsign\0]...` |
| 0x65 | 35 | Channel-join ack | `[65][...][u32 handle]` |
| 0xC2 | 750 | Table of 41 × 18-byte entries, mostly empty | `[C2]` + entries padded with `cc`/`ff ff ff` |
| 0x78 | 150 | Empty object list at launch (sim) [inferred] | all zero |
| 0x18 | 8 | Small status (medal?) | `[18][u32][ff 00 ff 01]` |
| 0x8C | 7 | Small status | `[8C][cc cc cc][u16?]` |
| 0x05 | 176 | **Other ship create/update** (handler `0x9cf7c8`): same layout as the C→S location report. Matched by player id @0x5c against slot `+0x15b4`; a new id takes the first free ship slot 10–199. Ship type u16 @0x46 (0xf9/0xfa = remove [inferred]), loc @0x44. This is what the fan server relays. | [static, 2026-11-17] |
| 0x33 | var | smallloc relay: another ship's 0x33, same layout as C→S | handler `0x9d573c`. Matched by player id @0x18; dropped unless clock @0x34 is newer. Position @0x1c, velocity i16×0.1 @0x12. Error < 200 u is blended into velocity, otherwise snapped (physics.md §8) [static 2026-11-11] |
| 0x34 | var | miniloc relay: another ship's 0x34, same layout as C→S | handler `0x9d5a9d`. id @0x14, position @0x18, clock @0x30. No velocity; same blend/snap rule [static 2026-11-11] |
| 0x61 | 4 | **Beacon update** (handler `0x9d67d5`) | `[61][state][u16 loc]`. Valid only for loc 201..=999 and only when contact slot 5 is active with model 2 (the sector beacon); otherwise prints "Received invalid beacon packet". `state` → location table +0x3c [inferred: tuned/owner; capture sent 0]. Station sectors 101–118 have no beacon. [static + live, 2026-11-17] |
| 0x45 | — | rank | handler `0x9d662a` |
| 0x8F | 896 | welcome / initial world state | `[8F]...` |

Notes:
- Server structs are fixed-size, padded with `0xCC` — the MSVC debug-build fill
  byte, suggesting the server is C/C++, possibly a debug build [inferred].
- Many one-byte S→C messages are RakNet-level acks/keepalive responses.
- Full decoded server dumps: `captures/server_messages_decoded.txt`.
- Hitting the station while docking produced no identifiable message — sim
  collisions appear to be handled client-side [capture].

---

## Space / sector (2026-11-10 space-flight capture, 2nd run)

A ~10 min space flight (878 msgs). The steady-state loop, repeating every ~13 s:
client sends `0x05`(176)+`0x17`(10); server answers a batch of `0xa7`(56)×~10 +
`0x42`(1) + `0xb3`(44). Launch is `C→S 0x07`; server replies `0x78`+`0x08`
(space-server redirect, IP as UTF-16), then streams the scene.

| Type | Len | Dir | Name / meaning | Layout |
|---|---|---|---|---|
| 0xA7 | 56 | S→C | **Sector object (station/base) record** | `[A7][u64 hdr/owner][name\0 … 0xCC pad][cat@34][f32 X@36][f32 Y@40][f32 Z@44][flag@48][u32 id@52]`. `cat` 2=public station, 4=player mining HQ, 8=special/factory. Streamed **in full every heartbeat**; positions static per id. Capture held **21 distinct** (13/3/5 by cat). |
| 0x87 | 260 | S→C | **Particle clouds ("weather"), universe-wide** — answer to 0x8A weather request | `[87][3 pad]` + 8 × 32 B records copied to `0xff1b18` (handler `0x9d8083`): `[type u8][kind u8][strength u8][b3][f32 radius][f32×3 drift unit vector][f32×3 position in **galaxy-map coords** (same space as locations.csv x/y/z)]`. type 0 = empty slot, 1 = particle cloud (2/3 = other phenomena, unseen). See "Particle clouds" below. [static + capture, 2026-11-17; replaces the earlier "near-field transforms" reading] |
| 0x86 | 36? | S→C | Single particle-cloud update (handler `0x9d80c1`) | `[86][slot<8]..` + one 32-byte record @4 → `0xff1b18 + slot*32` [static] |
| 0x73 | 251 | S→C | Object-slot occupancy table | `[73]` + `0xFF`-filled free slots + `u32 count` + zeros [inferred] |
| 0x78 | 151 | S→C | Object list at launch | mostly zero; `[78][u8 n]…` slot list [inferred] |
| 0xB3 | 44 | S→C | Own-ship physics echo (per heartbeat) | `[B3][3 pad]` + floats (one @12 ~7.2e7, one @26 =100.0) [inferred] |
| 0x42 | 1 | S→C | Heartbeat ack (pairs with 0x17) | `[42]` |
| 0x62 | 800 | S→C | Large table, empty in capture (sector contacts?) | zeros [inferred] |
| 0x13 | 788 | S→C | Large table (world/sector state?) | [inferred] |

Client → server in space (also see reversing-map "Weapon hits & damage": C→S 0x1D hit report, 0x1A mkill, 0x44 who-killed-me; S→C 0x03 damage):
| Type | Len | Name | Layout / notes |
|---|---|---|---|
| 0x05 | 176 | **"gloc 3" full location/state report** (every 17th location tick: ~13.6 s idle, ~5 s while steering) | `[05][state@1][flags@2][salt@3]` + equipment/cargo slot array (`fe fe`=empty, same format as 0x06 char record) + callsign`\0` + … + **`f32 clock_s @100`** (session clock, seconds) + … + **`f32 X @144, Y @148, Z @152`** (ship position, sector coordinates) + fixed capacity consts after. Sent by `0x9cd410`; see "Location update path" below. [capture-verified 2026-11-10: track matches route, gates, mining spot] |
| 0x33 | 56 | **"small loc" fast position update** (only when another ship is within 10,000 units) | `[33][flags@1][state@2][salt@3][u16 loc@4]` + 6×i16 orientation (fwd+up unit vectors ×10000) @6 + 3×i16 **velocity ×10** @0x12 [static, 2026-11-11: packed from snapshot `0xff2ad0..d8` × 10.0 @`0xb05ac8`] + `u32@0x18` + **f32 X/Y/Z @0x1c** + f32 throttle·speed @0x28 + f32 @0x2c, @0x30 + **f32 clock_s @0x34** [static] |
| 0x34 | 52 | **"mini loc" fast position update** (alternates with 0x33, same gating) | same as 0x33 minus the three ×10 shorts: 6×i16 orientation @6, `u32@0x14`, **f32 X/Y/Z @0x18**, f32 @0x24/0x28/0x2c, **f32 clock_s @0x30** [static] |
| 0x17 | 10 | Compact companion report (1:1 with 0x05; sent by `0x9cb330` right after it, on the *state* connection) | `[17]` + 9 bytes that change every send — compact heading/input or state-hash [inferred]. |
| 0x71 | 52 | **Bot (conflux) state report "botloc"** — sent by the bot's controlling client | `[71][state][hull%][shield%][b][b][u16 id]…[u16 loc]` + i16 vel/orientation + `u32 player` + f32 pos. One per bot this client controls, after every 0x05; empty when you control none (so all-zero in the solo capture). See reversing-map "Conflux / bot AI". |
| 0x2C | 12 | "bxr" bot handoff request | sent when I'm closer to a bot than its current controller |
| 0x74 | 16 | **Asteroid amount request/update** | `[74][asteroid id][loc id=0x0ef9]…`. Client *queries* the server for a mineable asteroid's ore amount; strings: "request/update asteroid amount packet". |
| 0x0F | 392 | Building / station interaction | sent on approach to a station. |

### Where the map comes from  [capture + strings + disk]
> **Superseded:** asteroids/gates/beacons are in fact **hardcoded in the exe**
> (~35k objects; see reversing-map.md "Static world objects" and
> `tools/jg_static_dump.py`). Only player-built stations (0xA7) are streamed.
> The bullets below are kept as history.

- **No local universe/placement table on disk** — only art (`.x` meshes, `.bmp`
  textures incl. `asteriod25.bmp`) and config `.bin`. Models/textures are local;
  *placement* is not.
- **Stations/bases are streamed** (`0xA7`) — they are player-built content
  (player-named: "Burgalr's CHEAP!!", "OldGuy's Artifacts"), so placement must be
  server-authoritative. Full re-sync every heartbeat, not deltas.
- **Asteroids are NOT procedurally generated** [confirmed in code, 2026-11-10].
  The binary has no sector-seeded generator and no noise function: the only two
  RNGs are the message obfuscation (`salt*0x89 → srand`, the 0x9cb–0x9cf cluster)
  and UI flavor-text selection (`FUN_0045bb10`, `rand()` over description
  strings). None of the usual procedural constants (Murmur `0xcc9e2d51`, MT
  `0x6c078965`, FNV, golden `0x9e3779b9`, LCG `0x41c64e6d`) appear anywhere.
- With no on-disk universe table and no generator, **asteroid placement must be
  server-sourced**, like stations. Asteroids are enumerated by small integer ids
  (mining-amount accumulators live in a local array at `0xbe5e14`), and per-
  asteroid **amount/depletion** is server-authoritative via `0x74`
  (`request/update asteroid amount`). The large empty S→C tables `0x62`(800B)/
  `0x13`(788B)/`0x78`(151B) are the likely carriers of the per-sector object
  list — they came through empty only because the capture was a station hub.
- Not yet positively captured: an asteroid-placement message (none sent in the
  depot area). Decisive test = a capture flown into an actual asteroid field.
- **Correction:** an earlier note here said the field was generated locally /
  procedurally. That is wrong — no such code exists. Placement is server-sourced
  for stations and asteroids alike; `0x74` depletion is the only extra per-
  asteroid state.

### Location update path (why 0x05 is every ~13 s, and what PvP uses)  [static, 2026-11-10]
One sender, `0x9cd410` (strings "gloc 3", "small loc", "mini loc"), driven by a
timer in `0x9ce1c0..0x9ce2f6`; all three types go on the **space** connection.
- **Tick rate:** QueryPerformanceCounter (`0xbe5dc8`/freq `0xbe5dd8`). One tick
  when >0.3 s have passed *and* the ship **position** (`*0x144094c/50/54`;
  corrected 2026-11-11, see physics.md) moved by more than 1 unit since the last tick,
  otherwise when >0.8 s have passed. Ship/state ids 249/250 (`0xff2a94`) tick every frame.
- **Rotation:** the counter `0xbe5c18` cycles 2..18. Odd values send 0x33 small loc,
  even values send 0x34 mini loc, and 18 sends the 0x05 full report plus the 0x17
  companion, then resets. That makes 17 ticks per 0x05: 17 × 0.8 s ≈ **13.6 s** when
  idle or cruising straight (matches the capture), ~5 s while steering.
- **The fast types are only sent when it matters:** 0x33/0x34 go out only if some
  ship in the contact table (`0x1042118 + i*0x160c`, i = 10..199, active
  and not dead) has a distance (`+0x94`) under **10,000** units, *and*
  the packet differs from the last one sent (`0x9c77f0`/`0x9c7920` compare
  against the copy at `0xba7910`). Flying alone → only 0x05 every ~13.6 s, which
  is why the solo captures never showed them. In a dogfight → ~3 updates/s.
- `0x9ceb70` forces an immediate 0x05 (sets the counter to 18), called from `0x9d5dde`.
- After every 100th 0x05 (`0xbe5c10`) with loc < 1000, also sends `0x75`
  "request asteroid" (4 B, state conn).
- The 0x05/0x33/0x34 share one snapshot block at `0xff2a50..0xff2af0`:
  orientation `0xff2ab8..cc`, **velocity** `0xff2ad0..d8` (copied from the
  velocity cells), position `0xff2aac..b4`,
  clock `0xff2aa0`.

### Particle clouds (S→C 0x87 / 0x86)  [static + capture, 2026-11-17]
Galaxy-scale "weather". The list is the same for every sector because it *is*
universe-wide. After storing it, `0xa1f1d0` walks every location (from 101) and
gives each one the effect of its nearest cloud (`0xa1eff0`), written into the
location struct `0xfd9220 + loc*100`:
- `+0x4c` type, `+0x4d` kind, `+0x4e` b3; kind 3 also sets `0xb4b8a7`.
- `+0x50` **intensity I = strength · r² / d²**, with d the distance from the
  location's galaxy-map XYZ (struct +0x2c) to the cloud.
- `+0x54` = I·k2 and `+0x58..0x60` = drift · I · k1, where (k1, k2) by type/kind:
  type1/kind5 (15, 0), type1/kind6 (0, 0), type1/kind7 (0, 500), type2 (25, 0),
  type3 (20, 500). Meaning of the two outputs not traced yet.

Effects in the client:
- **Tint** (`0x9ddea0`, 13 callers, mostly the renderer `0x9ede30`): for type 1,
  the scene colour is blended with the kind's colour (`0xa1edf0`): 1 red,
  2 green, 3 blue, 4 yellow, 5 purple, 6 teal, 7 orange, other grey.
- **Ambient sound on arrival** (0x08 handler `0x9d4106`): if the destination's
  `+0x58` vector is non-zero, `play_ui_sound(0x25, …)` at a volume from I.
- **Gas harvesting** (`0x439a10`, from the flight code at `0x42a09b`): needs a
  non-zero cloud type in the current location, speed > 380 (`0xaeade8`; the item
  text says "400v") and the **FuelScoop** device (device index 13 = item 165)
  equipped. It accumulates `rand·dt·0.06 · clamp(1 + (speed−380)·0.015, ≤7.5)`,
  × I when I > 1, and adds commodities to the hold (`0xff7d2c`), entirely
  **client-side**. Roll 0–100 per kind (hold index i = cargo id 222+i):
  kind 5 Helium <60, Water <75, else Xenon; kind 6 Helium <50, Water <70,
  Boron <74, Lithium <78, Nitrogen <90, else Magnesium; kind 7 Helium <70,
  Water <85, Phosphorus <92, else Nitrogen; other kinds Helium.
- Renderer also special-cases kind 7 (`0x9ee302`) [effect not traced].

**Drift is server-side.** The client only stores what it receives. Across the
capture's 8 snapshots each cloud moved exactly along its drift vector
(direction · vector = 1.000) at ≈ **0.0507 galaxy units/s per strength point**
(strength 1: 17.4 in 343 s; strength 2: twice that). Radius stayed constant.
The capture's 4th record (strength 0, non-unit vector (5, 0, 2), r 23) never
moved and has zero intensity, so it's dormant.
