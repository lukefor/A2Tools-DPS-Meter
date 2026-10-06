//! Where live packets come from: this machine's interfaces through libpcap, or
//! the router's through SSH (`router_capture`). Settings switches between them
//! while the meter runs.

use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::captured_payload::CapturedPayload;
use super::pcap_capturer::PcapCapturer;
use super::router_capture::{RouterCapture, RouterTarget};

enum Source {
    /// Nothing: the pcap library would not load.
    None,
    Local(PcapCapturer),
    Router(RouterCapture),
}

pub struct LiveCapture {
    sender: mpsc::Sender<CapturedPayload>,
    source: Mutex<Source>,
}

impl LiveCapture {
    pub fn new(sender: mpsc::Sender<CapturedPayload>) -> Self {
        Self { sender, source: Mutex::new(Source::None) }
    }

    /// Capture on `router`, or on this machine when it is `None`. Returns
    /// false when this machine was asked for and its pcap library is missing.
    pub fn switch_to(&self, router: Option<RouterTarget>) -> bool {
        let mut source = self.source.lock();
        match std::mem::replace(&mut *source, Source::None) {
            Source::None => {}
            Source::Local(pcap) => pcap.stop(),
            Source::Router(remote) => remote.stop(),
        }
        match router {
            Some(target) => {
                *source = Source::Router(RouterCapture::start(target, self.sender.clone()));
                true
            }
            None if crate::platform::pcap::library_available() => {
                let pcap = PcapCapturer::new(self.sender.clone());
                pcap.start();
                *source = Source::Local(pcap);
                true
            }
            None => false,
        }
    }

    /// The device the router's packets carry, while capturing there.
    pub fn router_label(&self) -> Option<String> {
        match &*self.source.lock() {
            Source::Router(r) => Some(r.label().to_string()),
            _ => None,
        }
    }

    /// Why the router capture is not working, while it is not.
    pub fn router_problem(&self) -> Option<String> {
        match &*self.source.lock() {
            Source::Router(r) => r.problem(),
            _ => None,
        }
    }
}
