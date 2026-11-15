//! Append-only, hash-chained audit log (spec 6.3).
//!
//! Each event's hash covers the previous hash, so editing or removing an
//! earlier event breaks verification from that point on. Credential values
//! are never recorded, only ids.

use crate::sha256;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum AuditKind {
    Connect,
    Disconnect,
    CredentialTyped,
    HostKeyApproved,
    HostKeyChangedRejected,
    KeyEnrolled,
    DeviceRevoked,
    SettingChanged,
    Sync,
}

impl AuditKind {
    fn as_str(&self) -> &'static str {
        match self {
            AuditKind::Connect => "connect",
            AuditKind::Disconnect => "disconnect",
            AuditKind::CredentialTyped => "credential_typed",
            AuditKind::HostKeyApproved => "host_key_approved",
            AuditKind::HostKeyChangedRejected => "host_key_changed_rejected",
            AuditKind::KeyEnrolled => "key_enrolled",
            AuditKind::DeviceRevoked => "device_revoked",
            AuditKind::SettingChanged => "setting_changed",
            AuditKind::Sync => "sync",
        }
    }

    fn parse(s: &str) -> Option<Self> {
        Some(match s {
            "connect" => AuditKind::Connect,
            "disconnect" => AuditKind::Disconnect,
            "credential_typed" => AuditKind::CredentialTyped,
            "host_key_approved" => AuditKind::HostKeyApproved,
            "host_key_changed_rejected" => AuditKind::HostKeyChangedRejected,
            "key_enrolled" => AuditKind::KeyEnrolled,
            "device_revoked" => AuditKind::DeviceRevoked,
            "setting_changed" => AuditKind::SettingChanged,
            "sync" => AuditKind::Sync,
            _ => return None,
        })
    }
}

