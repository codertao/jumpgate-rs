# Launching from a station  [static + capture, 2026-11-20]

How the stock client puts a ship into space on launch. The client sends 0x07
launch, and the server answers with S→C 0x08 sub 6 (message-types.md) naming the
station sector and a **launch tube**. Addresses are for `fef6e50f…`.

## Launch tubes

Each station model has up to four launch tubes, hard-coded in its model
loader beside the docking tube (physics.md §6, `extracts/station_points.csv`):

| tube k | inner end | outer end |
|---|---|---|
| 0–3 | `+0x834 + 0x20k` | `+0x8b4 + 0x20k` |

A tube whose two ends coincide (all zero in the extract) doesn't exist on
that model. Core stations and depots have four (models 4, 5, 7, 8, 321–323,
338, 380–383); model 6 has three; 411–413 have two; models 343–375 have none.

**The server picks the tube**: 0x08 @2 (`0x9d34a3`, kept in `0xbe5af3`).
Captures: tube 2 at Quantar Depot (118) and at 109, tube 0 at Quantar Core
(107). One exception in the client: in loc 101, ship types 105 and 45 always
use tube 0. A player-station launch (0x08 sub 0x63, `0x9d3a3a`) uses the same
layout on model 413 (`0x14ffd04`).

## Placement (`0x9d34c8`)

```
dir   = normalize(out_k − in_k)                 // model space; stations are unrotated
pos   = station_pos + in_k + dir · depth
depth = f142 × (5 for types 241–242, 4 for 246–248, 2 for 120–121, else 1)
v     = 0, speed = 0
fwd   = dir; up from a per-orientation table (0x102eb90 + 0x34·[0xbe5b07])
```

`f142` is ship-type field +0x142 (`ship_types.csv` column `f142`: Storm 15,
Thunder 52). It's probably the ship's length, so a bigger ship starts
further down the tube. The handler then sets the "just launched" flag
`0xbe5af6` (`0x9ccaed`).

Checked against all three captured launches: each first report after the
0x08 is at rest at exactly this point (within 1 unit).

## The catapult (`0x40e370`, in the per-frame collision pass `0x40d630`)

For each tube k = 0..3 of the sector's station, with `rel = pos − (station +
in_k)`:
- along = rel · dir, and the two lateral components a, b on the tube's
  basis;
- if along > 0, (size/2)² + a² + b² < **449.44** (radius ≈ 21.2), along <
  **175**, and the flag `0xbe5af6` is set: **velocity += dir × 3** (`0xac2aa8`).

`size` is ship-type field +0x144 (`ship_types.csv` `size`). The push is per
frame, not × dt, so the exit speed depends on frame rate. With a standing
start it's about 3·√(2·(175 − depth)·fps/3) from the push alone: ~170 u/s at
30 fps and ~240 at 60 for a Storm, plus whatever its engines add. A low-drag
hull can leave fast enough to trip the 1225 u/s speed tripwire (physics.md
§9).

The flag is cleared in several places (`0x40cfa6`, `0x40e1c8`, `0x40eeb7`,
`0x43de1a`); which one ends the catapult wasn't traced. In practice the ship
leaves the 175-unit tube within a second or two.
