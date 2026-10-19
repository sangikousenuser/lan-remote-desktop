//! Bounded native application queues for a separately authenticated transport.
use super::*;

pub struct Driver {
    priority: Receiver<Packet>,
    bulk: Receiver<Packet>,
    events: SyncSender<Packet>,
    state: Sender,
    burst: u8,
}
pub fn start() -> Result<(Sender, Receiver<Packet>, Driver)> {
    let (priority, priority_rx) = mpsc::sync_channel(64);
    let (bulk, bulk_rx) = mpsc::sync_channel(2);
    let (events, incoming) = mpsc::sync_channel(8);
    let sender = Sender {
        priority,
        bulk,
        socket: None,
        stopped: Arc::new(AtomicBool::new(false)),
        counters: Arc::new(Counters::default()),
        closing: Arc::new(AtomicBool::new(false)),
        identity: crate::random_u64()?,
    };
    let driver = Driver {
        priority: priority_rx,
        bulk: bulk_rx,
        events,
        state: sender.clone(),
        burst: 0,
    };
    Ok((sender, incoming, driver))
}
impl Driver {
    pub fn aborted(&self) -> bool {
        self.state.stopped.load(Ordering::Acquire)
    }
    pub fn closing(&self) -> bool {
        self.state.closing.load(Ordering::Acquire)
    }
    pub fn outgoing(&mut self) -> Option<Packet> {
        if self.burst >= 8 {
            self.burst = 0;
            self.bulk
                .try_recv()
                .ok()
                .or_else(|| self.priority.try_recv().ok())
        } else if let Ok(packet) = self.priority.try_recv() {
            self.burst += 1;
            Some(packet)
        } else {
            self.burst = 0;
            self.bulk.try_recv().ok()
        }
    }
    pub fn deliver(&self, packet: Packet) -> std::result::Result<(), TrySendError<Packet>> {
        self.events.try_send(packet)
    }
    pub fn sent(&self, bytes: usize) {
        self.state
            .counters
            .sent
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }
    pub fn received(&self, bytes: usize) {
        self.state
            .counters
            .received
            .fetch_add(bytes as u64, Ordering::Relaxed);
    }
    pub fn rtt(&self, elapsed: Duration) {
        self.state.counters.rtt_us.store(
            elapsed.as_micros().min(u64::MAX as u128) as u64,
            Ordering::Relaxed,
        );
    }
    pub fn dropped(&self) {
        self.state
            .counters
            .queue_full
            .fetch_add(1, Ordering::Relaxed);
    }
}
impl Drop for Driver {
    fn drop(&mut self) {
        self.state.abort();
    }
}