/// Reads one `"name":` value from a line written by [`AuditLog::to_jsonl`]:
/// a number, or a string with the escapes `to_jsonl` produces.
fn json_value(line: &str, name: &str) -> Option<String> {
    let key = format!("\"{name}\":");
    let start = line.find(&key)? + key.len();
    let rest = &line[start..];
    if let Some(body) = rest.strip_prefix('"') {
        let mut out = String::new();
        let mut chars = body.chars();
        while let Some(c) = chars.next() {
            match c {
                '"' => return Some(out),
                '\\' => match chars.next()? {
                    'u' => {
                        let hex: String = chars.by_ref().take(4).collect();
                        out.push(char::from_u32(u32::from_str_radix(&hex, 16).ok()?)?);
                    }
                    other => out.push(other),
                },
                c => out.push(c),
            }
        }
        None
    } else {
        Some(rest.chars().take_while(|c| c.is_ascii_digit()).collect())
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AuditEvent {
    pub seq: u64,
    pub at: u64,
    pub kind: AuditKind,
    pub host_id: Option<String>,
    pub credential_id: Option<String>,
    pub detail: String,
    pub prev_hash: [u8; 32],
    pub hash: [u8; 32],
}

fn field(out: &mut Vec<u8>, s: &str) {
    out.extend_from_slice(&(s.len() as u32).to_be_bytes());
    out.extend_from_slice(s.as_bytes());
}

fn compute_hash(e: &AuditEvent) -> [u8; 32] {
    let mut buf = Vec::new();
    buf.extend_from_slice(&e.prev_hash);
    buf.extend_from_slice(&e.seq.to_be_bytes());
    buf.extend_from_slice(&e.at.to_be_bytes());
    field(&mut buf, e.kind.as_str());
    field(&mut buf, e.host_id.as_deref().unwrap_or(""));
    field(&mut buf, e.credential_id.as_deref().unwrap_or(""));
    field(&mut buf, &e.detail);
    sha256::digest(&buf)
}

#[derive(Clone, Debug, Default)]
pub struct AuditLog {
    events: Vec<AuditEvent>,
}

impl AuditLog {
    pub fn append(
        &mut self,
        at: u64,
        kind: AuditKind,
        host_id: Option<&str>,
        credential_id: Option<&str>,
        detail: &str,
    ) -> &AuditEvent {
        let prev_hash = self.events.last().map(|e| e.hash).unwrap_or([0; 32]);
        let mut e = AuditEvent {
            seq: self.events.len() as u64,
            at,
            kind,
            host_id: host_id.map(str::to_string),
            credential_id: credential_id.map(str::to_string),
            detail: detail.to_string(),
            prev_hash,
            hash: [0; 32],
        };
        e.hash = compute_hash(&e);
        self.events.push(e);
        self.events.last().unwrap()
    }

    pub fn events(&self) -> &[AuditEvent] {
        &self.events
    }

    /// Returns the sequence number of the first broken event, if any.
    pub fn verify(events: &[AuditEvent]) -> Result<(), u64> {
        let mut prev = [0u8; 32];
        for (i, e) in events.iter().enumerate() {
            if e.seq != i as u64 || e.prev_hash != prev || compute_hash(e) != e.hash {
                return Err(i as u64);
            }
            prev = e.hash;
        }
        Ok(())
    }

    /// Rebuilds a log saved with [`AuditLog::to_jsonl`] by replaying its
    /// events, which recomputes the hash chain. Returns the log and the
    /// sequence number of the first event whose saved hash doesn't match (the
    /// saved log was altered), if any. Unreadable lines are skipped.
    pub fn from_jsonl(text: &str) -> (AuditLog, Option<u64>) {
        let mut log = AuditLog::default();
        let mut broken = None;
        for line in text.lines().filter(|l| !l.trim().is_empty()) {
            let parsed = (|| {
                let at = json_value(line, "at")?.parse().ok()?;
                let kind = AuditKind::parse(&json_value(line, "kind")?)?;
                let host = json_value(line, "host_id")?;
                let cred = json_value(line, "credential_id")?;
                let detail = json_value(line, "detail")?;
                let hash = json_value(line, "hash")?;
                Some((at, kind, host, cred, detail, hash))
            })();
            let Some((at, kind, host, cred, detail, hash)) = parsed else { continue };
            let opt = |s: &str| if s.is_empty() { None } else { Some(s.to_string()) };
            let (host, cred) = (opt(&host), opt(&cred));
            let e = log.append(at, kind, host.as_deref(), cred.as_deref(), &detail);
            if broken.is_none() && sha256::hex(&e.hash) != hash {
                broken = Some(e.seq);
            }
        }
        (log, broken)
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// JSON Lines export for a SIEM. Minimal escaping of `"` and `\` and control chars.
    pub fn to_jsonl(&self) -> String {
        fn esc(s: &str) -> String {
            let mut o = String::with_capacity(s.len());
            for c in s.chars() {
                match c {
                    '"' => o.push_str("\\\""),
                    '\\' => o.push_str("\\\\"),
                    c if (c as u32) < 0x20 => o.push_str(&format!("\\u{:04x}", c as u32)),
                    c => o.push(c),
                }
            }
            o
        }
        self.events
            .iter()
            .map(|e| {
                format!(
                    "{{\"seq\":{},\"at\":{},\"kind\":\"{}\",\"host_id\":\"{}\",\"credential_id\":\"{}\",\"detail\":\"{}\",\"hash\":\"{}\"}}\n",
                    e.seq,
                    e.at,
                    e.kind.as_str(),
                    esc(e.host_id.as_deref().unwrap_or("")),
                    esc(e.credential_id.as_deref().unwrap_or("")),
                    esc(&e.detail),
                    sha256::hex(&e.hash)
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn chain_verifies_and_detects_tampering() {
        let mut log = AuditLog::default();
        log.append(1, AuditKind::Connect, Some("dbb"), None, "ssh taedeen@dbb");
        log.append(2, AuditKind::CredentialTyped, Some("dbb"), Some("cred-sudo"), "sudo prompt");
        log.append(3, AuditKind::Disconnect, Some("dbb"), None, "");
        assert_eq!(AuditLog::verify(log.events()), Ok(()));

        let mut tampered = log.events().to_vec();
        tampered[1].credential_id = Some("cred-root".into());
        assert_eq!(AuditLog::verify(&tampered), Err(1));

        let mut removed = log.events().to_vec();
        removed.remove(1);
        assert_eq!(AuditLog::verify(&removed), Err(1));
    }

    #[test]
    fn jsonl_round_trips() {
        let mut log = AuditLog::default();
        log.append(1, AuditKind::Connect, Some("dbb"), None, "ssh \"x\"\n\\ é");
        log.append(2, AuditKind::Sync, None, None, "pulled 3, pushed 1");
        log.append(3, AuditKind::CredentialTyped, Some("dbb"), Some("cred-sudo"), "");
        let (back, broken) = AuditLog::from_jsonl(&log.to_jsonl());
        assert_eq!(broken, None);
        assert_eq!(back.events(), log.events());

        let tampered = log.to_jsonl().replace("pulled 3", "pulled 4");
        let (_, broken) = AuditLog::from_jsonl(&tampered);
        assert_eq!(broken, Some(1));
    }

    #[test]
    fn jsonl_export_escapes() {
        let mut log = AuditLog::default();
        log.append(5, AuditKind::SettingChanged, None, None, "label \"x\"\n");
        let out = log.to_jsonl();
        assert!(out.contains("\\\"x\\\"\\u000a"));
        assert!(out.ends_with("}\n"));
        assert!(!out.contains("hunter2"));
    }
}
