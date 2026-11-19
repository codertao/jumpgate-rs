//! A minimal RakNet 2.x connection endpoint (both the client and the server
//! role), enough to drive a real session end-to-end: offline connect →
//! SecuredConnection handshake → AES-encrypted reliability datagrams carrying
//! 0x51-framed game messages. Feature `mitm` (needs the RSA handshake).
//!
//! This is what a mock server (replaying a capture) and a from-scratch client
//! both need.
//!
//! Reliability: every packet's 16-bit message number goes through a wrap-aware
//! duplicate window (as the reference `receivedPacketsBaseIndex`); reliable
//! packets are acked, and reliable packets we send are resent until acked, with a
//! timeout of 3× the smoothed round-trip time (30 ms..3 s, backing off), measured
//! from ack timing. A reliable packet unacked for `ACK_TIMEOUT` marks the
//! connection lost. On receive, RELIABLE_ORDERED packets are held until the
//! gap before them fills, and stale sequenced packets are dropped, per channel.
//! Still missing (docs/raknet.md): split packets
//! (messages stay under the MTU).

use crate::crypto::DataBlockEncryptor;
use crate::game::{self, GameMessage};
use crate::handshake::{self, COOKIE_LEN, RANDOM_LEN};
use crate::ids::{MessageId, Reliability};
use crate::reliability::{AckRange, Datagram, InternalPacket};
use crate::rsa::KeyPair;
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::time::{Duration, Instant};

const NUM_CHANNELS: usize = 32;

/// Client MTU: default 1500, clamped to 512..=1500 by SetMTUSize (`0xa69b76`,
/// `0xa64122`). As in the reference reliability layer, a datagram's plaintext
/// budget is MTU − 28 (IP+UDP), minus 16 more once encryption is on.
const MTU: usize = 1500;
const UDP_HEADER: usize = 28;
const ENCRYPTION_OVERHEAD: usize = 16;

/// Most ordered packets held per channel waiting for a gap to fill. The peer
/// resends the missing one, so this only bounds a misbehaving peer.
const MAX_HELD_PER_CHANNEL: usize = 4096;

/// `idx` is older than `expected` (mod 2^16, within half the range), as the
/// reference `IsOlderOrderedPacket`.
fn is_older(idx: u16, expected: u16) -> bool {
    let behind = expected.wrapping_sub(idx);
    behind != 0 && behind < 0x8000
}

/// Resend timing, after the reference reliability layer (`PING_MULTIPLIER_TO_RESEND`
/// 3.0, `MIN_PING_TO_RESEND` 30 ms). Before the first RTT sample we wait 1 s.
const RTT_MULTIPLIER: f64 = 3.0;
const RTO_MIN: Duration = Duration::from_millis(30);
const RTO_INITIAL: Duration = Duration::from_millis(1000);
const RTO_MAX: Duration = Duration::from_secs(3);
/// A reliable packet still unacked this long after first sending: connection lost.
/// The reference default is 10 s; we allow more so a client stalled on a sector
/// load isn't dropped.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(15);

/// A reliable packet we sent and haven't seen acked.
struct Unacked {
    pkt: InternalPacket,
    /// Sent under AES (false only for handshake packets before the key is live).
    encrypt: bool,
    first_sent: Instant,
    last_sent: Instant,
    sends: u32,
}

fn is_reliable(r: Reliability) -> bool {
    matches!(r, Reliability::Reliable | Reliability::ReliableOrdered | Reliability::ReliableSequenced)
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    Server,
    Client,
}

/// Something that happened as a result of feeding in a datagram.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    /// Handshake finished; the link is encrypted and usable.
    Connected,
    /// A decoded game message arrived (`body[0]` is the message type).
    Message(GameMessage),
    /// The peer sent ID_DISCONNECTION_NOTIFICATION; the session is over.
    Disconnected,
}

struct OutMsg {
    data: Vec<u8>,
    reliability: Reliability,
    channel: u8,
}

pub struct Endpoint {
    role: Role,
    crypto: Option<DataBlockEncryptor>,
    /// Key agreed in the 0x07 exchange but not yet active. Stock 2.x installs it
    /// into the reliability layer only once ACCEPTED is processed (client
    /// `0xa6e183`), so ACCEPTED travels in the clear. The server turns it on
    /// when the first datagram decrypts with it.
    pending_crypto: Option<DataBlockEncryptor>,
    connected: bool,
    /// Peer address echoed in ACCEPTED / NEW_INCOMING_CONNECTION.
    peer: ([u8; 4], u16),

    // Server handshake material.
    keypair: Option<KeyPair>,
    our_cookie: Option<[u8; COOKIE_LEN]>,
    // Client handshake material.
    client_random: Option<[u8; RANDOM_LEN]>,

    // Reliability bookkeeping.
    next_message_number: u16,
    ordering_index: [u16; NUM_CHANNELS],
    /// Sequenced types count separately from ordered ones, per channel, as the
    /// reference's `waitingForSequencedPacketWriteIndex`.
    sequenced_index: [u16; NUM_CHANNELS],
    /// Receive side: next expected ordered index and lowest acceptable
    /// sequenced index per channel, and ordered packets held for a gap.
    ordered_read: [u16; NUM_CHANNELS],
    sequenced_read: [u16; NUM_CHANNELS],
    held: Vec<BTreeMap<u16, Vec<u8>>>,
    acks_pending: Vec<u16>,
    /// Duplicate window: every number below `recv_base` (mod 2^16, within half
    /// the range) has arrived or been given up on. `recv_window[i]` is `None` if
    /// `recv_base + i` has arrived, else the time we stop waiting for it. Holes
    /// expire as in the reference, since lost unreliable packets never come.
    recv_base: u16,
    recv_window: VecDeque<Option<Instant>>,
    /// Reliable packets awaiting an ack, by message number.
    unacked: HashMap<u16, Unacked>,
    /// Smoothed round-trip time (seconds), from acks of packets sent once.
    srtt: Option<f64>,
    lost: bool,
    /// Test hook: a fixed "now" instead of the system clock.
    fake_now: Option<Instant>,

