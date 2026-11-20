# Auto-Dock (Docking Computer MODx)  [static, 2026-11-20]

How the stock client docks itself when the **Docking Computer** MODx is
fitted. The code is in `0x42a110` (flight_attitude_autodock; physics.md §4),
which runs every frame in space before the linear dynamics. Everything here
is from the binary of `fef6e50f…` and has not been watched live.

Notation: `this` = own-ship object `0xbdf578`; the station is contact slot 6,
with its model's points from `station_points.csv` (physics.md §6).

## Arming

- Flag `this+0x22` (`0xbdf59a`) = "Auto-Dock armed". The launch handler
  (`0x9ccc9b`) and one other path (`0x48f087`) clear it, then set it to 1 if
  `has_device_fitted(0x35)` (`0x439f70`). Device 0x35 = item 205, **Docking
  Computer** ("designed to keep new pilots safe while they learn to fly in
  zero gravity").
- `/dockoff` "Turns the Auto-Dock MODx off" (string `0x6f3cac`); the body
  wasn't traced (slash-commands.md).
- Flag `this+0x21` = "Auto-Dock running", state `this+0x66f4` (`0xbe5c6c`).

## The docking beacon

Every frame while armed (and the station slot is live, `this+0x46afe8`):

```
axis   = normalize(model[+0x814] − model[+0x7f4])         // dock_start → dock_end, outward
k      = (loc == 107) ? 2550 : 1500                         // 0x42a1c5: cmp loc, 0x6b
beacon = station_pos + model[+0x814] + axis · k            // model point added without rotation
d      = |beacon − own_pos|
```

So the beacon is on the tube axis, **1500 units out from the tube's outer end
(`dock_end`)**, or 2550 at Quantar-Core Station's sector (107), presumably
because of that station's size (bounding radius ≈ 2247). Constants:
1500 `0xac289c`, 2550 `0xaeae48`. Model-space points are added straight to the
slot position, which is right because stations are placed unrotated.

**Engage:** `d < 350` (`0xaeaaf8`) and `this+0x6714 == 6`
[inferred: the selected target is the station, contact slot 6] → if not yet
running, set running and state 1. While running, the stick inputs are
cleared every frame (`this+0x861315`, `*this+0x6554`), so the pilot can't
steer.

## States

The throttle cell is a 0–100 integer (`this+0x86130c`). Velocity cells
`this+0x861380/84/88`, speed `this+0x86138c`, nose `this+0x8613bc/c0/c4`.
Steps are **per frame**, so the autopilot's timing depends on frame rate
(physics.md §4).

**State 1: stop.**
- Each frame: velocity × 0.98 and speed × 0.98 (`0xaeae40`), throttle = −1.
  It's a direct velocity damp, not brakes or drag.
- When speed ≤ 2 (`0xaeae44`): velocity, speed and throttle = 0 → state 2.

**State 2: line up on the beacon and creep onto it.**
- Nose += 0.023 (`0xaeae3c`) × unit(beacon − pos), renormalised, and the
  up/right axes are rebuilt with cross products. That's a turn of about
  0.023 rad per frame toward the beacon.
- If cos(nose, direction) > 0.9999 (`0xac2840`) and speed < 25 (`0xac2a30`):
  throttle + 1 per frame, capped at **22**.
- Otherwise, if speed > 30 (`0xaeae38`): throttle = 0.
- Within **3** units of the beacon (double `0xaeae30`): throttle, sticks and
  velocity are zeroed, the nose is set exactly to the direction → state 3.

**State 3: down the tube.**
- Target = station_pos + model[+0x7f4] (the tube's inner end, `dock_start`).
- Same nose steering (+0.023 × direction per frame).
- If aligned (> 0.9999) and speed < 75 (`0xaeae2c`): throttle + 1 per frame
  (no cap in this branch). If speed > 90 (`0xaeae28`): throttle = 0.
- Within **60** (`0xac2aa4`) of `dock_start`: state back to 1, running and
  armed both cleared (Auto-Dock is single-use until re-armed).

The dock itself is not done by the autopilot. The ordinary docking test in
`0x40d630` (physics.md §6: inside the tube, < 60 along the axis, speed < 100)
fires on the way in, sends 0x0F + 0x07 dock, and freezes the ship. Coming in
from the beacon at ≤ 90 u/s along the axis passes `dock_end` first, so in
practice the dock fires near the tube's outer end, before the autopilot's own
60-unit stop.

## Shape of the approach

1. Fly (manually) to within 350 of the beacon with the station targeted.
2. Coast to a dead stop (≈ 2–3 s from cruise at 30 fps, since 0.98ⁿ).
3. Turn to the beacon, creep to it at ~25 u/s, stop dead on it.
4. Turn down the axis, accelerate to ~75–90 u/s and fly straight into the
   tube.

Because the stop happens wherever the ship was when it came within 350,
and the creep then goes straight to the beacon, the ship always enters the
tube from the beacon, exactly on the axis, regardless of how it arrived.
Players without the MODx dock by hand under the same docking test
(physics.md §6).
