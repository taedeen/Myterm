//! `myterm import`: hosts from an OpenSSH config file.
//!
//! Reads `Host` blocks with `HostName`, `User`, `Port` and `ProxyJump` (the
//! first hop, when it names another imported host or an existing one).
//! Wildcard patterns (`Host *`) and `Include` are skipped; keys stay in
//! `~/.ssh` (MyTerm doesn't copy private keys).

use std::collections::HashMap;

use anyhow::Result;
use myterm_ffi::api::HostDto;

#[derive(Debug, Default, Clone, PartialEq)]
pub struct Entry {
    pub alias: String,
    pub hostname: Option<String>,
    pub user: Option<String>,
    pub port: Option<u16>,
    pub proxy_jump: Option<String>,
}

pub fn parse(text: &str) -> Vec<Entry> {
    let mut out: Vec<Entry> = Vec::new();
    let mut current: Vec<usize> = Vec::new();
    for raw in text.lines() {
        let line = raw.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        let (key, value) = match line.split_once(|c: char| c.is_whitespace() || c == '=') {
            Some((k, v)) => (k.to_lowercase(), v.trim().trim_start_matches('=').trim().trim_matches('"').to_string()),
            None => continue,
        };
        match key.as_str() {
            "host" => {
                current.clear();
                for alias in value.split_whitespace() {
                    if alias.contains(['*', '?', '!']) {
                        continue;
                    }
                    out.push(Entry { alias: alias.to_string(), ..Default::default() });
                    current.push(out.len() - 1);
                }
            }
            "match" => current.clear(),
            _ => {
                for &i in &current {
                    let e = &mut out[i];
                    match key.as_str() {
                        "hostname" if e.hostname.is_none() => e.hostname = Some(value.clone()),
                        "user" if e.user.is_none() => e.user = Some(value.clone()),
                        "port" if e.port.is_none() => e.port = value.parse().ok(),
                        "proxyjump" if e.proxy_jump.is_none() => {
                            let first = value.split(',').next().unwrap_or("").trim();
                            // user@host:port forms name a host directly; keep the host part.
                            let host = first.rsplit('@').next().unwrap_or(first).split(':').next().unwrap_or("");
                            if !host.is_empty() && host != "none" {
                                e.proxy_jump = Some(host.to_string());
                            }
                        }
                        _ => {}
                    }
                }
            }
        }
    }
    out
}

/// Adds or updates hosts. Existing hosts are matched by label. Returns
/// (added, updated).
pub fn apply(engine: &myterm_ffi::api::MyTerm, entries: &[Entry]) -> Result<(usize, usize)> {
    let existing = engine.list_hosts();
    let mut ids: HashMap<String, String> = existing.iter().map(|h| (h.label.clone(), h.id.clone())).collect();
    for e in entries {
        ids.entry(e.alias.clone()).or_insert_with(crate::store::new_device_id);
    }
    let (mut added, mut updated) = (0, 0);
    for e in entries {
        let id = ids[&e.alias].clone();
        let old = existing.iter().find(|h| h.id == id);
        let jump = e.proxy_jump.as_ref().and_then(|j| ids.get(j)).cloned();
        let host = HostDto {
            id: id.clone(),
            label: e.alias.clone(),
            address: e.hostname.clone().unwrap_or_else(|| e.alias.clone()),
            port: e.port.or(old.and_then(|h| h.port)),
            username: e.user.clone().or(old.map(|h| h.username.clone())).unwrap_or_default(),
            group_id: old.and_then(|h| h.group_id.clone()),
            credential_id: old.and_then(|h| h.credential_id.clone()),
            hardware_key_id: old.and_then(|h| h.hardware_key_id.clone()),
            tags: old.map(|h| h.tags.clone()).unwrap_or_else(|| vec!["imported".into()]),
            os: old.and_then(|h| h.os.clone()),
            keybar_profile_id: old.and_then(|h| h.keybar_profile_id.clone()),
            hardware_keys_only: old.and_then(|h| h.hardware_keys_only),
            sync_history: old.and_then(|h| h.sync_history),
            jump_chain: jump.map(|j| vec![j]).unwrap_or_else(|| old.map(|h| h.jump_chain.clone()).unwrap_or_default()),
            use_certificate: old.and_then(|h| h.use_certificate),
            persistent: old.and_then(|h| h.persistent),
        };
        engine.upsert_host(host)?;
        if old.is_some() {
            updated += 1;
        } else {
            added += 1;
        }
    }
    Ok((added, updated))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_hosts_and_skips_wildcards() {
        let cfg = "\
Host *
    ServerAliveInterval 30
Host dbb backup
    HostName 10.168.19.20
    User taedeen
    Port 2222
Host pi
    HostName=10.168.20.5
    ProxyJump taedeen@dbb:22
# comment
Match host foo
    User nobody
";
        let e = parse(cfg);
        assert_eq!(e.len(), 3);
        assert_eq!(e[0], Entry { alias: "dbb".into(), hostname: Some("10.168.19.20".into()), user: Some("taedeen".into()), port: Some(2222), proxy_jump: None });
        assert_eq!(e[1].alias, "backup");
        assert_eq!(e[1].port, Some(2222));
        assert_eq!(e[2].hostname.as_deref(), Some("10.168.20.5"));
        assert_eq!(e[2].proxy_jump.as_deref(), Some("dbb"));
        assert_eq!(e[2].user, None, "Match blocks don't leak into the previous host");
    }
}