    out_queue: VecDeque<OutMsg>,
    raw_out: Vec<Vec<u8>>, // fully-formed datagrams ready to send
    salt: u8,
    clock_ms: u32,
    /// `RAKNET_TRACE` set: log each received datagram and handshake step to stderr
    /// (lengths, IDs and decisions only — never payload bytes).
    trace: bool,
}

impl Endpoint {
    pub fn client() -> Self {
        Self::new(Role::Client)
    }

    pub fn server() -> Self {
        let mut e = Self::new(Role::Server);
        e.keypair = Some(KeyPair::generate());
        e
    }

    fn new(role: Role) -> Self {
        Endpoint {
            role,
            crypto: None,
            pending_crypto: None,
            connected: false,
            peer: ([0; 4], 0),
            keypair: None,
            our_cookie: None,
            client_random: None,
            next_message_number: 0,
            ordering_index: [0; NUM_CHANNELS],
            sequenced_index: [0; NUM_CHANNELS],
            ordered_read: [0; NUM_CHANNELS],
            sequenced_read: [0; NUM_CHANNELS],
            held: vec![BTreeMap::new(); NUM_CHANNELS],
            acks_pending: Vec::new(),
            recv_base: 0,
            recv_window: VecDeque::new(),
            unacked: HashMap::new(),
            srtt: None,
            lost: false,
            fake_now: None,
            out_queue: VecDeque::new(),
            raw_out: Vec::new(),
            salt: 0,
            clock_ms: 1,
            trace: std::env::var_os("RAKNET_TRACE").is_some(),
        }
    }

    pub fn is_connected(&self) -> bool {
        self.connected
    }

    /// A reliable packet went unacked for [`ACK_TIMEOUT`]: the peer is gone.
    pub fn is_lost(&self) -> bool {
        self.lost
    }

    /// Smoothed round-trip time, once an ack has been timed.
    pub fn rtt(&self) -> Option<Duration> {
        self.srtt.map(Duration::from_secs_f64)
    }

    /// Reliable packets sent and not yet acked.
    pub fn unacked_count(&self) -> usize {
        self.unacked.len()
    }

    /// Test hook: freeze the clock at `t` (resend timers read it).
    #[doc(hidden)]
    pub fn set_time(&mut self, t: Instant) {
        self.fake_now = Some(t);
    }

    fn now(&self) -> Instant {
        self.fake_now.unwrap_or_else(Instant::now)
    }

    /// Resend timeout for a packet already sent `sends` times.
    fn rto(&self, sends: u32) -> Duration {
        let base = match self.srtt {
            Some(rtt) => Duration::from_secs_f64(rtt * RTT_MULTIPLIER).clamp(RTO_MIN, RTO_MAX),
            None => RTO_INITIAL,
        };
        (base * (1u32 << sends.saturating_sub(1).min(4))).min(RTO_MAX * 2)
    }

    /// Record a reliable packet as sent, for resending until acked.
    fn track(&mut self, pkt: &InternalPacket, encrypt: bool) {
        if is_reliable(pkt.reliability) {
            let now = self.now();
            self.unacked.insert(
                pkt.message_number,
                Unacked { pkt: pkt.clone(), encrypt, first_sent: now, last_sent: now, sends: 1 },
            );
        }
    }

    /// The peer acked `min..=max` (wrapping).
    fn on_acks(&mut self, ranges: &[AckRange]) {
        let now = self.now();
        for r in ranges {
            let mut n = r.min;
            loop {
                if let Some(u) = self.unacked.remove(&n) {
                    // Karn: only time packets sent once (no ambiguity about which copy).
                    if u.sends == 1 {
                        let sample = now.duration_since(u.first_sent).as_secs_f64();
                        self.srtt = Some(match self.srtt {
                            Some(s) => s * 0.875 + sample * 0.125,
                            None => sample,
                        });
                    }
                }
                if n == r.max {
                    break;
                }
                n = n.wrapping_add(1);
            }
        }
    }

    /// Duplicate check for message number `n`, marking it received. Wrap-aware:
    /// numbers up to half the range behind the window count as already seen.
    fn is_duplicate(&mut self, n: u16) -> bool {
        let hole = n.wrapping_sub(self.recv_base);
        if hole >= 0x8000 {
            return true;
        }
        let hole = hole as usize;
        let now = self.now();
        if hole < self.recv_window.len() {
            if self.recv_window[hole].is_none() {
                return true;
            }
            self.recv_window[hole] = None;
        } else {
            self.recv_window.resize(hole, Some(now + ACK_TIMEOUT));
            self.recv_window.push_back(None);
        }
        // Advance past arrivals and holes we've stopped waiting for.
        while let Some(&front) = self.recv_window.front() {
            if front.is_some_and(|give_up| give_up > now) {
                break;
            }
            self.recv_window.pop_front();
            self.recv_base = self.recv_base.wrapping_add(1);
        }
        false
    }

