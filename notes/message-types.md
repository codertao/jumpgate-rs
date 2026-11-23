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
| 0x07 | 12 | **Launch / Dock / Jump** | `[07][action][u16 from_loc][u32 to_loc][u32 player_id]`; action 1=launch, 2=dock, 3=jump. **Death is a dock with to_loc 0xfe** (`player_death` `0x9cf168`) |
| 0x09 | 16 | **Command: market listing, buy, sell, misc** | `[09][cmd][u16 loc][u32 index @4][u32 player_id @8][f32 quantity @12]`. Listing requests, buys and sells: see "Station market" below. Other cmds are slash commands (slash-commands.md). [capture jgcap2, 2026-11-18] |
| 0x08 | 8 | **Change ship** (sends `0x47932a`, `0x47a708`) | `[08][u8 ship_type][2 bytes uninitialised][u32 player_id]`. Seen switching to type 94 (Hurricane) while docked. The server answered with 13 × 0x45 (the old ship's fittings moved to the item list) and a 0x06. [capture jgcap2] |
| 0x4B | 60 | **Contract board** | `[4b][3 junk][u32 player_id @4][u8 job @8][u8 op @9]`, rest zero except u8 @0x18 = the board (7 = trade/mining board, 0x0e = the courier/patrol board; echoed in the reply). op 1–3 = view offer n, 0x65 = accept, 0x96+n = close offer n. Answered by S→C 0x4B. [capture jgcap2] |
| 0x0E | 16 | **Consumables used** | `[0E][3 junk][f32 fuel_used @4][f32 hull_used @8][f32 magazines_used @0xc]`. Sent once per dock, after the S→C 0x08 to a station; no reply was seen. See "Consumables and servicing". [static + 4 captures] |
| 0x04 | 27 | Chat line | `[04][05][u16 loc][u32 player_id][callsign\0][text...]` |
| 0x65 | 36 | Join chat channel ("chat") | `[65][u16 loc?][u32 player_id]["chat"\0]...` |
| 0x6A | 8 | Player-list / player request | `[6A][...][u32 player_id]` |
| 0x71 | 52 | Bot state report "botloc" relayed (handler `0x9d6f25`) | Same layout as C→S 0x71: `@8` bot id, `@0xa` loc, `@0xc` type, `@0x20` u32 controller, `@0x24` u32 target, `@0x28` f32×3 position. An unknown id **creates** the bot at that position. **The receiver makes the bot's target its controller** (`0x9d7191`): if @0x24 is me, I fly the bot from now on (slot `+2` = 0, pbotloc `+0xc` = me) and answer with a botloc at once (`0x9c9d10`); otherwise the bot is tied to the target's ship slot and pbotloc `+0xc` is set to the target's id, and if that player isn't in my sector the bot is removed. So a hand-off is a 0x71 naming the new pilot in both fields. See ai.md "Spawning" and "Hand-off". [static 2026-11-19/20] |
| 0x8F | 1 | **Keepalive** | `[8F]` — one per connection every ~16 s |
| 0xC1 | 12 | Job-list request | `[C1][sub][u16 loc][u32 player_id][u32]` |
| 0xC8 | 8 | Logout / session end | `[C8][u16 loc?][u32 player_id]` |

Other C→S types mapped **[static]** from send sites (names from the client's
"Error Sending ..." strings; see `captures/send_types.txt`): 0x08 ship, 0x0C
impact, 0x0F building(392B), 0x10 prodboost, 0x16 reject-spawn, 0x36 wingman,
0x37–0x41 ("packet 55"–"65" in the strings = decimal), 0x43 hits-counter,
0x44 who-killed-me, 0x4A trade-ack(sub@1), 0x4C mission, 0x4D modx, 0x4E mentor,
0x4F artifact pickup (decoded below), 0x59 home, 0x74 asteroid-amount, 0x7B registry, 0x83 list-channels,
0x8A weather-request(1B), 0x90 findPOSowner, 0x91 missions, 0x93 mode, 0x96
volunteer, 0x9C event/query (sub = u16 @ off 8: 0xEC/0xED...), 0x9D vouch,
0xAA/0xAB invite, 0xAC shipname, 0xBB home-off, 0xC3 "util" (sub@1: 0xF2 miltour,
0xF3 util-243, 0xF4 insurance, 0xF5 squad mayday, 0xF6 faction mayday; the maydays are 2 bytes and key-bound, see physics.md §9 item 7), 0xC5 commission/away.

---

## Server → client (decoded from capture)

| Type | Len | Name / meaning | Layout |
|---|---|---|---|
| 0x06 | 544 | Character / ship record (handler `0x9d205f`) | Full field map in **"0x06 character record"** below. `[06][faction][rank]` + callsign`\0` @3 … **space-server address @0x22** (UTF-16, same rule as 0x08, acted on only if **mode @0x45 == 1**) … ship type @0x42 … **u16 loc @0x50**, **u32 player id @0x54** (→ `0xbe5da0`) … exp @0x5c, credits @0x68, fittings @0x70, "My Ship" @0x8f, 48 item slots @0x9c. [static + capture, 2026-11-17/19] |
| 0x81 | ~850 | Message of the day | `[81]` + ASCII ("Welcome back! ...") |
| 0xFE | 321 | **Memory patch** | `[FE]` then records `[01][u32 addr][u16 len][bytes]`. In capture: only award/medal name strings. See network-protocol.md §3. |
| 0x0A/0x0B/0x0D, 0x50–0x56 | 896 | **Station market, one category per message** | `[type][3 pad]` + N × `(u32 stock, u32 price)` + N flag bytes at +0x31C. Entry i = item i of that category. See "Station market" below. [static + capture jgcap2, 2026-11-18] |
| 0x45 | 400 | **Credits and item list** (handler `0x9d662a`; previously called "rank") | `[45][7 pad][u64 credits @8 → 0xff7cf8]` then, unless byte @0x10 is `0xfd` (credits-only update), 48 × 8-byte slots @0x10 copied to `0xff7d2c`: `[u8 category][u8 index][cc cc][f32 quantity]`, `fe` = empty. The list holds cargo and unfitted equipment. C→S 0x09 sell names a slot index from it. **The list has no gaps** [capture jgcap2, 2026-11-21]: when a sale or a fit uses up an entry, the classic server removes it and the later entries move up one (the 0x06 after each of five sells shows this, and the client's next sell names the shifted slot). Unfits go to consecutive slots, stacking with a matching entry. Sent after every credit change, purchase, fitting, and while mining. [static + capture jgcap2] |
| 0x57 | ≥8 | **Set rank** (handler `0x9d6789`) | `[57][3 ?][u32 rank @4]`. Accepted only for 1..50: written to the rank global `0xbe5c7c` (what the market checks), `0xff2a64` (sent as the 0x05 report's +0x14) and `0xba7978`. Otherwise it falls through to the next case [not traced]. The way to change rank mid-session; the 0x06 @2 sets it at login. See items.md "Rank". [static, 2026-11-21] |
| 0x92 | 8 | **Experience total** (handler `0x9d8120`) | `[92][3 pad][u32 experience @4 → 0xbe5da8]`. Rises by exactly the experience in the reward chat lines. [static + capture jgcap2] |
| 0x77 | 2 | **Medal awarded** (handler `0x9d7ccb`) | `[77][u8 medal id]`. Followed by a system chat "You receive N experience for the medal." The medal names are what the tri server's 0xFE patches into client memory. [static + capture jgcap2] |
| 0x70 | 6 | **Bot spawn order** (handler `0x9cbac0`) | `[70][u8 ai_type][u16 spawn#][cc cc]`. **ai_type = bot ship type − 219** (spawn_bot reads `0xbeb63c + t·64` = ai_types.csv row 219 + t, whose +0x38 holds that ship type; capture jgcap2: ai_type 0x0d/0x0e came back in botloc @0xc as ships 232/233, C2/C3 'Squid'). So 1–6 = defenders 220–225, 11 = Jellyfish, 12–38 = C1–C27, 39/40 = turrets. spawn# = the bot id in botloc @8 and C→S 0x44 (captured: 0, 1 per sector). Queued in a 40-entry table at `0xba7ad0`. If the table is full, the client sends it back as **C→S 0x16 RejectSpawn** (same bytes, type changed). Sent on sector entry, right after 0x73. **No position or target**: the client picks the spawn point (station launch point, structure hard-point = spawn# /100 and %100, nav marker, repulsor, or random ±16 km around the player), and the target is the receiving player (ai.md "Spawning"). The client then flies the bot (C→S 0x71 botloc) and reports its death with C→S 0x44. No S→C 0x71 was seen in this solo session. [static + capture jgcap2] |
| 0x4B | 60 | **Contract** (handler `0x9d66ee`) | Bytes 8..0x3c are copied to `0xff1e58`. `[u8 job type @8]` (0 = no contract; seen 2 trade, 9 mining, 0x0b/0x0f/0x10/0x13 courier, 0x0d patrol) `[u8 state @9]` (0x33–0x35 = viewing offer 1–3, 1 = active), **10 waypoint-visited flags @0x0a–0x13** (00 = to visit, 01 = unused slot), **u8 waypoint count @0x14**, `[u8 category @0x15][u8 item index @0x16]` (goods, 0xcc = none), u8 @0x17 (always 0xfe), u8 board @0x18, **u16 waypoint locs @0x1a + 2i** (up to 10; the first is the destination; a patrol lists 3–5 sectors). For job 3, entering waypoint i's sector sets its flag and sends C→S 0x64(8) (`0x40ff7f`). **`u32 experience @0x30`** (UI `mission_experienceearned`), **`u32 credits @0x34`** (UI `mission_creditsearn`), **`f32 remaining @0x38`**. When goods of the matching category/index are sold at the destination, the client subtracts the quantity from `remaining` (`0x4569cf`). The server's completion message carried 1 − sold (−15 after selling 16). [static + capture jgcap2] |
| 0x14 | 16 | **Conflux swarm moved** (handler `0x9c9660`) | `[14][3 pad][u32 swarm 0–2][u32 loc now][u32 loc before]` (0 = no move, e.g. the 3 sent on entering space). Stored as `0xb909e0[swarm] = loc` (read by the galaxy map, `0x4ae98e`). On a move into a sector of the player's current faction, the client prints "ALERT: A swarm of conflux has entered %s from %s." The capture's swarms wandered C-1100, The Chase, Outskirts, Saron's Shoulder (faction 6 space). [static + capture jgcap2] |
| 0x08 | 64 | **Location change / space-server redirect** (handler `0x9d30c6`) | `[08][sub]` … space-server address @0x0c (UTF-16LE, ≤16 units incl. NUL, field ends at 0x2c) … **u16 dest loc @0x2c** (checked 1..1005) … @0x34. If the address differs from the current one (`0xba79a0`), the client drops space and connects to `<addr>:5151` (`0x9c7190`). Dock sends **"none" as narrow ASCII bytes**: the compare is `wcscmp` (0xaabf39) against a narrow literal at `0xb05afc`, which only works because both are the same raw bytes. **sub @1 = contact slot to appear at**: the arrival gate number (= extract `gate_dest_gate`) for a jump, 6 for a launch, 0xcc on dock (the handler uses `slot = msg[1]`, position = slot pos + random offset). Verified against all captured jumps. [static + capture, 2026-11-17] **@2 = launch tube 0–3** when sub is 6 (`0x9d34a3`, stored to `0xbe5af3`): the client spawns the ship at rest in that tube of the station model (launch.md). Captures: 2 at 118 and 109, 0 at 107. [static + capture, 2026-11-20] |
| 0x04 | 20 / 69 | Chat line (e.g. "KTRI" news bot) | `[04][sub][u16 loc][u32 player_id][callsign\0]...` |
| 0x65 | 35 | Channel-join ack | `[65][...][u32 handle]` |
| 0xC2 | 750 | Table of 41 × 18-byte entries, mostly empty | `[C2]` + entries padded with `cc`/`ff ff ff` |
| 0x58 | 36 | **Mineral vein + artifact** (handler `0x9d883f`; space sectors only, loc ≥ 201) | `[58][u8 commodity][u8 asteroid type][cc][i32×3 vein pos][u8 burst][u8 artifact look][cc cc][i32×3 artifact pos][u8 burst-old-artifacts][cc cc cc]`, positions `^ 0xc0b55` + sector origin. Every 0x58 moves the vein and replaces all artifacts. See "Mineral vein and artifact" below. [static + capture] |
| 0x78 | 151 | **Building sites** (see "Building sites" below) | `[78]` + 50 × 3-byte entries; all zero in empty sectors |
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
| +0x14..+0x17 | | u8 ×4 | +0x1580.. | capture `1e fe 01 01`. **+0x14 = pilot rank** (sender copies 0x06 @2 via `0x9d22cc`/`0xff2a64`; other clients show it as the level). **+0x15 = fitted ECM index while the ECM is switched on, else 0xfe** (`0x9cd686`). +0x16 ?. **+0x17 → slot +0x1583 = towing a cargo pod (`cargo.x`, model 119)**, which adds the pod to rendering and collision. The sender sets it to 1 exactly for ship types 32 Pioneer, 62 Condor and 92 Thunder, the "Cargo Tow" hulls (`0x9cd6cd`) [static 2026-11-20] |
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
| +0xa4 | | f32 | +0x1600 | **hull** (`0xff2ae4`, the copy of the hull cell); the server echoes it in S→C 0xB3 @0xc. jgcap2: 14 490 000 in 0x05 and in 0xB3 [static + capture 2026-11-18] |
| +0xa8 | +0x28 | f32 | +0x1608 | **effective thrust** (thrust × throttle) used by dead reckoning; capture 7.2e6 with drag 44 → √(7.2e6/44) = 404 u/s = measured |v| |
| +0xac | | f32 | | **fuel**, rounded to a whole number (`send_location_tick` `0x9cdfb4`): 100.0 → 99.0 → … The server echoes it in S→C 0xB3 @0x18 [static + capture 2026-11-18] |

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
| 0x73 | 251 | S→C | **Asteroid ore levels** for the sector (reply to 0x75, and on sector entry) | `[73]` + 1000 × 2-bit level, one per world object; see "Asteroid ore levels" [static + capture] |
| 0x78 | 151 | S→C | **Building sites** of the destination sector, sent before every 0x08 (launch, jump) | 50 × `[b0][cat][item]`; see "Building sites" [static + capture] |
| 0xB3 | 44 | S→C | **Own-ship state** (reply to every C→S 0x17, about every 13 s; handler `0x9d94ea`) | Fully decoded, every byte: `[B3][3 pad]`; **i32 ammo** for gun slots 0–7 @4, @8, @0x10, @0x14, @0x1c, @0x20, @0x24, @0x28 (into cells `0x140ac2c + 0xc·slot`); **f32 hull @0xc**; **f32 fuel @0x18**. All three are server-authoritative and overwrite the client's values: fuel and ammo always, hull unless the client's hull is exactly 1000.0 (`0xaeadbc`, purpose unknown). Live captures: fuel 100 on the first 0xB3 after each launch, then the client's own 0x05 fuel (@0xac); hull = full (ship-type armour × 3000: Thunder 72e6, Typhoon 14.49e6, Hurricane 14.1e6); ammo 0 (energy guns only). This is how a launch refuels and repairs. See "Consumables and servicing". [static + capture 2026-11-18] |
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
| 0x71 | 52 | **Bot (conflux) state report "botloc"** — sent by the bot's controlling client (`0x9c9d10`, space link) | `[71][state @1][shield % @2][hull % @3][pbotloc +2 @4][+3 @5][u16 +6 target slot @6][u16 id @8][u16 loc @0xa][u16 type @0xc]` + i16×3 velocity ×10 @0xe + i16×3 fwd ×10⁴ @0x14 (slot +0x74) + i16×3 up ×10⁴ @0x1a (slot +0x68) + `u32 controller = me @0x20` + `u32 target @0x24` (pbotloc +0x10) + f32×3 pos @0x28. Percentages are value / max × 100 (`0xac2aa0`; shields = slot +0x15f8/+0x15fc, hull = +0x1600/+0x1604; the receiver multiplies by 0.01 × its max). One per bot this client controls, after every 0x05, and one at once when a bot becomes ours; empty when you control none (so all-zero in the solo capture). (An older version of this row had shields and hull swapped.) [static 2026-11-20] |
| 0x2C | 12 | **"bxr" bot hand-off request** (online_bot_ai `0x436651`, space link) | `[2C][? @1, never written][u16 bot id @2][u16 0 @4][u16 loc @6][u32 me @8]`. Sent for a bot someone else controls when my score is inside the type's hand-off range and the controller's score is worse; the score is distance − (v·(pos − bot)/distance)·T, T = the lifetime of the bot's first gun's shot (ai.md "Hand-off"). What the classic server answered isn't recorded; the client's 0x71 handler makes a botloc naming the new pilot as target hand the bot over. [static 2026-11-20] |
| 0x16 | 6 | **Bot gone** (two senders, one layout `[16][u8][u16 id][u16 ?]`, state link) | **Spawn bounce** (`0x9cb930`, queue or table full, or a sector change flushing the queue): `[16][ai_type][u16 spawn#]`. **Bot removed by its controller** (`remove_bot` `0x9cb9c0` with report = 1, only when I'm the controller, online and not in loc 1005): `[16][slot +1 "dead" byte][u16 bot id]`; bytes 4–5 are stack garbage. Called when a bot despawns (target gone, defender docked) [static 2026-11-20] |
| 0x0C | 20 | **Impact death** (`0x40ea4c`, state link) | `[0C][FE][u16 0xff2a92 @2][u32 me @4]` + three dwords from the collision code's locals (not identified). Sent when a collision kills me, only if online (`0xbe5b90`), and followed by `player_death(0)` (0x44, 0x07). @2 is likely the location [inferred] (physics.md §6) [static 2026-11-20] |
| 0x74 | 16 | **Mining report** (sender in `0x43b1b0` @`0x43d525`, every ≥10 s while mining) | `[74][junk][u16 asteroid id @2][u16 loc @4][2 junk][f32 ore mined since last report @8][u32 player_id @0x0c]`. Bytes 1 and 6–7 are uninitialised stack. The amount is the accumulator `0xbe5e10`, reset after each send. See "Asteroid ore levels" [static + capture jgcap2] |
| 0x0F | 392 | **Cargo report** (sender `0x439f16`; named "building" in the error strings) | `[0f][u8 flag][2 bytes uninitialised][48 × 8-byte item-list slots @4, copied from 0xff7d2c][u32 player_id @0x184]`. Sent every ~10 s while the mined-amount accumulator `0xbe5e10` is non-zero (it's reset after each send), and once just before docking. flag was 1 during mining and 0 in the final pre-dock report. **The client reports its own mined ore**: the classic server accepted these amounts (0x45 echoed them), so ore is client-authoritative unless the server checks it. [static + capture jgcap2] |
| 0x44 | 16 | **"Who killed me"**: a bot's death (sent by its controlling client) or **the sender's own death** (state link, sender `0x9cab20`) | `[44][u8][u16 bot id @2][u32 reporter pid @4][i32 killer @8][i32 extra @0xc]`. Own death: bot id 0xfe, killer = attacker of the last S→C 0x03 (`0xbe5ca0`), extra −2. Bot death (`0x9ce69e`): the bot's id, the killer passed to `0x9ce490`, extra 0. **The client sign-extends the killer from 16 bits** (`movsx`), so pids above 0x7fff don't survive. Earlier versions of this row had killer and reporter swapped; the capture couldn't tell (both were the same pilot). Bot death answered with "Received 3000 credits and 320 experience" (3300/350 for the second), "<callsign> destroyed a member of the Conflux", 0x92, 0x45. [static 2026-11-20 + capture jgcap2] |
| 0x1D | 32 | **Hit report** (one per hit-queue entry; see "Combat: hits, damage, death" below) | `[1D][3 pad][u32 shooter @4][u16 loc @8][2 pad][u32 victim pid @0xc][u16 bot id @0x10][u16 count @0x12][u8 kind @0x14][u8 weapon @0x15][2 pad][f32 value @0x18][u8 class @0x1c]` [static 2026-11-20] |
| 0x32 | 40 | **Missile** create/remove (same layout both ways; S→C handler `0x9d4686`) | `[32][u8 missile id @1][u8 type @2 (missile index, e.g. 7 = Purgatory; **0xfa = removed**)][0 @3][u16 target bot id @4, 0xfe = not a bot][u16 @6 → slot +0x15ae, ?][i16×3 velocity ×10 @8][i16×3 fwd ×10⁴ @0xe][u32 target pid @0x14][u32 owner pid @0x18][f32×3 position @0x1c]`. One create and one remove per missile fired. Receivers key it by (owner, id), put it in a free ship slot and the `0x14fd998` table, and fly it for display. **Target** (`0x9d557c`): target pid == me and @4 = 0xfe → it's on me (lock warning sound, and my last attacker becomes the owner if unset); @4 ≠ 0xfe → the bot with that id; else a target pid ≤ 0x7fff is a player, 0x8000–0x83e7 a slot (pid − 0x8000), above that an object. Captured: @4 = 1 (a conflux), target pid = the shooter's own id. A remove carries the final position; types 18–21 get the big explosion. [static + capture jgcap2, 2026-11-20] |
| 0x1A | 16 | **"mkill"**: I shot down someone's missile (`0x43aedf`, space link) | `[1A][missile id @1][u16 @2 = that missile's 0x32 @4][u16 loc @4][? @6][i32 its target slot on my client @8][u32 owner pid @0xc]`. Server handling unknown [static 2026-11-20] |
| 0x4F | 20 | **Artifact pickup** ("teleportPacket", sender `0x43c8a9`) | `[4f][3 junk][u32 player_id @4][f32 own position ×3 @8]`. Sent after 4 s of InSight beam on an artifact with a Displacer or Burglar fitted. No artifact id. See "Mineral vein and artifact". [static 2026-11-19, never captured] |
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
  `0x13`(788B) are the likely carriers (`0x78` turned out to be building sites, 2026-11-18) of the per-sector object
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
  "request asteroid" (4 B, state conn). The answer is S→C 0x73 (asteroid ore
  levels), whose handler zeroes `0xbe5c10`.
- The 0x05/0x33/0x34 share one snapshot block at `0xff2a50..0xff2af0`:
  orientation `0xff2ab8..cc`, **velocity** `0xff2ad0..d8` (copied from the
  velocity cells), position `0xff2aac..b4`,
  clock `0xff2aa0`.

### 0x06 login rejection  [static + capture of the text table, 2026-11-21]

The classic server refused a login with a **0x06 whose byte @0x42 (the ship
type in a real record) is an error code**. The handler checks it first
(`0x9d207f`, reading `0xba8e62`). On a match it calls `0x403010(text)`, which opens the
"Connection Error!" panel (`opt_conerror`, art `interface/opt_conerror.bmp`)
with the text in its box (`opt_context`) and a "Quit Jumpgate" button
(`opt_conquit`). It then clears the session flag `0xbe5b90` and skips the rest
of the record. The texts are UI text slots `0xb549a8 + i*0x384`, where slot i
is line i of the client's text table `sampleslarge.txt`:

| code @0x42 | slot / line | text |
|---|---|---|
| 0xc8 | 120 | "You are running an incorrect version of Jumpgate…" |
| 0xc9 | 121 | "Check the spelling and case of your username. This username is not valid:" |
| 0xcb | 119 | "Your pilot is already connected to the Jumpgate universe" |
| 0xcc, 0xcf | 111 | "Your account has expired…" |
| 0xcd | 118 | "Your account is not enabled for use…" |
| 0xce | 117 | "Password or username incorrect" |
| 0xd0 | 116 | "Expired command line" |
| 0xd1 | 122 | "Your account is temporarily disabled." + " Remaining time: %.2f hours." (f32 @0x60; German variant if `0x14ff650` == 1) |

0xca is not handled, so it is parsed as a normal record. Nothing else in the
record is read on this path. The panel's text-slot ↔ line mapping is inferred
from these eight matching meanings (e.g. 0xd1 is the only code with a time,
and it lands on "temporarily disabled"). Not yet seen live.

### 0x06 character record  [static 2026-11-19, handler `0x9d205f`]

544 bytes. The handler copies each field into the client's own-pilot globals
and finishes with loadout_seal (`0x9cb650`), so a loadout sent by the server is
accepted as legitimate.

| off | type | field |
|---|---|---|
| @0 | u8 | 0x06 |
| @1 | u8 | faction: 1 Solrain, 2 Octavius, 3 Quantar (→ `0xbe5b0e`) |
| @2 | u8 | rank |
| @3 | char[11] | callsign, NUL-terminated |
| @0x0e | 5 B | → `0xff2a8d` (capture `04 00 7d 05 0d`, the same bytes as 0x05 report @0x3e) [unknown] |
| @0x13 | | squad name |
| @0x22 | wchar[16] | space-server address (only used when mode @0x45 == 1) |
| @0x42 | u8 | ship type (capture 0x5b; Storm = 90) |
| @0x43, @0x44 | u8 | [unknown] (capture `fe`, `01`) |
| @0x45 | u8 | mode: 1 = connect to the address @0x22 |
| @0x50 | u16 | location |
| @0x52 | i16 | market price factor: 1 + v/10000 (see "Station market") |
| @0x54 | u32 | player id |
| @0x58 | u32 | [unknown] |
| @0x5c | u32 | experience |
| @0x60 | f32 | [unknown] |
| @0x68 | u64 | credits |
| @0x70..0x77 | u8 ×8 | gun slots (gun index, 0xfe = empty) |
| @0x78..0x7f | u8 ×8 | missile racks |
| @0x80..0x87 | u8 ×8 | devices |
| @0x88..0x8d | u8 ×6 | engine, powerplant, radar, ECM, shield, capacitor (index within the category) |
| @0x8e | u8 | [unknown] |
| @0x8f | char[13] | ship name ("My Ship") |
| @0x9c | 48 × 8 | item list, same slots as 0x45: `[cat][idx][cc cc][f32 qty]`, `fe fe` = empty |
| @0x21c | u32 | [unknown] |

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
- **What the market list shows** (`0x44f870`, one case per category; the
  list is then sorted by price, `0x449ae0`): an equipment entry is listed
  only if **stock > 0 and price > 200** (`cmp …, 0xc8`), and the item's own
  table entry has its "available" byte set (guns `+0x9`, missiles `+0x1`, …;
  the `b09`/`o01` columns of the extracts). A GM flag (`0xbe3d75`) drops the
  price test, and a "show all" toggle (`0xb4bd04`) lists zero-stock entries
  too. So **equipment priced ≤ 200 never appears**; prices ≤ 200 effectively
  mean "not sold here". Commodities go through their own case. [static
  2026-11-20; confirmed live: a server listing all equipment at 99 showed
  empty equipment tabs]
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
  The client always displays this formula's price, so a server that pays
  anything else shows one price and pays another.

### Building sites (S→C 0x78)  [static + capture, 2026-11-18]

Every sector has invisible **building-site markers**: world objects with ids
**950–999** (model `0x153`, type 339 in `extracts/sector_objects_all.csv`, where
the id is the `object_id` column). Station sectors have 50 sites; space sectors
have 5 (ids 950–954). Each has a position and orientation in the extract.
S→C 0x78 says what stands on each site. The server sends it before every 0x08
(launch, gate jump, `!goto`) for the destination sector.

Layout: `[78]` + 50 entries × 3 bytes (151 B). Entry *i* is for site 950+*i*:

| byte | meaning |
|---|---|
| b0 | `type = b0 >> 2`, `stage = b0 & 3`; 0 = empty site |
| b1 | item category produced (Gun 0, Msl 1, Com 3, Eng 5, Ppt 6, Rdr 7, Ecm 8, Shd 9, Cap 10, Mdx 11) [inferred] |
| b2 | item index in that category [inferred] |

Handler: the dispatcher case `0x9d7e2f` unpacks the entries into `0x14b4858 + 16*i`
(`type, stage, cat, item`). If not docked (`0xbe5b08`), it then calls
`spawn_building_site_object` `0x4b4750(i, type, stage, item, cat)` for each
nonzero type. That fills **special-object ship slot 300+i**
(`0x1042118 + slot*0x160c`): active, `+3 = 0xff`, armour 50000, position and
fwd/up copied from world object 950+i (`0x140ad48 + (950+i)*0xdc + 0x60…`), stage
@+0x3e, item @+0x44, cat @+0x48.

- **Model = `0x150 + b0`**, i.e. `b{type}s{stage}.x` (the `bNsM.x` building
  meshes, models `0x154`–`0x17b`; stage 0–3 shows as 15/50/85/100 in the
  bldginfo dump, so it's probably construction progress).
- **type 11 (SDS)** doesn't place anything at the site. If contact slot 6 is a
  `station7a_body` (model `0x17c`: Solrain/Octavius/Quantar Depot), the
  station's model becomes `0x17c + stage` (`b11s1..3`, the station with
  defences). Elsewhere it does nothing.
- **type 15 (CFX)** places model `0x1ae` = `cf_eruption.x` (a conflux
  eruption; physics.md "repulsor": pushes ships within 2000, destroys
  missiles), item field = site index.
- Type names, from the S→C 0x19 handler `0x9c9250` (appends one building record to
  `bldginfo.txt`): 0 Nothing, 1 TC (Tooling Center), 2 NA (Nano Assembler),
  3 NY (Naval Yard), 4 SF, 5 OD, 6 CP, 7 RF, 8 RP, 9 RA, 10 ST, 11 SDS,
  15 CFX. The long names for 4–10 aren't certain. The client's module list
  `0x457550` gives "Tooling Center", "Nano Assembler", "Naval Yard"…
- Zero entries don't clear slots. Slots are reset on location change.

Captured (the item names support b1/b2 = what the building makes):

| sector | sites |
|---|---|
| 118 Quantar Depot | 0: SDS stage 3 (depot station upgraded) |
| 249 Tictac's Hook | 0: SF s3, Gun 47 Vantage |
| 108 TriPoint-Station | 0: NY s3 Gun 8 Straker; 1: CP s3 Com 59 Fiber Optics; 2: CP s3 Com 52 Synthetics; 3: NA s3 Com 43 Helium; 5: NY s3 Gun 15 V.A.P.O.R. |
| 109 The Corridor-Station | 0/5/10/15: NY s3 Gun 6 Calefactor / 9 Landlord / 11 Rasp / 13 Serializer |
| 107 Quantar-Core Station | 0: NY Gun 10 Peeler; 1: CP Com 40 Chemicals; 2: CP Msl 20 Glaive; 3: CP Com 97 Quanus Ice; 4: SF Gun 44 RB-2; 5: NY Gun 12 Senator; 10: NY Gun 14 Broker |
| 241, 253, 254, 257 (space) | none |

(Commodity index = row in `cargo.csv` [inferred].) Naval Yards sit on every
fifth site with guns, and CPs make commodities. This looks like the
player/faction production economy. Its server side is unknown: what the
buildings do (stock a station's market? need inputs?) isn't visible from the
client.

### Asteroid ore levels (S→C 0x73, C→S 0x74/0x75)  [static + capture, 2026-11-18]

**S→C 0x73** = `[73]` + 250 bytes = 1000 two-bit levels, one per world object
id (the `object_id` column of `extracts/sector_objects_all.csv`). Object *i*
is at byte `1 + i/4`, bits `2*(i%4)` (`0x9c7a00`). The handler (`0x9d7b39`)
copies the bitmap to `0xba79c8` and sets each object's minable ore
`0xbe5e14[i] = base(type) × f(level)`:

| world-object type (`+0`) | base | | level | f |
|---|---|---|---|---|
| 15, 18, 21, 24, 27 (large) | 48 | | 3 | 1 |
| 16, 19, 22, 25, 28 (medium) | 16 | | 2 | 0.6666 |
| 17, 20, 23, 26, 29 (small) | 8 | | 1 | 0.3333 |
| 0x13d–0x13f | 128 | | 0 | 0 (mined out) |
| anything else | 0 | | | |

The base equals the extract's `mining_divisor`. Checked against all 10
captured 0x73s: the nonzero entries are exactly that sector's asteroids, plus
id 948 in space sectors (the object S→C 0x58 positions), and nothing else. All
were level 3 except one: TriPoint (108) asteroid 108, `ast5a.x`, at level 2,
i.e. someone had mined it. So the live server keeps per-asteroid depletion
and reports it in thirds.

**C→S 0x74** is the mining report. Every ≥10 s while mining, it carries the asteroid id,
loc and the ore mined since the last report (f32). jgcap2 (mining at
Quantar-Core, 107): asteroid 172 (cap 16) 6.8 over 7 reports, 140 (16) 7.6,
133 (48) 19.6, 145 (16) 10.0. The ore itself reaches the cargo hold through the
client's C→S 0x0F cargo report (see above).

**How depletion looks** [static, 2026-11-18]. Nothing in the client animates
depletion over time. It's gradual because the amount itself changes
continuously:
- **Mining** (`0x43ce8b`–`0x43cfac` in `0x43b1b0`, every frame the laser is on a
  rock): the yield is `rate × remaining/base × multipliers`, where the multipliers
  come from thresholds 72/48/12/6 on `[0x1440904]` (×2…×3.2, lower for types
  ≥ 0x1e) and ×1.2 when `esi` is in 90–119 [both inferred: laser grade, ship].
  It is added to the 0x74 accumulator and **subtracted from `0xbe5e14[i]`
  locally** (floor 0). Because yield ∝ remaining, the amount decays
  exponentially. Each tick takes less, and a rock never empties abruptly. The
  captured 0x74 amounts fall steadily (asteroid 133: 2.5, 1.4, 1.36, 1.32,
  1.25…).
- **Rendering** (`render_world`, `0x9f1753`): for asteroid types (and not
  object 948), `d = |remaining/base − 1|` (fraction mined, 0…1). The rock is
  drawn with `set_scene_light` (`0x9ddea0`, a D3D directional light):
  **diffuse = the sector's light colour** (`0xbf00a0 + loc*0xff0`, +0/+4/+8),
  unchanged, and **ambient = sector ambient (+0xc/+0x10/+0x14) +
  (−0.5·d, −d, +d)**. The unlit side of a mined rock loses red and green and
  gains blue, in proportion to what's been taken. The mesh and size don't
  change.
- Mining is the only continuous change. Anything else that changes the amount
  jumps: **a 0x73 overwrites the local amounts** with base × {0, ⅓, ⅔, 1}, so
  other players' mining shows up only in thirds, at sector entry or the next
  0x75/0x73 (every 100th 0x05; about 23 min of solo cruising, sooner in
  combat). A rock you mined from 16 to 12 snaps back to full colour if the
  server still says level 3.

**C→S 0x75** asks for the levels again (every 100th 0x05 in a sector). Answer with
0x73.

### Mineral vein and artifact (S→C 0x58)  [static + capture, 2026-11-18]

Sent on entering a space sector (loc ≥ 201; the handler `0x9d883f` ignores it
otherwise). It carries 36 bytes in two records. Coordinates are **obfuscated i32**:
`0x9c7460` does `float(v ^ 0xc0b55)`, then adds the sector origin
(`0x1044cec/f0/f4`).

| off | meaning |
|---|---|
| 1 | **commodity index** mined from the vein (item 222+n, cat 3) → `0xbe5c2c` |
| 2 | asteroid type (15–29, as world-object `+0`) → `0xbe5b26`; `0xbe6ce4` = 6/15/30 by type [use unknown] |
| 3 | pad (`cc`) |
| 4, 8, 12 | vein position x, y, z (obfuscated) |
| 0x10 | 1 = first play a particle burst at the old vein, coloured from `0xbe6e8c[old commodity]` (only at high detail `0xff1d80 == 1`) [inferred: depleted/moved] |
| 0x11 | **artifact look**: model = `0x190 + b/16` (`artifact1.x`…`artifact5.x`), skin = `b % 4` (`art1.bmp`…`art4.bmp`). Keep b < 80; higher values name models that aren't artifacts. Captured: 0x02, 0x14, 0x15, 0x39, 0x4c |
| 0x14, 0x18, 0x1c | artifact position (obfuscated) |
| 0x20 | 1 = play an explosion burst at each old artifact before it's removed (high detail only) |

The vein is **world object 948** (`0x140ad48 + 948*0xdc`): reset, given that
type and position, mass 100000, active. Everything that handles 948 does so
specially:
- **Mining** (`0x43cfd1`): an ordinary rock yields a random ore kind 1–5
  (cargo item 174+k = 175 Common Metals, 176 Radioactive Metals, 177 Ice,
  178 Semifluxors, 179 Precious Metals; the roll odds depend on rock type).
  Object 948 yields **commodity `msg[1]`** directly, and its yield isn't zeroed
  when the local amount reaches 0.
- **Rendering** (`0x9f18d1`): no depletion tint. It uses the commodity's colour
  from `0xbe6e8c + n*12`. The HUD (`0x9e980b`) also handles it separately.
- It is set in every 0x73 (level 3).

**The artifact** (`spawn_artifacts` `0x4b4b00`, called on every 0x58)
[static 2026-11-19]:
- It first removes **every** contact with a model in 0x190–0x194 (slots
  0–699), whatever byte 0x20 says. The flag only adds the burst. Your target
  is cleared if it was one of them.
- It then creates **one** artifact in the first free contact slot **10–59**,
  which is part of the remote-ship pool. With no free slot, there's no
  artifact.
- The new contact gets the model and skin from byte 0x11, the position from
  0x14–0x1c, and mass 50,000.
- The skin is set on the shared model (`0xa60d90`), so it applies to every
  artifact using that model.
- **It has no name or item identity on the client.** The only strings are the
  file names (`artifact1–5.x`, `art1–4.bmp`). The cargo table's 32 artifact
  items (285–316: PC-DSS1–4 in Poor/plain/Rare/Perfect, PC-UC1–16) are not
  referenced by this code. What a pickup is worth is the server's choice.

**Picking it up** (`0x43b1b0` weapon-hit code, branch `0x43c79a`):
- Hold an **InSight** beam on it. The gun must be class 3 (`gun +0xe0`), and
  InSight is the only class-3 gun.
- A **Displacer** (item 204) or a **Burglar** (220) must be fitted. They are
  device indices 52/68 = item − 152, checked by `0x439f70`.
- An unknown value at `*0x1440904` must be > 5.0.
- After **4 s** of beam time (accumulator `0xbe70e4`), the client sends
  **C→S 0x4F** (the "teleportPacket"):
  - layout: `[4f][3 junk][u32 player_id @4][f32 own position x, y, z @8]`, 20 B
  - it names **no artifact**: the server has to match the player's position
    to the artifact it placed.
- Locally, own missiles aimed at the artifact are retargeted and nothing is
  removed. The server presumably answers with the item (0x45) and a fresh 0x58
  with byte 0x20 = 1: the old artifact bursts and a new one appears elsewhere
  [inferred; never captured].
- In the simulator the client only plays a sound.

Captured veins (one per space sector): 241 Long Walk = 16 Iron (−759, 6622,
25663); 249 Tictac's Hook = 18 Magnesium; 257 The Dark Trail = 24 Radium;
253 Outer Roh-Cloud = 6 Cesium; 254 Inner Roh-Cloud = 11 Gallium. These are
the refined elements the contract texts describe (`0x9c57d0`). Byte 0x10 was
0 and byte 0x20 was 0 in all five.

### Consumables and servicing (C→S 0x0E; S→C 0xA9, 0x79, 0x9E)  [static + capture, 2026-11-18]

**C→S 0x0E, 16 B** (sender `0x9cabc0`). It reports what the ship used up
since it was last full. Each value is "1 − fraction left":

| off | type | meaning |
|---|---|---|
| 0 | u8 | 0x0E; bytes 1–3 are stack junk |
| 4 | f32 | fuel used = 1 − fuel/100 (the fuel cell `0x14408b0`; 100.0 is full) |
| 8 | f32 | hull used = 1 − `0xff2ae4` / (ship-type armour `+0x150` × 3000). `0xff2ae4` is a copy of the hull cell |
| 0xc | f32 | magazines used = Σ over gun slots 0–7 with ammo (`gun+0x64` ≠ 0) of (1 − ammo/max). Energy guns don't count |

How it's produced:
- `snapshot_consumables` (`0x9c74c0`) fills three globals
  (`0xff2708/0c/10`). It runs in two places:
  - in the docking code (`0x40e2a2`), just before the client sends 0x0F
    cargo and the 0x07 dock request;
  - in `player_death` (`0x9cf141`), after it sets `0xbe5af4 = 2`. With that
    flag set it reports a total loss: fuel 1, hull 1, and one magazine per
    ammo gun.
- It is sent from the station branch of the S→C 0x08 handler (new loc @0x2c
  < 101, `0x9d4223`), right after C→S 0x6A and 0xC1. The globals are then
  zeroed. So a death followed by a respawn at a station also sends it,
  with the total-loss values.

Captured 2026-11-10, 2026-11-18 and jgcap2: fuel 0.06, 0.45, 0.17, 0.13;
hull and magazines 0 every time. The live server sent nothing back, and
credits did not change around it. Whether it charges a fee later can't be
told from the captures (a contract payout landed in the same window).

**The client never refills itself on a plain dock.** The only writers of the
fuel cell are listed below; hull and ammo have the same set.
- startup (`0xa25215`): fuel = 100;
- a ship change (jgcap2: fuel used dropped from 0.45 to 0.17 only after a
  0x08 change-ship);
- one missile-slot item with id 0x121 (`fire_missile`): fuel = 100
  [inferred: a fuel-cell consumable];
- these server messages (0xB3 is the one the live server actually uses):
  - **S→C 0xA9 service** (≥ 5 B, handler `0x9d9a31`) [static only, never captured]:
    - msg[4] ≠ 0: repair, hull cell = max hull (`0x14e86bc`); sets flag `0xbe5ade`.
    - msg[2] ≠ 0: refuel, fuel = 100; sets flag `0xbe5adf`.
    - msg[3] ≠ 0: rearm, every gun slot (up to ship-type `+0x154` slots) gets
      ammo = `gun+0x64`.
    - msg[1] is not read here.
    - This is the natural reply to 0x0E.
  - **S→C 0x79 service pad** (handler `0x9d7f33`): msg[1] is a contact slot
    whose model chooses the service: 0x16f refuel (fuel = 100), 0x173
    repair (hull = max), 0x177 rearm (all guns full). Each also shows a
    message (`0x9c3ca0` ids 0x23 / 0x24 / 0x1a). This answers the in-space
    service beam (C→S 0x79, physics.md §6).
  - **S→C 0x9E enter space** (`enter_space_handler` `0x9cc750`) [static only,
    never captured]: loads hull @+8, fuel @+0x70, @+0x6c (→ `0x14e86c0`)
    and more cells @+0x80..0x98 from the message. Layout not decoded yet.

  - **S→C 0xB3 own-ship state** (correction, 2026-11-18): this is how the live
    server refuels **and repairs** on launch. 0xB3 answers every C→S 0x17
    (about every 13 s in space). Layout, 44 B, every byte accounted for:

    | off | type | into | meaning |
    |---|---|---|---|
    | 0 | u8 | | 0xB3; bytes 1–3 are padding (0xcc on the live server) |
    | 4, 8 | i32 | `0x140ac2c`, `0x140ac38` | ammo, gun slots 0, 1 |
    | 0xc | f32 | hull cell `0x14e86c4` | hull; skipped if the client's hull is exactly 1000.0 |
    | 0x10, 0x14 | i32 | `0x140ac44`, `0x140ac50` | ammo, gun slots 2, 3 |
    | 0x18 | f32 | fuel cell `0x14408b0` | fuel (100 = full) |
    | 0x1c–0x28 | i32 ×4 | `0x140ac5c` … `0x140ac80` | ammo, gun slots 4–7 |

    - Ammo: the cell for gun slot i is the one `snapshot_consumables` pairs
      with `0xff7d00+i`. A full magazine is `gun+0x64` (items.md); energy
      guns have 0.
    - The hull test is `fucompp; test ah,0x44; jnp skip`, i.e.
      `if (hull != 1000.0f) hull = msg.hull`. I first read it backwards. Why
      1000 is exempt is unknown; no ship type has armour 1/3.
    - Live captures:
      - Fuel: 100.00 in the first 0xB3 after every launch. After that it
        follows the fuel the client reports in 0x05 @0xac (jgcap2 went down
        to 55 at the dock, matching 0x0E "fuel used 0.45").
      - Hull: always exactly full, ship-type armour × 3000. jgcap Thunder
        24000 → 72e6; jgcap2 Typhoon 4830 → 14.49e6, then Hurricane 4700 →
        14.1e6 after the ship change. No damage was taken in these sessions,
        so whether the server echoes damage mid-flight (0x05 @0xa4) is
        unconfirmed.
      - Ammo: always 0 (energy guns only).
    - The 0x05 report carries fuel and hull but **no ammo**, so the server
      has to track ammo itself (or just send full magazines on launch). How
      the live server tracks ammo is unknown.
    - This matches the user's report that every live launch is fully fuelled
      and repaired. Neither comes from the startup reset: `0xa25215` and
      `cells_alloc` run once, from WinMain (`0xa241e0`, called by the CRT at
      `0xaac6d0`).
    - Related: S→C 0xA1 `[a1][3 pad][f32 hull @4]` sets hull (clamped to max;
      handler `0x9d94b2`). It never appears in the captures.

0xA9 stays available for station services. Treat the 0x0E values as a report,
not as truth (they're computed by the client).

### Rewards seen (classic server rules, jgcap2)

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
- **Tint** (`0x9ddea0` `set_scene_light`, 13 callers, mostly the renderer
  `0x9ede30`): it fills a D3D directional light (`D3DLIGHT8` @`0x14e82d4`,
  diffuse = args 0–2, ambient = args 3–5) and calls SetLight. When the current
  location's cloud type is 1, the colours are first blended with the kind's
  colour (`0xa1edf0`): 1 red, 2 green, 3 blue, 4 yellow, 5 purple, 6 teal,
  7 orange, other grey. Also used for asteroid depletion shading (see
  "Asteroid ore levels").
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

## Combat: hits, damage, death  [static 2026-11-20]

### Hit queue → C→S 0x1D
The shooter's client collects hits in a 16-entry queue at `0xb34660`
(0x14-byte entries: `+0 victim pid, +4 u16 bot id, +6 u16 count, +8 kind,
+9 weapon, +0xc f32 value, +0x10 class`). `0x43a3d0` flushes it from the
location tick (`0x9cdcd9`), one 0x1D per entry on the **space** link
(`0x9c9c90`), then clears it. Hits count up to 40 per entry
(`0x43a0a1`). The adders:

| adder | caller | class | kind | value | meaning |
|---|---|---|---|---|---|
| `0x43a060` | gun hit test `0x43fbe6` | 0 | 0 | 0 | direct gun hits × count; weapon = gun index (shot `+8`) |
| `0x43a1a0` | `0x43e900` in splash `0x43e650` | 0 | 0 | damage | gun splash; the shooter already subtracted it locally |
| `0x43efc0` | missile update `0x42d86f` | 0 | 1 | 0 | direct missile hits × count; weapon = missile type (`0x14fd090`), **0xfe if the missile's damage arg ≤ 1.0** |
| `0x43a130` | `0x43eda4` in splash `0x43eb30` | 0 | 1 | damage | missile splash |
| `0x43a210` | `0x43c71d` | 1 | 0 | accumulated | weapons 0x2b–0x2e on objects 300–349: victim 0x7fff, bot id = object − 300 (not ship damage) |
| `0x43a2c0` | beam `0x43be1e`/`0x43bee3` | 2 | 0 | accumulated | beam damage |
| `0x43a360` | `0x43c209` | 3 | 0 | 0 | special-weapon hit, count 1 |

Victim = the target slot's player id (`+0x15b4`), bot id = slot `+0x32`
(0xfe when the slot isn't a bot). **A hit on a bot the shooter itself
controls is never queued** (the adders' callers skip it when slot `+2` == 0):
the controlling client applies it locally. So only hits on other players and
on other clients' bots reach the server.

### S→C 0x03 damage (handler `0x9d1463`)
`[03][?][u16 bot id @2][u32 attacker @4][f32 damage @8]`.
- Bot id 0xfe: the receiver's own ship. Skipped while dead (`0xbe5bbc`) or not
  in space (`0xbe5af6` ≠ 1). Saves the attacker in `0xbe5ca0` (named later in
  0x44). `shields −= damage`; if negative, the overflow comes off the hull and
  shields are 0; hull ≤ 0 → `player_death(1)`.
- Otherwise: the pbotloc entry with controller == me and that bot id. The
  attacker's ship slot becomes the bot's target (only hull types 30–229), then
  the damage goes to the bot ship's shields (`+0x15f8`) and hull.
- Attacker ≤ −1 takes another branch (not traced; environment damage?).

### Player death (`player_death` `0x9cec40`)
Sends, in order: an mloc (`0x9ca840`); **0x44** (bot id 0xfe, killer, −2);
0x4C mission packets if on a mission; **0x07 action 2 with to_loc 0xfe** on
the state link. Then sets `0xbe5bbc = 1`, which makes the client ignore
further 0x03. `snapshot_consumables` runs with the total-loss flag, so the
0x0E sent on the next station 0x08 reports fuel, hull and magazines all used.
What the classic server answered isn't captured. The client is waiting for a
0x08 to some station.

### Shield regeneration of remote ships (`0x433309`)
Every client regenerates *other* ships' shields locally between reports: it
finds the shield item whose capacity equals the slot's max shields (0x05
@0xa0) and adds its **regen × dt**, capped at the max. No efficiency factor.
If no shield matches, it uses 1000/s (`0xaeaf08`). Applied only when
`max > 0xac2aa0` and `current < max`. The own-ship rule wasn't traced.

