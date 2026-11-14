//! Versioned record merge for settings and credential sync (spec 8.3).
//!
//! Every synced record (host, group, identity, credential, snippet, key-bar
//! profile, known host) is a set of fields. Each field carries a stamp
//! `(time, device)` and merges last-writer-wins, with the device id as a
//! deterministic tie-break. Field values are opaque here: in practice they are
//! ciphertext produced by the vault, so the server never sees plaintext.
//! A version vector records which device edits a replica has seen, so the UI
//! can tell "newer" from "edited on two devices at once".

use std::cmp::Ordering;
use std::collections::BTreeMap;

use crate::{DeviceId, Id};

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Stamp {
    pub at: u64,
    pub device: DeviceId,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct VersionVector(pub BTreeMap<DeviceId, u64>);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Causality {
    Equal,
    Newer,
    Older,
    Concurrent,
}

impl VersionVector {
    pub fn bump(&mut self, device: &str) {
        *self.0.entry(device.to_string()).or_insert(0) += 1;
    }

    pub fn merge(&mut self, other: &VersionVector) {
        for (d, n) in &other.0 {
            let v = self.0.entry(d.clone()).or_insert(0);
            *v = (*v).max(*n);
        }
    }

    /// How `self` relates to `other`.
    pub fn compare(&self, other: &VersionVector) -> Causality {
        let mut greater = false;
        let mut less = false;
        let keys = self.0.keys().chain(other.0.keys());
        for k in keys {
            let a = self.0.get(k).copied().unwrap_or(0);
            let b = other.0.get(k).copied().unwrap_or(0);
            match a.cmp(&b) {
                Ordering::Greater => greater = true,
                Ordering::Less => less = true,
                Ordering::Equal => {}
            }
        }
        match (greater, less) {
            (false, false) => Causality::Equal,
            (true, false) => Causality::Newer,
            (false, true) => Causality::Older,
            (true, true) => Causality::Concurrent,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Field {
    pub value: Vec<u8>,
    pub stamp: Stamp,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SyncRecord {
    pub id: Id,
    pub kind: String,
    pub fields: BTreeMap<String, Field>,
    pub version: VersionVector,
    /// Deleted at this stamp; a later field edit resurrects the record.
    pub tombstone: Option<Stamp>,
    /// "This device only" credentials never leave the device (spec 8.1).
    pub device_only: bool,
}

impl SyncRecord {
    pub fn new(id: impl Into<Id>, kind: impl Into<String>) -> Self {
        SyncRecord {
            id: id.into(),
            kind: kind.into(),
            fields: BTreeMap::new(),
            version: VersionVector::default(),
            tombstone: None,
            device_only: false,
        }
    }

    pub fn set(&mut self, name: &str, value: impl Into<Vec<u8>>, at: u64, device: &str) {
        self.fields.insert(name.to_string(), Field { value: value.into(), stamp: Stamp { at, device: device.to_string() } });
        self.version.bump(device);
    }

    pub fn delete(&mut self, at: u64, device: &str) {
        self.tombstone = Some(Stamp { at, device: device.to_string() });
        self.version.bump(device);
    }

    fn latest_edit(&self) -> Option<&Stamp> {
        self.fields.values().map(|f| &f.stamp).max()
    }

    pub fn is_deleted(&self) -> bool {
        match (&self.tombstone, self.latest_edit()) {
            (Some(t), Some(e)) => t >= e,
            (Some(_), None) => true,
            (None, _) => false,
        }
    }

    /// Merges a remote replica of the same record. Returns how the remote
    /// related to us before merging (the UI flags `Concurrent` edits).
    pub fn merge(&mut self, remote: &SyncRecord) -> Causality {
        assert_eq!(self.id, remote.id, "merging different records");
        let relation = remote.version.compare(&self.version);
        for (name, theirs) in &remote.fields {
            match self.fields.get(name) {
                Some(mine) if mine.stamp >= theirs.stamp => {}
                _ => {
                    self.fields.insert(name.clone(), theirs.clone());
                }
            }
        }
        self.tombstone = match (&self.tombstone, &remote.tombstone) {
            (Some(a), Some(b)) => Some(a.max(b).clone()),
            (a, b) => a.clone().or_else(|| b.clone()),
        };
        self.version.merge(&remote.version);
        relation
    }
}

/// Records that may be uploaded: everything except "this device only".
pub fn outbound<'a>(records: impl IntoIterator<Item = &'a SyncRecord>) -> Vec<&'a SyncRecord> {
    records.into_iter().filter(|r| !r.device_only).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn field_lww_and_commutativity() {
        let mut phone = SyncRecord::new("h1", "host");
        phone.set("address", "192.168.0.19", 10, "phone");
        let mut desk = phone.clone();
        phone.set("label", "Bronze", 20, "phone");
        desk.set("port", "2222", 15, "desk");
        desk.set("label", "bronze-nas", 25, "desk");

        let mut a = phone.clone();
        let rel = a.merge(&desk);
        let mut b = desk.clone();
        b.merge(&phone);
        assert_eq!(rel, Causality::Concurrent);
        assert_eq!(a.fields, b.fields);
        assert_eq!(a.fields["label"].value, b"bronze-nas", "later edit wins");
        assert_eq!(a.fields["port"].value, b"2222", "non-conflicting fields both kept");
        assert_eq!(a.version, b.version);

        let before = a.clone();
        a.merge(&desk);
        assert_eq!(a, before, "idempotent");
    }

    #[test]
    fn tie_break_by_device() {
        let mut x = SyncRecord::new("r", "snippet");
        let mut y = x.clone();
        x.set("body", "ls", 5, "aaa");
        y.set("body", "ls -la", 5, "zzz");
        x.merge(&y);
        assert_eq!(x.fields["body"].value, b"ls -la");
    }

    #[test]
    fn causality() {
        let mut a = SyncRecord::new("r", "host");
        a.set("f", "1", 1, "phone");
        let mut b = a.clone();
        b.set("f", "2", 2, "phone");
        assert_eq!(a.clone().merge(&b), Causality::Newer);
        assert_eq!(b.clone().merge(&a), Causality::Older);
        assert_eq!(a.clone().merge(&a), Causality::Equal);
    }

    #[test]
    fn delete_and_resurrect() {
        let mut a = SyncRecord::new("r", "host");
        a.set("address", "10.0.0.1", 1, "phone");
        let mut b = a.clone();
        b.delete(5, "desk");
        a.merge(&b);
        assert!(a.is_deleted());
        a.set("address", "10.0.0.2", 9, "phone");
        assert!(!a.is_deleted());
    }

    #[test]
    fn device_only_never_uploads() {
        let mut root_pw = SyncRecord::new("c1", "credential");
        root_pw.device_only = true;
        let normal = SyncRecord::new("c2", "credential");
        let all = [root_pw, normal];
        let out = outbound(&all);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].id, "c2");
    }
}
