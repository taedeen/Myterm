//! Talking to the MyTerm sync server (see server/README.md): join a vault,
//! pull and push records, devices, SSH certificates. The same protocol as
//! the apps' sync service.

use std::collections::BTreeMap;
use std::time::Duration;

use anyhow::{anyhow, bail, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde_json::{json, Value};

use crate::store::{Scope, Store};

pub const DEFAULT_URL: &str = "https://myterm.example.com";

#[derive(Debug)]
pub struct HttpError {
    pub status: u16,
    pub message: String,
    pub body: Value,
}

impl std::fmt::Display for HttpError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}
impl std::error::Error for HttpError {}

pub fn normalise_url(raw: &str) -> Result<String> {
    let mut u = raw.trim().to_string();
    if u.is_empty() {
        u = DEFAULT_URL.into();
    }
    if !u.contains("://") {
        u = format!("https://{u}");
    }
    while u.ends_with('/') {
        u.pop();
    }
    if !(u.starts_with("https://") || u.starts_with("http://")) || u.split("://").nth(1).unwrap_or("").is_empty() {
        bail!("enter a server address like {DEFAULT_URL}");
    }
    Ok(u)
}

fn agent() -> ureq::Agent {
    ureq::AgentBuilder::new().timeout_connect(Duration::from_secs(10)).timeout(Duration::from_secs(60)).build()
}

pub struct Auth<'a> {
    pub server_token: Option<&'a str>,
    pub vault_token: Option<&'a str>,
    /// (device id, device name) for the vault's device list.
    pub device: Option<(&'a str, &'a str)>,
}

pub fn http(base: &str, method: &str, path: &str, body: Option<Value>, auth: &Auth) -> Result<Value> {
    let mut req = agent().request(method, &format!("{base}{path}")).set("Accept", "application/json");
    if let Some(t) = auth.server_token {
        req = req.set("Authorization", &format!("Bearer {t}"));
    }
    if let Some(t) = auth.vault_token {
        req = req.set("X-MyTerm-Vault-Token", t);
        if let Some((id, name)) = auth.device {
            req = req.set("X-MyTerm-Device", id);
            let name: String = name.chars().filter(|c| (' '..='~').contains(c)).take(64).collect();
            if !name.trim().is_empty() {
                req = req.set("X-MyTerm-Device-Name", name.trim());
            }
        }
    }
    let res = match body {
        Some(b) => req.send_json(b),
        None => req.call(),
    };
    match res {
        Ok(r) => {
            let text = r.into_string()?;
            if text.is_empty() {
                return Ok(json!({}));
            }
            Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
        }
        Err(ureq::Error::Status(code, r)) => {
            let text = r.into_string().unwrap_or_default();
            let body: Value = serde_json::from_str(&text).unwrap_or(json!({}));
            let message = body["error"].as_str().map(str::to_string).unwrap_or_else(|| format!("HTTP {code}"));
            Err(HttpError { status: code, message, body }.into())
        }
        Err(e) => Err(anyhow!("can't reach the sync server: {e}")),
    }
}

/// `myterm join`: finds the vault by name and unlocks it with the master
/// password. Returns the raw master key.
pub fn join(store: &mut Store, url: &str, server_token: &str, name: &str, password: &str) -> Result<Vec<u8>> {
    let base = normalise_url(url)?;
    http(&base, "GET", "/v1/health", None, &Auth { server_token: None, vault_token: None, device: None })?;
    let name = name.trim().to_lowercase();
    let found = http(
        &base,
        "GET",
        &format!("/v1/vaults/lookup?name={}", urlencode(&name)),
        None,
        &Auth { server_token: Some(server_token.trim()), vault_token: None, device: None },
    )?;
    let w = found["wrapped"].clone();
    let num = |k: &str| w[k].as_u64().unwrap_or(0) as u32;
    let mk = store
        .engine
        .join_vault(
            password.to_string(),
            B64.decode(w["password_wrapped"].as_str().unwrap_or(""))?,
            B64.decode(w["kdf_salt"].as_str().unwrap_or(""))?,
            num("kdf_m_kib"),
            num("kdf_t"),
            num("kdf_p"),
            found["vault_id"].as_str().unwrap_or("").to_string(),
        )
        .map_err(|e| {
            if e.to_string().contains("wrong master password") {
                anyhow!("wrong master password for that vault")
            } else {
                e
            }
        })?;
    store.set_wrapped(&w)?;
    store.meta.insert("syncUrl".into(), json!(base));
    store.meta.insert("syncVaultName".into(), json!(name));
    store.meta.insert("syncCursor".into(), json!(0));
    store.meta.remove("syncDigests");
    store.write_meta()?;
    Ok(mk)
}

