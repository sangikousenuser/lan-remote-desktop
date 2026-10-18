use crate::{ConnectionState, RECONNECT_GRACE_SECONDS};
use std::time::Duration;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PeerIdentity {
    pub public_key: [u8; 32],
    pub user_session: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionError {
    InvalidState,
    ApprovalRequired,
    DifferentPeer,
    Expired,
    InvalidEpoch,
    StaleInput,
}

/// Times are durations from the caller's monotonic clock, never wall-clock timestamps.
#[derive(Debug)]
pub struct Session {
    state: ConnectionState,
    peer: PeerIdentity,
    session_id: u64,
    epoch: u64,
    last_input: u64,
    disconnected_at: Option<Duration>,
    approved: bool,
}

impl Session {
    pub fn discovered(peer: PeerIdentity) -> Self {
        Self {
            state: ConnectionState::AwaitingApproval,
            peer,
            session_id: 0,
            epoch: 0,
            last_input: 0,
            disconnected_at: None,
            approved: false,
        }
    }

    pub fn state(&self) -> ConnectionState {
        self.state
    }

    pub fn approve(&mut self) -> Result<(), SessionError> {
        if self.state != ConnectionState::AwaitingApproval {
            return Err(SessionError::InvalidState);
        }
        self.approved = true;
        self.state = ConnectionState::Authenticating;
        Ok(())
    }

    /// Must only be called with an identity obtained from an authenticated transport.
    pub fn authenticated(
        &mut self,
        peer: PeerIdentity,
        session_id: u64,
        fresh_epoch: u64,
        now: Duration,
    ) -> Result<(), SessionError> {
        if !self.approved {
            return Err(SessionError::ApprovalRequired);
        }
        if self.state != ConnectionState::Authenticating {
            return Err(SessionError::InvalidState);
        }
        if self.expire(now) {
            return Err(SessionError::Expired);
        }
        if peer != self.peer {
            return Err(SessionError::DifferentPeer);
        }
        if session_id == 0 || session_id == self.session_id || fresh_epoch <= self.epoch {
            return Err(SessionError::InvalidEpoch);
        }
        self.session_id = session_id;
        self.epoch = fresh_epoch;
        self.last_input = 0;
        self.disconnected_at = None;
        self.state = ConnectionState::Connected;
        Ok(())
    }

    pub fn accept_input(
        &mut self,
        session_id: u64,
        epoch: u64,
        sequence: u64,
    ) -> Result<(), SessionError> {
        if self.state != ConnectionState::Connected {
            return Err(SessionError::InvalidState);
        }
        if session_id != self.session_id || epoch != self.epoch || sequence <= self.last_input {
            return Err(SessionError::StaleInput);
        }
        self.last_input = sequence;
        Ok(())
    }

    /// The caller must release all held OS keys when this succeeds.
    pub fn link_lost(&mut self, now: Duration) -> Result<(), SessionError> {
        if self.state != ConnectionState::Connected {
            return Err(SessionError::InvalidState);
        }
        self.state = ConnectionState::Reconnecting;
        self.disconnected_at = Some(now);
        Ok(())
    }

    pub fn begin_resume(&mut self, peer: PeerIdentity, now: Duration) -> Result<(), SessionError> {
        if self.state != ConnectionState::Reconnecting {
            return Err(SessionError::InvalidState);
        }
        if peer != self.peer {
            return Err(SessionError::DifferentPeer);
        }
        if self.expire(now) {
            return Err(SessionError::Expired);
        }
        self.state = ConnectionState::Authenticating;
        Ok(())
    }

    /// Retry failed handshakes without extending the original reconnect deadline.
    pub fn resume_failed(&mut self) -> Result<(), SessionError> {
        if self.state != ConnectionState::Authenticating || self.disconnected_at.is_none() {
            return Err(SessionError::InvalidState);
        }
        self.state = ConnectionState::Reconnecting;
        Ok(())
    }

    /// Returns true once the display restoration deadline has passed.
    pub fn expire(&mut self, now: Duration) -> bool {
        let expired = self.disconnected_at.is_some_and(|at| {
            now.checked_sub(at)
                .is_none_or(|elapsed| elapsed > Duration::from_secs(RECONNECT_GRACE_SECONDS))
        });
        if expired {
            self.close();
        }
        expired
    }

    pub fn close(&mut self) {
        self.state = ConnectionState::Closed;
        self.approved = false;
        self.disconnected_at = None;
        self.last_input = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn peer() -> PeerIdentity {
        PeerIdentity {
            public_key: [7; 32],
            user_session: 1,
        }
    }
    fn connected() -> Session {
        let mut s = Session::discovered(peer());
        s.approve().unwrap();
        s.authenticated(peer(), 1, 1, Duration::ZERO).unwrap();
        s
    }
    #[test]
    fn approval_and_authentication_are_both_required() {
        let mut s = Session::discovered(peer());
        assert_eq!(
            s.authenticated(peer(), 1, 1, Duration::ZERO),
            Err(SessionError::ApprovalRequired)
        );
        assert!(s.accept_input(1, 1, 1).is_err());
        s.approve().unwrap();
        assert!(s.accept_input(1, 1, 1).is_err());
    }
    #[test]
    fn stale_duplicate_and_foreign_session_inputs_are_rejected() {
        let mut s = connected();
        assert!(s.accept_input(1, 1, 1).is_ok());
        assert!(s.accept_input(1, 1, 1).is_err());
        assert!(s.accept_input(2, 1, 2).is_err());
        assert!(s.accept_input(1, 0, 2).is_err());
        s.link_lost(Duration::from_secs(10)).unwrap();
        assert!(s.accept_input(1, 1, 2).is_err());
        s.begin_resume(peer(), Duration::from_secs(130)).unwrap();
        assert!(s
            .authenticated(peer(), 2, 1, Duration::from_secs(130))
            .is_err());
        s.authenticated(peer(), 2, 2, Duration::from_secs(130))
            .unwrap();
        assert!(s.accept_input(1, 1, 3).is_err());
        assert!(s.accept_input(2, 2, 1).is_ok());
    }
    #[test]
    fn manual_close_never_resumes() {
        let mut s = connected();
        s.close();
        assert!(s.begin_resume(peer(), Duration::ZERO).is_err());
        assert!(s
            .authenticated(peer(), 2, 2, Duration::from_secs(130))
            .is_err());
    }
    #[test]
    fn retry_does_not_extend_deadline() {
        let mut s = connected();
        s.link_lost(Duration::ZERO).unwrap();
        s.begin_resume(peer(), Duration::from_secs(90)).unwrap();
        s.resume_failed().unwrap();
        assert_eq!(
            s.begin_resume(peer(), Duration::from_secs(121)),
            Err(SessionError::Expired)
        );
        assert_eq!(s.state(), ConnectionState::Closed);
    }
    #[test]
    fn handshake_completion_after_deadline_is_rejected() {
        let mut s = connected();
        s.link_lost(Duration::ZERO).unwrap();
        s.begin_resume(peer(), Duration::from_secs(119)).unwrap();
        assert_eq!(
            s.authenticated(peer(), 2, 2, Duration::from_secs(121)),
            Err(SessionError::Expired)
        );
        assert_eq!(s.state(), ConnectionState::Closed);
    }
    #[test]
    fn different_key_or_windows_user_cannot_resume() {
        let mut s = connected();
        s.link_lost(Duration::ZERO).unwrap();
        for p in [
            PeerIdentity {
                public_key: [8; 32],
                ..peer()
            },
            PeerIdentity {
                user_session: 2,
                ..peer()
            },
        ] {
            assert_eq!(
                s.begin_resume(p, Duration::ZERO),
                Err(SessionError::DifferentPeer)
            );
        }
    }
    #[test]
    fn rollback_of_monotonic_clock_expires() {
        let mut s = connected();
        s.link_lost(Duration::from_secs(10)).unwrap();
        assert!(s.expire(Duration::ZERO));
    }
}
