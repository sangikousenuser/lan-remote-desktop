use super::*;
use std::cell::{Cell, RefCell};
#[derive(Default)]
struct Store {
    state: RefCell<Option<Snapshot>>,
    writes: Cell<usize>,
    fail_save: Cell<bool>,
    fail_active: Cell<bool>,
}
impl Storage for Store {
    fn status(&self, phase: Phase) -> Result<()> {
        let status = Status::new(phase);
        assert!(status.updated > 0);
        if self.fail_active.get() && matches!(phase, Phase::Active) {
            fail("status disk full")
        } else {
            Ok(())
        }
    }
    fn load(&self) -> Result<Option<Snapshot>> {
        Ok(self.state.borrow().clone())
    }
    fn save(&self, s: Option<&Snapshot>) -> Result<()> {
        if self.fail_save.get() {
            return fail("disk full");
        }
        *self.state.borrow_mut() = s.cloned();
        self.writes.set(self.writes.get() + 1);
        Ok(())
    }
    fn revoked(&self) -> Result<bool> {
        Ok(false)
    }
    fn clear_revoke(&self) -> Result<()> {
        Ok(())
    }
}
struct Network {
    inventory: RefCell<Vec<Adapter>>,
    adds: Cell<u32>,
    removes: Cell<u32>,
    conflict: Cell<bool>,
    dad_fail: Cell<bool>,
    cancel_after_add: Cell<bool>,
}
fn adapter() -> Adapter {
    Adapter {
        index: 7,
        name: "en7".into(),
        label: "Ethernet".into(),
        mac: "02:00:00:00:00:07".into(),
        active: true,
        addresses: vec![(Ipv4Addr::new(169, 254, 12, 34), 16)],
    }
}
impl Default for Network {
    fn default() -> Self {
        Self {
            inventory: RefCell::new(vec![adapter()]),
            adds: Cell::new(0),
            removes: Cell::new(0),
            conflict: Cell::new(false),
            dad_fail: Cell::new(false),
            cancel_after_add: Cell::new(false),
        }
    }
}
impl Backend for Network {
    fn cancelled(&self) -> bool {
        self.cancel_after_add.get() && self.adds.get() > 0
    }
    fn inventory(&self) -> Result<Vec<Adapter>> {
        Ok(self.inventory.borrow().clone())
    }
    fn preflight(&self, _: &Adapter, _: Ipv4Addr) -> Result<()> {
        if self.conflict.get() {
            fail("conflict")
        } else {
            Ok(())
        }
    }
    fn add(&self, s: &Snapshot) -> Result<()> {
        self.adds.set(self.adds.get() + 1);
        self.inventory.borrow_mut()[0]
            .addresses
            .push((s.added, PREFIX));
        if self.dad_fail.get() {
            fail("duplicate")
        } else {
            Ok(())
        }
    }
    fn remove(&self, s: &Snapshot) -> Result<()> {
        self.removes.set(self.removes.get() + 1);
        let mut inventory = self.inventory.borrow_mut();
        let a = inventory
            .iter_mut()
            .find(|a| a.index == s.index && a.mac == s.mac)
            .ok_or("wrong NIC")?;
        a.addresses.retain(|(ip, _)| *ip != s.added);
        Ok(())
    }
}
#[test]
fn cancellation_and_status_failure_after_add_both_restore_before_exit() {
    for fail_status in [false, true] {
        let store = Store::default();
        let net = Network::default();
        net.cancel_after_add.set(true);
        store.fail_active.set(fail_status);
        let result = run(&store, &net, adapter(), MAC_ADDRESS);
        assert_eq!(result.is_err(), fail_status);
        assert_eq!(net.adds.get(), 1);
        assert_eq!(net.removes.get(), 1);
        assert_eq!(net.inventory.borrow()[0].addresses, adapter().addresses);
        assert!(store.load().unwrap().is_none());
    }
}
#[test]
fn prepared_snapshot_with_missing_alias_still_checks_os_row_before_clearing() {
    let store = Store::default();
    let net = Network::default();
    begin(&store, &net, adapter(), WINDOWS_ADDRESS).unwrap();
    net.inventory.borrow_mut()[0]
        .addresses
        .retain(|(ip, _)| *ip != WINDOWS_ADDRESS);
    recover(&store, &net).unwrap();
    assert_eq!(net.removes.get(), 1);
    assert!(store.load().unwrap().is_none());
    let roundtrip: Status =
        serde_json::from_slice(&serde_json::to_vec(&Status::default()).unwrap()).unwrap();
    assert!(matches!(roundtrip.phase, Phase::Idle));
}
#[test]
fn journal_precedes_add_and_recovery_preserves_dhcp_and_manual_additions() {
    let store = Store::default();
    let net = Network::default();
    begin(&store, &net, adapter(), MAC_ADDRESS).unwrap();
    assert_eq!(store.writes.get(), 1);
    // Replug changed OS index; no operation may target the reused old index.
    {
        let mut inv = net.inventory.borrow_mut();
        inv[0].index = 99;
        inv[0].name = "en99".into();
        inv[0].addresses.push((Ipv4Addr::new(192, 168, 1, 10), 24));
        let mut other = adapter();
        other.mac = "02:00:00:00:00:08".into();
        inv.push(other);
    }
    recover(&store, &net).unwrap();
    let inv = net.inventory.borrow();
    assert_eq!(inv[0].index, 99);
    assert_eq!(inv[0].addresses.len(), 2);
    assert_eq!(inv[1].addresses, adapter().addresses);
    assert!(store.load().unwrap().is_none());
}
#[test]
fn conflict_dad_failure_and_disk_failure_do_not_leave_alias() {
    let store = Store::default();
    let net = Network::default();
    net.conflict.set(true);
    assert!(begin(&store, &net, adapter(), WINDOWS_ADDRESS).is_err());
    assert_eq!(net.adds.get(), 0);
    net.conflict.set(false);
    store.fail_save.set(true);
    assert!(begin(&store, &net, adapter(), WINDOWS_ADDRESS).is_err());
    assert_eq!(net.adds.get(), 0);
    store.fail_save.set(false);
    net.dad_fail.set(true);
    assert!(begin(&store, &net, adapter(), WINDOWS_ADDRESS).is_err());
    assert_eq!(net.adds.get(), 1);
    assert_eq!(net.removes.get(), 1);
    assert_eq!(net.inventory.borrow()[0].addresses, adapter().addresses);
}
#[test]
fn recovery_keeps_journal_when_missing_ambiguous_or_manually_changed() {
    let store = Store::default();
    let net = Network::default();
    begin(&store, &net, adapter(), MAC_ADDRESS).unwrap();
    net.inventory.borrow_mut()[0]
        .addresses
        .last_mut()
        .unwrap()
        .1 = 24;
    assert!(recover(&store, &net).is_err());
    assert_eq!(net.removes.get(), 0);
    net.inventory.borrow_mut().clear();
    assert!(recover(&store, &net).is_err());
    assert!(store.load().unwrap().is_some());
    net.inventory.borrow_mut().extend([adapter(), adapter()]);
    assert!(recover(&store, &net).is_err());
    assert_eq!(net.removes.get(), 0);
}
#[test]
fn usable_address_and_route_overlap_cannot_be_reconfigured() {
    let store = Store::default();
    let net = Network::default();
    let mut usable = adapter();
    usable.addresses = vec![(Ipv4Addr::new(192, 168, 1, 2), 24)];
    assert!(begin(&store, &net, usable, MAC_ADDRESS).is_err());
    net.inventory.borrow_mut()[0].addresses = vec![(Ipv4Addr::new(10, 0, 0, 2), 8)];
    assert!(begin(&store, &net, adapter(), MAC_ADDRESS).is_err());
    assert!(overlaps(Ipv4Addr::new(10, 254, 240, 0), 30));
    assert!(overlaps(WINDOWS_ADDRESS, 32));
    assert!(!overlaps(Ipv4Addr::new(10, 254, 240, 4), 30));
    assert!(!overlaps(Ipv4Addr::UNSPECIFIED, 0));
    assert!(!overlaps(Ipv4Addr::new(192, 168, 1, 2), 24));
    assert_eq!(net.adds.get(), 0);
}
