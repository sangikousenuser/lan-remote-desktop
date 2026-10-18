//! Platform-independent policy. Callers must authenticate peers before opening a session.

pub mod session;
pub mod telemetry;

pub const RECONNECT_GRACE_SECONDS: u64 = 120;
pub const TARGET_DISPLAY_FPS: u32 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConnectionState {
    Idle,
    Discovered,
    AwaitingApproval,
    Authenticating,
    Connected,
    Reconnecting,
    Closed,
}

pub fn may_resume(
    previously_approved: bool,
    manually_disconnected: bool,
    elapsed_secs: u64,
) -> bool {
    previously_approved && !manually_disconnected && elapsed_secs <= RECONNECT_GRACE_SECONDS
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reconnect_only_within_grace() {
        assert!(may_resume(true, false, 119));
        assert!(may_resume(true, false, 120));
        assert!(!may_resume(true, false, 121));
        assert!(!may_resume(false, false, 1));
        assert!(!may_resume(true, true, 1));
    }
}
