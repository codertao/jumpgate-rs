# NPC agents (`jumpgate-agent`)

> Bare references to classic-game notes (message-types, physics.md, ai.md,
> slash-commands.md, `extracts/`, …) mean the files in `../notes/`.

Since 2026-11-19, NPCs are **independent client processes**, not something
simulated inside the server. Each agent logs in over the normal client protocol
with its own account, sockets and thread, and runs the simulation a client
would run: flight model, AI and, later, weapons and collisions. The server
treats agents exactly like players and needs no NPC code. Agents can run on
another machine, be restarted on their own, or be swapped for different
behaviours.

```sh
cargo run -p jumpgate-server -- --templates jgcap.bin
JG_AGENT_PASSWORD=<pw> cargo run -p jumpgate-agent -- --server 127.0.0.1 --count 2
```

Options: `--server ip`, `--state-port 5150`, `--space-port 5151`, `--name
Agent` (callsigns `Agent01`, `Agent02`, …, at most 8 characters before the
suffix), `--count N`, `--laps 1`, `--dock-wait 20` (seconds docked before
each launch; agent i waits 3·(i−1) s longer), `--cycles 0` (launch/dock cycles
before logging out; 0 = forever), `--data <extracts>`, `--verbose`. The
password comes from `JG_AGENT_PASSWORD` (`--password` also works, but shows in
the process list). The server provisions each callsign on its first login
(server.md "Accounts and economy").

**Scope:** agents connect only to `--server`. The space connection always goes
to that host, whatever address the server's 0x08 names, so a misconfigured or
foreign server can't redirect them. Point them only at your own server
(CLAUDE.md).

## Crate layout

| file | role |
|---|---|
| `session.rs` | One `Link` (UDP socket + `raknet::session::Endpoint::client()`) per connection. Runs the client message flow, sends keepalives and reports |
| `physics.rs` | `Flight`: the own-ship model from physics.md §3–§4. Thrust/quadratic drag/brakes, kinematic turning at 1000 × rate × turnScale rad/s, fixed 30 Hz step |
| `ai.rs` | `Route` (station geometry → patrol ring) and `Patrol` (state machine) |
| `main.rs` | CLI; one thread per agent, which reconnects 10 s after a failed session |

It shares `jumpgate-common` with the server: `proto` (login, 0x07, 0x05/0x33
reports, 0x0F, 0x17 builders; 0x06/0x08 parsers) and `world` (hulls, engines,
stations, station points).

## Message flow (as the stock client)

1. State connect (5150), then **0x05 login** (33 B; never logged).
2. **0x06**: pid, ship type, loc, fitted engine. The agent is docked.
3. After `dock-wait`: **0x07 launch** `(1, loc, 0, pid)`.
4. **0x08** with sub 6 and a space address: open the space connection (5151)
   and start the sortie at the station's launch point.
5. While flying:
   - a **0x05 report** on space every 5 s (the first one binds the space
     session to our pid on the server), each followed by a **0x17** on state;
   - **0x33** (unreliable sequenced) every 0.3 s, but only while another
     ship's 0x05/0x33/0x34 has reached us in the last 30 s. The real client
     likewise sends fast updates only when a ship is within 10,000 units.
   - **0x8f** keepalive every 10 s on each connection.
6. When the AI says dock: **0x0F** cargo report (flag 0, empty list) and then
   **0x07 dock** `(2, sector, 0, pid)`. This is the order the client uses
   (physics.md §6).
7. **0x08 "none"**: drop space, docked again, back to step 3.

Reports carry the simulated state: position, velocity, fwd/up, effective
thrust (thrust × throttle), mass and full armour. Remote clients dead-reckon
with the same thrust/drag law (physics.md §8), so the agent's motion between
updates should match what they predict.

## Patrol AI (first behaviour)

Shaped like the stock defender (ai.md §2: launch → patrol → return → dock).
Geometry comes from `extracts/station_points.csv` (physics.md §6, "Station
points extract"):
- **Launch:** the ship appears at the model's launch point (`+0x8b4`), facing
  away from the station horizontally. It flies 800 units straight out at half
  speed.
- **Patrol:** a ring of 12 waypoints per lap, counter-clockwise about the
  station's up axis, in the plane of the dock point. The radius is
  max(3000, 2.5 × launch distance): 4150 at Quantar Core and 3000 at
  Quantar Depot. The ring starts at the exit's bearing and ends on the
  docking axis after `laps` full turns. Waypoints are flown at top speed
  (√(thrust/drag), 335 u/s for a starter Storm).
- **Entry:** a point on the docking axis, one ring radius out from `dock_end`.
  The ship slows towards it to line up.
- **Approach:** down the axis to `dock_end`, at most 70 u/s (like the
  Auto-Dock autopilot's state 3).
- **Dock:** within 50 units of `dock_end` and under 100 u/s (the client
  allows < 60 and < 100).

Unit test: the whole patrol flown at the real Quantar Core and Depot geometry.
At Core it docks after about 204 s and never goes more than ~200 units outside
the ring.

## Status

- 2026-11-19: end to end against our server on loopback. Two agents were
  provisioned, launched into 107, had their space sessions bound, flew the
  patrol, docked at 7 and logged out. The snapshot had Storm / 200,000 /
  last base 7 with argon2id hashes only.
- **Not yet checked with the real client:** that agents appear and move
  smoothly, and that they vanish on dock (0xfa removal).
- Approximations: mass = hull + 5000 (fitted equipment isn't summed yet). No
  collision with the station: the route stays outside it and comes in along
  the tube. Launch facing doesn't follow the guide segments exactly (captures:
  fwd (0, 0, −1) at Core).

## Next

- Weapons and combat: targeting, fire (0x1D hit queue / 0x03 damage on the
  server, combat authority still open), missiles (ai.md guidance).
- Collisions with stations and asteroids, using the extracted volumes.
- More behaviours: traders on gate routes (buy, jump, sell, so they also
  drive the economy), miners, pirates, conflux-like hunters.
- Gate travel: 0x07 jump, then arrive at the 0x08's gate slot (needs gate
  positions from the extract).
- Track fittings and cargo from 0x06/0x45 for mass, thrust and trading.
