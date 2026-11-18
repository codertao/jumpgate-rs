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
| 0x09 | 16 | **Command: market listing, buy, sell, misc** | `[09][cmd][u16 loc][u32 index @4][u32 player_id @8][f32 quantity @12]`. Listing requests, buys and sells: see "Station market" below. Other cmds are slash commands (slash-commands.md). [capture jgcap2, 2026-11-18] |
| 0x08 | 8 | **Change ship** (sends `0x47932a`, `0x47a708`) | `[08][u8 ship_type][2 bytes uninitialised][u32 player_id]`. Seen switching to type 94 (Hurricane) while docked. The server answered with 13 × 0x45 (the old ship's fittings moved to the item list) and a 0x06. [capture jgcap2] |
| 0x4B | 60 | **Contract board** | `[4b][3 junk][u32 player_id @4][u8 job @8][u8 op @9]`, rest zero except u8 @0x18 = the board (7 = trade/mining board, 0x0e = the courier/patrol board; echoed in the reply). op 1–3 = view offer n, 0x65 = accept, 0x96+n = close offer n. Answered by S→C 0x4B. [capture jgcap2] |
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
| 0x0A/0x0B/0x0D, 0x50–0x56 | 896 | **Station market, one category per message** | `[type][3 pad]` + N × `(u32 stock, u32 price)` + N flag bytes at +0x31C. Entry i = item i of that category. See "Station market" below. [static + capture jgcap2, 2026-11-18] |
| 0x45 | 400 | **Credits and item list** (handler `0x9d662a`; previously called "rank") | `[45][7 pad][u64 credits @8 → 0xff7cf8]` then, unless byte @0x10 is `0xfd` (credits-only update), 48 × 8-byte slots @0x10 copied to `0xff7d2c`: `[u8 category][u8 index][cc cc][f32 quantity]`, `fe` = empty. The list holds cargo and unfitted equipment. C→S 0x09 sell names a slot index from it. Sent after every credit change, purchase, fitting, and while mining. [static + capture jgcap2] |
| 0x92 | 8 | **Experience total** (handler `0x9d8120`) | `[92][3 pad][u32 experience @4 → 0xbe5da8]`. Rises by exactly the experience in the reward chat lines. [static + capture jgcap2] |
| 0x77 | 2 | **Medal awarded** (handler `0x9d7ccb`) | `[77][u8 medal id]`. Followed by a system chat "You receive N experience for the medal." The medal names are what the tri server's 0xFE patches into client memory. [static + capture jgcap2] |
| 0x70 | 6 | **Bot spawn order** (handler `0x9cbac0`) | `[70][u8 ai_type][u16 spawn#][cc cc]`. ai_type = ai_types.csv row (12–14 = conflux types 236–238 in The Dark Trail / Inner Roh-Cloud). Queued in a 40-entry table at `0xba7ad0`. If the table is full, the client sends it back as **C→S 0x16 RejectSpawn** (same bytes, type changed). Sent on sector entry, right after 0x73. The client then flies the bot (C→S 0x71 botloc) and reports its death with C→S 0x44. No S→C 0x71 was seen in this solo session. [static + capture jgcap2] |
| 0x4B | 60 | **Contract** (handler `0x9d66ee`) | Bytes 8..0x3c are copied to `0xff1e58`. `[u8 job type @8]` (0 = no contract; seen 2 trade, 9 mining, 0x0b/0x0f/0x10/0x13 courier, 0x0d patrol) `[u8 state @9]` (0x33–0x35 = viewing offer 1–3, 1 = active), **10 waypoint-visited flags @0x0a–0x13** (00 = to visit, 01 = unused slot), **u8 waypoint count @0x14**, `[u8 category @0x15][u8 item index @0x16]` (goods, 0xcc = none), u8 @0x17 (always 0xfe), u8 board @0x18, **u16 waypoint locs @0x1a + 2i** (up to 10; the first is the destination; a patrol lists 3–5 sectors). For job 3, entering waypoint i's sector sets its flag and sends C→S 0x64(8) (`0x40ff7f`). **`u32 experience @0x30`** (UI `mission_experienceearned`), **`u32 credits @0x34`** (UI `mission_creditsearn`), **`f32 remaining @0x38`**. When goods of the matching category/index are sold at the destination, the client subtracts the quantity from `remaining` (`0x4569cf`). The server's completion message carried 1 − sold (−15 after selling 16). [static + capture jgcap2] |
| 0x14 | 16 | **Conflux swarm moved** (handler `0x9c9660`) | `[14][3 pad][u32 swarm 0–2][u32 loc now][u32 loc before]` (0 = no move, e.g. the 3 sent on entering space). Stored as `0xb909e0[swarm] = loc` (read by the galaxy map, `0x4ae98e`). On a move into a sector of the player's current faction, the client prints "ALERT: A swarm of conflux has entered %s from %s." The capture's swarms wandered C-1100, The Chase, Outskirts, Saron's Shoulder (faction 6 space). [static + capture jgcap2] |
| 0x08 | 64 | **Location change / space-server redirect** (handler `0x9d30c6`) | `[08][sub]` … space-server address @0x0c (UTF-16LE, ≤16 units incl. NUL, field ends at 0x2c) … **u16 dest loc @0x2c** (checked 1..1005) … @0x34. If the address differs from the current one (`0xba79a0`), the client drops space and connects to `<addr>:5151` (`0x9c7190`). Dock sends **"none" as narrow ASCII bytes**: the compare is `wcscmp` (0xaabf39) against a narrow literal at `0xb05afc`, which only works because both are the same raw bytes. **sub @1 = contact slot to appear at**: the arrival gate number (= extract `gate_dest_gate`) for a jump, 6 for a launch, 0xcc on dock (the handler uses `slot = msg[1]`, position = slot pos + random offset). Verified against all captured jumps. [static + capture, 2026-11-17] |
| 0x04 | 20 / 69 | Chat line (e.g. "KTRI" news bot) | `[04][sub][u16 loc][u32 player_id][callsign\0]...` |
| 0x65 | 35 | Channel-join ack | `[65][...][u32 handle]` |
| 0xC2 | 750 | Table of 41 × 18-byte entries, mostly empty | `[C2]` + entries padded with `cc`/`ff ff ff` |
| 0x78 | 150 | Empty object list at launch (sim) [inferred] | all zero |
| 0x18 | 8 | Small status (medal?) | `[18][u32][ff 00 ff 01]` |
| 0x8C | 7 | Small status | `[8C][cc cc cc][u16?]` |
| 0x05 | 176 | **Other ship create/update** (handler `0x9cf7c8`): same layout as the C→S location report. Matched by player id @0x5c against slot `+0x15b4`; a new id takes the first free ship slot 10–199. Ship type u16 @0x46 (0xf9/0xfa = remove [inferred]), loc @0x44. This is what the fan server relays. | [static, 2026-11-17] |

#### S→C 0x05 / 0x33 field map (contact slot `0x1042118 + i*0x160c`)  [static, 2026-11-17]
From pairing each handler read with the slot field it's stored in (`0x9cf7c8`
new-ship path `0x9cff20`; `0x9d573c` for 0x33). This is what the server uses to
build NPC ships (`server/src/proto.rs` `ship_report` / `ship_small_loc`).
The message is first copied to `0xba8e20`.