    /// A bare offline ID_OPEN_CONNECTION_REQUEST `[0A 00]`: the first datagram of
    /// a new connection attempt. A server that sees this from a peer it thinks
    /// is connected must start over with a fresh endpoint: the client has a new
    /// key and restarts its message numbers.
    pub fn is_open_connection_request(datagram: &[u8]) -> bool {
        datagram.len() <= 2 && datagram.first() == Some(&(MessageId::OpenConnectionRequest as u8))
    }

    /// Set the peer's address, echoed in ACCEPTED (server) or
    /// NEW_INCOMING_CONNECTION (client). Optional: zeros work otherwise.
    pub fn set_peer_addr(&mut self, addr: std::net::SocketAddr) {
        if let std::net::SocketAddr::V4(v4) = addr {
            self.peer = (v4.ip().octets(), v4.port());
        }
    }

    /// `[id][u32 binaryAddress][u16 port]`: address octets in network order,
    /// port little-endian (written as raw native ints, client `0xa6e10a`).
    fn addr_message(&self, id: MessageId) -> Vec<u8> {
        let mut m = vec![id as u8];
        m.extend_from_slice(&self.peer.0);
        m.extend_from_slice(&self.peer.1.to_le_bytes());
        m
    }

    /// Initial datagrams to send (client kicks off the offline connect).
    pub fn start(&mut self) -> Vec<Vec<u8>> {
        if self.role == Role::Client {
            // ID_OPEN_CONNECTION_REQUEST: bare [0A 00], exactly 2 bytes (0xa6d34f).
            self.raw_out.push(vec![MessageId::OpenConnectionRequest as u8, 0]);
        }
        std::mem::take(&mut self.raw_out)
    }

    /// Queue an application game message (default: reliable-ordered, channel 0).
    pub fn queue_message(&mut self, msg: &[u8]) {
        self.queue_message_rel(msg, Reliability::ReliableOrdered, 0);
    }

    pub fn queue_message_rel(&mut self, msg: &[u8], reliability: Reliability, channel: u8) {
        // Frame as the game layer does: server adds the 0x1A timestamp.
        let add_ts = self.role == Role::Server;
        let framed = self.frame_game(msg, add_ts);
        self.out_queue.push_back(OutMsg { data: framed, reliability, channel });
    }

    fn frame_game(&mut self, msg: &[u8], add_ts: bool) -> Vec<u8> {
        let obf = game::obfuscate(msg, self.salt);
        self.salt = (self.salt + 1) % 255;
        let mut out = Vec::new();
        if add_ts {
            out.push(0x1A);
            out.extend_from_slice(&self.clock_ms.to_le_bytes());
            self.clock_ms = self.clock_ms.wrapping_add(1);
        }
        out.push(0x51);
        out.extend_from_slice(&obf);
        out
    }

    // ---- outgoing datagram helpers ----

    fn take_message_number(&mut self) -> u16 {
        let n = self.next_message_number;
        self.next_message_number = self.next_message_number.wrapping_add(1);
        n
    }

    fn make_packet(&mut self, data: Vec<u8>, reliability: Reliability, channel: u8) -> InternalPacket {
        let message_number = self.take_message_number();
        let counter = match reliability {
            Reliability::UnreliableSequenced | Reliability::ReliableSequenced => {
                Some(&mut self.sequenced_index[channel as usize])
            }
            Reliability::ReliableOrdered => Some(&mut self.ordering_index[channel as usize]),
            _ => None,
        };
        let ordering_index = counter.map_or(0, |c| {
            let i = *c;
            *c = i.wrapping_add(1);
            i
        });
        InternalPacket {
            message_number,
            reliability,
            ordering_channel: channel,
            ordering_index,
            split: None,
            data,
        }
    }

    /// Emit a single transport message (handshake) as its own datagram, carrying
    /// any pending acks along with it so they share its encryption state.
    fn emit_transport(&mut self, data: Vec<u8>, encrypt: bool) {
        let pkt = self.make_packet(data, Reliability::Reliable, 0);
        self.track(&pkt, encrypt);
        let mut pending = std::mem::take(&mut self.acks_pending);
        let acks = Self::ack_ranges(&mut pending);
        let dg = Datagram { acks, packets: vec![pkt] };
        let bytes = dg.serialize();
        self.raw_out.push(self.maybe_encrypt(bytes, encrypt));
    }

    fn maybe_encrypt(&self, bytes: Vec<u8>, encrypt: bool) -> Vec<u8> {
        match (encrypt, &self.crypto) {
            (true, Some(c)) => c.encrypt(&bytes),
            _ => bytes,
        }
    }

    fn ack_ranges(nums: &mut Vec<u16>) -> Vec<AckRange> {
        nums.sort_unstable();
        nums.dedup();
        let mut ranges = Vec::new();
        let mut iter = nums.iter().copied();
        if let Some(first) = iter.next() {
            let (mut min, mut max) = (first, first);
            for n in iter {
                if n == max.wrapping_add(1) {
                    max = n;
                } else {
                    ranges.push(AckRange { min, max });
                    min = n;
                    max = n;
                }
            }
            ranges.push(AckRange { min, max });
        }
        ranges
    }

