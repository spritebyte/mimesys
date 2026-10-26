// Link cable over the network (TCP). Plain std::net: no Godot, no extra crates.
//
// One side hosts (listens on a port), the other joins (connects to host IP + port).
// Each runs its own game and save. Messages are 7 bytes: [kind, mode, value (4, LE), seq]
//   HELLO  value = protocol version (sent by both on connect)
//   XFER   "I drove a transfer, here's my data"     -> the other side clocks it in
//   REPLY  "...and here's mine back" (same seq)     -> the driver's transfer finishes
//   STATE  value = our port state (GBA: bit 0 = in multiplayer mode), sent when it changes
// `mode` is 0 for the Game Boy (1 byte); the GBA uses its SIO mode (8/32-bit, multiplayer).
// The cable works with any link port through `NetPort` (Game Boy serial, GBA SIO).
//
// The Game Boy that drives a transfer stops (its frame freezes) until the REPLY arrives:
// emulated time doesn't pass, so the game can't tell the byte took a network round trip.
// On a LAN that's well under a millisecond. Until someone connects (or after they leave)
// the port behaves as unplugged: transfers read $FF.

use std::io::{ErrorKind, Read, Write};
use std::net::{SocketAddr, TcpListener, TcpStream, ToSocketAddrs, UdpSocket};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::gameboy::gb_link::LinkCable;
use crate::gameboy::gb_serial::Serial;

pub const DEFAULT_PORT: u16 = 47100;
const VERSION: u8 = 3;
const MSG: usize = 7;

/// What a link port offers the network cable.
pub trait NetPort {
    /// Our finished transfer, waiting for the other side's answer: (mode, value).
    fn outgoing(&self) -> Option<(u8, u32)>;
    /// The other side drove a transfer: clock its value in, return ours.
    fn external(&mut self, mode: u8, value: u32) -> u32;
    /// The answer to our transfer arrived.
    fn finish(&mut self, value: u32);
    /// Nobody connected: finish our transfer with the idle line (all ones).
    fn unplugged(&mut self);
    /// Port state the other side should know about (GBA: in multiplayer mode).
    fn local_state(&self) -> u8 { 0 }
    /// The other side's port state (0 when nobody is connected).
    fn set_remote_state(&mut self, _state: u8) {}
}

impl NetPort for Serial {
    fn outgoing(&self) -> Option<(u8, u32)> { self.awaiting.map(|b| (0, b as u32)) }
    fn external(&mut self, _mode: u8, value: u32) -> u32 { self.external_clock(value as u8) as u32 }
    fn finish(&mut self, value: u32) { Serial::finish(self, value as u8) }
    fn unplugged(&mut self) { Serial::finish(self, 0xFF) }
}

fn msg(kind: u8, mode: u8, value: u32, seq: u8) -> [u8; MSG] {
    let v = value.to_le_bytes();
    [kind, mode, v[0], v[1], v[2], v[3], seq]
}
const HELLO: u8 = 0x01;
const XFER: u8 = 0x02;
const REPLY: u8 = 0x03;
const STATE: u8 = 0x04;

enum State {
    Listening(TcpListener),
    Connecting(Option<JoinHandle<std::io::Result<TcpStream>>>, String),
    Connected(TcpStream, SocketAddr),
    Closed(String),
}

pub struct NetLink {
    state: State,
    port: u16,
    inbuf: Vec<u8>,
    outbuf: Vec<u8>,
    seq: u8,
    /// seq of the XFER we sent and are waiting a REPLY for
    waiting_seq: Option<u8>,
    peer_ok: bool,
    /// we host (listen); for the GBA the host is the multiplayer parent
    host: bool,
    /// port state last sent to the other side
    sent_state: Option<u8>,
}

impl NetLink {
    /// Listen on `port` (0 = any free port; see `port()`).
    pub fn host(port: u16) -> Result<Self, String> {
        let listener = TcpListener::bind(("0.0.0.0", port))
            .map_err(|e| format!("can't listen on port {port}: {e}"))?;
        listener.set_nonblocking(true).map_err(|e| e.to_string())?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let mut l = Self::with_state(State::Listening(listener), port);
        l.host = true;
        Ok(l)
    }

    /// Connect to a host. Returns at once; the connection is made in the background.
    pub fn join(address: &str, port: u16) -> Result<Self, String> {
        let addr = (address.trim(), port).to_socket_addrs()
            .map_err(|e| format!("bad address '{address}': {e}"))?
            .next()
            .ok_or_else(|| format!("bad address '{address}'"))?;
        let handle = std::thread::spawn(move || TcpStream::connect_timeout(&addr, Duration::from_secs(5)));
        Ok(Self::with_state(State::Connecting(Some(handle), addr.to_string()), port))
    }

    fn with_state(state: State, port: u16) -> Self {
        Self { state, port, inbuf: Vec::new(), outbuf: Vec::new(), seq: 0, waiting_seq: None, peer_ok: false, host: false, sent_state: None }
    }

    pub fn port(&self) -> u16 { self.port }

    /// This side hosts the game (listens).
    pub fn is_host(&self) -> bool { self.host }

