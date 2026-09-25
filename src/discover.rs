use std::io::ErrorKind;
use std::net::{IpAddr, Ipv4Addr, ToSocketAddrs, UdpSocket};
use std::sync::mpsc::{self, Sender};
use std::thread;
use std::time::{Duration, Instant};

use crate::device::{Firmware, Http, detect};

pub struct Found {
    pub host: String,
    pub via: &'static str,
    pub firmware: Firmware,
}

// Stock firmware hotspot, then CrossPoint's own hotspot.
const PROBE_IPS: [&str; 2] = ["192.168.3.3", "192.168.4.1"];

/// Runs UDP broadcast, mDNS and fixed-IP probes in parallel for up to 2 s.
/// With `all == false` it returns at the first answer.
pub fn find(all: bool, trace: bool) -> Vec<Found> {
    let (tx, rx) = mpsc::channel();
    // Stateful firewalls (NixOS default) drop replies to a broadcast because conntrack can't match them,
    // so also ask each host on the local /24. Packets waiting on ARP hold send-buffer space and a
    // blocking send stalls after ~150 of them, so the sweep is spread over sockets of 64 hosts.
    // ponytail: assumes a /24; send subnet-directed per interface if bigger LANs matter.
    let mut batches = vec![vec![Ipv4Addr::BROADCAST]];
    if let Some(IpAddr::V4(me)) = local_ip() {
        let [a, b, c, _] = me.octets();
        let hosts: Vec<Ipv4Addr> = (1..=254).map(|d| Ipv4Addr::new(a, b, c, d)).collect();
        batches.extend(hosts.chunks(64).map(<[_]>::to_vec));
    }
    for targets in batches {
        let t = tx.clone();
        thread::spawn(move || udp(t, targets));
    }
    let t = tx.clone();
    // ponytail: uses the system resolver, so it needs nss-mdns/Bonjour; UDP covers CrossPoint without it.
    thread::spawn(move || {
        if let Some(a) = ("crosspoint.local", 80)
            .to_socket_addrs()
            .ok()
            .and_then(|mut a| a.find(|a| a.is_ipv4()))
        {
            let _ = t.send(Found {
                host: a.ip().to_string(),
                via: "mdns",
                firmware: Firmware::CrossPoint,
            });
        }
    });
    for ip in PROBE_IPS {
        let t = tx.clone();
        thread::spawn(move || {
            if let Ok(firmware) = detect(&Http::new(ip, trace, Duration::from_millis(1500))) {
                let _ = t.send(Found {
                    host: ip.into(),
                    via: "probe",
                    firmware,
                });
            }
        });
    }
    drop(tx);

    let deadline = Instant::now() + Duration::from_secs(2);
    let mut out: Vec<Found> = Vec::new();
    while let Ok(f) = rx.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
        if !out.iter().any(|o| o.host == f.host) {
            out.push(f);
        }
        if !all {
            break;
        }
    }
    out
}

/// CrossPoint answers "hello" on UDP 8134 with "crosspoint (on <hostname>);<ws port>".
fn udp(tx: Sender<Found>, targets: Vec<Ipv4Addr>) -> std::io::Result<()> {
    let sock = UdpSocket::bind("0.0.0.0:0")?;
    sock.set_broadcast(true)?;
    sock.set_read_timeout(Some(Duration::from_secs(2)))?;
    for ip in targets {
        let _ = sock.send_to(b"hello", (ip, 8134));
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut buf = [0u8; 256];
    while Instant::now() < deadline {
        match sock.recv_from(&mut buf) {
            Ok((n, from)) if buf[..n].starts_with(b"crosspoint") => {
                let found = Found {
                    host: from.ip().to_string(),
                    via: "udp",
                    firmware: Firmware::CrossPoint,
                };
                if tx.send(found).is_err() {
                    break;
                }
            }
            Err(e) if matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut) => break,
            // Windows reports ICMP port-unreachable from swept hosts as recv errors.
            _ => {}
        }
    }
    Ok(())
}

/// Address of the default-route interface. Connecting a UDP socket sends nothing.
fn local_ip() -> Option<IpAddr> {
    let s = UdpSocket::bind("0.0.0.0:0").ok()?;
    s.connect("192.0.2.1:9").ok()?;
    s.local_addr().ok().map(|a| a.ip())
}