    /// Produce datagrams to send now: handshake replies, resends that are due,
    /// and (once connected) a datagram carrying pending acks and queued messages.
    /// Call it regularly even with nothing queued: resend timers run from here.
    pub fn flush(&mut self) -> Vec<Vec<u8>> {
        let mut out = std::mem::take(&mut self.raw_out);
        let now = self.now();

        // Handshake packets sent in the clear are moot once the key is live:
        // the peer only switches to AES after receiving them.
        if self.crypto.is_some() {
            self.unacked.retain(|_, u| u.encrypt);
        }
        if self.unacked.values().any(|u| now.duration_since(u.first_sent) >= ACK_TIMEOUT) {
            if !self.lost {
                self.tr("reliable packet unacked past ACK_TIMEOUT -> connection lost");
            }
            self.lost = true;
        }

        // Resends that are due, oldest first.
        let mut due: Vec<(Instant, u16)> = self
            .unacked
            .iter()
            .filter(|(_, u)| !self.lost && now.duration_since(u.last_sent) >= self.rto(u.sends))
            .map(|(n, u)| (u.first_sent, *n))
            .collect();
        due.sort();
        let mut resend = Vec::new();
        for (_, n) in due {
            let u = self.unacked.get_mut(&n).unwrap();
            u.last_sent = now;
            u.sends += 1;
            if u.encrypt {
                resend.push(u.pkt.clone());
            } else {
                // Pre-key handshake packet: resend alone, in the clear.
                let dg = Datagram { acks: vec![], packets: vec![u.pkt.clone()] };
                out.push(dg.serialize());
            }
        }
        if self.trace && !resend.is_empty() {
            eprintln!("[raknet:{:?}] resending {} packet(s)", self.role, resend.len());
        }

        // New messages wait for the connection; acks and resends don't.
        let mut packets = resend;
        while self.connected {
            let Some(m) = self.out_queue.pop_front() else { break };
            let pkt = self.make_packet(m.data, m.reliability, m.channel);
            self.track(&pkt, true);
            packets.push(pkt);
        }
        let mut acks_pending = std::mem::take(&mut self.acks_pending);
        let acks = Self::ack_ranges(&mut acks_pending);
        if acks.is_empty() && packets.is_empty() {
            return out;
        }
        if self.trace && !self.connected {
            eprintln!("[raknet:{:?}] -> ack-only datagram ({} range(s))", self.role, acks.len());
        }
        let budget = MTU - UDP_HEADER - if self.crypto.is_some() { ENCRYPTION_OVERHEAD } else { 0 };

        // Pack into as few datagrams as fit the budget; acks ride in the first.
        let mut dg = Datagram { acks, packets: Vec::new() };
        for pkt in packets {
            dg.packets.push(pkt);
            if dg.serialize().len() <= budget {
                continue;
            }
            if dg.packets.len() == 1 && dg.acks.is_empty() {
                // No split-packet support yet; real server messages stay <= 896 B.
                eprintln!(
                    "[raknet:{:?}] warning: {}-byte packet exceeds the {budget}-byte budget (no split support)",
                    self.role,
                    dg.packets[0].data.len()
                );
                continue;
            }
            // Over budget: ship what fit and start the next datagram with this packet.
            let pkt = dg.packets.pop().unwrap();
            let full = std::mem::replace(&mut dg, Datagram { acks: vec![], packets: vec![pkt] });
            out.push(self.maybe_encrypt(full.serialize(), true));
        }
        if !dg.acks.is_empty() || !dg.packets.is_empty() {
            out.push(self.maybe_encrypt(dg.serialize(), true));
        }
        out
    }

    // ---- incoming ----

    /// Feed one received UDP payload; returns resulting events. Call [`flush`]
    /// afterwards to collect datagrams to send.
    pub fn recv(&mut self, datagram: &[u8]) -> Vec<Event> {
        if self.trace {
            eprintln!(
                "[raknet:{:?}] <- {}B first=0x{:02x} key={}",
                self.role,
                datagram.len(),
                datagram.first().copied().unwrap_or(0),
                self.crypto.is_some()
            );
        }
        // Bare offline messages (not reliability-wrapped, never encrypted). They
        // are only ever 2 bytes, so longer datagrams starting with these bytes
        // fall through to the reliability parser.
        let offline = datagram.len() <= 2;
        match datagram.first().copied().and_then(MessageId::from_u8) {
            Some(MessageId::OpenConnectionRequest) if offline && self.role == Role::Server => {
                self.tr("offline OPEN_CONNECTION_REQUEST -> OPEN_CONNECTION_REPLY");
                self.raw_out.push(vec![MessageId::OpenConnectionReply as u8, 0]);
                return vec![];
            }
            Some(MessageId::OpenConnectionReply) if offline && self.role == Role::Client => {
                self.tr("offline OPEN_CONNECTION_REPLY -> CONNECTION_REQUEST");
                // Begin the secured handshake with a CONNECTION_REQUEST.
                self.emit_transport(vec![MessageId::ConnectionRequest as u8], false);
                return vec![];
            }
            _ => {}
        }

        let mut events = Vec::new();

        // Decrypt if a key is active (fall back to raw for late pre-key frames).
        // A server holding a pending key activates it on the first datagram
        // that decrypts — proof the client accepted our RSA key.
        let plain = if let Some(c) = &self.crypto {
            c.decrypt(datagram).unwrap_or_else(|_| datagram.to_vec())
        } else if let Some(Ok(pl)) = self.pending_crypto.as_ref().map(|p| p.decrypt(datagram)) {
            self.crypto = self.pending_crypto.take();
            self.tr("first encrypted datagram decrypted -> key active, connected");
            if !self.connected {
                self.connected = true;
                events.push(Event::Connected);
            }
            pl
        } else {
            datagram.to_vec()
        };

        let dg = match Datagram::parse(&plain) {
            Ok(dg) => dg,
            Err(e) => {
                if self.trace {
                    eprintln!("[raknet:{:?}] datagram parse failed: {e:?}", self.role);
                }
                return vec![];
            }
        };
        if self.trace {
            eprintln!(
                "[raknet:{:?}]    {} ack range(s), {} packet(s): {}",
                self.role,
                dg.acks.len(),
                dg.packets.len(),
                dg.packets
                    .iter()
                    .map(|p| format!(
                        "#{} {:?} id=0x{:02x} {}B",
                        p.message_number,
                        p.reliability,
                        p.data.first().copied().unwrap_or(0),
                        p.data.len()
                    ))
                    .collect::<Vec<_>>()
                    .join(", ")
            );
        }

        self.on_acks(&dg.acks);
        for pkt in dg.packets {
            // Ack every reliable packet, handshake and duplicates included, as the
            // stock reliability layer does: otherwise the peer keeps resending.
            if is_reliable(pkt.reliability) {
                self.acks_pending.push(pkt.message_number);
            }
            // Every packet carries a message number, so all are de-duplicated.
            if self.is_duplicate(pkt.message_number) {
                continue;
            }
            for data in self.release_in_order(pkt) {
                self.handle_packet(data, &mut events);
            }
        }
        events
    }

