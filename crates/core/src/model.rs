//! Hosts, groups and inheritance (spec 3.1).
//!
//! A host inherits settings from its group, which inherits from its parent
//! group, and so on to the root. The nearest value wins; environment
//! variables are merged with the nearest definition of each name winning.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::fmt;

use crate::Id;

pub const DEFAULT_SSH_PORT: u16 = 22;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum Protocol {
    #[default]
    Ssh,
    Mosh,
}

/// Settings that can be set on a group and overridden on a host.
/// `None` means "inherit".
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Overrides {
    pub port: Option<u16>,
    pub identity_id: Option<Id>,
    pub jump_chain: Option<Vec<Id>>,
    pub keybar_profile_id: Option<Id>,
    pub startup_snippet_id: Option<Id>,
    pub env: Vec<(String, String)>,
    /// Refuse software keys for this host (spec 3.4.1 "hardware keys only").
    pub hardware_keys_only: Option<bool>,
    /// Include this host's command history in sync (spec 8.2).
    pub sync_history: Option<bool>,
    /// Allow remote-aware completion side channel (spec 3.3).
    pub remote_completion: Option<bool>,
    /// Agent forwarding is off unless explicitly enabled (spec 3.4).
    pub agent_forwarding: Option<bool>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Group {
    pub id: Id,
    pub name: String,
    pub parent_id: Option<Id>,
    pub defaults: Overrides,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct Host {
    pub id: Id,
    pub label: String,
    pub address: String,
    pub protocol: Protocol,
    pub group_id: Option<Id>,
    pub tags: Vec<String>,
    /// Detected from /etc/os-release after first connect, e.g. "debian".
    pub os: Option<String>,
    pub settings: Overrides,
}

/// Fully resolved connection settings for one host.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ResolvedHost {
    pub host_id: Id,
    pub address: String,
    pub port: u16,
    pub protocol: Protocol,
    pub identity_id: Option<Id>,
    pub jump_chain: Vec<Id>,
    pub keybar_profile_id: Option<Id>,
    pub startup_snippet_id: Option<Id>,
    pub env: BTreeMap<String, String>,
    pub hardware_keys_only: bool,
    pub sync_history: bool,
    pub remote_completion: bool,
    pub agent_forwarding: bool,
    /// Group ids from nearest to root, used to find related hosts for completion.
    pub group_path: Vec<Id>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum ModelError {
    MissingGroup(Id),
    GroupCycle(Id),
    JumpCycle(Id),
    EmptyAddress(Id),
}

impl fmt::Display for ModelError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            ModelError::MissingGroup(id) => write!(f, "group {id} does not exist"),
            ModelError::GroupCycle(id) => write!(f, "group {id} is its own ancestor"),
            ModelError::JumpCycle(id) => write!(f, "host {id} appears in its own jump chain"),
            ModelError::EmptyAddress(id) => write!(f, "host {id} has no address"),
        }
    }
}

impl std::error::Error for ModelError {}

/// Returns the group chain for a host, nearest group first.
fn group_chain<'a>(
    host: &Host,
    groups: &'a HashMap<Id, Group>,
) -> Result<Vec<&'a Group>, ModelError> {
    let mut chain = Vec::new();
    let mut seen = HashSet::new();
    let mut next = host.group_id.clone();
    while let Some(gid) = next {
        if !seen.insert(gid.clone()) {
            return Err(ModelError::GroupCycle(gid));
        }
        let group = groups.get(&gid).ok_or(ModelError::MissingGroup(gid))?;
        chain.push(group);
        next = group.parent_id.clone();
    }
    Ok(chain)
}

/// Resolves a host against its group hierarchy.
pub fn resolve(host: &Host, groups: &HashMap<Id, Group>) -> Result<ResolvedHost, ModelError> {
    if host.address.trim().is_empty() {
        return Err(ModelError::EmptyAddress(host.id.clone()));
    }
    let chain = group_chain(host, groups)?;

    // Layers from nearest (host) to farthest (root group).
    let layers: Vec<&Overrides> = std::iter::once(&host.settings)
        .chain(chain.iter().map(|g| &g.defaults))
        .collect();

    fn first<T: Clone>(layers: &[&Overrides], get: impl Fn(&Overrides) -> Option<T>) -> Option<T> {
        layers.iter().find_map(|l| get(l))
    }

    let jump_chain = first(&layers, |l| l.jump_chain.clone()).unwrap_or_default();
    if jump_chain.iter().any(|j| j == &host.id) {
        return Err(ModelError::JumpCycle(host.id.clone()));
    }

    // Env: apply root first so nearer layers overwrite.
    let mut env = BTreeMap::new();
    for layer in layers.iter().rev() {
        for (k, v) in &layer.env {
            env.insert(k.clone(), v.clone());
        }
    }

    Ok(ResolvedHost {
        host_id: host.id.clone(),
        address: host.address.trim().to_string(),
        port: first(&layers, |l| l.port).unwrap_or(DEFAULT_SSH_PORT),
        protocol: host.protocol,
        identity_id: first(&layers, |l| l.identity_id.clone()),
        jump_chain,
        keybar_profile_id: first(&layers, |l| l.keybar_profile_id.clone()),
        startup_snippet_id: first(&layers, |l| l.startup_snippet_id.clone()),
        env,
        hardware_keys_only: first(&layers, |l| l.hardware_keys_only).unwrap_or(false),
        sync_history: first(&layers, |l| l.sync_history).unwrap_or(true),
        remote_completion: first(&layers, |l| l.remote_completion).unwrap_or(false),
        agent_forwarding: first(&layers, |l| l.agent_forwarding).unwrap_or(false),
        group_path: chain.iter().map(|g| g.id.clone()).collect(),
    })
}

