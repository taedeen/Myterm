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
        }
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
    fn jsonl_export_escapes() {
        let mut log = AuditLog::default();
        log.append(5, AuditKind::SettingChanged, None, None, "label \"x\"\n");
        let out = log.to_jsonl();
        assert!(out.contains("\\\"x\\\"\\u000a"));
        assert!(out.ends_with("}\n"));
        assert!(!out.contains("hunter2"));
    }
}
