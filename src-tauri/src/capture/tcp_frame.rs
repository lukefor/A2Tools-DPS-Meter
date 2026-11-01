//! Reading a TCP segment out of a captured frame: where IPv4 starts for each
//! pcap link type, and the payload the segment carries. The router's tcpdump
//! stream is pcap, so its frames are read exactly as libpcap's would be.

use std::ffi::c_int;
use std::time::{SystemTime, UNIX_EPOCH};

use super::captured_payload::CapturedPayload;

fn now_ms() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_millis() as i64
}

/// Where IPv4 starts in a frame of pcap link type `link_type`, or `None` if this
/// frame is not IPv4 under that type. Link types are libpcap's (DLT_*/LINKTYPE_*),
/// the same on every OS.
fn link_header_len(link_type: c_int, frame: &[u8]) -> Option<usize> {
    let be16 = |at: usize| frame.get(at..at + 2).map(|b| u16::from_be_bytes([b[0], b[1]]));
    match link_type {
        // Ethernet, with up to two 802.1Q / 802.1ad VLAN tags (some NICs, e.g.
        // Realtek with "Priority & VLAN" on, tag every frame).
        1 => {
            let mut at = 12;
            for _ in 0..2 {
                match be16(at)? {
                    0x8100 | 0x88A8 => at += 4,
                    _ => break,
                }
            }
            (be16(at)? == 0x0800).then_some(at + 2)
        }
        // BSD loopback / Npcap loopback: 4-byte address family, AF_INET = 2.
        0 => (frame.get(..4)? == [2, 0, 0, 0]).then_some(4),
        // Raw IP (VPN and tunnel adapters, WireGuard).
        12 | 14 | 101 | 228 => Some(0),
        // Linux "cooked" capture v1: protocol at bytes 14-15, 16-byte header.
        113 => (be16(14)? == 0x0800).then_some(16),
        // Linux "cooked" capture v2: protocol at bytes 0-1, 20-byte header.
        276 => (be16(0)? == 0x0800).then_some(20),
        _ => None,
    }
}

/// The original guess at the link layer, from the frame alone. Kept as the
/// fallback so nothing that parsed before the link type was consulted can stop
/// parsing now.
fn guess_link_header_len(frame: &[u8]) -> Option<usize> {
    if frame.len() >= 14 {
        let ether_type = u16::from_be_bytes([frame[12], frame[13]]);
        if ether_type == 0x0800 {
            Some(14) // Standard Ethernet
        } else if frame[0] == 2 && frame[1] == 0 && frame[2] == 0 && frame[3] == 0 {
            Some(4) // NULL/Loopback: AF_INET (little-endian 2) = IPv4
        } else if (frame[0] >> 4) == 4 {
            Some(0) // Raw IPv4
        } else {
            None
        }
    } else if (frame[0] >> 4) == 4 {
        Some(0) // Raw IPv4 (short frame)
    } else {
        None
    }
}