    pub fn is_connected(&self) -> bool {
        matches!(self.state, State::Connected(..)) && self.peer_ok
    }

    fn close(&mut self, why: impl Into<String>) {
        self.state = State::Closed(why.into());
        self.waiting_seq = None;
        self.peer_ok = false;
    }

    fn on_connected(&mut self, stream: TcpStream) {
        let peer = stream.peer_addr().unwrap_or_else(|_| SocketAddr::from(([0, 0, 0, 0], 0)));
        let _ = stream.set_nodelay(true);
        if stream.set_nonblocking(true).is_err() {
            self.close("couldn't set up the connection");
            return;
        }
        self.inbuf.clear();
        self.outbuf.clear();
        self.sent_state = None;
        self.outbuf.extend_from_slice(&msg(HELLO, 0, VERSION as u32, 0));
        self.state = State::Connected(stream, peer);
    }

    /// Accept / finish connecting, then read and write whatever is ready.
    fn pump(&mut self) {
        match &mut self.state {
            State::Listening(listener) => {
                if let Ok((stream, _)) = listener.accept() {
                    self.on_connected(stream);
                }
            }
            State::Connecting(handle, addr) => {
                if handle.as_ref().map_or(false, |h| h.is_finished()) {
                    let addr = addr.clone();
                    match handle.take().unwrap().join() {
                        Ok(Ok(stream)) => self.on_connected(stream),
                        Ok(Err(e)) => self.close(format!("couldn't connect to {addr}: {e}")),
                        Err(_) => self.close("connection attempt failed"),
                    }
                }
            }
            _ => {}
        }
        let State::Connected(stream, _) = &mut self.state else { return; };
        // write
        while !self.outbuf.is_empty() {
            match stream.write(&self.outbuf) {
                Ok(0) => { self.close("the other player disconnected"); return; }
                Ok(n) => { self.outbuf.drain(..n); }
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => { self.close(format!("connection lost: {e}")); return; }
            }
        }
        // read
        let mut buf = [0u8; 256];
        loop {
            match stream.read(&mut buf) {
                Ok(0) => { self.close("the other player disconnected"); return; }
                Ok(n) => self.inbuf.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == ErrorKind::WouldBlock => break,
                Err(e) => { self.close(format!("connection lost: {e}")); return; }
            }
        }
    }

    fn handle_messages<P: NetPort + ?Sized>(&mut self, port: &mut P) {
        // a version-1 peer sends 3-byte HELLOs: catch that before framing goes wrong
        if !self.peer_ok && self.inbuf.len() >= 3 && self.inbuf[0] == HELLO && self.inbuf[1] == 1 {
            self.close(format!("the other emulator speaks link protocol v1, this one v{VERSION} (update both)"));
            return;
        }
        while self.inbuf.len() >= MSG {
            let kind = self.inbuf[0];
            let mode = self.inbuf[1];
            let value = u32::from_le_bytes([self.inbuf[2], self.inbuf[3], self.inbuf[4], self.inbuf[5]]);
            let seq = self.inbuf[6];
            self.inbuf.drain(..MSG);
            match kind {
                HELLO if value == VERSION as u32 => self.peer_ok = true,
                HELLO => {
                    self.close(format!("the other emulator speaks link protocol v{value}, this one v{VERSION} (update both)"));
                    return;
                }
                XFER => {
                    let reply = port.external(mode, value);
                    self.outbuf.extend_from_slice(&msg(REPLY, mode, reply, seq));
                }
                REPLY if self.waiting_seq == Some(seq) => {
                    self.waiting_seq = None;
                    if port.outgoing().is_some() {
                        port.finish(value);
                    }
                }
                REPLY => {}                                     // stale (transfer was cancelled)
                STATE => port.set_remote_state(value as u8),
                _ => { self.close("garbled data from the other side"); return; }
            }
        }
    }
}

impl NetLink {
    /// Handle traffic for any link port: deliver the other side's transfers, send ours,
    /// and if we're waiting for a reply wait up to `wait` for it.
    pub fn service_port<P: NetPort + ?Sized>(&mut self, port: &mut P, wait: Duration) {
        let deadline = Instant::now() + wait;
        loop {
            self.pump();
            self.handle_messages(port);
            if !self.is_connected() {
                // nobody on the other end: behave like an unplugged port
                port.set_remote_state(0);
                if port.outgoing().is_some() {
                    port.unplugged();
                }
                return;
            }
            let state = port.local_state();
            if self.sent_state != Some(state) {
                self.sent_state = Some(state);
                self.outbuf.extend_from_slice(&msg(STATE, 0, state as u32, 0));
            }
            if let Some((mode, out)) = port.outgoing() {
                if self.waiting_seq.is_none() {
                    self.seq = self.seq.wrapping_add(1);
                    self.waiting_seq = Some(self.seq);
                    self.outbuf.extend_from_slice(&msg(XFER, mode, out, self.seq));
                    continue;                                   // send it now
                }
            } else {
                self.waiting_seq = None;
            }
            if port.outgoing().is_none() || Instant::now() >= deadline {
                self.pump();                                    // flush any reply we owe
                return;
            }
            std::thread::sleep(Duration::from_micros(100));
        }
    }

