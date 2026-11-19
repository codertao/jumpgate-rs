# Our `raknet` crate: behaviour and completeness

The RakNet 2.x wire format as Jumpgate uses it is in
../notes/raknet-spec.md and ../notes/differences-from-samp-raknet.md.
This file is about our implementation (`raknet/`). [ours] = our behaviour,
not necessarily the client's.

## Where we differ from the reference

- **Handshake reliability** (differences §1.8): SA:MP sends 0x06/0x07 UNRELIABLE.
  Our crate [ours] sends them **RELIABLE** (`emit_transport`), and the real
  client accepts that [live].
- **When AES switches on** (differences §1.9): SA:MP uses a size heuristic.
  Our server [ours] holds the key as pending and turns it on with the
  first datagram that decrypts and passes the checksum. Proof, not a guess.

## How complete is it?

**The wire format is complete and verified.** Every byte the client sends or
accepts in the handshake, the crypto, the datagram/packet/ack layout and the
game framing has been proven live. What's missing is the **reliability
machinery**, which matters as soon as packets get lost, arrive out of order,
or a session runs for hours. Roughly in priority order:

| # | gap | effect | reference behaviour |
|---|---|---|---|
| 1 | ~~Duplicate set never pruned~~ **Fixed 2026-11-17/18** | Now a wrap-aware sliding window over all packets (`recv_base` + `recv_window`), as in the reference. As there, a hole is given up on after a timeout (we use `ACK_TIMEOUT`, 15 s), because lost unreliable packets never arrive and would otherwise pin the window. Tests: 200,000 numbers (three wraps); 100,000 with every other number lost. | sliding window over `receivedPacketsBaseIndex`; holes expire at `timeoutTime` |
| 2 | ~~No retransmission~~ **Done 2026-11-17** | Reliable packets are held until acked and resent after 3× smoothed RTT (30 ms..3 s, doubling per resend up to 16×, capped at 6 s). Pre-key handshake packets resend in the clear and are dropped once AES is live. Test: 300 messages over 30 % loss both ways, all delivered once. | resend list keyed by message number; resend after ~3× ping (min 30 ms) |
| 3 | ~~No ordering/sequencing on receive~~ **Done 2026-11-18** | Per channel: RELIABLE_ORDERED packets are held until the gap before them fills (up to 4096 per channel), and sequenced packets older than the newest handled are dropped. Ordered and sequenced indices are tracked separately. Tests: hold-and-release across the u16 wrap; 300 messages over 30 % loss arrive in exact order. | per-channel `waitingForOrderedPacketReadIndex` / `…Sequenced…` |
| 4 | **No split packets** | Sending: a message over the ~1,456-byte budget goes out oversize, with a warning. Receiving: split packets aren't reassembled. Captured traffic never exceeds 896 B, so this is unused so far. | `splitPacketChannelList` |
| 5 | ~~No RTT or timeouts~~ **Done 2026-11-17** (ack-based) | RTT is timed from send to ack for packets sent once (Karn), smoothed 7/8. A reliable packet unacked for 15 s marks the connection lost, and the server's listener drops it. We still don't send pings: ack timing covers RTT without depending on the client answering a server ping (unverified). | periodic `PingInternal`; `timeoutTime` |
| 6 | **Disconnect/full handling** | We now *receive* `0x14`. We never *send* it (kick/shutdown) and never refuse with `0x13` when full. | both |
| 7 | **No congestion control** | We send everything at once. Fine at our traffic levels. | send-rate control (`STARTING_SEND_BPS` …) |
| 8 | ~~Send reliability is one-size~~ **Done 2026-11-18** | The server sends 0x33/0x34 position updates UNRELIABLE_SEQUENCED on channel 0, as the client does (`proto::is_position_update`). Everything else stays RELIABLE_ORDERED channel 0. Sequenced packets have their own per-channel index counter, as in the reference. | per-message choice |
| 9 | **Static data / RPC** (`0x04`/`0x1C`, `0x08`/`0x0C`) | Not implemented. The client has handlers, but no capture shows them in use. Probably unneeded. | implemented |

**Estimate:** #1 is a small, self-contained fix. #2 + #5 (resend with RTT)
are the main work. #3 and #4 are moderate. All together it's a few hundred
lines in `session.rs`, testable in-process with a lossy/reordering pump like
the existing `connected_pair` tests. Nothing here needs more reverse
engineering except confirming the client's own resend/dedupe constants if we
want to match them exactly.
