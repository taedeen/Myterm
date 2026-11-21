//! Trust-on-first-use host key checks (spec 3.4, threat model 6.2).
//!
//! A changed host key is a hard stop: the session must not continue until the
//! user explicitly approves the new key after seeing both fingerprints.

use crate::sshkey;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KnownHost {
    /// OpenSSH form: `host` for port 22, `[host]:port` otherwise.
    pub pattern: String,
    pub key_type: String,
    pub key_blob: Vec<u8>,
    pub approved_at: u64,
    pub approved_by_device: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Verdict {
    Trusted,
    /// Never seen: show fingerprint, ask to trust.
    Unknown { fingerprint: String },
    /// Host known, but not with this key type (e.g. server added ed25519). Ask, with context.
    NewKeyType { fingerprint: String, known_types: Vec<String> },
    /// Same key type, different key: possible MITM. Hard stop.
    Changed { expected: String, presented: String },
}

pub fn pattern(host: &str, port: u16) -> String {
    let host = host.trim().to_ascii_lowercase();
    if port == 22 {
        host
    } else {
        format!("[{host}]:{port}")
    }
}

pub fn check(store: &[KnownHost], host: &str, port: u16, presented_blob: &[u8]) -> Verdict {
    let pat = pattern(host, port);
    let presented_type = sshkey::key_type(presented_blob).unwrap_or_default();
    let presented_fp = sshkey::fingerprint_sha256(presented_blob);
    let for_host: Vec<&KnownHost> = store.iter().filter(|k| k.pattern == pat).collect();
    if for_host.is_empty() {
        return Verdict::Unknown { fingerprint: presented_fp };
    }
    if for_host.iter().any(|k| k.key_blob == presented_blob) {
        return Verdict::Trusted;
    }
    if let Some(same_type) = for_host.iter().find(|k| k.key_type == presented_type) {
        return Verdict::Changed {
            expected: sshkey::fingerprint_sha256(&same_type.key_blob),
            presented: presented_fp,
        };
    }
    let mut known_types: Vec<String> = for_host.iter().map(|k| k.key_type.clone()).collect();
    known_types.sort();
    known_types.dedup();
    Verdict::NewKeyType { fingerprint: presented_fp, known_types }
}

/// Imports lines from an OpenSSH known_hosts file. Hashed (`|1|`) entries
/// and markers (`@cert-authority`, `@revoked`) are skipped and counted.
pub fn import_openssh(text: &str, now: u64, device: &str) -> (Vec<KnownHost>, usize) {
    let mut out = Vec::new();
    let mut skipped = 0;
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if line.starts_with('|') || line.starts_with('@') {
            skipped += 1;
            continue;
        }
        let mut parts = line.splitn(2, char::is_whitespace);
        let hosts = parts.next().unwrap_or_default();
        let rest = parts.next().unwrap_or_default().trim();
        match sshkey::parse_authorized_key(rest) {
            Ok((key_type, blob, _)) => {
                for h in hosts.split(',') {
                    out.push(KnownHost {
                        pattern: h.to_ascii_lowercase(),
                        key_type: key_type.clone(),
                        key_blob: blob.clone(),
                        approved_at: now,
                        approved_by_device: device.to_string(),
                    });
                }
            }
            Err(_) => skipped += 1,
        }
    }
    (out, skipped)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sshkey::{ecdsa_p256_blob, ed25519_blob};

    fn ed(b: u8) -> Vec<u8> {
        ed25519_blob(&[b; 32])
    }

    fn known(pat: &str, blob: Vec<u8>) -> KnownHost {
        KnownHost {
            pattern: pat.into(),
            key_type: sshkey::key_type(&blob).unwrap(),
            key_blob: blob,
            approved_at: 0,
            approved_by_device: "desk".into(),
        }
    }

    #[test]
    fn verdicts() {
        let store = vec![known("192.0.2.19", ed(1)), known("[nas.lan]:2222", ed(2))];
        assert_eq!(check(&store, "192.0.2.19", 22, &ed(1)), Verdict::Trusted);
        assert_eq!(check(&store, "NAS.lan", 2222, &ed(2)), Verdict::Trusted, "case-insensitive, port form");
        assert!(matches!(check(&store, "nas.lan", 22, &ed(2)), Verdict::Unknown { .. }), "different port is a different host");
        match check(&store, "192.0.2.19", 22, &ed(9)) {
            Verdict::Changed { expected, presented } => assert_ne!(expected, presented),
            v => panic!("expected Changed, got {v:?}"),
        }
        let mut point = vec![4u8];
        point.extend([3u8; 64]);
        let ecdsa = ecdsa_p256_blob(&point).unwrap();
        assert!(matches!(check(&store, "192.0.2.19", 22, &ecdsa), Verdict::NewKeyType { .. }));
    }

    #[test]
    fn imports_openssh_file() {
        let blob = ed(5);
        let line = sshkey::authorized_keys_line(&blob, "").unwrap();
        let text = format!(
            "# comment\nweb1,192.0.2.21 {line}\n|1|hashed= {line}\n@revoked * {line}\nbad line here\n"
        );
        let (hosts, skipped) = import_openssh(&text, 7, "desk");
        assert_eq!(hosts.len(), 2);
        assert_eq!(hosts[1].pattern, "192.0.2.21");
        assert_eq!(skipped, 3);
        assert_eq!(check(&hosts, "web1", 22, &blob), Verdict::Trusted);
    }
}