    /// Apply ordering and sequencing (reference `ReliabilityLayer`): returns the
    /// payloads that may be handled now, in order.
    fn release_in_order(&mut self, pkt: InternalPacket) -> Vec<Vec<u8>> {
        let ch = pkt.ordering_channel as usize;
        let idx = pkt.ordering_index;
        match pkt.reliability {
            Reliability::UnreliableSequenced | Reliability::ReliableSequenced => {
                if is_older(idx, self.sequenced_read[ch]) {
                    return vec![]; // superseded by a newer one already handled
                }
                self.sequenced_read[ch] = idx.wrapping_add(1);
                vec![pkt.data]
            }
            Reliability::ReliableOrdered => {
                let expected = self.ordered_read[ch];
                if idx != expected {
                    if !is_older(idx, expected) {
                        let held = &mut self.held[ch];
                        if held.len() < MAX_HELD_PER_CHANNEL {
                            held.insert(idx.wrapping_sub(expected), pkt.data);
                        } else if self.trace {
                            eprintln!("[raknet:{:?}] channel {ch}: hold queue full, dropping", self.role);
                        }
                    }
                    return vec![];
                }
                // Deliver this one plus any held packets that now follow on.
                // Held keys are offsets from `expected`, so they sort correctly
                // across the u16 wrap; rebase them after releasing.
                let mut out = vec![pkt.data];
                let mut next = 1u16;
                while let Some(d) = self.held[ch].remove(&next) {
                    out.push(d);
                    next += 1;
                }
                self.ordered_read[ch] = expected.wrapping_add(next);
                if !self.held[ch].is_empty() {
                    let old = std::mem::take(&mut self.held[ch]);
                    self.held[ch] = old.into_iter().map(|(k, v)| (k - next, v)).collect();
                }
                out
            }
            _ => vec![pkt.data],
        }
    }

    fn handle_packet(&mut self, data: Vec<u8>, events: &mut Vec<Event>) {
        match data.first().copied() {
            Some(b) if b == MessageId::ConnectionRequest as u8 && self.role == Role::Server => {
                self.server_send_response();
            }
            Some(b) if b == MessageId::SecuredConnectionResponse as u8 && self.role == Role::Client => {
                self.client_handle_response(&data, events);
            }
            Some(b) if b == MessageId::SecuredConnectionConfirmation as u8 && self.role == Role::Server => {
                self.server_handle_confirmation(&data);
            }
            Some(b) if b == MessageId::ConnectionRequestAccepted as u8 && self.role == Role::Client => {
                // Client accepts only the exact 9-byte form (0xa6e0ad).
                if data.len() != 9 || self.connected {
                    self.tr("ACCEPTED ignored (not 9 bytes, or already connected)");
                    return;
                }
                if let Some(k) = self.pending_crypto.take() {
                    self.crypto = Some(k);
                }
                self.tr("ACCEPTED -> key active, NEW_INCOMING_CONNECTION (encrypted)");
                let nic = self.addr_message(MessageId::NewIncomingConnection);
                self.emit_transport(nic, true);
                self.connected = true;
                events.push(Event::Connected);
            }
            Some(b) if b == MessageId::DisconnectionNotification as u8 => {
                self.tr("DISCONNECTION_NOTIFICATION");
                if self.connected {
                    self.connected = false;
                    events.push(Event::Disconnected);
                }
            }
            Some(b) if b == MessageId::NewIncomingConnection as u8 => {
                self.tr("NEW_INCOMING_CONNECTION");
            }
            Some(0x00) if data.len() >= 5 => {
                // ID_INTERNAL_PING [00][u32 sendTime] -> ID_CONNECTED_PONG
                // [03][u32 sendTime][u32 ourTime], unreliable.
                let mut pong = vec![MessageId::ConnectedPong as u8];
                pong.extend_from_slice(&data[1..5]);
                pong.extend_from_slice(&self.clock_ms.to_le_bytes());
                self.out_queue.push_back(OutMsg {
                    data: pong,
                    reliability: Reliability::Unreliable,
                    channel: 0,
                });
            }
            _ => {
                // A 0x51-framed game message (or an unknown transport id).
                if let Some(msg) = game::decode_payload(&data) {
                    events.push(Event::Message(msg));
                }
            }
        }
    }

