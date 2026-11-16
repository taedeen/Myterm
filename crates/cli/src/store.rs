//! This device's files, in the same formats as the apps:
//! `~/.local/share/myterm/meta.json` (device settings, the password-wrapped
//! vault key, sync state) and `store.bin` (the encrypted vault).

use std::fs;
use std::io::Write;
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};
use std::path::PathBuf;

use anyhow::{bail, Context, Result};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use myterm_ffi::api::MyTerm;
use myterm_vault::{self as vault, KdfParams};
use serde_json::{json, Map, Value};

/// What this device takes from the vault (`myterm join` asks).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Scope {
    /// Everything, like the phone and PC.
    Full,
    /// Hosts, snippets, history, known hosts and an SSH certificate; no
    /// stored passwords or private keys. The default for servers.
    Certs,
    /// Hosts, snippets, history and known hosts only.
    Hosts,
}

impl Scope {
    pub fn as_str(self) -> &'static str {
        match self {
            Scope::Full => "full",
            Scope::Certs => "certs",
            Scope::Hosts => "hosts",
        }
    }
    pub fn parse(s: &str) -> Option<Scope> {
        match s {
            "full" => Some(Scope::Full),
            "certs" => Some(Scope::Certs),
            "hosts" => Some(Scope::Hosts),
            _ => None,
        }
    }
    /// Vault record kinds this device never takes from the sync server.
    pub fn skipped_kinds(self) -> Vec<String> {
        match self {
            Scope::Full => vec![],
            Scope::Certs | Scope::Hosts => vec!["credential".into()],
        }
    }
    pub fn describe(self) -> &'static str {
        match self {
            Scope::Full => "everything, stored passwords and keys included",
            Scope::Certs => "hosts, snippets, history and an SSH certificate; no stored passwords or keys",
            Scope::Hosts => "hosts, snippets and history only",
        }
    }
}

pub fn data_dir() -> PathBuf {
    if let Ok(d) = std::env::var("MYTERM_HOME") {
        return PathBuf::from(d);
    }
    let base = std::env::var("XDG_DATA_HOME")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_else(|_| ".".into())).join(".local/share"));
    base.join("myterm")
}

pub struct Store {
    pub dir: PathBuf,
    pub meta: Map<String, Value>,
    pub engine: MyTerm,
}

