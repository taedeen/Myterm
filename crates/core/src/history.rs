//! Per-device command history with conflict-free sync merge (spec 8.2).
//!
//! Each device only ever increments its own counter for a command, so
//! merging two replicas is a per-device `max` (a grow-only counter). The
//! last-used time takes the latest value, and deletions are tombstones that
//! win until the command is run again afterwards. Merging is commutative,
//! associative and idempotent, so devices can sync in any order.

use std::collections::{BTreeMap, HashMap};

use crate::prompt::PromptKind;
use crate::scrub;
use crate::{DeviceId, Id};

pub const DEFAULT_CAP_PER_HOST: usize = 5_000;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Entry {
    /// Run count per device (G-counter).
    pub counts: BTreeMap<DeviceId, u64>,
    pub last_used: u64,
    /// Time of the latest "delete everywhere"; entry is hidden while >= last_used.
    pub deleted_at: Option<u64>,
}

impl Entry {
    pub fn total(&self) -> u64 {
        self.counts.values().sum()
    }

    pub fn is_live(&self) -> bool {
        match self.deleted_at {
            Some(d) => self.last_used > d,
            None => true,
        }
    }

    fn merge(&mut self, other: &Entry) {
        for (dev, n) in &other.counts {
            let mine = self.counts.entry(dev.clone()).or_insert(0);
            *mine = (*mine).max(*n);
        }
        self.last_used = self.last_used.max(other.last_used);
        self.deleted_at = match (self.deleted_at, other.deleted_at) {
            (Some(a), Some(b)) => Some(a.max(b)),
            (a, b) => a.or(b),
        };
    }
}