/// Hosts related to `host` for completion ranking (spec 3.3 rank 3):
/// same nearest group, or same detected OS. Excludes the host itself.
pub fn related_hosts<'a>(host: &Host, all: impl IntoIterator<Item = &'a Host>) -> Vec<Id> {
    all.into_iter()
        .filter(|h| h.id != host.id)
        .filter(|h| {
            let same_group = host.group_id.is_some() && h.group_id == host.group_id;
            let same_os = host.os.is_some() && h.os == host.os;
            same_group || same_os
        })
        .map(|h| h.id.clone())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn groups() -> HashMap<Id, Group> {
        let root = Group {
            id: "root".into(),
            name: "Homelab".into(),
            parent_id: None,
            defaults: Overrides {
                port: Some(2222),
                identity_id: Some("id-admin".into()),
                env: vec![("LANG".into(), "en_GB.UTF-8".into()), ("TZ".into(), "UTC".into())],
                hardware_keys_only: Some(true),
                ..Default::default()
            },
        };
        let nas = Group {
            id: "nas".into(),
            name: "NAS".into(),
            parent_id: Some("root".into()),
            defaults: Overrides {
                jump_chain: Some(vec!["bastion".into()]),
                env: vec![("TZ".into(), "Europe/London".into())],
                ..Default::default()
            },
        };
        HashMap::from([(root.id.clone(), root), (nas.id.clone(), nas)])
    }

    fn host() -> Host {
        Host {
            id: "truenas".into(),
            label: "truenas".into(),
            address: " 192.0.2.59 ".into(),
            group_id: Some("nas".into()),
            settings: Overrides { port: Some(22), ..Default::default() },
            ..Default::default()
        }
    }

    #[test]
    fn nearest_value_wins_and_env_merges() {
        let r = resolve(&host(), &groups()).unwrap();
        assert_eq!(r.address, "192.0.2.59");
        assert_eq!(r.port, 22, "host override beats root group");
        assert_eq!(r.identity_id.as_deref(), Some("id-admin"), "inherited from root");
        assert_eq!(r.jump_chain, vec!["bastion".to_string()], "inherited from nas");
        assert_eq!(r.env["TZ"], "Europe/London", "nearer group wins");
        assert_eq!(r.env["LANG"], "en_GB.UTF-8");
        assert!(r.hardware_keys_only);
        assert_eq!(r.group_path, vec!["nas".to_string(), "root".to_string()]);
    }

    #[test]
    fn safe_defaults_without_groups() {
        let h = Host { id: "h".into(), address: "10.0.0.1".into(), ..Default::default() };
        let r = resolve(&h, &HashMap::new()).unwrap();
        assert_eq!(r.port, DEFAULT_SSH_PORT);
        assert!(!r.agent_forwarding, "agent forwarding off by default");
        assert!(!r.remote_completion, "remote completion is opt-in");
        assert!(r.sync_history);
        assert!(!r.hardware_keys_only);
    }

    #[test]
    fn detects_group_cycle_and_missing_group() {
        let mut g = groups();
        g.get_mut("root").unwrap().parent_id = Some("nas".into());
        assert_eq!(resolve(&host(), &g), Err(ModelError::GroupCycle("nas".into())));

        let mut h = host();
        h.group_id = Some("nope".into());
        assert_eq!(resolve(&h, &groups()), Err(ModelError::MissingGroup("nope".into())));
    }

    #[test]
    fn rejects_self_jump_and_empty_address() {
        let mut h = host();
        h.settings.jump_chain = Some(vec!["truenas".into()]);
        assert_eq!(resolve(&h, &groups()), Err(ModelError::JumpCycle("truenas".into())));
        h.address = "  ".into();
        assert_eq!(resolve(&h, &groups()), Err(ModelError::EmptyAddress("truenas".into())));
    }

    #[test]
    fn related_by_group_or_os() {
        let a = Host { id: "a".into(), group_id: Some("g".into()), os: Some("debian".into()), ..Default::default() };
        let b = Host { id: "b".into(), group_id: Some("g".into()), ..Default::default() };
        let c = Host { id: "c".into(), os: Some("debian".into()), ..Default::default() };
        let d = Host { id: "d".into(), os: Some("ubuntu".into()), ..Default::default() };
        let all = [a.clone(), b, c, d];
        assert_eq!(related_hosts(&a, &all), vec!["b".to_string(), "c".to_string()]);
    }
}
