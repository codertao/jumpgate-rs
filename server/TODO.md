# jumpgate-server TODO

Parked 2026-11-18 while we explore the client further. `notes/server.md` holds
the design and the request → response table. `notes/message-types.md` holds the
wire layouts. Tags: [bin] = static from the client, [cap] = seen in a capture,
[inferred] = needs confirming.

## Before anyone else connects

- [ ] **Accounts.** Today any password is accepted and dropped. Store argon2id
      (or bcrypt) salted hashes, never log the password, and warn players not to
      reuse one. Gate the `!` dev commands behind GM rights once accounts exist.
- [ ] **Server identity.** Make sure the MOTD (0x81) and the login text say
      clearly that this is the preservation server, not tri.
- [ ] **Live test pass with the real client.** Cover what's still untested:
      `!goto` into the sector you're already in, removing a ship that changes
      sector (0xfa), and whether the client's `from_loc` agrees with ours on
      jumps (we log it when it doesn't).

## Sector content: replace replayed templates with per-sector builders

These templates are replayed in every sector today (`game.rs` `reply_tpl`):

- [ ] **0x73 asteroid ore levels** (sent on 0x8a in space). The captured copy is
      the Quantar Depot bitmap, so asteroids elsewhere probably read as empty
      and can't be mined.
  - Build it from `sector_objects_all.csv`: 2 bits per world-object id
    (`object_id`, not `slot`), set 3 for the sector's asteroids, and add 948
    (the vein) in space sectors.
  - Track depletion server-side from the C→S 0x74 mining reports (f32 mined
    @8) against base(type) × level. Round to the nearest third.
  - Send only on sector entry and in reply to C→S 0x75. Each 0x73 makes the
    client's local amounts jump to the new third.
  - Regrowth rate is a design choice.
- [ ] **0x78 building sites** (on launch and jump). Build per sector: 50
      `[b0][cat][item]` entries for world objects 950+i. Start with all
      zero (nothing built), or use a server-owned list of player buildings.
- [ ] **0x58 mineral vein** (not sent at all yet). Per space sector (loc ≥ 201):
      commodity, asteroid type, obfuscated position (i32 ^ 0xc0b55, relative to
      the sector origin). Artifacts are optional (@0x10/0x11/0x14.., models
      0x190–0x194). Decide where veins sit and how often they move.
- [ ] **0xA7 station list** (not sent yet): build per sector.
- [ ] **0x87 particle clouds**: these are universe-wide, so one list for
      everyone is correct. Optionally the server could own the list and drift
      it.
- [ ] **0x61 beacon**: currently only for sectors 201–999. Check what it should
      carry per sector.

## Economy

- [ ] Station listings (0x09) from the extracts, with the server's own stock and
      prices instead of the captured table. Prices are a design question.
- [ ] Buy/sell applied to a per-player 0x45 (credits + items), 0x92 experience,
      and 0x4B contracts (trade and mining).
- [ ] Ore sale prices must follow the client's mineral-composition formula
      (`compute_ore_prices` 0x449690; message-types "Station market").
- [ ] **Don't trust cargo reports.** C→S 0x0F cargo and 0x74 mining amounts are
      client-controlled. Cap them by mining time × laser rate × asteroid
      remaining before crediting anything.
- [ ] **0xB3 own-ship state is a replayed template: build it.** Today every
      0x17 gets the captured 0xB3, which resets fuel to 100 every ~13 s and
      zeroes ammo in every gun slot. Send fuel 100 and full magazines on the
      first 0x17 after a launch (hull = ship-type armour × 3000 repairs it),
      then echo the client's 0x05 fuel (@0xac) and hull (@0xa4). See message-types
      "Consumables and servicing".
- [ ] C→S 0x0E consumables used (decoded): log it and charge from the server's
      own view if prices are wanted. S→C 0xA9 `[a9][0][r][a][h]` refuel/rearm/
      repair is there for station services.
- [ ] Ammo: 0x05 doesn't report it, so the server must track ammo itself (or
      fill magazines on launch). 0xB3 always overwrites all 8 ammo cells.

## Persistence

- [ ] Player state (ship, cargo, credits, location, experience) saved across
      logins. Today everything is per process.
- [ ] World state (asteroid levels, building sites, veins) saved too, if it is
      meant to evolve.

## NPCs (see CLAUDE.md "Design facts" and server.md "NPCs")

- [ ] Choose the mix, which is open and the user's call: (a) server-simulated
      ships presented as remote players, (b) stock client-owned bots via
      0x70/0x71/0x2C, or a hybrid.
- [ ] Behaviours beyond `Orbit`: traders on gate routes, miners, patrols,
      pirates. Keep updates physics-consistent (physics.md) so dead reckoning
      stays smooth.
- [ ] Interest management in `feed.rs`: the 190 remote-slot budget (10–199) is
      shared by players, bots and missiles.
- [ ] Conflux: spawn with S→C 0x70 on sector entry, take C→S 0x44 deaths, then
      handle 0x71 botloc / 0x2C handoff (currently ignored).
- [ ] Combat authority: C→S 0x1D hit queue → S→C 0x03 damage. Check who
      resolves bot fire before relying on it.

## Transport / RakNet

- [ ] Split packets (send and receive). Large messages will need them.
- [ ] Check whether any client handler looks at the connection tag
      (`[pkt+0x10]`); today all replies go on the state connection.

## Housekeeping

- [ ] Keep removing template types as builders land. The goal is for
      `--templates` to become optional and then go away.
- [ ] Grow the request table from the `--verbose` "unhandled … first seen"
      log lines.
- [ ] Keep `notes/server.md` in sync as items close.