| 0x05 off | 0x33 off | type | slot | meaning |
|---|---|---|---|---|
| +1 | (set from thrust) | u8 | +0x156d | engine glow: 0x33 sets 0, or 50 when thrust ≠ 0 |
| +3 | +1 | u8 | +0x156f | ? (0 in capture) |
| +4..+0x13 | | 16 B | +0x1568.. | equipment array (`fe` empty) |
| +0x14..+0x17 | | u8 ×4 | +0x1580.. | ? (capture `1e fe 01 01`) |
| +0x19 | | cstr | +0x1584 | **callsign**; the message is **ignored if strlen ≤ 1** |
| +0x2f | | cstr | +0x159a | squad; `"$master$"` → stored empty |
| +0x3e..+0x42 | | u8 ×5 | +0x15a9.. | ? (capture `04 00 7d 05 0d`) |
| +0x44 | +4 | u16 | +0x15ae | location; **must equal the client's own** (`0xbe5b3e`) or the message is dropped |
| +0x46 | | u16 | +0x15b0 | ship type; 0xf9/0xfa remove, 0xfb special |
| +0x48 | +6 | i16 ×3 ×1e-4 | +0x15d4 | forward unit vector |
| +0x4e | +0xc | i16 ×3 ×1e-4 | +0x15e0 | up unit vector |
| +0x54 | +0x12 | i16 ×3 ×0.1 | +0x15ec | velocity |
| +0x5c | +0x18 | u32 | +0x15b4 | player id (slot key) |
| +0x60 | | u32 | +0x15b8 | ? (0) |
| +0x64 | +0x34 | f32 | +0x15bc | clock (s); must be **strictly newer** than the slot's or the message is dropped |
| +0x68 | | u32 | +0x15c0 | mass (int; capture 110650 for a loaded Thunder) |
| +0x6c | | u32 | +0x15c4 | ? (0) |
| +0x90 | +0x1c | f32 ×3 | +0x15c8 | position |
| +0x9c | +0x2c | f32 | +0x15f8 | armour-ish (capture 24e6 = Thunder armour 24000 ×1000) [inferred] |
| +0xa0 | | f32 | +0x15fc | same, max [inferred] |
| +0xa4 | | f32 | +0x1600 | ? (capture 72e6 = 3× the above) |
| +0xa8 | +0x28 | f32 | +0x1608 | **effective thrust** (thrust × throttle) used by dead reckoning; capture 7.2e6 with drag 44 → √(7.2e6/44) = 404 u/s = measured |v| |
| +0xac | | f32 | | ? (100.0) |