    fn tr(&self, what: &str) {
        if self.trace {
            eprintln!("[raknet:{:?}]    {what}", self.role);
        }
    }

    fn server_send_response(&mut self) {
        self.tr("CONNECTION_REQUEST -> SECURED_CONNECTION_RESPONSE (0x06)");
        let cookie: [u8; COOKIE_LEN] = handshake::random_bytes();
        self.our_cookie = Some(cookie);
        let (e, n) = self.keypair.as_ref().unwrap().public_wire();
        let resp = handshake::build_response(&cookie, &e, &n);
        self.emit_transport(resp, false); // pre-key: unencrypted
    }

    fn client_handle_response(&mut self, data: &[u8], _events: &mut [Event]) {
        let Some(resp) = handshake::parse_response(data) else {
            self.tr("0x06 did not parse (expected 57 bytes)");
            return;
        };
        self.tr("SECURED_CONNECTION_RESPONSE -> CONFIRMATION (0x07), key set");
        let random: [u8; RANDOM_LEN] = handshake::random_bytes();
        self.client_random = Some(random);
        let (conf, key) =
            handshake::build_confirmation(&resp.cookie, &resp.e_wire, &resp.n_wire, &random);
        // 0x07 is unencrypted; the key goes live when ACCEPTED arrives.
        self.emit_transport(conf, false);
        self.pending_crypto = Some(DataBlockEncryptor::new(&key));
    }

