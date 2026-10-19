//! Verified received and queued outgoing file payload counters, with no paths.
use std::{
    sync::atomic::{AtomicU64, Ordering},
    time::Instant,
};
static SENT: AtomicU64 = AtomicU64::new(0);
static RECEIVED: AtomicU64 = AtomicU64::new(0);
pub fn sent(bytes: usize) {
    SENT.fetch_add(bytes as u64, Ordering::Relaxed);
}
pub fn received(bytes: usize) {
    RECEIVED.fetch_add(bytes as u64, Ordering::Relaxed);
}
pub struct Meter {
    at: Instant,
    sent: u64,
    received: u64,
}
impl Default for Meter {
    fn default() -> Self {
        Self {
            at: Instant::now(),
            sent: SENT.load(Ordering::Relaxed),
            received: RECEIVED.load(Ordering::Relaxed),
        }
    }
}
impl Meter {
    pub fn sample(&mut self) -> (u64, u64) {
        let sent = SENT.load(Ordering::Relaxed);
        let received = RECEIVED.load(Ordering::Relaxed);
        let seconds = self.at.elapsed().as_secs_f64().max(0.001);
        let result = (
            ((sent.saturating_sub(self.sent)) as f64 / seconds) as u64,
            ((received.saturating_sub(self.received)) as f64 / seconds) as u64,
        );
        self.at = Instant::now();
        self.sent = sent;
        self.received = received;
        result
    }
}