fn urlencode(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'a'..=b'z' | b'A'..=b'Z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn base(store: &Store) -> Result<String> {
    store.str("syncUrl").ok_or_else(|| anyhow!("sync isn't set up: run `myterm join`"))
}

/// Pulls other devices' changes, then pushes this device's. Returns a summary.
pub fn sync_now(store: &mut Store) -> Result<String> {
    let base = base(store)?;
    if store.meta.get("syncMoved").and_then(Value::as_bool) == Some(true) {
        bail!("a device was revoked on another device, so your vault has a new key: run `myterm join` again");
    }
    let id = store.engine.sync_identity()?;
    let (device, name) = (store.device_id(), store.device_name());
    let auth = Auth { server_token: None, vault_token: Some(&id.vault_token), device: Some((&device, &name)) };
    let path = format!("/v1/vaults/{}/records", id.vault_id);
    let scope = store.scope();

    let mut cursor = store.meta.get("syncCursor").and_then(Value::as_i64).unwrap_or(0);
    let (mut received, mut changed) = (0usize, 0u32);
    loop {
        let page = match http(&base, "GET", &format!("{path}?since={cursor}"), None, &auth) {
            Ok(p) => p,
            Err(e) => return Err(explain(store, e)),
        };
        let server_cursor = page["cursor"].as_i64().unwrap_or(0);
        if server_cursor < cursor {
            // The server's database was reset or restored: start over.
            cursor = 0;
            store.meta.remove("syncDigests");
            continue;
        }
        let mut blobs = Vec::new();
        for r in page["records"].as_array().cloned().unwrap_or_default() {
            if r["device_id"].as_str() != Some(device.as_str()) {
                blobs.push(B64.decode(r["data"].as_str().unwrap_or(""))?);
            }
        }
        received += blobs.len();
        if !blobs.is_empty() {
            changed += store.engine.sync_apply_scoped(blobs, scope.skipped_kinds())?;
        }
        cursor = server_cursor;
        if page["more"].as_bool() != Some(true) {
            break;
        }
    }
    store.meta.insert("syncCursor".into(), json!(cursor));

    let items = store.engine.sync_collect()?;
    let mut sent: BTreeMap<String, String> = store
        .meta
        .get("syncDigests")
        .and_then(Value::as_object)
        .map(|m| m.iter().map(|(k, v)| (k.clone(), v.as_str().unwrap_or("").to_string())).collect())
        .unwrap_or_default();
    let todo: Vec<_> = items.into_iter().filter(|i| sent.get(&i.key) != Some(&i.digest)).collect();
    for batch in todo.chunks(200) {
        let records: Vec<Value> = batch.iter().map(|i| json!({"key": i.key, "data": B64.encode(&i.data)})).collect();
        if let Err(e) = http(&base, "POST", &path, Some(json!({"device_id": device, "records": records})), &auth) {
            return Err(explain(store, e));
        }
        for i in batch {
            sent.insert(i.key.clone(), i.digest.clone());
        }
        store.meta.insert("syncDigests".into(), json!(sent));
    }
    store.meta.insert(
        "syncLastAt".into(),
        json!(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0)),
    );
    let summary = format!("received {received} ({changed} changed here), sent {}", todo.len());
    if changed > 0 || !todo.is_empty() {
        store.engine.audit_sync(summary.clone());
    }
    store.write_meta()?;
    store.save()?;
    Ok(summary)
}