    fn server_handle_confirmation(&mut self, data: &[u8]) {
        let (Some(conf), Some(cookie)) =
            (handshake::parse_confirmation(data), self.our_cookie)
        else {
            self.tr("0x07 did not parse, or no cookie issued yet");
            return;
        };
        let kp = self.keypair.as_ref().unwrap();
        let Some(key) = handshake::derive_key_from_confirmation(&cookie, kp, &conf) else {
            eprintln!("[session] cookie mismatch on 0x07");
            return;
        };
        self.tr("SECURED_CONNECTION_CONFIRMATION ok -> CONNECTION_REQUEST_ACCEPTED (clear), key pending");
        self.pending_crypto = Some(DataBlockEncryptor::new(&key));
        // ACCEPTED (9 bytes) goes in the clear; see `pending_crypto`.
        let mut accepted = self.addr_message(MessageId::ConnectionRequestAccepted);
        accepted.extend_from_slice(&0u16.to_le_bytes()); // playerIndex
        self.emit_transport(accepted, false);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pump datagrams between a fresh server and client until both connect.
    fn connected_pair() -> (Endpoint, Endpoint, Vec<Event>) {
        let mut server = Endpoint::server();
        let mut client = Endpoint::client();
        let mut to_server = client.start();
        let mut to_client: Vec<Vec<u8>> = Vec::new();
        let mut client_events = Vec::new();
        for _ in 0..20 {
            for dg in to_client.drain(..) {
                client_events.extend(client.recv(&dg));
            }
            for dg in to_server.drain(..) {
                server.recv(&dg);
            }
            to_client.extend(server.flush());
            to_server.extend(client.flush());
            if client.is_connected() && server.is_connected()
                && to_client.is_empty() && to_server.is_empty()
            {
                break;
            }
        }
        assert!(client.is_connected(), "client should connect");
        assert!(server.is_connected(), "server should connect");
        (server, client, client_events)
    }

    /// A burst of large messages is spread over datagrams that fit the MTU,
    /// and all arrive intact and in order.
    #[test]
    fn flush_respects_mtu() {
        let (mut server, mut client, _) = connected_pair();
        let script: Vec<Vec<u8>> = (0..20u8)
            .map(|i| {
                let mut m = vec![0x0d, i];
                m.resize(896, i);
                m
            })
            .collect();
        for m in &script {
            server.queue_message(m);
        }
        let dgs = server.flush();
        assert!(dgs.len() > 1, "burst should span several datagrams");
        let mut got = Vec::new();
        for dg in &dgs {
            assert!(dg.len() <= MTU - UDP_HEADER, "datagram of {} bytes exceeds MTU", dg.len());
            for ev in client.recv(dg) {
                if let Event::Message(m) = ev {
                    got.push(m.body);
                }
            }
        }
        assert_eq!(got, script);
    }

    /// Drive a full client↔server session in-process (no sockets) and verify a
    /// scripted set of server→client messages arrives decoded intact.
    #[test]
    fn loopback_handshake_and_replay() {
        let (mut server, mut client, client_events) = connected_pair();
        assert!(client_events.contains(&Event::Connected));

        // Server replays some captured game messages.
        let script: Vec<Vec<u8>> = vec![
            vec![0x8F],                   // welcome
            vec![0x45, 0x01, 0x02, 0x03], // rank
            vec![0x33, 0xAA, 0xBB],       // smallloc
        ];
        for m in &script {
            server.queue_message(m);
        }
        let dgs = server.flush();
        let mut got = Vec::new();
        for dg in dgs {
            for ev in client.recv(&dg) {
                if let Event::Message(m) = ev {
                    got.push(m.body);
                }
            }
        }
        assert_eq!(got, script, "replayed messages must arrive intact");
    }

    /// A peer that disconnects and reconnects (same address) brings a new key
    /// and restarts its message numbers, so the old server endpoint can't carry
    /// the new session. This is why the server's listener replaces the endpoint
    /// on a fresh ID_OPEN_CONNECTION_REQUEST.
    #[test]
    fn reconnect_needs_a_fresh_server_endpoint() {
        let (mut server, _old_client, _) = connected_pair();
        let mut client = Endpoint::client();
        let first = client.start();
        assert!(first.iter().all(|d| Endpoint::is_open_connection_request(d)));
        let mut to_server = first;
        for _ in 0..20 {
            let mut to_client = Vec::new();
            for dg in to_server.drain(..) {
                server.recv(&dg);
            }
            to_client.extend(server.flush());
            for dg in to_client {
                client.recv(&dg);
            }
            to_server.extend(client.flush());
        }
        server.queue_message(&[0x8f]);
        let got: Vec<Event> = server.flush().iter().flat_map(|d| client.recv(d)).collect();
        assert!(
            !got.iter().any(|e| matches!(e, Event::Message(_))),
            "stale endpoint unexpectedly carried the new session"
        );
    }

    #[test]
    fn disconnection_notification_ends_the_session() {
        let (mut server, mut client, _) = connected_pair();
        client.emit_transport(vec![MessageId::DisconnectionNotification as u8], true);
        let evs: Vec<Event> = client.flush().iter().flat_map(|d| server.recv(d)).collect();
        assert!(evs.contains(&Event::Disconnected));
        assert!(!server.is_connected());
    }

    fn messages(evs: Vec<Event>) -> Vec<Vec<u8>> {
        evs.into_iter()
            .filter_map(|e| if let Event::Message(m) = e { Some(m.body) } else { None })
            .collect()
    }

    /// The window must survive many wraps of the 16-bit message number and
    /// still reject recent repeats (the old HashSet dropped everything after
    /// the first wrap).
    #[test]
    fn duplicate_window_survives_message_number_wrap() {
        let mut e = Endpoint::client();
        for i in 0u32..200_000 {
            let n = i as u16;
            assert!(!e.is_duplicate(n), "fresh #{i} flagged as duplicate");
            assert!(e.is_duplicate(n), "repeat #{i} not caught");
        }
        // Out of order within the window: a gap, then the gap filled.
        let base = e.recv_base;
        assert!(!e.is_duplicate(base.wrapping_add(5)));
        assert!(!e.is_duplicate(base.wrapping_add(2)));
        assert!(!e.is_duplicate(base));
        assert!(e.is_duplicate(base.wrapping_add(5)));
        assert!(e.is_duplicate(base.wrapping_sub(1000)), "far behind = old");
    }

    /// Lost unreliable packets leave holes that never fill. They must expire,
    /// or the window stops advancing and, 32768 numbers later, every new
    /// packet looks like a duplicate.
    #[test]
    fn duplicate_window_gives_up_on_holes() {
        let mut e = Endpoint::client();
        let mut t = Instant::now();
        e.set_time(t);
        let mut n = 0u16;
        for _ in 0..100_000u32 {
            n = n.wrapping_add(2); // every other packet lost
            t += Duration::from_millis(1);
            e.set_time(t);
            assert!(!e.is_duplicate(n), "fresh {n} flagged");
        }
        // Only the last ACK_TIMEOUT of holes is kept (2 numbers per ms here).
        let bound = 2 * ACK_TIMEOUT.as_millis() as usize + 2;
        assert!(e.recv_window.len() <= bound, "window grew to {}", e.recv_window.len());
        // A hole filled before it expires is still accepted once.
        assert!(!e.is_duplicate(n.wrapping_sub(1)));
        assert!(e.is_duplicate(n.wrapping_sub(1)));
    }

    #[test]
    fn sequenced_and_ordered_count_separately() {
        let mut e = Endpoint::client();
        let a = e.make_packet(vec![1], Reliability::ReliableOrdered, 0);
        let b = e.make_packet(vec![2], Reliability::UnreliableSequenced, 0);
        let c = e.make_packet(vec![3], Reliability::UnreliableSequenced, 0);
        let d = e.make_packet(vec![4], Reliability::ReliableOrdered, 0);
        assert_eq!(
            [a.ordering_index, b.ordering_index, c.ordering_index, d.ordering_index],
            [0, 0, 1, 1]
        );
        assert_eq!([a.message_number, b.message_number, c.message_number, d.message_number], [0, 1, 2, 3]);
    }

    #[test]
    fn lost_datagram_is_resent_and_rtt_is_measured() {
        let (mut server, mut client, _) = connected_pair();
        let t0 = Instant::now();
        server.set_time(t0);
        client.set_time(t0);
        // Settle handshake acks so only our message is outstanding.
        for _ in 0..3 {
            for d in client.flush() {
                server.recv(&d);
            }
            for d in server.flush() {
                client.recv(&d);
            }
        }
        server.unacked.clear();
        server.srtt = None; // the handshake was timed at ~0 ms; start from the initial RTO

        server.queue_message(&[0x8f, 1]);
        let lost = server.flush();
        assert_eq!(lost.len(), 1);
        assert_eq!(server.unacked_count(), 1);
        // Dropped. Nothing is due before the initial timeout...
        server.set_time(t0 + Duration::from_millis(500));
        assert!(server.flush().is_empty());
        // ...and after it the same packet goes again.
        server.set_time(t0 + Duration::from_millis(1100));
        let again = server.flush();
        assert_eq!(again.len(), 1);
        let got: Vec<Vec<u8>> = again.iter().flat_map(|d| messages(client.recv(d))).collect();
        assert_eq!(got, vec![vec![0x8f, 1]]);
        // A second copy arriving late is suppressed.
        let got: Vec<Vec<u8>> = lost.iter().flat_map(|d| messages(client.recv(d))).collect();
        assert!(got.is_empty(), "duplicate delivered");
        for d in client.flush() {
            server.recv(&d);
        }
        assert_eq!(server.unacked_count(), 0);
        assert_eq!(server.rtt(), None, "resent packets aren't timed (Karn)");

        // A packet acked after 40 ms gives an RTT sample and a 120 ms timeout.
        let t1 = t0 + Duration::from_secs(2);
        server.set_time(t1);
        server.queue_message(&[0x8f, 2]);
        let d = server.flush();
        client.set_time(t1 + Duration::from_millis(40));
        server.set_time(t1 + Duration::from_millis(40));
        for x in &d {
            client.recv(x);
        }
        for x in client.flush() {
            server.recv(&x);
        }
        let rtt = server.rtt().unwrap();
        assert!((rtt.as_secs_f64() - 0.040).abs() < 1e-6, "{rtt:?}");
        assert_eq!(server.rto(1), Duration::from_millis(120));
        assert_eq!(server.rto(2), Duration::from_millis(240), "backs off");
    }

    #[test]
    fn unacked_past_timeout_means_connection_lost() {
        let (mut server, _client, _) = connected_pair();
        let t0 = Instant::now();
        server.set_time(t0);
        server.unacked.clear();
        server.queue_message(&[0x8f]);
        server.flush();
        server.set_time(t0 + ACK_TIMEOUT - Duration::from_millis(1));
        server.flush();
        assert!(!server.is_lost());
        server.set_time(t0 + ACK_TIMEOUT);
        server.flush();
        assert!(server.is_lost());
    }

    /// 30% loss both ways, acks included: every message still arrives exactly once.
    #[test]
    fn lossy_link_delivers_everything_once() {
        let (mut server, mut client, _) = connected_pair();
        let mut seed = 0x2545F491u32;
        let mut drop = || {
            seed ^= seed << 13;
            seed ^= seed >> 17;
            seed ^= seed << 5;
            seed % 10 < 3
        };
        let mut t = Instant::now();
        let sent: Vec<Vec<u8>> = (0..300u16).map(|i| vec![0x8f, (i >> 8) as u8, i as u8]).collect();
        for m in &sent {
            server.queue_message(m);
        }
        let mut got = Vec::new();
        for _ in 0..400 {
            t += Duration::from_millis(50);
            server.set_time(t);
            client.set_time(t);
            for d in server.flush() {
                if !drop() {
                    got.extend(messages(client.recv(&d)));
                }
            }
            for d in client.flush() {
                if !drop() {
                    server.recv(&d);
                }
            }
            if server.unacked_count() == 0 && got.len() == sent.len() {
                break;
            }
        }
        assert!(!server.is_lost());
        assert_eq!(server.unacked_count(), 0, "everything acked eventually");
        assert_eq!(got, sent, "every message exactly once, in order");
    }

    fn pkt(n: u16, rel: Reliability, idx: u16, data: u8) -> InternalPacket {
        InternalPacket {
            message_number: n,
            reliability: rel,
            ordering_channel: 0,
            ordering_index: idx,
            split: None,
            data: vec![data],
        }
    }

    #[test]
    fn ordered_packets_are_held_until_the_gap_fills() {
        let mut e = Endpoint::client();
        let ro = Reliability::ReliableOrdered;
        e.ordered_read[0] = 65534; // across the wrap
        assert!(e.release_in_order(pkt(0, ro, 0, 3)).is_empty());
        assert!(e.release_in_order(pkt(1, ro, 2, 5)).is_empty());
        assert!(e.release_in_order(pkt(2, ro, 65535, 2)).is_empty());
        assert_eq!(e.release_in_order(pkt(3, ro, 65534, 1)), vec![vec![1], vec![2], vec![3]]);
        assert_eq!(e.ordered_read[0], 1);
        assert_eq!(e.release_in_order(pkt(4, ro, 1, 4)), vec![vec![4], vec![5]]);
        assert_eq!(e.ordered_read[0], 3);
        assert!(e.held[0].is_empty());
        // An old ordered index is dropped, not held forever.
        assert!(e.release_in_order(pkt(5, ro, 0, 9)).is_empty());
        assert!(e.held[0].is_empty());
    }

    #[test]
    fn stale_sequenced_packets_are_dropped_without_blocking_ordered() {
        let mut e = Endpoint::client();
        let us = Reliability::UnreliableSequenced;
        let ro = Reliability::ReliableOrdered;
        assert_eq!(e.release_in_order(pkt(0, us, 5, 1)), vec![vec![1]]);
        assert!(e.release_in_order(pkt(1, us, 3, 2)).is_empty(), "older than 5");
        assert_eq!(e.release_in_order(pkt(2, us, 9, 3)), vec![vec![3]], "gaps are fine");
        // Same channel, separate counter: ordered index 0 is still expected.
        assert_eq!(e.release_in_order(pkt(3, ro, 0, 4)), vec![vec![4]]);
    }
}
