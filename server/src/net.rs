//! One UDP port, many clients: a RakNet server [`Endpoint`] per peer address.

use crate::proto;
use raknet::Reliability;
use raknet::session::{Endpoint, Event};
use std::collections::HashMap;
use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::time::{Duration, Instant};

/// Drop a peer after this long with no datagrams (the client keepalives every ~16 s).
pub const PEER_TIMEOUT: Duration = Duration::from_secs(60);

pub enum NetEvent {
    Connected(SocketAddr),
    Message(SocketAddr, Vec<u8>),
    Disconnected(SocketAddr),
}

struct Peer {
    endpoint: Endpoint,
    last_heard: Instant,
}

pub struct Listener {
    pub tag: &'static str,
    sock: UdpSocket,
    peers: HashMap<SocketAddr, Peer>,
}

impl Listener {
    pub fn bind(tag: &'static str, addr: SocketAddr) -> io::Result<Self> {
        let sock = UdpSocket::bind(addr)?;
        sock.set_nonblocking(true)?;
        eprintln!("[{tag}] listening on {addr}");
        Ok(Listener { tag, sock, peers: HashMap::new() })
    }

    /// Drain the socket; returns what happened. Handshake replies go out here.
    pub fn poll(&mut self) -> Vec<NetEvent> {
        let mut events = Vec::new();
        let mut buf = [0u8; 2048];
        loop {
            match self.sock.recv_from(&mut buf) {
                Ok((n, from)) => {
                    // A new connection attempt from a peer we think is connected
                    // (the client dropped space on dock and is relaunching): the
                    // old session is dead, so start over with a fresh endpoint.
                    let fresh = Endpoint::is_open_connection_request(&buf[..n]);
                    if fresh && self.peers.get(&from).is_some_and(|p| p.endpoint.is_connected()) {
                        eprintln!("[{}] {from} reconnecting; dropping old session", self.tag);
                        self.peers.remove(&from);
                        events.push(NetEvent::Disconnected(from));
                    }
                    let peer = self.peers.entry(from).or_insert_with(|| {
                        eprintln!("[{}] new peer {from}", self.tag);
                        let mut endpoint = Endpoint::server();
                        endpoint.set_peer_addr(from);
                        Peer { endpoint, last_heard: Instant::now() }
                    });
                    peer.last_heard = Instant::now();
                    let mut closed = false;
                    for ev in peer.endpoint.recv(&buf[..n]) {
                        match ev {
                            Event::Connected => events.push(NetEvent::Connected(from)),
                            Event::Message(m) => events.push(NetEvent::Message(from, m.body)),
                            Event::Disconnected => closed = true,
                        }
                    }
                    let out = peer.endpoint.flush();
                    Self::send_raw(&self.sock, self.tag, from, out);
                    if closed {
                        eprintln!("[{}] {from} disconnected", self.tag);
                        self.peers.remove(&from);
                        events.push(NetEvent::Disconnected(from));
                    }
                }
                Err(e) if matches!(e.kind(), io::ErrorKind::WouldBlock | io::ErrorKind::TimedOut) => break,
                // Windows reports ICMP port-unreachable as a recv error; ignore.
                Err(e) if e.kind() == io::ErrorKind::ConnectionReset => continue,
                Err(e) => {
                    eprintln!("[{}] recv error: {e}", self.tag);
                    break;
                }
            }
        }
        let now = Instant::now();
        let stale: Vec<SocketAddr> = self
            .peers
            .iter()
            .filter(|(_, p)| now.duration_since(p.last_heard) > PEER_TIMEOUT || p.endpoint.is_lost())
            .map(|(a, _)| *a)
            .collect();
        for a in stale {
            let why = if self.peers[&a].endpoint.is_lost() { "stopped acking" } else { "timed out" };
            eprintln!("[{}] peer {a} {why}", self.tag);
            self.peers.remove(&a);
            events.push(NetEvent::Disconnected(a));
        }
        events
    }

    pub fn queue(&mut self, to: SocketAddr, msg: &[u8]) -> bool {
        match self.peers.get_mut(&to) {
            Some(p) if p.endpoint.is_connected() => {
                if proto::is_position_update(msg) {
                    // As the client sends them: a lost one is superseded by the
                    // next, so it mustn't hold up ordered traffic behind it.
                    p.endpoint.queue_message_rel(msg, Reliability::UnreliableSequenced, 0);
                } else {
                    p.endpoint.queue_message(msg);
                }
                true
            }
            _ => false,
        }
    }

    pub fn flush(&mut self) {
        for (addr, p) in self.peers.iter_mut() {
            let out = p.endpoint.flush();
            Self::send_raw(&self.sock, self.tag, *addr, out);
        }
    }

    fn send_raw(sock: &UdpSocket, tag: &str, to: SocketAddr, datagrams: Vec<Vec<u8>>) {
        for dg in datagrams {
            if let Err(e) = sock.send_to(&dg, to) {
                eprintln!("[{tag}] send to {to} failed: {e}");
            }
        }
    }
}
