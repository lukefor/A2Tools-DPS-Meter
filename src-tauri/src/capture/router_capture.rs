//! Capture on the router instead of this machine: run `tcpdump` there over SSH
//! and read the pcap stream it writes to stdout.
//!
//! The router forwards every packet the game sends and receives, so it sees
//! the same TCP segments Npcap would, and nothing needs installing on the PC
//! the game runs on. What the router needs is `tcpdump` (`tcpdump-mini` is
//! enough) and this PC's SSH key; see docs/router-capture.md.
//!
//! The filter runs on the router, and lets through only this PC's connection
//! to the game server's port. Nothing else this PC (or anything else on the
//! network) does ever leaves the router, so it cannot be logged, kept in
//! memory or uploaded by anything in the meter. The SSH session is excluded as
//! well, though the port already rules it out.

use std::io::{self, BufRead, BufReader, Read};
use std::process::{Child, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use parking_lot::Mutex;
use tokio::sync::mpsc;
use tracing::{info, warn};

use super::captured_payload::CapturedPayload;
use super::pcap_capturer::parse_tcp_payload;

/// The interface OpenWrt bridges its LAN ports and Wi-Fi into.
pub const DEFAULT_INTERFACE: &str = "br-lan";
/// The game's connection and nothing else: this PC's TCP to and from the game
/// server's port. `$1` is this PC's address as the router sees it (from
/// `$SSH_CLIENT`). 13328 is the game server's port in the test captures and on
/// the live game this was checked against; if a region uses another, set
/// `capture.routerFilter`.
const DEFAULT_FILTER: &str = "tcp and host $1 and port 13328";
/// A pcap record larger than this means the stream is out of step (tcpdump's
/// own snapshot length is 262144).
const MAX_RECORD_BYTES: usize = 1 << 20;
/// Waits between attempts to reach the router, doubling up to the last.
const RETRY_SECS: [u64; 6] = [2, 4, 8, 15, 30, 60];
/// How long the router's clock offset is learned over before starting afresh,
/// so a clock that steps backwards is followed within two of these.
const CLOCK_WINDOW_MS: i64 = 10_000;

/// Where to capture: an `ssh` destination (`root@192.168.1.1`, an
/// `ssh://` URL or a Host from ~/.ssh/config), the router's interface,
/// and a tcpdump filter in place of the default.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RouterTarget {
    pub ssh: String,
    pub interface: String,
    pub filter: Option<String>,
}

impl RouterTarget {
    /// The label packets carry as their device.
    pub fn label(&self) -> String {
        format!("Router {} ({})", self.ssh, self.interface)
    }

    /// The command the router's shell runs. `$SSH_CLIENT` is "this PC's
    /// address, its port, the router's SSH port".
    fn remote_command(&self) -> String {
        let filter = match &self.filter {
            Some(custom) => format!("({})", custom),
            None => DEFAULT_FILTER.to_string(),
        };
        format!(
            "set -- $SSH_CLIENT; [ -n \"$3\" ] || {{ echo 'SSH_CLIENT is not set on the router' >&2; exit 1; }}; \
             exec tcpdump -i {} -U --immediate-mode -s 0 -w - {}\" and not port $3\"",
            shell_quote(&self.interface),
            // The default refers to $1, so it must expand; a custom filter is
            // the player's text and must not.
            if self.filter.is_some() { shell_quote(&filter) } else { format!("\"{}\"", filter) },
        )
    }
}

/// Quote for a POSIX shell (the router's ash).
fn shell_quote(s: &str) -> String {
    format!("'{}'", s.replace('\'', r"'\''"))
}

/// Captures on the router for as long as it runs, reconnecting when the SSH
/// session ends.
pub struct RouterCapture {
    running: Arc<AtomicBool>,
    child: Arc<Mutex<Option<Child>>>,
    /// Why the last attempt failed, for Settings; `None` while it works.
    problem: Arc<Mutex<Option<String>>>,
    label: String,
}

impl RouterCapture {
    pub fn start(target: RouterTarget, sender: mpsc::Sender<CapturedPayload>) -> Self {
        let running = Arc::new(AtomicBool::new(true));
        let child = Arc::new(Mutex::new(None));
        let problem = Arc::new(Mutex::new(None));
        let label = target.label();
        {
            let (running, child, problem) = (running.clone(), child.clone(), problem.clone());
            std::thread::spawn(move || run(target, sender, running, child, problem));
        }
        Self { running, child, problem, label }
    }

    pub fn stop(&self) {
        self.running.store(false, Ordering::SeqCst);
        if let Some(mut child) = self.child.lock().take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }

    /// Why it is not capturing, when it is not.
    pub fn problem(&self) -> Option<String> {
        self.problem.lock().clone()
    }

    pub fn label(&self) -> &str {
        &self.label
    }
}

