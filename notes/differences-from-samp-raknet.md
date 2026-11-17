# Jumpgate's RakNet 2.x vs SA:MP's RakNet 2.x  [2026-11-17]

**Compared against:** `github.com/0xcds4r/RakNet` @ `e2363998` (2020-01-05). This is
the RakSAMP lineage (SA:MP 0.3.7, P3ti's `samp_netencr`). SA:MP RakNet forks
disagree with each other in places. Our older notes cite a fork with inserted
`ID_DUMMY_*` IDs and a `^0x6969` cookie, for example. Where a value below
comes from this fork only, it says so.

**Jumpgate side:** `Jumpgate.exe` `fef6e50f…` and our `raknet` crate. Tags:
**[bin]** verified in the binary, **[live]** proven against the real client,
**[ours]** our implementation's behaviour (not necessarily the client's).

Short version: the two share the RakNet 2.x core almost byte for byte (AES
wrapper, checksum, RSA/SHA1 handshake, internal-packet and ack layout). SA:MP
then renumbers everything, adds its own datagram scrambling and auth, and
widens the reliability field. Jumpgate has none of those additions. Every
SA:MP value we copied without checking broke the first live tests
(raknet-spec.md §1, §3.1).

---

## 1. Differences

### 1.1 Datagram scrambling (SA:MP only)
SA:MP passes **every UDP datagram** through `kyretardizeDatagram`
(`SAMP/samp_netencr.cpp`, called from `SocketLayer.cpp:504`):
- It prefixes a 1-byte checksum: XOR over all bytes of `(b & 0xAA)`.
- Each byte goes through a fixed 256-byte substitution table (`sampEncrTable`).
- Every other byte is also XORed with `(port ^ 0xCC)`.

**Jumpgate: none.** [live] A datagram is either the plaintext RakNet bitstream
(before the key) or the DataBlockEncryptor output (after). Nothing wraps it.

### 1.2 Message ID numbering
SA:MP renumbers the whole enum. This fork has explicit values (`ID_INTERNAL_PING = 6`,
…) and inserts `ID_AUTH_KEY`, `ID_OPEN_CONNECTION_COOKIE`, `ID_BROADCAST_PINGS`,
etc. Jumpgate uses classic early-2.x `PacketEnumerations.h` order. [bin]
(network-protocol.md §1.1b)

| message | Jumpgate [bin] | SA:MP (this fork) |
|---|---|---|
| ID_INTERNAL_PING | 0x00 | 6 |
| ID_PING / ID_PING_OPEN_CONNECTIONS | 0x01 / 0x02 | 7 / 8 |
| ID_CONNECTED_PONG | 0x03 | 9 |
| ID_REQUEST_STATIC_DATA | 0x04 | 10 |
| ID_CONNECTION_REQUEST | 0x05 | 11 |
| ID_AUTH_KEY | — | 12 |
| ID_SECURED_CONNECTION_RESPONSE | 0x06 | 15 |
| ID_SECURED_CONNECTION_CONFIRMATION | 0x07 | 16 |
| ID_RPC_MAPPING | 0x08 | 17 |
| ID_DETECT_LOST_CONNECTIONS | 0x09 | 23 |
| ID_OPEN_CONNECTION_REQUEST | 0x0A | 24 |
| ID_OPEN_CONNECTION_REPLY | 0x0B | 25 |
| ID_OPEN_CONNECTION_COOKIE | — | 26 |
| ID_RPC | 0x0C | 20 |
| ID_CONNECTION_REQUEST_ACCEPTED | 0x10 | 34 |
| ID_CONNECTION_ATTEMPT_FAILED | 0x11 | 29 |
| ID_NEW_INCOMING_CONNECTION | 0x12 | 30 |
| ID_NO_FREE_INCOMING_CONNECTIONS | 0x13 | 31 |
| ID_DISCONNECTION_NOTIFICATION | 0x14 | 32 |
| ID_CONNECTION_LOST | 0x15 | 33 |
| ID_MODIFIED_PACKET | 0x19 | 38 |
| ID_TIMESTAMP | 0x1A | 40 |
| ID_RECEIVED_STATIC_DATA | 0x1C | 41 |

Above the transport, SA:MP uses RakNet RPCs (`SAMPRPC.h`) and sync IDs 200–212.
Jumpgate sends **no RPCs** that we've seen. Game traffic is plain messages
framed as `[0x51][obfuscated]`, plus a `0x1A` timestamp wrap S→C (raknet-spec.md §4).