Server-driven ships use the same two messages: the client can't tell an NPC
from a remote player. **Runtime-confirmed 2026-11-17:** a server-built 0x05
(fields above, unknowns copied from a real report) + 5 Hz 0x33 shows a Chinook
with smooth motion. Not yet confirmed: that 0xfa removes it.
| 0x33 | var | smallloc relay: another ship's 0x33, same layout as C→S | handler `0x9d573c`. Matched by player id @0x18; dropped unless clock @0x34 is newer. Position @0x1c, velocity i16×0.1 @0x12. Error < 200 u is blended into velocity, otherwise snapped (physics.md §8) [static 2026-11-11] |
| 0x34 | var | miniloc relay: another ship's 0x34, same layout as C→S | handler `0x9d5a9d`. id @0x14, position @0x18, clock @0x30. No velocity; same blend/snap rule [static 2026-11-11] |
| 0x61 | 4 | **Beacon update** (handler `0x9d67d5`) | `[61][state][u16 loc]`. Valid only for loc 201..=999 and only when contact slot 5 is active with model 2 (the sector beacon); otherwise prints "Received invalid beacon packet". `state` → location table +0x3c. In jgcap2, state was 0 on entering an untuned sector, and 3 right after the player tuned the beacon (C→S 0x64), so it's probably the owning faction [inferred]. Station sectors 101–118 have no beacon. [static + live, 2026-11-17; capture jgcap2] |
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
| 0x0F | 392 | **Cargo report** (sender `0x439f16`; named "building" in the error strings) | `[0f][u8 flag][2 bytes uninitialised][48 × 8-byte item-list slots @4, copied from 0xff7d2c][u32 player_id @0x184]`. Sent every ~10 s while the mined-amount accumulator `0xbe5e10` is non-zero (it's reset after each send), and once just before docking. flag was 1 during mining and 0 in the final pre-dock report. **The client reports its own mined ore**: the server accepted these amounts (0x45 echoed them), so ore is client-authoritative unless our server checks it. [static + capture jgcap2] |
| 0x44 | 16 | **Bot destroyed** ("who killed me", sent by the bot's controlling client) | `[44][u8][u16 bot spawn#][u32 killer pid][u32 reporter pid][4 zero]`. The server answered with "Received 3000 credits and 320 experience" (3300/350 for the second), "<callsign> destroyed a member of the Conflux", 0x92, 0x45. [capture jgcap2] |
| 0x32 | 40 | **Missile** create/remove | `[32][u8 missile id][u16 type @2]` (missile index, e.g. 7 = Purgatory; **0xfa = removed**, the same value as a 0x05 ship removal) … `u32 owner pid @0x14`, `@0x18`, `f32×3 position @0x1c`. One create and one remove per missile fired. [capture jgcap2] |
| 0x64 | 8 | **Beacon / contract event** (sender `0x9cad80`) | `[64][u8 arg][2 bytes uninitialised stack][u32 player_id]`. Three callers: beacon tuned (`0x41006f`; arg = the faction the beacon now belongs to, set in the location table `0xfd925c + loc*0x64`; the server confirms with 0x61 state = arg), job-3 waypoint reached (arg 8), and a job-0x0d event (arg 9). [static + capture jgcap2] |
| 0x43 | 12 | **Combat stats**, every 30 s in space if any are non-zero (sender `0x9caa30`) | `[43][u8 missiles fired @1][u8 missile hits @2][pad][u16 gun shots @4][u16 gun hits on ships @6][u32 player_id]`. Counters: `0xff266d` (launch, `0x436e38`), `0xff266e` (missile impact on a ship/bot slot, `0x42d4db`), `0xff2670` (gun fired, `0x4375a5`), `0xff2672` (gun hit on hull types 30–119 or bots 219–259, `0x43f60a`). Reset after sending. [static + capture jgcap2] |
| 0x66 | 12 | **Fitting change** (38 send sites, `0x4781bf`..`0x47b865` + missile fire `0x43753d`) | `[66][u8 item-list slot @1][u8 category @2][u8 ship slot @3][u8 op @4][3 bytes stack junk][u32 player_id]`. op 0 = unfit ship slot → item-list slot, 1 = fit item-list slot → ship slot, 2 = missile fired (rack slot emptied). The server answers fits/unfits with an updated 0x45. Changing ship sent 13 unfits (engine, powerplant, radar, shield, capacitor, 4 guns into one stacked slot, 4 devices), then each item was fitted back. [static + capture jgcap2] |

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

### Station market (C→S 0x09 ↔ listings)  [static + capture jgcap2, 2026-11-18]

Each category has a listing request, a reply type, an item count, and a buy sub-op:

| category | listing cmd | reply | entries | client array | buy cmd |
|---|---|---|---|---|---|
| 0 guns | 0x00 | 0x0A | 60 | `0xff5e80` | 0x04 |
| 1 missiles | 0x01 | 0x0B | 24 | `0xff6060` | 0x05 |
| 3 commodities | 0x03 | 0x0D | 99 (cargo ids 222–320) | `0xff6750` | 0x07 |
| 5 engines | 0x5A | 0x50 | 27 | `0xff6120` | 0x0A* |
| 6 powerplants | 0x5B | 0x51 | 27 | `0xff61f8` | 0x0B* |
| 7 radars | 0x5C | 0x52 | 20 | `0xff6398` | 0x0C* |
| 8 ECMs | 0x5D | 0x53 | 8 | `0xff6438` | 0x0D* |
| 9 shields | 0x5E | 0x54 | 25 | `0xff62d0` | 0x0E* |
| 10 capacitors | 0x5F | 0x55 | 21 | `0xff6478` | 0x0F* |
| 11 devices | 0x60 | 0x56 | 70 | `0xff6520` | 0x10 |

\* inferred from the static sub-op list (04 05 07 08 0a–10); the others were seen.

- **Listing entry i** = item i of the category, in extract order. Commodities are
  cargo id 222 + i, so 0 = Water … 37 = Electronics … 98 = Trap. Each entry is
  `(u32 stock, u32 price)`. The market popup (`0x44c7b0`) prints the low 16 bits of
  stock under `market_popup_quantity`. Price is read as unsigned, converted to
  float and multiplied by `0xbe6dc0`. That factor is set from the 0x06 character
  record (`0x9d23a4`), defaults to 1.0, and is 1.01 in one place (`0xa25481`): a
  standing or faction markup [inferred]. A buy multiplies by the quantity
  (`0xbe5e00`). The factor is **1 + v/10000**, where v is the **i16 @0x52 of
  the 0x06 character record** (29–30 in jgcap2, so about +0.3%). It's
  confirmed: buying 16 Electronics at 29,955 cost exactly 480,717 =
  16 × 29,955 × 1.0030.
- **Flag byte per item** (+0x31C, copied to `0xbe5a55`; the commodity handler
  zeroes them): the market popup adds **0.02 to the price factor when the flag is
  0** and nothing when it's set (`0x448684`, also `0x447e71`/`0x44c897`). So
  flag = "no 2% surcharge". Confirmed by equipment buys at Quantar Core:
  DeepRadar (flag 0) cost 87,975 × 1.0229, and 2 × Financier (flag 1) cost
  218,000 × 1.0029. Commodities never get the 2% (their flags are zeroed, yet
  the Electronics buy was × 1.0030). Flagged items at Quantar Core included
  BCU-QUAN, Witness, Oracle, station building kits and some guns (Thorn,
  Financier, …), probably locally made goods [inferred].
- **Buy:** `09 <buy cmd> <loc> <u32 item index> <pid> <f32 quantity>`. Seen:
  16 × Electronics (Corridor stock 59 → 43 in the next listing), 2 × Financier,
  6 × Purgatory, 1 × DeepRadar.
- **Sell:** cmd 0x08 with the u32 = **slot in the 0x45 item list** and an f32
  quantity. Ore sells in fractional tons (41.29, 0.442, ...).
- After a buy or sell, the server sends 0x45 (credits and items), sometimes 0x06,
  and the client re-requests the open category's listing.
- Stock and price drift slowly between listings (prices by a few credits). Mined
  ores are commodity indices 175–179 (cargo 397 Common Metals … 401 Precious
  Metals). They aren't in the 99-entry market. **Ore prices are computed by
  the client** from the commodity listing (`0x449690`, called by the 0x0D
  handler right after it copies the 99 entries) as a fixed mineral
  composition, truncated to an integer, and stored into commodity entries
  175–179 (`0xff6ccc`…`0xff6cec`):

  | ore | price = Σ weight × that mineral's listed price |
  |---|---|
  | Common Metals | .25 Iron + .16 Aluminum + .12 Silicon + .10 Copper + .09 Titanium + .08 Zinc + .06 Molybdenum + .06 Gallium + .05 Chromium + .03 Barium |
  | Radioactive Metals | .28 Cobalt + .20 Iron + .16 Uranium + .15 Helium + .12 Silicon + .06 Radium + .03 Cesium |
  | Ice | .50 Water + .18 Nitrogen + .09 Carbon + .08 Lithium + .07 Boron + .05 Xenon + .03 Magnesium |
  | Semifluxors | .26 Carbon + .26 Silicon + .15 Iron + .13 Germanium + .09 Phosphorus + .05 Indium + .04 Erbium + .03 Antimony (sums to 1.01) |
  | Precious Metals | .20 Copper + .17 Silver + .15 Iron + .12 Silicon + .10 Iridium + .09 Gold + .08 Vanadium + .05 Palladium + .04 Platinum |

  Checked against the jgcap2 sales at Quantar Core: Semifluxors, Ice and
  Precious Metals were paid exactly formula × quantity (8,284 / 6,629 /
  12,922 per t, no markup). Common Metals (which also completed the mining
  contract, +76,475) and Radioactive Metals came out ~6% and ~8% above the
  formula. That's unexplained, perhaps a server-side bonus. The same handler
  sets the prices of entries 150–159 (X-artifacts, Recon Relay, …) to 0.
  A server must use the same formula when buying ore, or the
  client will show one price and pay another.

### Rewards seen (server-side rules, jgcap2)

- Conflux kill: "Received 3000 credits and 320 experience." (the second kill: 3300/350).
- Beacon income, periodic per tuned beacon: "You gain c1666, and 46 exp for
  holding sector Hook of Roh beacon" (amounts vary per sector).
- Medals (0x77) with a fixed experience bonus each: 250, 500 or 2500.
- Contracts: experience and credits are in the 0x4B offer. Delivery is done
  by selling the goods at the destination station.

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
