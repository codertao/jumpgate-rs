# jumpgate-server — the reactive fan server

Crate `server/` (binary `jumpgate-server`), started 2026-11-17. It replaces the
open-loop `jumpgate-proxy replay` with a server that answers each client
request. Scope is as in CLAUDE.md: our own server, players opt in through
`SERVER_IP`, no contact with jumpgate-tri.org.

```sh
cargo run -p jumpgate-server -- --templates jgcap.bin --verbose
# then on the game machine:
SERVER_IP=<server host> Jumpgate.exe
```

Options: `--listen ip`, `--advertise ip` (the address put in 0x08 space
redirects; defaults to `--listen`, else 127.0.0.1), `--state-port 5150`,
`--space-port 5151` (the client always uses 5151 for space, so change it only
for tests), `--home-station 18`, `--verbose` (types and hex prefixes; the login
is never printed).

## Layout

| file | role |
|---|---|
| `net.rs` | One UDP socket per port, one RakNet `Endpoint::server()` per peer address, 60 s idle timeout |
| `game.rs` | Session state and handlers. Pure: `(conn, peer, msg) → Vec<Out>`, so it's unit-tested without sockets |
| `proto.rs` | Layouts the server reads/writes: login, 0x07, 0x05 report, 0x06 patcher, 0x08/0x81/0x04/0x61 builders |
| `templates.rs` | Captured S→C messages used as response bodies until each has a builder |
| `world.rs` | Static world from `notes/extracts/` (`--data`, default the workspace copy): location names, gate graph |

## Templates (temporary)

Most S→C bodies are only partly decoded, so the server answers with the first
captured copy of that type and patches the fields it understands. The capture
is read from a local path at startup and never committed.

Never taken from the capture: **0xFE** (client memory write; never sent at
all), **0x81** MOTD and **0x04** chat (other people's words; we build our own).
Every captured 0x06/0x08 has its space address rewritten.

Goal: replace templates one type at a time with builders driven by
`notes/extracts/*.csv` (locations, items, prices once designed).

## Gate travel (from the extract)

`sector_objects_all.csv` jumpgate rows (both models, `jump_ends.x` and
`x_ai_gate.x`) give `(loc, gate_number) → (dest_loc, dest_gate)`. That's **417
links** from the client's own table (`0x146d7a0`, filled by `0x4a7460`).
- C→S 0x07 jump carries gate number − 1.
- The S→C 0x08 sub byte is the arrival gate: the client spawns the ship at that
  contact slot (handler reads `slot = msg[1]`, position = slot pos + random
  offset).
- Verified: all 3 captured jumps resolve to the captured destination, and the
  built 0x08 equals the captured one byte for byte outside the address field
  (`built_jump_redirect_matches_capture`).
- Quirks in the game data, followed as-is: 3 one-way links (268/3 → 309,
  309/1 → 268 whose return goes to 266, 348/1 → 349). 8 gates have no link
  (loc 0, 1000–1005); jumping through one gets a system chat line instead.
- Not yet checked: that the client's `from_loc` agrees with the server's (it's
  logged if not).

## Request → response table (as implemented)

All responses go on the **state** connection, except keepalive answers, which
use their own connection. The client has one dispatcher for both connections,
and the replay milestone ran entirely on state. Whether any handler checks the
`[pkt+0x10]` connection tag is still unverified.

| C→S | conn | Server does | Source |
|---|---|---|---|
| 0x05 (33 B) login | state | new player id; 0x06 (template, patched callsign/loc/pid, space IP cleared), 0x8c, our 0x81, 0x18 | capture |
| 0x09 cmd | state | the listing for that cmd (template, else zero table 896 B) | capture order |
| 0xc1 | state | 0xc2 | capture |
| 0x65 | state | 0x65 ack | capture |
| 0x04 chat | state | relayed verbatim to every other player [inferred] | — |
| 0x63 / 0xc3 / 0x8a | state | 0x62 / 3×0x14 / weather: docked 0x87, in space 0x73+0x87, plus 0x61 beacon only in sectors 201–999 | capture, pairing [inferred] |
| 0x91 / 0x4b | state | 0x91+2×0x76 / 0x4b | capture |
| 0x17 | state | 0x42 + 0xb3 | capture |
| 0x07 launch | state | loc = station+100; 0x06, 0x78, **0x08 → our IP**, dest sector | capture |
| 0x07 jump | state | gate = arg + 1 → extract link → 0x78, 0x08 (our IP, dest loc, sub = arrival gate). Built, no template | extract; byte-identical to captured jumps |
| 0x07 dock | state | loc = sector−100; 0x45, 0x92, 0xba (pid patched), 0x4b, 0x06, 0x8c, **0x08 "none"** | capture |
| 0xc8 | state | logout | — |
| 0x8f | both | 0x8f on the same connection (the client's dispatcher ignores it, `0x9d3006`) | capture |
| 0x05 (176 B) report | space | stored and relayed to sector-mates as S→C 0x05 (handler `0x9cf7c8` creates/updates that ship). Newcomers are sent everyone's last report | static |
| 0x33 / 0x34 | space | relayed to sector-mates if the id field is the sender's | static |
| 0x71 / 0x2c | space | ignored (bots not implemented) | — |

Server-side authority already enforced: player id and loc in relayed 0x05 are
overwritten with the server's values. A space session can only claim a player
whose state session is on the same host. Fast updates must name the sender.
When a player leaves a sector, the others get their last report with ship type
@0x46 = 0xfa [inferred removal value].

Space-session binding: on handshake, the newest in-space player on the same
host with no space session. A later 0x05 report (pid @0x5c) can correct it.

## Not done yet / next

- Live test with the real client (unit, capture-flow and UDP login tests pass;
  the real client hasn't run against it yet).
- Accounts: any password is accepted and dropped. Add salted slow hashes
  (argon2/bcrypt) before anyone else connects.
- Sector content: 0xA7 station list and 0x58, per sector. Currently the
  captured sector's copies are sent everywhere. (0x87 is universe-wide particle
  clouds, so one list for everyone is correct. The server could own the cloud
  list and drift it: see message-types "Particle clouds".)
- NPCs: the next big step (see CLAUDE.md, "Design facts"). Server-simulated
  ships can reuse the S→C 0x05/0x33/0x34 relay path above.
- RakNet `session` has no retransmission, which is fine on a LAN but needs adding
  for real networks.
- Unhandled types are logged once each. Use `--verbose` sessions to grow the table.
