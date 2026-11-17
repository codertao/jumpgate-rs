//! Starts the real `jumpgate-server` binary on spare ports and logs in over UDP
//! with the library's RakNet client: handshake → AES → login → 0x06 + MOTD.

use raknet::session::{Endpoint, Event};
use std::net::UdpSocket;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

struct Kill(Child);
impl Drop for Kill {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

#[test]
fn login_over_udp() {
    let state_port = 25150 + (std::process::id() % 1000) as u16;
    let space_port = state_port + 1000;
    let _srv = Kill(
        Command::new(env!("CARGO_BIN_EXE_jumpgate-server"))
            .args(["--listen", "127.0.0.1"])
            .args(["--state-port", &state_port.to_string()])
            .args(["--space-port", &space_port.to_string()])
            .stderr(Stdio::null())
            .spawn()
            .expect("start server"),
    );
    std::thread::sleep(Duration::from_millis(300));

    let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
    sock.set_read_timeout(Some(Duration::from_millis(20))).unwrap();
    sock.connect(("127.0.0.1", state_port)).unwrap();
    let mut client = Endpoint::client();
    for dg in client.start() {
        sock.send(&dg).unwrap();
    }

    let mut login = vec![0u8; 33];
    login[0] = 0x05;
    login[1..7].copy_from_slice(b"tester");
    login[0x16..0x1c].copy_from_slice(b"2.0127");

    let mut sent_login = false;
    let mut got: Vec<u8> = Vec::new();
    let mut buf = [0u8; 2048];
    let deadline = Instant::now() + Duration::from_secs(10);
    while Instant::now() < deadline && !(got.contains(&0x06) && got.contains(&0x81)) {
        if let Ok(n) = sock.recv(&mut buf) {
            for ev in client.recv(&buf[..n]) {
                if let Event::Message(m) = ev {
                    got.push(m.body[0]);
                }
            }
        }
        if client.is_connected() && !sent_login {
            client.queue_message(&login);
            sent_login = true;
        }
        for dg in client.flush() {
            sock.send(&dg).unwrap();
        }
    }
    assert!(got.contains(&0x06), "character record, got {got:02x?}");
    assert!(got.contains(&0x81), "MOTD, got {got:02x?}");
    assert!(!got.contains(&0xfe));
}