/// Why a command was not recorded.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Skip {
    Empty,
    Incognito,
    HiddenInput,
    Secret(scrub::SecretReason),
    LeadingSpace,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct RecordContext {
    pub incognito: bool,
    /// The prompt shown before the user typed (from [`crate::prompt::detect`]).
    pub prompt: Option<PromptKind>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HistoryStore {
    pub device_id: DeviceId,
    pub cap_per_host: usize,
    hosts: HashMap<Id, HashMap<String, Entry>>,
}

impl HistoryStore {
    pub fn new(device_id: impl Into<DeviceId>) -> Self {
        HistoryStore { device_id: device_id.into(), cap_per_host: DEFAULT_CAP_PER_HOST, hosts: HashMap::new() }
    }

    /// Records a command run on `host_id` at `now` (unix seconds).
    pub fn record(&mut self, host_id: &str, command: &str, now: u64, ctx: RecordContext) -> Result<(), Skip> {
        if ctx.incognito {
            return Err(Skip::Incognito);
        }
        if ctx.prompt.is_some_and(|p| p.is_hidden()) {
            return Err(Skip::HiddenInput);
        }
        // Shell convention (HISTCONTROL=ignorespace): leading space = don't remember.
        if command.starts_with(' ') {
            return Err(Skip::LeadingSpace);
        }
        let cmd = command.trim();
        if cmd.is_empty() {
            return Err(Skip::Empty);
        }
        if let Some(reason) = scrub::secret_reason(cmd) {
            return Err(Skip::Secret(reason));
        }
        let entries = self.hosts.entry(host_id.to_string()).or_default();
        let e = entries.entry(cmd.to_string()).or_default();
        *e.counts.entry(self.device_id.clone()).or_insert(0) += 1;
        e.last_used = e.last_used.max(now);
        let cap = self.cap_per_host;
        Self::enforce_cap(entries, cap, now);
        Ok(())
    }

    /// "Purge everywhere": tombstone a command on one host.
    pub fn delete(&mut self, host_id: &str, command: &str, now: u64) {
        let e = self.hosts.entry(host_id.to_string()).or_default().entry(command.trim().to_string()).or_default();
        e.deleted_at = Some(e.deleted_at.unwrap_or(0).max(now));
    }

    /// Clears a host's history on every device (tombstones all its entries).
    pub fn clear_host(&mut self, host_id: &str, now: u64) {
        if let Some(entries) = self.hosts.get_mut(host_id) {
            for e in entries.values_mut() {
                e.deleted_at = Some(e.deleted_at.unwrap_or(0).max(now));
            }
        }
    }

    pub fn merge(&mut self, other: &HistoryStore) {
        for (host, theirs) in &other.hosts {
            let mine = self.hosts.entry(host.clone()).or_default();
            for (cmd, e) in theirs {
                mine.entry(cmd.clone()).or_default().merge(e);
            }
        }
    }

    /// Copy suitable for upload: hosts with sync off are left out.
    pub fn sync_snapshot(&self, include_host: impl Fn(&str) -> bool) -> HistoryStore {
        HistoryStore {
            device_id: self.device_id.clone(),
            cap_per_host: self.cap_per_host,
            hosts: self.hosts.iter().filter(|(h, _)| include_host(h)).map(|(h, e)| (h.clone(), e.clone())).collect(),
        }
    }

    /// Live entries for a host.
    pub fn entries(&self, host_id: &str) -> impl Iterator<Item = (&str, &Entry)> {
        self.hosts
            .get(host_id)
            .into_iter()
            .flat_map(|m| m.iter())
            .filter(|(_, e)| e.is_live())
            .map(|(c, e)| (c.as_str(), e))
    }

    pub(crate) fn raw_hosts(&self) -> &HashMap<Id, HashMap<String, Entry>> {
        &self.hosts
    }

    pub(crate) fn insert_raw(&mut self, host_id: &str, command: String, entry: Entry) {
        self.hosts.entry(host_id.to_string()).or_default().insert(command, entry);
    }

    /// Frecency: run count weighted by a 7-day half-life.
    pub fn score(entry: &Entry, now: u64) -> f64 {
        let age_days = now.saturating_sub(entry.last_used) as f64 / 86_400.0;
        (1.0 + entry.total() as f64).ln() * 0.5f64.powf(age_days / 7.0)
    }

    fn enforce_cap(entries: &mut HashMap<String, Entry>, cap: usize, now: u64) {
        let live = entries.values().filter(|e| e.is_live()).count();
        if live <= cap {
            return;
        }
        let mut ranked: Vec<(String, f64)> = entries
            .iter()
            .filter(|(_, e)| e.is_live())
            .map(|(c, e)| (c.clone(), Self::score(e, now)))
            .collect();
        ranked.sort_by(|a, b| a.1.partial_cmp(&b.1).unwrap_or(std::cmp::Ordering::Equal));
        for (cmd, _) in ranked.into_iter().take(live - cap) {
            entries.remove(&cmd);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const DAY: u64 = 86_400;

    fn ctx() -> RecordContext {
        RecordContext::default()
    }

    #[test]
    fn records_and_skips() {
        let mut h = HistoryStore::new("phone");
        assert!(h.record("server1", "docker ps", 100, ctx()).is_ok());
        assert_eq!(h.record("server1", "  ", 100, ctx()), Err(Skip::LeadingSpace));
        assert_eq!(h.record("server1", "", 100, ctx()), Err(Skip::Empty));
        assert_eq!(h.record("server1", "ls", 100, RecordContext { incognito: true, prompt: None }), Err(Skip::Incognito));
        assert_eq!(
            h.record("server1", "hunter2", 100, RecordContext { incognito: false, prompt: Some(PromptKind::Sudo) }),
            Err(Skip::HiddenInput)
        );
        assert!(matches!(h.record("server1", "mysql -pX db", 100, ctx()), Err(Skip::Secret(_))));
        assert_eq!(h.entries("server1").count(), 1);
    }

    #[test]
    fn merge_is_order_independent_and_idempotent() {
        let mut phone = HistoryStore::new("phone");
        let mut desk = HistoryStore::new("desk");
        phone.record("server1", "docker ps", 10, ctx()).unwrap();
        phone.record("server1", "docker ps", 20, ctx()).unwrap();
        desk.record("server1", "docker ps", 30, ctx()).unwrap();
        desk.record("server1", "htop", 40, ctx()).unwrap();

        let mut a = phone.clone();
        a.merge(&desk);
        let mut b = desk.clone();
        b.merge(&phone);
        let mut again = a.clone();
        again.merge(&desk);
        again.merge(&phone);

        let get = |s: &HistoryStore, c: &str| s.entries("server1").find(|(k, _)| *k == c).map(|(_, e)| e.clone()).unwrap();
        assert_eq!(get(&a, "docker ps").total(), 3, "no double counting");
        assert_eq!(get(&a, "docker ps"), get(&b, "docker ps"));
        assert_eq!(get(&a, "docker ps"), get(&again, "docker ps"));
        assert_eq!(get(&a, "docker ps").last_used, 30);
        assert_eq!(a.entries("server1").count(), 2);
    }

    #[test]
    fn tombstone_wins_until_used_again() {
        let mut phone = HistoryStore::new("phone");
        phone.record("server1", "rm -rf build", 10, ctx()).unwrap();
        let mut desk = phone.clone();
        desk.device_id = "desk".into();
        desk.delete("server1", "rm -rf build", 20);

        phone.merge(&desk);
        assert_eq!(phone.entries("server1").count(), 0, "deletion propagates");

        phone.record("server1", "rm -rf build", 30, ctx()).unwrap();
        desk.merge(&phone);
        assert_eq!(desk.entries("server1").count(), 1, "running it again revives it");
    }

    #[test]
    fn snapshot_respects_host_opt_out() {
        let mut h = HistoryStore::new("phone");
        h.record("server1", "ls", 1, ctx()).unwrap();
        h.record("bank", "ls", 1, ctx()).unwrap();
        let snap = h.sync_snapshot(|host| host != "bank");
        assert_eq!(snap.entries("server1").count(), 1);
        assert_eq!(snap.entries("bank").count(), 0);
    }

    #[test]
    fn frecency_and_cap() {
        let e_new = Entry { counts: BTreeMap::from([("p".into(), 3)]), last_used: 100 * DAY, deleted_at: None };
        let e_old = Entry { counts: BTreeMap::from([("p".into(), 3)]), last_used: 86 * DAY, deleted_at: None };
        let now = 100 * DAY;
        let ratio = HistoryStore::score(&e_old, now) / HistoryStore::score(&e_new, now);
        assert!((ratio - 0.25).abs() < 1e-9, "two half-lives = quarter score");

        let mut h = HistoryStore::new("p");
        h.cap_per_host = 2;
        h.record("x", "a", 1, ctx()).unwrap();
        h.record("x", "b", 2 * DAY, ctx()).unwrap();
        h.record("x", "b", 2 * DAY, ctx()).unwrap();
        h.record("x", "c", 3 * DAY, ctx()).unwrap();
        let mut left: Vec<_> = h.entries("x").map(|(c, _)| c.to_string()).collect();
        left.sort();
        assert_eq!(left, vec!["b", "c"], "lowest-scoring entry evicted");
    }
}
