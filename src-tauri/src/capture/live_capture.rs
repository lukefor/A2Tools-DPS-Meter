//! Where live packets come from: the router, through SSH (`router_capture`).
//! This build captures nowhere else. Settings names the router and can change
//! it while the meter runs.

use parking_lot::Mutex;
use tokio::sync::mpsc;

use super::captured_payload::CapturedPayload;
use super::router_capture::{RouterCapture, RouterTarget};

pub struct LiveCapture {
    sender: mpsc::Sender<CapturedPayload>,
    router: Mutex<Option<RouterCapture>>,
}

impl LiveCapture {
    pub fn new(sender: mpsc::Sender<CapturedPayload>) -> Self {
        Self { sender, router: Mutex::new(None) }
    }

    /// Capture on `router`, or stop capturing when it is `None`.
    pub fn switch_to(&self, router: Option<RouterTarget>) {
        let mut current = self.router.lock();
        if let Some(old) = current.take() {
            old.stop();
        }
        *current = router.map(|target| RouterCapture::start(target, self.sender.clone()));
    }

    /// The device the router's packets carry, while capturing there.
    pub fn router_label(&self) -> Option<String> {
        self.router.lock().as_ref().map(|r| r.label().to_string())
    }

    /// Why nothing is being captured, while nothing is.
    pub fn problem(&self) -> Option<String> {
        match &*self.router.lock() {
            Some(r) => r.problem(),
            None => Some("Set Capture on router in Settings".into()),
        }
    }
}