impl Store {
    /// Opens this device's store (creating the directory and device id).
    pub fn open() -> Result<Store> {
        let dir = data_dir();
        fs::create_dir_all(&dir).with_context(|| format!("creating {}", dir.display()))?;
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o700))?;
        let meta_path = dir.join("meta.json");
        let mut meta = if meta_path.exists() {
            match serde_json::from_str::<Value>(&fs::read_to_string(&meta_path)?)? {
                Value::Object(m) => m,
                _ => bail!("{} is not a JSON object", meta_path.display()),
            }
        } else {
            Map::new()
        };
        if !meta.contains_key("deviceId") {
            meta.insert("deviceId".into(), json!(new_device_id()));
        }
        let device_id = meta["deviceId"].as_str().unwrap_or_default().to_string();
        let engine = MyTerm::new(device_id);
        let store_path = dir.join("store.bin");
        if store_path.exists() {
            engine.import_store(fs::read(&store_path)?)?;
        }
        let s = Store { dir, meta, engine };
        s.write_meta()?;
        Ok(s)
    }

    pub fn device_id(&self) -> String {
        self.meta["deviceId"].as_str().unwrap_or_default().to_string()
    }

    pub fn device_name(&self) -> String {
        self.meta
            .get("deviceName")
            .and_then(Value::as_str)
            .map(str::to_string)
            .unwrap_or_else(|| format!("{} (Linux)", hostname()))
    }

    pub fn scope(&self) -> Scope {
        self.meta.get("scope").and_then(Value::as_str).and_then(Scope::parse).unwrap_or(Scope::Certs)
    }

    pub fn has_vault(&self) -> bool {
        self.meta.contains_key("passwordWrapped")
    }

    pub fn str(&self, key: &str) -> Option<String> {
        self.meta.get(key).and_then(Value::as_str).map(str::to_string)
    }

    pub fn write_meta(&self) -> Result<()> {
        write_private(&self.dir.join("meta.json"), serde_json::to_string_pretty(&Value::Object(self.meta.clone()))?.as_bytes())
    }

    /// Saves the vault (history included). Needs it unlocked.
    pub fn save(&self) -> Result<()> {
        if !self.engine.is_unlocked() {
            return Ok(());
        }
        let blobs = self.engine.export_store()?;
        write_private(&self.dir.join("store.bin"), &blobs.store)
    }

    /// Stores the password-wrapped key from the server (after joining).
    pub fn set_wrapped(&mut self, w: &Value) -> Result<()> {
        for (ours, theirs) in [("passwordWrapped", "password_wrapped"), ("salt", "kdf_salt")] {
            self.meta.insert(ours.into(), w[theirs].clone());
        }
        self.meta.insert("m".into(), w["kdf_m_kib"].clone());
        self.meta.insert("t".into(), w["kdf_t"].clone());
        self.meta.insert("p".into(), w["kdf_p"].clone());
        self.write_meta()
    }

    /// The raw master key from the master password (for the key holder).
    pub fn unwrap(&self, password: &str) -> Result<Vec<u8>> {
        let wrapped = B64.decode(self.str("passwordWrapped").context("no vault")?)?;
        let salt: [u8; 16] = B64
            .decode(self.str("salt").context("no salt")?)?
            .as_slice()
            .try_into()
            .map_err(|_| anyhow::anyhow!("bad salt"))?;
        let num = |k: &str| self.meta.get(k).and_then(Value::as_u64).unwrap_or(0) as u32;
        let params = KdfParams { m_cost_kib: num("m"), t_cost: num("t"), p_cost: num("p"), salt };
        let kek = vault::derive_password_key(password.as_bytes(), &params)?;
        let mk = vault::unwrap_master_key(&wrapped, &kek, "password").map_err(|_| anyhow::anyhow!("wrong master password"))?;
        Ok(mk.expose_for_wrapping().to_vec())
    }
}

/// Writes a file only this user can read, atomically.
pub fn write_private(path: &std::path::Path, data: &[u8]) -> Result<()> {
    let tmp = path.with_extension("tmp");
    {
        let mut f = fs::OpenOptions::new().write(true).create(true).truncate(true).mode(0o600).open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
    }
    fs::rename(&tmp, path)?;
    Ok(())
}

pub fn hostname() -> String {
    fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .ok()
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "linux".into())
}

/// A random version-4 UUID, like the apps' device ids.
pub fn new_device_id() -> String {
    let mut b = [0u8; 16];
    getrandom::getrandom(&mut b).expect("random bytes");
    b[6] = (b[6] & 0x0f) | 0x40;
    b[8] = (b[8] & 0x3f) | 0x80;
    let h: String = b.iter().map(|x| format!("{x:02x}")).collect();
    format!("{}-{}-{}-{}-{}", &h[0..8], &h[8..12], &h[12..16], &h[16..20], &h[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn device_ids_look_like_uuids() {
        let id = new_device_id();
        assert_eq!(id.len(), 36);
        assert_eq!(&id[14..15], "4");
        assert_ne!(id, new_device_id());
    }

    #[test]
    fn scopes_round_trip_and_hide_credentials() {
        for s in [Scope::Full, Scope::Certs, Scope::Hosts] {
            assert_eq!(Scope::parse(s.as_str()), Some(s));
        }
        assert!(Scope::Full.skipped_kinds().is_empty());
        assert_eq!(Scope::Certs.skipped_kinds(), vec!["credential".to_string()]);
    }
}