### 1.3 Reliability field
| | Jumpgate [bin, live] | SA:MP |
|---|---|---|
| width in the internal-packet header | **3 bits** | 4 bits |
| UNRELIABLE … RELIABLE_SEQUENCED | **0–4** (stock) | 6–10 (`PacketPriority.h`) |

The rest of the internal-packet layout is identical (§2). Jumpgate's
`SendGameMessage` uses only 1 (UNRELIABLE_SEQUENCED) and 3 (RELIABLE_ORDERED). [bin]

### 1.4 Offline connect
| | Jumpgate [bin, live] | SA:MP (this fork) |
|---|---|---|
| request | bare `[0A 00]`, exactly 2 bytes | `[ID][u16 cookie]`, 3 bytes |
| cookie step | none | server answers `ID_OPEN_CONNECTION_COOKIE`, client echoes it XOR `NETCODE_CONNCOOKIELULZ` (defined outside the repo; `0x6969` in other SA:MP sources) |
| reply | `[0B 00]` (or `[13 00]` when full) | `[REPLY 00]` / `[NO_FREE 00]` |

### 1.5 CONNECTION_REQUEST_ACCEPTED
| | Jumpgate [bin] | SA:MP |
|---|---|---|
| layout | `[10][u32 ip][u16 port][u16 playerIndex]` | same + **`u32 challenge`** (`_uiRndSrvChallenge`, for SA:MP auth) |
| size check | client accepts **exactly 9 bytes** (`0xa6e0ad`) | 13 bytes |
| encryption | must travel in the clear; the client installs the AES key on receipt | also unencrypted; key armed afterwards |

### 1.6 Authentication (SA:MP only)
SA:MP adds `ID_AUTH_KEY` (`SAMP/samp_auth.cpp`) and the ACCEPTED challenge, on
top of RakNet. **Jumpgate has no transport-level auth.** The login (cleartext
password) is an ordinary game message (message-types.md 0x05).

### 1.7 SecuredConnection use
- **SA:MP:** the code is present but off by default (`usingSecurity = false`),
  and SA:MP relies on its scrambling (§1.1) instead [ref; believed unused by
  SA:MP servers].
- **Jumpgate:** always uses it: 256-bit RSA, SHA1 syn-cookie, AES-128. [bin, live]
  The client never calls `InitializeSecurity`, so it accepts **any** server key
  (no pinning, raknet-spec.md §1).

### 1.8 Handshake message reliability
- **SA:MP:** sends 0x06/0x07 (secured response/confirmation) with `SendImmediate`
  as **UNRELIABLE**.
- **Our crate** [ours]: sends them **RELIABLE** (`emit_transport`), and the real
  client accepts that [live].
- **Jumpgate client:** what reliability it uses for its own 0x07 is not checked [open].

### 1.9 When AES switches on (server side)
- **SA:MP:** sets `SET_ENCRYPTION_ON_MULTIPLE_16_BYTE_PACKET` and turns the key
  on when a received datagram's length is a multiple of 8 (a size heuristic).
- **Our server** [ours]: holds the key as pending and turns it on with the
  first datagram that decrypts and passes the checksum. Proof, not a guess.
- **Jumpgate client:** installs its key when ACCEPTED arrives (`0xa6e183`).
  Once a key is set, it drops datagrams that fail to decrypt. [bin]

### 1.10 MTU limits
| | Jumpgate [bin] | SA:MP |
|---|---|---|
| default MTU | 1500 (`0xa69b76`) | 1500 (1264 with `_COMPATIBILITY_1`) |
| clamp / maximum | SetMTUSize clamps 512..1500 (`0xa64122`); the packet reader rejects data ≥ 1500 bytes | `MAXIMUM_MTU_SIZE` 576 |

---

## 2. Identical in both [bin + ref, line-for-line where noted]

- **DataBlockEncryptor** (§2 of raknet-spec): the layout (checksum, randomChar,
  encodedPad, random padding), the padding formula, the manual block chaining
  and the block-0-last decrypt are line for line the same.
- **CheckSum**: seeds 55665 / 52845 / 22719, same `Add`.
- **Rijndael**: AES-128, ECB primitive.
- **RSA**: `big::u256` (32-byte modulus, `e` u32). Both put it on the wire
  little-endian on x86 (SA:MP byte-swaps only for big-endian hosts). This
  matches our live finding.
