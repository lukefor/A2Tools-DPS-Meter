/// Raw TCP payload captured from pcap.
#[derive(Debug, Clone)]
pub struct CapturedPayload {
    pub src_port: u16,
    pub dst_port: u16,
    pub data: Vec<u8>,
    pub device_name: Option<String>,
    pub captured_at_ms: i64,
    pub src_ip: Option<std::net::Ipv4Addr>,
    pub dst_ip: Option<std::net::Ipv4Addr>,
    pub tcp_seq: u32,
    pub tcp_ack: u32,
}

impl CapturedPayload {
    /// When libpcap captured the segment. A payload that never came from
    /// libpcap (a test, a replay) has no capture time worth using.
    pub fn capture_time_ms(&self) -> Option<i64> {
        (1_000_000_000_000..2_000_000_000_000).contains(&self.captured_at_ms).then_some(self.captured_at_ms)
    }
}

/// The stream name a server-to-client segment is logged and sliced under:
/// `Client:<client port>:<server port>`. One name per connection, so a second
/// connection from the same server port (the game server also talks TLS from
/// it, and a reconnect reuses it) never runs into the game's bytes. Readers
/// take the server port from after the last `:`, as they did from the older
/// `Client:<server port>`.
pub fn stream_key(server_port: u16, client_port: u16) -> String {
    format!("Client:{client_port}:{server_port}")
}