impl Drop for RouterCapture {
    fn drop(&mut self) {
        self.stop();
    }
}

fn run(
    target: RouterTarget,
    sender: mpsc::Sender<CapturedPayload>,
    running: Arc<AtomicBool>,
    child_slot: Arc<Mutex<Option<Child>>>,
    problem_slot: Arc<Mutex<Option<String>>>,
) {
    let label = target.label();
    let mut attempt = 0usize;
    while running.load(Ordering::SeqCst) {
        info!("Router capture: connecting to {} ({})", target.ssh, target.interface);
        let set_problem = |problem: String| *problem_slot.lock() = Some(problem);

        let mut child = match spawn_ssh(&target) {
            Ok(c) => c,
            Err(e) => {
                warn!("Router capture: {e}");
                set_problem(e);
                attempt += 1;
                wait_to_retry(&running, attempt);
                continue;
            }
        };
        let stdout = child.stdout.take();
        let stderr = child.stderr.take();
        *child_slot.lock() = Some(child);
        if !running.load(Ordering::SeqCst) {
            // Stopped while spawning: `stop` may have run before the child was
            // stored, so make sure it does not outlive us.
            if let Some(mut c) = child_slot.lock().take() {
                let _ = c.kill();
                let _ = c.wait();
            }
            break;
        }

        // The router's stderr: tcpdump's "listening on ..." and every error,
        // from SSH or the router's shell. The last line says why it stopped.
        let last_err = Arc::new(Mutex::new(String::new()));
        let err_thread = stderr.map(|stderr| {
            let last_err = last_err.clone();
            std::thread::spawn(move || {
                for line in BufReader::new(stderr).lines().map_while(Result::ok) {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }
                    info!("Router capture: {line}");
                    if !is_tcpdump_chatter(&line) {
                        *last_err.lock() = line;
                    }
                }
            })
        });

        let mut packets = 0u64;
        let result = match stdout {
            Some(stdout) => read_stream(BufReader::with_capacity(64 * 1024, stdout), &mut ClockMap::default(), |ts, frame, link_type| {
                if packets == 0 {
                    info!("Router capture: receiving packets (link type {link_type})");
                    *problem_slot.lock() = None;
                }
                packets += 1;
                if let Some(mut payload) = parse_tcp_payload(frame, link_type, &label) {
                    payload.captured_at_ms = ts;
                    let _ = sender.try_send(payload);
                }
            }),
            None => Err(io::Error::other("ssh gave no output")),
        };

        let exit = child_slot.lock().take().map(|mut c| {
            let _ = c.kill();
            c.wait()
        });
        if let Some(t) = err_thread {
            let _ = t.join();
        }
        if !running.load(Ordering::SeqCst) {
            break;
        }

        let last_err = last_err.lock().clone();
        let problem = explain(&last_err, exit.and_then(|e| e.ok()).and_then(|s| s.code()), result.err());
        warn!("Router capture stopped after {packets} packets: {problem}");
        set_problem(problem);
        attempt = if packets > 0 { 1 } else { attempt + 1 };
        wait_to_retry(&running, attempt);
    }
    *problem_slot.lock() = None;
    info!("Router capture stopped");
}

/// What tcpdump says when all is well: "listening on br-lan, ..." at the start
/// and "12 packets captured" and the like at the end. Never why it stopped.
fn is_tcpdump_chatter(line: &str) -> bool {
    let line = line.strip_prefix("tcpdump: ").unwrap_or(line);
    line.starts_with("listening on ")
        || line.split_once(' ').is_some_and(|(n, rest)| {
            n.chars().all(|c| c.is_ascii_digit()) && rest.starts_with("packet")
        })
}

/// What to tell the player about a session that ended.
fn explain(stderr: &str, exit_code: Option<i32>, read_err: Option<io::Error>) -> String {
    let lower = stderr.to_lowercase();
    if lower.contains("tcpdump: not found") || exit_code == Some(127) {
        return "tcpdump is not installed on the router (opkg install tcpdump-mini, or apk add tcpdump-mini)".into();
    }
    if lower.contains("permission denied") {
        return format!("The router refused this PC's SSH key: {stderr}");
    }
    if !stderr.is_empty() {
        return stderr.to_string();
    }
    match (read_err, exit_code) {
        (Some(e), _) if e.kind() != io::ErrorKind::UnexpectedEof => e.to_string(),
        (_, Some(code)) => format!("ssh exited with code {code}"),
        _ => "The connection to the router closed".into(),
    }
}

fn wait_to_retry(running: &AtomicBool, attempt: usize) {
    let secs = RETRY_SECS[attempt.saturating_sub(1).min(RETRY_SECS.len() - 1)];
    for _ in 0..secs * 10 {
        if !running.load(Ordering::SeqCst) {
            return;
        }
        std::thread::sleep(Duration::from_millis(100));
    }
}