    pub fn status_line(&self) -> String {
        match &self.state {
            State::Listening(_) => format!("Hosting: waiting for a player on {}:{}", local_ip(), self.port),
            State::Connecting(_, addr) => format!("Connecting to {addr}..."),
            State::Connected(_, peer) if self.peer_ok => format!("Connected to {}", peer.ip()),
            State::Connected(..) => "Connected, saying hello...".into(),
            State::Closed(why) => format!("Not connected: {why}"),
        }
    }
}

impl LinkCable for NetLink {
    fn service(&mut self, serial: &mut Serial, wait: Duration) { self.service_port(serial, wait) }
    fn status(&self) -> String { self.status_line() }
}

/// This computer's LAN address (what the other player types in). No packets are sent:
/// connecting a UDP socket just asks the OS which interface it would use.
pub fn local_ip() -> String {
    UdpSocket::bind("0.0.0.0:0")
        .and_then(|s| { s.connect("192.168.1.1:9")?; s.local_addr() })
        .map(|a| a.ip().to_string())
        .unwrap_or_else(|_| "this computer's IP".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Pump both ends until `done` or 2 seconds pass.
    fn pump(a: &mut NetLink, sa: &mut Serial, b: &mut NetLink, sb: &mut Serial, done: impl Fn(&Serial, &Serial) -> bool) {
        let start = Instant::now();
        while !done(sa, sb) && start.elapsed() < Duration::from_secs(2) {
            a.service(sa, Duration::ZERO);
            b.service(sb, Duration::ZERO);
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    fn connected_pair() -> (NetLink, Serial, NetLink, Serial) {
        let mut host = NetLink::host(0).unwrap();
        let mut guest = NetLink::join("127.0.0.1", host.port()).unwrap();
        let (mut sh, mut sg) = (Serial::new(false), Serial::new(false));
        let start = Instant::now();
        while !(host.is_connected() && guest.is_connected()) && start.elapsed() < Duration::from_secs(3) {
            host.service(&mut sh, Duration::ZERO);
            guest.service(&mut sg, Duration::ZERO);
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(host.is_connected() && guest.is_connected(), "{} / {}", host.status(), guest.status());
        (host, sh, guest, sg)
    }

    #[test]
    fn unconnected_port_reads_ff() {
        let mut host = NetLink::host(0).unwrap();
        let mut s = Serial::new(false);
        s.write_sb(0x42);
        s.write_sc(0x81, false);
        s.tick(4096);
        host.service(&mut s, Duration::from_millis(50));
        assert_eq!((s.awaiting, s.sb), (None, 0xFF));
        assert!(host.status().starts_with("Hosting"), "{}", host.status());
    }

    #[test]
    fn bytes_cross_the_network_both_ways() {
        let (mut host, mut sh, mut guest, mut sg) = connected_pair();
        assert!(host.status().starts_with("Connected to 127.0.0.1"), "{}", host.status());
        // guest ready to receive, host drives
        sg.write_sb(0x99);
        sg.write_sc(0x80, false);
        sh.write_sb(0x42);
        sh.write_sc(0x81, false);
        sh.tick(4096);
        assert_eq!(sh.awaiting, Some(0x42));
        pump(&mut host, &mut sh, &mut guest, &mut sg, |a, _| a.awaiting.is_none());
        assert_eq!((sh.sb, sg.sb), (0x99, 0x42));
        assert!(sh.take_irq() && sg.take_irq());
        // and the other direction, using the blocking wait on the driving side
        sh.write_sb(0x55);
        sh.write_sc(0x80, false);
        sg.write_sb(0xAA);
        sg.write_sc(0x81, false);
        sg.tick(4096);
        let h = std::thread::spawn(move || {
            for _ in 0..200 { host.service(&mut sh, Duration::ZERO); std::thread::sleep(Duration::from_millis(1)); }
            sh
        });
        guest.service(&mut sg, Duration::from_secs(2));
        let sh = h.join().unwrap();
        assert_eq!((sg.awaiting, sg.sb, sh.sb), (None, 0x55, 0xAA));
    }

    #[test]
    fn disconnect_falls_back_to_unplugged() {
        let (host, sh, mut guest, mut sg) = connected_pair();
        drop((host, sh));
        sg.write_sc(0x81, false);
        sg.tick(4096);
        guest.service(&mut sg, Duration::from_millis(500));
        assert_eq!((sg.awaiting, sg.sb), (None, 0xFF));
        assert!(guest.status().contains("disconnected"), "{}", guest.status());
    }

    #[test]
    fn join_bad_host_reports_error() {
        // nothing listens on this port
        let free = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
        let mut j = NetLink::join("127.0.0.1", free).unwrap();
        let mut s = Serial::new(false);
        let start = Instant::now();
        while !j.status().starts_with("Not connected") && start.elapsed() < Duration::from_secs(6) {
            j.service(&mut s, Duration::ZERO);
            std::thread::sleep(Duration::from_millis(5));
        }
        assert!(j.status().starts_with("Not connected: couldn't connect"), "{}", j.status());
    }
}
