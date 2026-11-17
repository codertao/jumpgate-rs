//! One UDP port, many clients: a RakNet server [`Endpoint`] per peer address.

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
                    let peer = self.peers.entry(from).or_insert_with(|| {
                        eprintln!("[{}] new peer {from}", self.tag);
                        let mut endpoint = Endpoint::server();
                        endpoint.set_peer_addr(from);
                        Peer { endpoint, last_heard: Instant::now() }
                    });
                    peer.last_heard = Instant::now();
                    for ev in peer.endpoint.recv(&buf[..n]) {
                        match ev {
                            Event::Connected => events.push(NetEvent::Connected(from)),
                            Event::Message(m) => events.push(NetEvent::Message(from, m.body)),
                        }
                    }
                    let out = peer.endpoint.flush();
                    Self::send_raw(&self.sock, self.tag, from, out);
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
            .filter(|(_, p)| now.duration_since(p.last_heard) > PEER_TIMEOUT)
            .map(|(a, _)| *a)
            .collect();
        for a in stale {
            eprintln!("[{}] peer {a} timed out", self.tag);
            self.peers.remove(&a);
            events.push(NetEvent::Disconnected(a));
        }
        events
    }

    pub fn queue(&mut self, to: SocketAddr, msg: &[u8]) -> bool {
        match self.peers.get_mut(&to) {
            Some(p) if p.endpoint.is_connected() => {
                p.endpoint.queue_message(msg);
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