- **Syn-cookie**: SHA1(ip + port + random), 20 bytes, echoed in 0x07.
- **AES key** = `confirmation[1+j] XOR random[j]`, j = 0..15.
- **Internal packet** (except the reliability width): u16 messageNumber;
  5-bit channel + u16 orderingIndex for the ordered/sequenced types; split
  flag with u16 id + compressed u32 index/count; compressed u16 bit length;
  byte-aligned data.
- **Datagram**: lead `hasAcks` bit, then RangeList acks (compressed u16 count;
  per range `minEqualsMax` bit, u16 min, optional u16 max), else a `false` bit;
  then packets until the bits run out.
- **BitStream**: MSB-first packing; `WriteCompressed` rules.
- **RakNetTime**: 32-bit (`__GET_TIME_64BIT` off).
- **Ping/pong**: `[INTERNAL_PING][u32 time]` (5 B) → `[CONNECTED_PONG][u32 their time][u32 our time]` (9 B).
- **NEW_INCOMING_CONNECTION**: `[id][u32 ip][u16 port]`, 7 B, sent RELIABLE.
- **CONNECTION_REQUEST**: `[id][optional password]`, sent RELIABLE.
- **Duplicate detection**: a sliding window over the 16-bit message number
  (`receivedPacketsBaseIndex` + `hasReceivedPacketQueue`). Assumed the same
  in Jumpgate (stock 2.x) [not verified in the binary].

---

## 3. How complete is our implementation?

**The wire format is complete and verified.** Every byte the client sends or
accepts in the handshake, the crypto, the datagram/packet/ack layout and the
game framing has been proven live. What's missing is the **reliability
machinery**, which matters as soon as packets get lost, arrive out of order,
or a session runs for hours. Roughly in priority order:

| # | gap | effect | reference behaviour |
|---|---|---|---|
| 1 | **Duplicate set never pruned** (`seen_reliable: HashSet<u16>`) | **Bug.** Message numbers wrap after 65,536 packets per connection (all reliabilities count). After that, every reliable message whose number was seen before is acked but **silently dropped**. At ~1.5–4 packets/s per client connection that's roughly 4–12 hours. | sliding window over `receivedPacketsBaseIndex` |
| 2 | **No retransmission** | Incoming acks are parsed and ignored. A lost reliable message is never resent. Fine on loopback/LAN, breaks on real networks. | resend list keyed by message number; resend after ~3× ping (min 30 ms) |
| 3 | **No ordering/sequencing on receive** | RELIABLE_ORDERED messages are delivered in arrival order, not held for gaps. Stale UNRELIABLE_SEQUENCED packets aren't dropped. | per-channel `waitingForOrderedPacketReadIndex` / `…Sequenced…` |
| 4 | **No split packets** | Sending: a message over the ~1,456-byte budget goes out oversize, with a warning. Receiving: split packets aren't reassembled. Captured traffic never exceeds 896 B, so this is unused so far. | `splitPacketChannelList` |
| 5 | **No RTT or timeouts in the session** | We answer pings but never send them, so no RTT for resend timing. Liveness is only the listener's 60 s silence timeout. | periodic `PingInternal`; `timeoutTime` |
| 6 | **Disconnect/full handling** | We now *receive* `0x14`. We never *send* it (kick/shutdown) and never refuse with `0x13` when full. | both |
| 7 | **No congestion control** | We send everything at once. Fine at our traffic levels. | send-rate control (`STARTING_SEND_BPS` …) |
| 8 | **Send reliability is one-size** | `queue_message` always uses RELIABLE_ORDERED channel 0. Relayed positions (0x33/0x34) would be better UNRELIABLE_SEQUENCED, as the client sends them, to avoid head-of-line blocking once #2/#3 exist. | per-message choice |
| 9 | **Static data / RPC** (`0x04`/`0x1C`, `0x08`/`0x0C`) | Not implemented. The client has handlers, but no capture shows them in use. Probably unneeded. | implemented |

**Estimate:** #1 is a small, self-contained fix. #2 + #5 (resend with RTT)
are the main work. #3 and #4 are moderate. All together it's a few hundred
lines in `session.rs`, testable in-process with a lossy/reordering pump like
the existing `connected_pair` tests. Nothing here needs more reverse
engineering except confirming the client's own resend/dedupe constants if we
want to match them exactly.
