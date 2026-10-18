use std::time::{Duration, Instant};
use subtle::ConstantTimeEq;

pub const GRACE: Duration = Duration::from_secs(120);
/// The OS controller lease stays owned while waiting for the same authenticated
/// peer. Tickets are memory-only for one approved logical session; each TLS connection and input epoch is fresh.
pub struct Reservation<T> {
    peer: String,
    secret: String,
    deadline: Instant,
    lease: T,
}
pub struct Book<T> {
    entry: Option<Reservation<T>>,
}
impl<T> Default for Book<T> {
    fn default() -> Self {
        Self { entry: None }
    }
}
impl<T> Book<T> {
    pub fn reserve(&mut self, peer: String, secret: String, lease: T, now: Instant) {
        self.reserve_until(peer, secret, lease, now + GRACE);
    }
    pub fn reserve_until(&mut self, peer: String, secret: String, lease: T, deadline: Instant) {
        self.entry = Some(Reservation {
            peer,
            secret,
            deadline,
            lease,
        });
    }
    pub fn expire(&mut self, now: Instant) {
        if self.entry.as_ref().is_some_and(|e| now > e.deadline) {
            self.entry = None;
        }
    }
    pub fn reserved(&mut self, now: Instant) -> bool {
        self.expire(now);
        self.entry.is_some()
    }
    pub fn claim(&mut self, peer: &str, secret: &str, now: Instant) -> Option<T> {
        self.claim_with_deadline(peer, secret, now).map(|v| v.0)
    }
    pub fn claim_with_deadline(
        &mut self,
        peer: &str,
        secret: &str,
        now: Instant,
    ) -> Option<(T, Instant)> {
        self.expire(now);
        let e = self.entry.as_ref()?;
        if peer != e.peer
            || secret.len() != 64
            || !bool::from(e.secret.as_bytes().ct_eq(secret.as_bytes()))
        {
            return None;
        }
        self.entry.take().map(|e| (e.lease, e.deadline))
    }
}
pub fn token() -> crate::Result<String> {
    Ok(format!(
        "{:016x}{:016x}{:016x}{:016x}",
        crate::random_u64()?,
        crate::random_u64()?,
        crate::random_u64()?,
        crate::random_u64()?
    ))
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_reinitialization_keeps_original_deadline() {
        let now = Instant::now();
        let mut book = Book::default();
        book.reserve("p".into(), "a".repeat(64), 7, now);
        let (lease, deadline) = book
            .claim_with_deadline("p", &"a".repeat(64), now + Duration::from_secs(119))
            .unwrap();
        book.reserve_until("p".into(), "a".repeat(64), lease, deadline);
        assert!(book
            .claim("p", &"a".repeat(64), now + Duration::from_secs(121))
            .is_none());
    }
    #[test]
    fn only_same_peer_and_secret_within_grace_can_claim() {
        let now = Instant::now();
        let mut book = Book::default();
        book.reserve("peer".into(), "a".repeat(64), 42, now);
        assert!(book.claim("other", &"a".repeat(64), now).is_none());
        assert!(book.claim("peer", &"b".repeat(64), now).is_none());
        assert_eq!(book.claim("peer", &"a".repeat(64), now + GRACE), Some(42));
        assert!(!book.reserved(now));
    }
    #[test]
    fn deadline_releases_lease_and_cannot_be_extended_by_retries() {
        let now = Instant::now();
        let mut book = Book::default();
        book.reserve("peer".into(), "a".repeat(64), 42, now);
        assert!(book
            .claim("peer", "wrong", now + Duration::from_secs(119))
            .is_none());
        assert_eq!(
            book.claim("peer", &"a".repeat(64), now + Duration::from_secs(121)),
            None
        );
        assert!(!book.reserved(now + GRACE));
    }
}