/// Parse raw captured frame to extract TCP payload. The link type says where
/// IPv4 starts; when it does not fit, the frame-only guess gets a chance.
pub(crate) fn parse_tcp_payload(frame: &[u8], link_type: c_int, device_name: &str) -> Option<CapturedPayload> {
    if frame.len() < 4 {
        return None;
    }
    let ip_offset = link_header_len(link_type, frame)
        .filter(|&at| frame.get(at).is_some_and(|b| b >> 4 == 4))
        .or_else(|| guess_link_header_len(frame))?;

    if frame.len() < ip_offset + 20 {
        return None;
    }
    let ip_header = &frame[ip_offset..];
    if ip_header[0] >> 4 != 4 {
        return None;
    }
    let ip_header_len = ((ip_header[0] & 0x0F) as usize) * 4;
    if ip_header[9] != 6 {
        return None; // Not TCP
    }

    // Kept as numbers: formatting them as text cost two allocations for
    // every packet on every device, and nothing reads them as text.
    let src_ip = std::net::Ipv4Addr::new(ip_header[12], ip_header[13], ip_header[14], ip_header[15]);
    let dst_ip = std::net::Ipv4Addr::new(ip_header[16], ip_header[17], ip_header[18], ip_header[19]);

    let tcp_offset = ip_offset + ip_header_len;
    if frame.len() < tcp_offset + 20 {
        return None;
    }
    let tcp_header = &frame[tcp_offset..];
    let src_port = u16::from_be_bytes([tcp_header[0], tcp_header[1]]);
    let dst_port = u16::from_be_bytes([tcp_header[2], tcp_header[3]]);
    let tcp_seq = u32::from_be_bytes([tcp_header[4], tcp_header[5], tcp_header[6], tcp_header[7]]);
    let tcp_ack =
        u32::from_be_bytes([tcp_header[8], tcp_header[9], tcp_header[10], tcp_header[11]]);
    let tcp_header_len = ((tcp_header[12] >> 4) as usize) * 4;

    // The payload ends where the IP packet says it does, not at the end of the
    // frame: Ethernet pads short frames to 60 bytes, so a bare ACK carries 6
    // padding bytes after its TCP header. Most network cards pad with zeros,
    // which the framing skips; some pad with whatever was in the buffer, and
    // those bytes landed in the game stream and threw the framing off. One
    // player's meter read 77% of a boss's damage while their party member's,
    // in the same fight, read all of it (2026-10-06). A total length of 0
    // (segmentation offload) or past the frame keeps the frame's end.
    let ip_total_len = u16::from_be_bytes([ip_header[2], ip_header[3]]) as usize;
    let ip_end = ip_offset + ip_total_len;
    let end = if ip_total_len >= ip_header_len + 20 && ip_end <= frame.len() { ip_end } else { frame.len() };
    let payload_offset = tcp_offset + tcp_header_len;
    if payload_offset >= end {
        return None;
    }
    let payload = &frame[payload_offset..end];
    if payload.is_empty() {
        return None;
    }

    Some(CapturedPayload {
        src_port,
        dst_port,
        data: payload.to_vec(),
        device_name: Some(device_name.to_string()),
        captured_at_ms: now_ms(),
        src_ip: Some(src_ip),
        dst_ip: Some(dst_ip),
        tcp_seq,
        tcp_ack,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// IPv4 + TCP from 10.0.0.2:51000 to 193.202.112.99:13328, payload "hi".
    fn ipv4_tcp() -> Vec<u8> {
        let mut ip = vec![0x45, 0, 0, 42, 0, 0, 0, 0, 64, 6, 0, 0, 10, 0, 0, 2, 193, 202, 112, 99];
        let tcp = [0xC7, 0x38, 0x34, 0x10, 0, 0, 0, 1, 0, 0, 0, 0, 0x50, 0x18, 0, 0, 0, 0, 0, 0];
        ip.extend_from_slice(&tcp);
        ip.extend_from_slice(b"hi");
        ip
    }

    fn check(link_type: c_int, header: &[u8]) {
        let frame = [header, &ipv4_tcp()].concat();
        let p = parse_tcp_payload(&frame, link_type, "dev").expect("parsed");
        assert_eq!((p.src_port, p.dst_port), (51000, 13328), "link type {link_type}");
        assert_eq!(p.dst_ip, Some(std::net::Ipv4Addr::new(193, 202, 112, 99)));
        assert_eq!(p.data, b"hi");
    }

    #[test]
    fn reads_every_link_layer_the_meter_meets() {
        let ether = [[0u8; 12].as_slice(), &[0x08, 0x00]].concat();
        check(1, &ether); // Ethernet / Wi-Fi
        let vlan = [[0u8; 12].as_slice(), &[0x81, 0x00, 0x00, 0x05, 0x08, 0x00]].concat();
        check(1, &vlan); // Ethernet with an 802.1Q tag
        check(0, &[2, 0, 0, 0]); // Npcap / BSD loopback
        check(12, &[]); // raw IP (VPN, WireGuard)
        check(101, &[]);
        let sll = [[0u8; 14].as_slice(), &[0x08, 0x00]].concat();
        check(113, &sll); // Linux cooked v1
        let sll2 = [[0x08u8, 0x00].as_slice(), &[0u8; 18]].concat();
        check(276, &sll2); // Linux cooked v2
    }

    #[test]
    fn an_unknown_or_mismatched_link_type_still_gets_the_old_guess() {
        // Ethernet framing reported as some other link type: guessed as before.
        let ether = [[0u8; 12].as_slice(), &[0x08, 0x00]].concat();
        check(999, &ether);
        // Raw IP on a device that claims Ethernet: the exact read fails, the
        // guess finds IPv4 at 0, as it always did.
        check(1, &[]);
    }

    #[test]
    fn ethernet_padding_is_not_payload() {
        // A bare ACK (IP total length 40) padded to 60 bytes with non-zero
        // bytes, as one player's network card sends them (2026-10-06).
        let ether = [[0u8; 12].as_slice(), &[0x08, 0x00]].concat();
        let mut ack = ipv4_tcp();
        ack.truncate(40);
        ack[3] = 40;
        let frame = [ether.as_slice(), &ack, &[0xbe, 0xde, 0x00, 0x03, 0x66, 0xfd]].concat();
        assert!(parse_tcp_payload(&frame, 1, "dev").is_none());

        // Data followed by padding keeps only the data.
        let mut short = ipv4_tcp();
        short.truncate(41);
        short[3] = 41;
        let frame = [ether.as_slice(), &short, &[0xbe, 0xde, 0x00, 0x03, 0x66]].concat();
        assert_eq!(parse_tcp_payload(&frame, 1, "dev").unwrap().data, b"h");

        // A total length of 0 (segmentation offload) keeps the frame's end.
        let mut tso = ipv4_tcp();
        tso[2] = 0;
        tso[3] = 0;
        let frame = [ether.as_slice(), &tso].concat();
        assert_eq!(parse_tcp_payload(&frame, 1, "dev").unwrap().data, b"hi");
    }

    #[test]
    fn non_ipv4_frames_are_ignored() {
        let arp = [[0u8; 12].as_slice(), &[0x08, 0x06], &[0u8; 28]].concat();
        assert!(parse_tcp_payload(&arp, 1, "dev").is_none());
    }
}