fn spawn_ssh(target: &RouterTarget) -> Result<Child, String> {
    let mut cmd = crate::platform::shell::ssh_command();
    cmd.args([
        "-T",
        // Over IPv4, so $SSH_CLIENT is this PC's IPv4 address: the game talks
        // IPv4, and `openwrt.lan` may resolve to IPv6 first.
        "-4",
        // Never prompt: there is no terminal to answer on. A password prompt
        // would hang forever, so it is a key or nothing.
        "-o", "BatchMode=yes",
        // Trust the router's host key the first time, and only that key after.
        "-o", "StrictHostKeyChecking=accept-new",
        "-o", "ConnectTimeout=10",
        // Notice a router that went away, rather than waiting on TCP.
        "-o", "ServerAliveInterval=5",
        "-o", "ServerAliveCountMax=3",
    ])
    .arg(&target.ssh)
    .arg(target.remote_command())
    .stdin(Stdio::null())
    .stdout(Stdio::piped())
    .stderr(Stdio::piped());
    cmd.spawn().map_err(|e| match e.kind() {
        io::ErrorKind::NotFound => {
            format!("ssh was not found. {}", crate::platform::shell::SSH_MISSING_HELP)
        }
        _ => format!("Could not start ssh: {e}"),
    })
}

// ===== The pcap stream =====

/// The file header tcpdump writes first. The byte order is the router's,
/// and plenty of MIPS routers are big-endian.
#[derive(Debug, PartialEq, Eq)]
struct StreamHeader {
    big_endian: bool,
    nanos: bool,
    link_type: i32,
}

impl StreamHeader {
    fn u32(&self, b: &[u8]) -> u32 {
        let b = [b[0], b[1], b[2], b[3]];
        if self.big_endian { u32::from_be_bytes(b) } else { u32::from_le_bytes(b) }
    }
}

fn read_header(r: &mut impl Read) -> io::Result<StreamHeader> {
    let mut h = [0u8; 24];
    r.read_exact(&mut h)?;
    let (big_endian, nanos) = match h[..4] {
        [0xd4, 0xc3, 0xb2, 0xa1] => (false, false),
        [0xa1, 0xb2, 0xc3, 0xd4] => (true, false),
        [0x4d, 0x3c, 0xb2, 0xa1] => (false, true),
        [0xa1, 0xb2, 0x3c, 0x4d] => (true, true),
        _ => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("The router did not send a packet capture (it began {:?})", String::from_utf8_lossy(&h)),
            ))
        }
    };
    let mut header = StreamHeader { big_endian, nanos, link_type: 0 };
    // The upper bits of the link-type field carry FCS flags.
    header.link_type = (header.u32(&h[20..24]) & 0xffff) as i32;
    Ok(header)
}

/// Read a pcap stream to its end, handing each frame and its timestamp (on
/// this PC's clock, via `clock`) to `on_frame`.
fn read_stream(
    mut r: impl Read,
    clock: &mut ClockMap,
    mut on_frame: impl FnMut(i64, &[u8], i32),
) -> io::Result<()> {
    let header = read_header(&mut r)?;
    let mut rec = [0u8; 16];
    let mut frame = Vec::with_capacity(2048);
    loop {
        r.read_exact(&mut rec)?;
        let secs = header.u32(&rec[0..4]) as i64;
        let frac = header.u32(&rec[4..8]) as i64;
        let len = header.u32(&rec[8..12]) as usize;
        if len > MAX_RECORD_BYTES {
            return Err(io::Error::new(io::ErrorKind::InvalidData, format!("Bad record length {len}")));
        }
        frame.resize(len, 0);
        r.read_exact(&mut frame)?;
        let router_ms = secs * 1000 + if header.nanos { frac / 1_000_000 } else { frac / 1000 };
        on_frame(clock.to_local(router_ms, crate::clock::now_ms()), &frame, header.link_type);
    }
}

/// Moves the router's timestamps onto this PC's clock. The ping reading
/// compares capture times with the game's own clock on this PC, and nothing
/// says the router's clock agrees with it (one that never synced reads 1970).
///
/// A packet arrives here some time after the router stamped it, so
/// `local - router` is the clocks' offset plus that delay, and the smallest
/// of it is the offset plus the shortest delay, a millisecond or two on a
/// LAN. It is learned over a sliding pair of windows, so the estimate follows
/// either clock being stepped.
#[derive(Default)]
struct ClockMap {
    current: Option<i64>,
    previous: Option<i64>,
    window_start: i64,
}