fn explain(store: &mut Store, e: anyhow::Error) -> anyhow::Error {
    if let Some(h) = e.downcast_ref::<HttpError>() {
        if h.status == 410 && h.body["moved"].as_bool() == Some(true) {
            store.meta.insert("syncMoved".into(), json!(true));
            let _ = store.write_meta();
            return anyhow!("a device was revoked on another device, so your vault has a new key: run `myterm join` again");
        }
        if h.status == 401 {
            return anyhow!("the sync server rejected this vault (deleted or reset?): run `myterm join` again");
        }
    }
    e
}

/// The vault's devices: (id, name, last seen unix, records).
pub fn devices(store: &Store) -> Result<Vec<Value>> {
    let base = base(store)?;
    let id = store.engine.sync_identity()?;
    let (device, name) = (store.device_id(), store.device_name());
    let r = http(
        &base,
        "GET",
        &format!("/v1/vaults/{}/devices", id.vault_id),
        None,
        &Auth { server_token: None, vault_token: Some(&id.vault_token), device: Some((&device, &name)) },
    )?;
    Ok(r["devices"].as_array().cloned().unwrap_or_default())
}

/// Gets a certificate when this device has none or it expires within
/// `margin_secs`. Returns its expiry (unix seconds).
pub fn renew_certificate(store: &Store, force: bool, margin_secs: u64) -> Result<Option<u64>> {
    if store.scope() == Scope::Hosts {
        bail!("this device's sync scope (hosts only) doesn't include certificates");
    }
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs();
    let current = store.engine.ssh_certificate();
    if let Some(c) = &current {
        if !force && c.valid_before > now + margin_secs {
            return Ok(Some(c.valid_before));
        }
    }
    let base = base(store)?;
    let id = store.engine.sync_identity()?;
    let (device, name) = (store.device_id(), store.device_name());
    let public = store.engine.certificate_public_key(format!("myterm {name}"))?;
    let r = http(
        &base,
        "POST",
        &format!("/v1/vaults/{}/ssh-cert", id.vault_id),
        Some(json!({"public_key": public})),
        &Auth { server_token: None, vault_token: Some(&id.vault_token), device: Some((&device, &name)) },
    )
    .map_err(|e| match e.downcast_ref::<HttpError>() {
        Some(h) if h.status == 404 => anyhow!("SSH certificates aren't turned on on the sync server (MYTERM_SYNC_CERT_PRINCIPALS)"),
        _ => e,
    })?;
    let cert = store.engine.set_ssh_certificate(r["certificate"].as_str().unwrap_or("").to_string())?;
    store.save()?;
    Ok(Some(cert.valid_before))
}

/// The CA public key servers trust (plain text).
pub fn ca_public_key(store: &Store) -> Result<String> {
    let base = base(store)?;
    let r = agent().get(&format!("{base}/v1/ssh-ca.pub")).call().map_err(|e| match e {
        ureq::Error::Status(404, _) => anyhow!("SSH certificates aren't turned on on the sync server"),
        e => anyhow!("can't reach the sync server: {e}"),
    })?;
    Ok(r.into_string()?.trim().to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_normalised() {
        assert_eq!(normalise_url("").unwrap(), DEFAULT_URL);
        assert_eq!(normalise_url("sync.example.com/").unwrap(), "https://sync.example.com");
        assert_eq!(normalise_url("http://10.0.0.2:7341").unwrap(), "http://10.0.0.2:7341");
        assert!(normalise_url("ftp://x").is_err());
    }

    #[test]
    fn names_are_url_encoded() {
        assert_eq!(urlencode("home home"), "home%20home");
    }
}