impl ClockMap {
    fn to_local(&mut self, router_ms: i64, local_now: i64) -> i64 {
        if local_now - self.window_start >= CLOCK_WINDOW_MS {
            self.previous = self.current.take();
            self.window_start = local_now;
        }
        let sample = local_now - router_ms;
        let current = self.current.map_or(sample, |c| c.min(sample));
        self.current = Some(current);
        router_ms + self.previous.map_or(current, |p| p.min(current))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A pcap stream as `tcpdump -w -` writes it, in either byte order.
    fn stream(big_endian: bool, link_type: u32, frames: &[(u32, u32, &[u8])]) -> Vec<u8> {
        let w32 = |v: u32| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
        let w16 = |v: u16| if big_endian { v.to_be_bytes() } else { v.to_le_bytes() };
        let mut out = Vec::new();
        out.extend_from_slice(&w32(0xa1b2c3d4));
        out.extend_from_slice(&w16(2));
        out.extend_from_slice(&w16(4));
        out.extend_from_slice(&w32(0));
        out.extend_from_slice(&w32(0));
        out.extend_from_slice(&w32(262144));
        out.extend_from_slice(&w32(link_type));
        for (secs, usecs, frame) in frames {
            out.extend_from_slice(&w32(*secs));
            out.extend_from_slice(&w32(*usecs));
            out.extend_from_slice(&w32(frame.len() as u32));
            out.extend_from_slice(&w32(frame.len() as u32));
            out.extend_from_slice(frame);
        }
        out
    }

    #[test]
    fn reads_either_byte_order() {
        for big_endian in [false, true] {
            let bytes = stream(big_endian, 1, &[(1_700_000_000, 250_000, b"abc"), (1_700_000_001, 0, b"de")]);
            let mut seen = Vec::new();
            let mut clock = ClockMap::default();
            let end = read_stream(bytes.as_slice(), &mut clock, |_, frame, link| seen.push((frame.to_vec(), link)));
            assert_eq!(end.unwrap_err().kind(), io::ErrorKind::UnexpectedEof);
            assert_eq!(seen, vec![(b"abc".to_vec(), 1), (b"de".to_vec(), 1)], "big endian: {big_endian}");
        }
    }

    #[test]
    fn text_is_not_a_capture() {
        let err = read_stream(&b"ash: tcpdump: not found\n"[..], &mut ClockMap::default(), |_, _, _| {}).unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn router_time_lands_on_this_clock() {
        let mut clock = ClockMap::default();
        // The router is 5 s behind; packets reach us 3 ms, then 1 ms, after it
        // stamped them. The smallest delay wins, and spacing is the router's.
        let local = 1_800_000_000_000;
        assert_eq!(clock.to_local(local - 5_003, local), local);
        assert_eq!(clock.to_local(local - 4_901, local + 100), local + 100);
        assert_eq!(clock.to_local(local - 4_950, local + 200), local + 51);
        // Never after the moment it arrived.
        assert!(clock.to_local(local - 4_000, local + 1_000) <= local + 1_000);
    }

    #[test]
    fn follows_a_router_clock_stepped_back() {
        let mut clock = ClockMap::default();
        let local = 1_800_000_000_000;
        clock.to_local(local, local);
        // The router steps back an hour (NTP after boot); within two windows
        // its packets land at the time they arrived again.
        let mut t = local;
        for _ in 0..30 {
            t += 1_000;
            clock.to_local(t - 3_600_000, t);
        }
        assert_eq!(clock.to_local(t + 10 - 3_600_000, t + 10), t + 10);
    }

    #[test]
    fn says_why_a_session_ended() {
        assert!(is_tcpdump_chatter("tcpdump: listening on br-lan, link-type EN10MB (Ethernet), snapshot length 262144 bytes"));
        assert!(is_tcpdump_chatter("0 packets dropped by kernel"));
        assert!(!is_tcpdump_chatter("Host key verification failed."));
        assert!(explain("ash: exec: line 1: tcpdump: not found", Some(127), None).starts_with("tcpdump is not installed"));
        assert!(explain("root@192.168.1.1: Permission denied (publickey).", Some(255), None).contains("SSH key"));
        assert_eq!(explain("", Some(255), None), "ssh exited with code 255");
    }

    #[test]
    fn the_filter_never_captures_the_ssh_session() {
        let target = RouterTarget { ssh: "root@192.168.1.1".into(), interface: "br-lan".into(), filter: None };
        let cmd = target.remote_command();
        assert!(cmd.contains("-i 'br-lan'"));
        assert!(cmd.ends_with("\"tcp and host $1 and port 13328\"\" and not port $3\""), "{cmd}");

        let custom = RouterTarget { filter: Some("tcp and port 13328".into()), ..target };
        assert!(custom.remote_command().ends_with("'(tcp and port 13328)'\" and not port $3\""));
        let sneaky = RouterTarget { filter: Some("x' ; reboot ; '".into()), ..custom };
        assert!(sneaky.remote_command().contains(r"'(x'\'' ; reboot ; '\'')'"));
    }
}
