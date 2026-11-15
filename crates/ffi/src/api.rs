//! Functions and types exposed to Dart.
//!
//! One [`MyTerm`] object per app. It owns the vault (hosts, credentials,
//! snippets are all vault records, so they are encrypted at rest and in
//! sync), the command history, known hosts, the audit log and live
//! sessions. Persistence is a single opaque blob the app writes to its
//! private storage: [`MyTerm::export_store`] / [`MyTerm::import_store`].

use std::collections::HashMap;
use std::sync::{Arc, Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use flutter_rust_bridge::{frb, DartFnFuture};
use myterm_core::audit::{AuditKind, AuditLog};
use myterm_core::codec;
use myterm_core::completion::{self, CompletionInput, Snippet};
use myterm_core::history::{HistoryStore, RecordContext};
use myterm_core::inject::{ConfirmPolicy, CredentialMeta, InjectDecision, Refusal, SecretBuf};
use myterm_core::keybar::{self, KeyAction};
use myterm_core::knownhosts::{KnownHost, Verdict};
use myterm_core::model::{self, Group, Host, Overrides};
use myterm_core::session::Session;
use myterm_core::sha256;
use myterm_core::signer::Protection;
use myterm_core::sshkey;
use myterm_ssh::{self as ssh, AsyncDerSigner, Auth, ConnectOptions, Event, HostKeyPrompt, ShellHandle, Target};
use myterm_vault::{self as vault, KdfParams, MasterKey, Vault};
use tokio::runtime::Runtime;
use tokio::sync::mpsc;

use crate::frb_generated::StreamSink;

fn rt() -> &'static Runtime {
    static RT: OnceLock<Runtime> = OnceLock::new();
    RT.get_or_init(|| tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime"))
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

// ---------------------------------------------------------------- DTOs

pub struct HostDto {
    pub id: String,
    pub label: String,
    pub address: String,
    pub port: Option<u16>,
    pub username: String,
    pub group_id: Option<String>,
    pub credential_id: Option<String>,
    pub hardware_key_id: Option<String>,
    pub tags: Vec<String>,
    pub os: Option<String>,
    pub keybar_profile_id: Option<String>,
    pub hardware_keys_only: Option<bool>,
    pub sync_history: Option<bool>,
    pub jump_chain: Vec<String>,
}

pub struct GroupDto {
    pub id: String,
    pub name: String,
    pub parent_id: Option<String>,
    pub port: Option<u16>,
    pub username: Option<String>,
    pub credential_id: Option<String>,
    pub hardware_keys_only: Option<bool>,
}

pub enum ConfirmDto {
    Always,
    OncePerSession,
    Never,
}

pub enum CredentialKind {
    Password,
    PrivateKey,
}

pub struct CredentialDto {
    pub id: String,
    pub name: String,
    pub kind: CredentialKind,
    pub confirm: ConfirmDto,
    pub never_auto_type: bool,
    pub append_enter: bool,
    pub device_only: bool,
}

pub struct SnippetDto {
    pub id: String,
    pub name: String,
    pub body: String,
    pub params: Vec<String>,
}

pub enum KeyActionDto {
    Bytes(Vec<u8>),
    Snippet(String),
    TypePassword(Option<String>),
}

pub struct KeyDto {
    pub label: String,
    pub action: KeyActionDto,
}

pub struct KeybarDto {
    pub id: String,
    pub name: String,
    pub rows: Vec<Vec<KeyDto>>,
}

pub struct SuggestionDto {
    pub line: String,
    pub source: String,
    pub snippet_id: Option<String>,
}

pub enum InjectDto {
    Proceeded,
    NeedConfirmation,
    AskOverride,
    RefusedNeverAutoType,
    RefusedOneTimeCode,
}

#[derive(Clone, Debug)]
pub enum SessionEventDto {
    /// First event: the id to use for input, resize and Type password.
    Started(u32),
    Output(Vec<u8>),
    Exit(Option<u32>),
    Closed,
    Error(String),
}

/// What a device needs to talk to the sync server. Derived from the vault
/// master key, so every device in the vault gets the same values.
pub struct SyncIdentity {
    /// 32 hex chars; the server's id for this vault.
    pub vault_id: String,
    /// 64 hex chars; sent as X-MyTerm-Vault-Token. Never stored by the server.
    pub vault_token: String,
    /// SHA-256 of `vault_token`, registered with the server.
    pub auth_hash: String,
}

/// One record ready to upload.
pub struct SyncItem {
    /// Opaque per-record key (64 hex); hides the record id from the server.
    pub key: String,
    /// SHA-256 of the record's current plaintext encoding; the app skips
    /// uploading records whose digest it has already sent.
    pub digest: String,
    /// The record, encrypted under the vault master key.
    pub data: Vec<u8>,
}

pub struct StoreBlobs {
    /// Vault records (ciphertext) + history and audit sealed inside the vault.
    pub store: Vec<u8>,
}

pub struct VaultSetup {
    /// MK wrapped with the master password (store next to the store blob).
    pub password_wrapped: Vec<u8>,
    pub kdf_salt: Vec<u8>,
    pub kdf_m_kib: u32,
    pub kdf_t: u32,
    pub kdf_p: u32,
    /// Raw MK for native code to wrap with the Android Keystore key.
    /// The Dart side must zero this list after handing it over.
    pub master_key_for_device_wrap: Vec<u8>,
}

pub enum AuthChoice {
    /// Use the host's stored credential (password or private key).
    Stored,
    /// Use a hardware key held by native code; callbacks below are required.
    Hardware { key_id: String, public_sec1: Vec<u8>, protection: String },
    /// A password typed at connect time for this session only (not stored).
    Password { password: Vec<u8> },
}

// ---------------------------------------------------------------- internals

struct Live {
    session: Session<ShellHandle>,
}

struct State {
    device_id: String,
    vault: Vault,
    history: HistoryStore,
    known_hosts: Vec<KnownHost>,
    audit: AuditLog,
    sessions: HashMap<u32, Live>,
    next_session: u32,
}

const K_HOST: &str = "host";
const K_GROUP: &str = "group";
const K_CRED: &str = "credential";
const K_SNIPPET: &str = "snippet";
/// Trusted host keys. Synced, so a key trusted on one device is trusted on all.
const K_KNOWN: &str = "known_host";

fn s(v: &[u8]) -> String {
    String::from_utf8_lossy(v).into_owned()
}

fn opt(v: &str) -> Option<String> {
    (!v.is_empty()).then(|| v.to_string())
}

fn bool_field(v: &str) -> Option<bool> {
    match v {
        "1" => Some(true),
        "0" => Some(false),
        _ => None,
    }
}

fn bool_str(v: Option<bool>) -> &'static str {
    match v {
        Some(true) => "1",
        Some(false) => "0",
        None => "",
    }
}

impl State {
    fn field(&self, id: &str, f: &str) -> String {
        self.vault.get(id, f).map(|v| s(&v)).unwrap_or_default()
    }

    /// Writes a sealed blob only when its content changed, so unchanged
    /// history isn't re-stamped (and re-uploaded by sync) on every save.
    fn put_if_changed(&mut self, kind: &str, id: &str, data: &[u8], t: u64) -> anyhow::Result<()> {
        if self.vault.get(id, "data").is_ok_and(|cur| cur.as_slice() == data) {
            return Ok(());
        }
        self.vault.put(kind, id, "data", data, t)?;
        Ok(())
    }

    /// Saves a trusted host key to the vault (one record per host + key type).
    fn put_known_host(&mut self, k: &KnownHost) -> anyhow::Result<()> {
        let id = format!("kh-{}", sha256::hex(&sha256::digest(format!("{}\0{}", k.pattern, k.key_type).as_bytes())));
        let t = now();
        for (name, value) in [
            ("pattern", k.pattern.as_bytes()),
            ("key_type", k.key_type.as_bytes()),
            ("key_blob", k.key_blob.as_slice()),
            ("approved_at", k.approved_at.to_string().as_bytes()),
            ("approved_by_device", k.approved_by_device.as_bytes()),
        ] {
            self.vault.put(K_KNOWN, &id, name, value, t)?;
        }
        Ok(())
    }

    /// Rebuilds the in-memory trusted-key list from the vault. Needs the vault
    /// unlocked; does nothing (keeps the current list) while locked.
    fn load_known_hosts(&mut self) {
        if !self.vault.is_unlocked() {
            return;
        }
        let mut list: Vec<KnownHost> = Vec::new();
        for id in self.ids_of(K_KNOWN) {
            let Ok(blob) = self.vault.get(&id, "key_blob") else { continue };
            let k = KnownHost {
                pattern: self.field(&id, "pattern"),
                key_type: self.field(&id, "key_type"),
                key_blob: blob.to_vec(),
                approved_at: self.field(&id, "approved_at").parse().unwrap_or(0),
                approved_by_device: self.field(&id, "approved_by_device"),
            };
            if !k.pattern.is_empty() {
                list.push(k);
            }
        }
        // Keys trusted this session but not yet saved stay trusted.
        for k in std::mem::take(&mut self.known_hosts) {
            if !list.iter().any(|x| x.pattern == k.pattern && x.key_type == k.key_type) {
                list.push(k);
            }
        }
        self.known_hosts = list;
    }

    fn ids_of(&self, kind: &str) -> Vec<String> {
        self.vault.records().filter(|r| r.kind == kind && !r.is_deleted()).map(|r| r.id.clone()).collect()
    }

    fn host(&self, id: &str) -> Option<HostDto> {
        if !self.ids_of(K_HOST).iter().any(|h| h == id) {
            return None;
        }
        let f = |n: &str| self.field(id, n);
        Some(HostDto {
            id: id.to_string(),
            label: f("label"),
            address: f("address"),
            port: f("port").parse().ok(),
            username: f("username"),
            group_id: opt(&f("group_id")),
            credential_id: opt(&f("credential_id")),
            hardware_key_id: opt(&f("hardware_key_id")),
            tags: f("tags").split(',').filter(|t| !t.is_empty()).map(str::to_string).collect(),
            os: opt(&f("os")),
            keybar_profile_id: opt(&f("keybar_profile_id")),
            hardware_keys_only: bool_field(&f("hardware_keys_only")),
            sync_history: bool_field(&f("sync_history")),
            jump_chain: f("jump_chain").split(',').filter(|t| !t.is_empty()).map(str::to_string).collect(),
        })
    }

    fn group(&self, id: &str) -> GroupDto {
        let f = |n: &str| self.field(id, n);
        GroupDto {
            id: id.to_string(),
            name: f("name"),
            parent_id: opt(&f("parent_id")),
            port: f("port").parse().ok(),
            username: opt(&f("username")),
            credential_id: opt(&f("credential_id")),
            hardware_keys_only: bool_field(&f("hardware_keys_only")),
        }
    }

    fn model_host(&self, h: &HostDto) -> Host {
        Host {
            id: h.id.clone(),
            label: h.label.clone(),
            address: h.address.clone(),
            group_id: h.group_id.clone(),
            tags: h.tags.clone(),
            os: h.os.clone(),
            settings: Overrides {
                port: h.port,
                identity_id: h.credential_id.clone(),
                jump_chain: (!h.jump_chain.is_empty()).then(|| h.jump_chain.clone()),
                keybar_profile_id: h.keybar_profile_id.clone(),
                hardware_keys_only: h.hardware_keys_only,
                sync_history: h.sync_history,
                ..Default::default()
            },
            ..Default::default()
        }
    }

    fn model_groups(&self) -> HashMap<String, Group> {
        self.ids_of(K_GROUP)
            .into_iter()
            .map(|id| {
                let g = self.group(&id);
                let grp = Group {
                    id: g.id.clone(),
                    name: g.name,
                    parent_id: g.parent_id,
                    defaults: Overrides {
                        port: g.port,
                        identity_id: g.credential_id,
                        hardware_keys_only: g.hardware_keys_only,
                        ..Default::default()
                    },
                };
                (id, grp)
            })
            .collect()
    }

    fn cred_meta(&self, id: &str) -> Option<CredentialMeta> {
        if !self.ids_of(K_CRED).iter().any(|c| c == id) {
            return None;
        }
        let f = |n: &str| self.field(id, n);
        Some(CredentialMeta {
            id: id.to_string(),
            name: f("name"),
            confirm: match f("confirm").as_str() {
                "once" => ConfirmPolicy::OncePerSession,
                "never" => ConfirmPolicy::Never,
                _ => ConfirmPolicy::Always,
            },
            never_auto_type: f("never_auto_type") == "1",
            append_enter: f("append_enter") != "0",
        })
    }

    fn snippets(&self) -> Vec<Snippet> {
        self.ids_of(K_SNIPPET)
            .into_iter()
            .map(|id| Snippet { name: self.field(&id, "name"), body: self.field(&id, "body"), id })
            .collect()
    }
}

struct DartSigner {
    public: Vec<u8>,
    protection: Protection,
    key_id: String,
    sign: Arc<dyn Fn(String, Vec<u8>) -> DartFnFuture<Vec<u8>> + Send + Sync>,
}

#[async_trait::async_trait]
impl AsyncDerSigner for DartSigner {
    fn public_sec1(&self) -> Vec<u8> {
        self.public.clone()
    }
    fn protection(&self) -> Protection {
        self.protection
    }
    async fn sign_der(&self, data: Vec<u8>) -> Result<Vec<u8>, String> {
        let der = (self.sign)(self.key_id.clone(), data).await;
        if der.is_empty() {
            Err("cancelled or failed".into())
        } else {
            Ok(der)
        }
    }
}

struct DartPrompt(Arc<dyn Fn(String, u16, String, String) -> DartFnFuture<bool> + Send + Sync>);

#[async_trait::async_trait]
impl HostKeyPrompt for DartPrompt {
    async fn confirm(&self, host: &str, port: u16, verdict: &Verdict) -> bool {
        let (kind, fp) = match verdict {
            Verdict::Unknown { fingerprint } => ("unknown", fingerprint.clone()),
            Verdict::NewKeyType { fingerprint, .. } => ("new_key_type", fingerprint.clone()),
            _ => return false,
        };
        (self.0)(host.to_string(), port, kind.to_string(), fp).await
    }
}

fn protection_from(s: &str) -> Protection {
    match s {
        "StrongBox" => Protection::StrongBox,
        "TEE" => Protection::Tee,
        "TPM" => Protection::Tpm,
        "Secure Enclave" => Protection::SecureEnclave,
        "Security key" => Protection::SecurityKey,
        _ => Protection::Software,
    }
}

// ---------------------------------------------------------------- API

#[frb(opaque)]
pub struct MyTerm {
    state: Arc<Mutex<State>>,
}

impl MyTerm {
    #[frb(sync)]
    pub fn new(device_id: String) -> MyTerm {
        MyTerm {
            state: Arc::new(Mutex::new(State {
                vault: Vault::new(device_id.clone()),
                history: HistoryStore::new(device_id.clone()),
                device_id,
                known_hosts: Vec::new(),
                audit: AuditLog::default(),
                sessions: HashMap::new(),
                next_session: 1,
            })),
        }
    }

    fn st(&self) -> std::sync::MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    // ----- vault lifecycle

    /// First run: create MK, wrap it with the master password, unlock.
    pub fn create_vault(&self, master_password: String, mobile: bool) -> anyhow::Result<VaultSetup> {
        let params = if mobile { KdfParams::mobile() } else { KdfParams::desktop() };
        let kek = vault::derive_password_key(master_password.as_bytes(), &params)?;
        let mk = MasterKey::generate();
        let wrapped = vault::wrap_master_key(&mk, &kek, "password");
        let raw = mk.expose_for_wrapping().to_vec();
        self.st().vault.unlock(mk);
        Ok(VaultSetup {
            password_wrapped: wrapped,
            kdf_salt: params.salt.to_vec(),
            kdf_m_kib: params.m_cost_kib,
            kdf_t: params.t_cost,
            kdf_p: params.p_cost,
            master_key_for_device_wrap: raw,
        })
    }

    pub fn unlock_with_password(
        &self,
        master_password: String,
        password_wrapped: Vec<u8>,
        kdf_salt: Vec<u8>,
        kdf_m_kib: u32,
        kdf_t: u32,
        kdf_p: u32,
    ) -> anyhow::Result<()> {
        let salt: [u8; 16] = kdf_salt.as_slice().try_into().map_err(|_| anyhow::anyhow!("bad salt"))?;
        let params = KdfParams { m_cost_kib: kdf_m_kib, t_cost: kdf_t, p_cost: kdf_p, salt };
        let kek = vault::derive_password_key(master_password.as_bytes(), &params)?;
        let mk = vault::unwrap_master_key(&password_wrapped, &kek, "password")?;
        let mut st = self.st();
        st.vault.unlock(mk);
        // History and trusted host keys are sealed in the vault; load them
        // as soon as they can be read.
        Self::load_history(&mut st);
        Self::load_audit(&mut st);
        st.load_known_hosts();
        Ok(())
    }

    /// Everyday unlock: native code unwrapped MK with the Keystore key after biometrics.
    pub fn unlock_with_master_key(&self, master_key: Vec<u8>) -> anyhow::Result<()> {
        let bytes: [u8; 32] = master_key.as_slice().try_into().map_err(|_| anyhow::anyhow!("bad key length"))?;
        let mut st = self.st();
        st.vault.unlock(MasterKey::from_bytes(bytes));
        Self::load_history(&mut st);
        Self::load_audit(&mut st);
        st.load_known_hosts();
        Ok(())
    }

    #[frb(sync)]
    pub fn lock(&self) {
        self.st().vault.lock();
    }

    #[frb(sync)]
    pub fn is_unlocked(&self) -> bool {
        self.st().vault.is_unlocked()
    }

    // ----- persistence

    /// Seals history and audit into the vault, then returns everything as one blob.
    pub fn export_store(&self) -> anyhow::Result<StoreBlobs> {
        let mut st = self.st();
        // Merge what is already saved before overwriting it, so history that
        // was never loaded into memory (e.g. saved before unlock finished)
        // can't be replaced by an empty copy. Merging is idempotent.
        Self::load_history(&mut st);
        Self::load_audit(&mut st);
        let device = st.device_id.clone();
        let t = now();
        // Full history stays on this device; the synced copy leaves out hosts
        // with "Sync command history" turned off (spec 8.2).
        let opted_out: Vec<String> = st
            .ids_of(K_HOST)
            .into_iter()
            .filter(|id| bool_field(&st.field(id, "sync_history")) == Some(false))
            .collect();
        let local = codec::encode_history(&st.history);
        let shared = codec::encode_history(&st.history.sync_snapshot(|h| !opted_out.iter().any(|o| o == h)));
        st.put_if_changed("history", &format!("history-local-{device}"), &local, t)?;
        st.vault.set_device_only(&format!("history-local-{device}"), true)?;
        st.put_if_changed("history", &format!("history-{device}"), &shared, t)?;
        // Audit stays on this device.
        let audit = st.audit.to_jsonl();
        st.put_if_changed("audit", &format!("audit-{device}"), audit.as_bytes(), t)?;
        st.vault.set_device_only(&format!("audit-{device}"), true)?;
        Ok(StoreBlobs { store: codec::encode_records(st.vault.records()) })
    }

    /// Loads a blob written by `export_store`. Call before unlocking or after.
    pub fn import_store(&self, store: Vec<u8>) -> anyhow::Result<()> {
        let records = codec::decode_records(&store)?;
        let mut st = self.st();
        st.vault.load(records);
        Self::load_history(&mut st);
        Self::load_audit(&mut st);
        st.load_known_hosts();
        Ok(())
    }

    /// Loads this device's saved audit log, so it carries on across restarts
    /// instead of starting empty and replacing the saved copy on next save.
    fn load_audit(st: &mut State) {
        let id = format!("audit-{}", st.device_id);
        let Ok(bytes) = st.vault.get(&id, "data") else { return };
        let (saved, broken) = AuditLog::from_jsonl(&String::from_utf8_lossy(&bytes));
        // Events recorded since unlocking are already in memory; keep
        // whichever log is longer (the saved one, unless nothing was lost).
        if saved.len() >= st.audit.len() {
            st.audit = saved;
            if let Some(seq) = broken {
                st.audit.append(now(), AuditKind::SettingChanged, None, None, &format!("saved audit log failed verification at event {seq}"));
            }
        }
    }

    fn load_history(st: &mut State) {
        let device = st.device_id.clone();
        let ids: Vec<String> = st.vault.records().filter(|r| r.kind == "history").map(|r| r.id.clone()).collect();
        for id in ids {
            if let Ok(bytes) = st.vault.get(&id, "data") {
                if let Ok(mut h) = codec::decode_history(&bytes) {
                    if id == format!("history-{device}") || id == format!("history-local-{device}") {
                        h.device_id = device.clone();
                    }
                    st.history.merge(&h);
                }
            }
        }
    }

    // ----- sync server (see server/README.md)

    /// Sync ids and token for this vault. Needs the vault unlocked.
    #[frb(sync)]
    pub fn sync_identity(&self) -> anyhow::Result<SyncIdentity> {
        let st = self.st();
        let id = st.vault.derive("sync-vault-id")?;
        let token = sha256::hex(&*st.vault.derive("sync-auth")?);
        Ok(SyncIdentity {
            vault_id: sha256::hex(&id[..16]),
            auth_hash: sha256::hex(&sha256::digest(token.as_bytes())),
            vault_token: token,
        })
    }

    /// Every record that may leave this device, each sealed for upload.
    /// "This device only" records (audit log, full local history, flagged
    /// credentials) are never included.
    pub fn sync_collect(&self) -> anyhow::Result<Vec<SyncItem>> {
        self.export_store()?; // seal current history into the vault first
        let st = self.st();
        let key_salt = st.vault.derive("sync-record-key")?;
        let mut out = Vec::new();
        for r in st.vault.outbound() {
            let plain = codec::encode_records(std::iter::once(r));
            let mut k = key_salt.to_vec();
            k.extend_from_slice(r.id.as_bytes());
            out.push(SyncItem {
                key: sha256::hex(&sha256::digest(&k)),
                digest: sha256::hex(&sha256::digest(&plain)),
                data: st.vault.seal_blob("sync-record", &plain)?,
            });
        }
        Ok(out)
    }

    /// Merges records pulled from the sync server (other devices' copies).
    /// Returns how many records changed locally.
    pub fn sync_apply(&self, records: Vec<Vec<u8>>) -> anyhow::Result<u32> {
        let mut st = self.st();
        let mut changed = 0;
        for sealed in records {
            let plain = st.vault.open_blob("sync-record", &sealed)?;
            for r in codec::decode_records(&plain)? {
                if st.vault.merge_remote(&r) != myterm_core::sync::Causality::Equal {
                    changed += 1;
                }
            }
        }
        if changed > 0 {
            Self::load_history(&mut st);
            Self::load_audit(&mut st);
            st.load_known_hosts();
        }
        Ok(changed)
    }

    #[allow(clippy::too_many_arguments)]
    /// Joins an existing vault (another device's) using its master password
    /// and the password-wrapped key from the sync server. This device's own
    /// hosts and credentials are re-encrypted under the joined vault's key
    /// and kept. Returns the joined master key for biometric wrapping.
    pub fn join_vault(
        &self,
        master_password: String,
        password_wrapped: Vec<u8>,
        kdf_salt: Vec<u8>,
        kdf_m_kib: u32,
        kdf_t: u32,
        kdf_p: u32,
        expected_vault_id: String,
    ) -> anyhow::Result<Vec<u8>> {
        let salt: [u8; 16] = kdf_salt.as_slice().try_into().map_err(|_| anyhow::anyhow!("bad salt"))?;
        let params = KdfParams { m_cost_kib: kdf_m_kib, t_cost: kdf_t, p_cost: kdf_p, salt };
        let kek = vault::derive_password_key(master_password.as_bytes(), &params)?;
        let mk = vault::unwrap_master_key(&password_wrapped, &kek, "password")
            .map_err(|_| anyhow::anyhow!("wrong master password for that vault"))?;
        let derived_id = sha256::hex(&mk.derive("sync-vault-id")[..16]);
        if derived_id != expected_vault_id {
            anyhow::bail!("the server's vault doesn't match that key");
        }
        let raw = mk.expose_for_wrapping().to_vec();
        let mut st = self.st();
        if st.vault.is_unlocked() {
            st.vault.rekey(mk)?;
        } else {
            st.vault.unlock(mk);
        }
        Self::load_history(&mut st);
        Self::load_audit(&mut st);
        st.load_known_hosts();
        Ok(raw)
    }

    /// Records to upload to the sync backend (ciphertext only).
    pub fn sync_outbound(&self) -> anyhow::Result<Vec<u8>> {
        let _ = self.export_store()?;
        let st = self.st();
        Ok(codec::encode_records(st.vault.outbound()))
    }

    /// Applies records downloaded from the sync backend.
    pub fn sync_inbound(&self, records: Vec<u8>) -> anyhow::Result<u32> {
        let recs = codec::decode_records(&records)?;
        let mut st = self.st();
        let mut changed = 0;
        for r in &recs {
            if st.vault.merge_remote(r) != myterm_core::sync::Causality::Equal {
                changed += 1;
            }
        }
        Self::load_history(&mut st);
        Self::load_audit(&mut st);
        Ok(changed)
    }

    // ----- hosts and groups

    pub fn upsert_host(&self, host: HostDto) -> anyhow::Result<()> {
        let mut st = self.st();
        let t = now();
        let id = host.id.clone();
        let fields: Vec<(&str, String)> = vec![
            ("label", host.label),
            ("address", host.address),
            ("port", host.port.map(|p| p.to_string()).unwrap_or_default()),
            ("username", host.username),
            ("group_id", host.group_id.unwrap_or_default()),
            ("credential_id", host.credential_id.unwrap_or_default()),
            ("hardware_key_id", host.hardware_key_id.unwrap_or_default()),
            ("tags", host.tags.join(",")),
            ("os", host.os.unwrap_or_default()),
            ("keybar_profile_id", host.keybar_profile_id.unwrap_or_default()),
            ("hardware_keys_only", bool_str(host.hardware_keys_only).into()),
            ("sync_history", bool_str(host.sync_history).into()),
            ("jump_chain", host.jump_chain.join(",")),
        ];
        for (name, value) in fields {
            if st.vault.get(&id, name).map(|v| s(&v) != value).unwrap_or(true) {
                st.vault.put(K_HOST, &id, name, value.as_bytes(), t)?;
            }
        }
        Ok(())
    }

    #[frb(sync)]
    pub fn list_hosts(&self) -> Vec<HostDto> {
        let st = self.st();
        let mut hosts: Vec<HostDto> = st.ids_of(K_HOST).iter().filter_map(|id| st.host(id)).collect();
        hosts.sort_by(|a, b| a.label.to_lowercase().cmp(&b.label.to_lowercase()));
        hosts
    }

    pub fn delete_host(&self, host_id: String) -> anyhow::Result<()> {
        Ok(self.st().vault.delete(&host_id, now())?)
    }

    pub fn upsert_group(&self, group: GroupDto) -> anyhow::Result<()> {
        let mut st = self.st();
        let t = now();
        let id = group.id.clone();
        for (name, value) in [
            ("name", group.name),
            ("parent_id", group.parent_id.unwrap_or_default()),
            ("port", group.port.map(|p| p.to_string()).unwrap_or_default()),
            ("username", group.username.unwrap_or_default()),
            ("credential_id", group.credential_id.unwrap_or_default()),
            ("hardware_keys_only", bool_str(group.hardware_keys_only).into()),
        ] {
            st.vault.put(K_GROUP, &id, name, value.as_bytes(), t)?;
        }
        Ok(())
    }

    #[frb(sync)]
    pub fn list_groups(&self) -> Vec<GroupDto> {
        let st = self.st();
        st.ids_of(K_GROUP).iter().map(|id| st.group(id)).collect()
    }

    // ----- credentials (secrets never come back out to Dart)

    pub fn put_credential(&self, cred: CredentialDto, secret: Vec<u8>) -> anyhow::Result<()> {
        let mut st = self.st();
        let t = now();
        let id = cred.id.clone();
        let secret = SecretBuf::new(secret);
        st.vault.put(K_CRED, &id, vault::SECRET_FIELD, secret.expose(), t)?;
        let kind = match cred.kind {
            CredentialKind::Password => "password",
            CredentialKind::PrivateKey => "private_key",
        };
        let confirm = match cred.confirm {
            ConfirmDto::Always => "always",
            ConfirmDto::OncePerSession => "once",
            ConfirmDto::Never => "never",
        };
        for (name, value) in [
            ("name", cred.name.as_str()),
            ("kind", kind),
            ("confirm", confirm),
            ("never_auto_type", if cred.never_auto_type { "1" } else { "0" }),
            ("append_enter", if cred.append_enter { "1" } else { "0" }),
        ] {
            st.vault.put(K_CRED, &id, name, value.as_bytes(), t)?;
        }
        st.vault.set_device_only(&id, cred.device_only)?;
        Ok(())
    }

    #[frb(sync)]
    pub fn list_credentials(&self) -> Vec<CredentialDto> {
        let st = self.st();
        st.ids_of(K_CRED)
            .iter()
            .filter_map(|id| {
                let m = st.cred_meta(id)?;
                let device_only = st.vault.records().any(|r| &r.id == id && r.device_only);
                Some(CredentialDto {
                    id: m.id,
                    name: m.name,
                    kind: if st.field(id, "kind") == "private_key" { CredentialKind::PrivateKey } else { CredentialKind::Password },
                    confirm: match m.confirm {
                        ConfirmPolicy::Always => ConfirmDto::Always,
                        ConfirmPolicy::OncePerSession => ConfirmDto::OncePerSession,
                        ConfirmPolicy::Never => ConfirmDto::Never,
                    },
                    never_auto_type: m.never_auto_type,
                    append_enter: m.append_enter,
                    device_only,
                })
            })
            .collect()
    }

    /// True when the host (or its group) has a saved password or key, so the
    /// app can ask for a password before connecting when it doesn't.
    #[frb(sync)]
    pub fn host_has_credential(&self, host_id: String) -> bool {
        let st = self.st();
        let Some(h) = st.host(&host_id) else { return false };
        model::resolve(&st.model_host(&h), &st.model_groups())
            .ok()
            .and_then(|r| r.identity_id)
            .is_some_and(|id| st.cred_meta(&id).is_some())
    }

    // ----- snippets and key bar

    pub fn upsert_snippet(&self, id: String, name: String, body: String) -> anyhow::Result<()> {
        let mut st = self.st();
        let t = now();
        st.vault.put(K_SNIPPET, &id, "name", name.as_bytes(), t)?;
        st.vault.put(K_SNIPPET, &id, "body", body.as_bytes(), t)?;
        Ok(())
    }

    #[frb(sync)]
    pub fn list_snippets(&self) -> Vec<SnippetDto> {
        self.st()
            .snippets()
            .into_iter()
            .map(|s| SnippetDto { params: s.params(), id: s.id, name: s.name, body: s.body })
            .collect()
    }

    #[frb(sync)]
    pub fn render_snippet(&self, snippet_id: String, values: HashMap<String, String>) -> Option<String> {
        self.st().snippets().into_iter().find(|s| s.id == snippet_id).map(|s| s.render(&values))
    }

    #[frb(sync)]
    pub fn builtin_keybars(&self) -> Vec<KeybarDto> {
        keybar::builtin_profiles()
            .into_iter()
            .map(|p| KeybarDto {
                id: p.id,
                name: p.name,
                rows: p
                    .rows
                    .into_iter()
                    .map(|row| {
                        row.into_iter()
                            .map(|k| KeyDto {
                                label: k.label,
                                action: match k.action {
                                    KeyAction::Sequence(seq) => KeyActionDto::Bytes(keybar::encode_sequence(&seq).unwrap_or_default()),
                                    KeyAction::Text(t) => KeyActionDto::Bytes(t.into_bytes()),
                                    KeyAction::Snippet(id) => KeyActionDto::Snippet(id),
                                    KeyAction::TypePassword { credential_id } => KeyActionDto::TypePassword(credential_id),
                                },
                            })
                            .collect()
                    })
                    .collect(),
            })
            .collect()
    }

    #[frb(sync)]
    pub fn encode_keys(&self, sequence: String) -> anyhow::Result<Vec<u8>> {
        Ok(keybar::encode_sequence(&sequence)?)
    }

    // ----- hardware keys

    /// authorized_keys line for a hardware key created by native code.
    #[frb(sync)]
    pub fn hardware_public_key_line(&self, public_sec1: Vec<u8>, comment: String) -> anyhow::Result<String> {
        let blob = sshkey::ecdsa_p256_blob(&public_sec1)?;
        Ok(sshkey::authorized_keys_line(&blob, &comment)?)
    }

    #[frb(sync)]
    pub fn hardware_key_fingerprint(&self, public_sec1: Vec<u8>) -> anyhow::Result<String> {
        Ok(sshkey::fingerprint_sha256(&sshkey::ecdsa_p256_blob(&public_sec1)?))
    }

    // ----- sessions

    /// Connects to a host and opens a shell. Dart receives a stream:
    /// `Started(id)`, then `Output`... and finally `Closed`, or `Error`.
    #[allow(clippy::too_many_arguments)]
    pub async fn connect(
        &self,
        host_id: String,
        cols: u32,
        rows: u32,
        auth: AuthChoice,
        events: StreamSink<SessionEventDto>,
        confirm_host_key: impl Fn(String, u16, String, String) -> DartFnFuture<bool> + Send + Sync + 'static,
        hardware_sign: impl Fn(String, Vec<u8>) -> DartFnFuture<Vec<u8>> + Send + Sync + 'static,
    ) -> anyhow::Result<()> {
        match self.connect_inner(host_id, cols, rows, auth, &events, confirm_host_key, hardware_sign).await {
            Ok(()) => Ok(()),
            Err(e) => {
                let _ = events.add(SessionEventDto::Error(e.to_string()));
                Ok(())
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    async fn connect_inner(
        &self,
        host_id: String,
        cols: u32,
        rows: u32,
        auth: AuthChoice,
        events: &StreamSink<SessionEventDto>,
        confirm_host_key: impl Fn(String, u16, String, String) -> DartFnFuture<bool> + Send + Sync + 'static,
        hardware_sign: impl Fn(String, Vec<u8>) -> DartFnFuture<Vec<u8>> + Send + Sync + 'static,
    ) -> anyhow::Result<()> {
        let sign: Arc<dyn Fn(String, Vec<u8>) -> DartFnFuture<Vec<u8>> + Send + Sync> = Arc::new(hardware_sign);
        let (opts, final_host_id) = {
            let st = self.st();
            let groups = st.model_groups();
            let host = st.host(&host_id).ok_or_else(|| anyhow::anyhow!("unknown host"))?;
            let resolved = model::resolve(&st.model_host(&host), &groups)?;
            let mut chain_ids = resolved.jump_chain.clone();
            chain_ids.push(host_id.clone());
            let mut chain = Vec::new();
            for (i, hid) in chain_ids.iter().enumerate() {
                let is_last = i == chain_ids.len() - 1;
                let h = st.host(hid).ok_or_else(|| anyhow::anyhow!("unknown jump host {hid}"))?;
                let r = model::resolve(&st.model_host(&h), &groups)?;
                let user = if h.username.is_empty() {
                    r.group_path.iter().find_map(|g| st.group(g).username).unwrap_or_else(|| "root".into())
                } else {
                    h.username.clone()
                };
                let auth = match (&auth, is_last) {
                    (AuthChoice::Hardware { key_id, public_sec1, protection }, true) => Auth::Hardware(Arc::new(DartSigner {
                        public: public_sec1.clone(),
                        protection: protection_from(protection),
                        key_id: key_id.clone(),
                        sign: sign.clone(),
                    })),
                    (AuthChoice::Password { password }, true) => Auth::Password(SecretBuf::new(password.clone())),
                    _ => {
                        let cred_id = r
                            .identity_id
                            .clone()
                            .ok_or_else(|| anyhow::anyhow!("no saved password or key for {}", h.label))?;
                        let secret = st.vault.secret(&cred_id)?;
                        if st.field(&cred_id, "kind") == "private_key" {
                            Auth::PrivateKey { openssh: secret, passphrase: None }
                        } else {
                            Auth::Password(secret)
                        }
                    }
                };
                chain.push(Target { host: r.address, port: r.port, user, auth, hardware_keys_only: r.hardware_keys_only });
            }
            (
                ConnectOptions {
                    chain,
                    known_hosts: st.known_hosts.clone(),
                    prompt: Arc::new(DartPrompt(Arc::new(confirm_host_key))),
                    device_id: st.device_id.clone(),
                    now_unix: now(),
                    cols,
                    rows,
                    term: "xterm-256color".into(),
                },
                host_id.clone(),
            )
        };

        let (tx, mut rx) = mpsc::unbounded_channel();
        let connected = rt().spawn(ssh::open_shell(opts, tx)).await?;
        let (handle, approved) = match connected {
            Ok(v) => v,
            Err(e) => {
                let mut st = self.st();
                if let ssh::SshError::HostKeyRejected { .. } = e {
                    st.audit.append(now(), AuditKind::HostKeyChangedRejected, Some(&final_host_id), None, &e.to_string());
                }
                return Err(e.into());
            }
        };

        let id = {
            let mut st = self.st();
            for k in approved {
                st.audit.append(now(), AuditKind::HostKeyApproved, Some(&final_host_id), None, &k.pattern);
                // Persist, or the host is "unknown" again after a restart.
                if let Err(e) = st.put_known_host(&k) {
                    st.audit.append(now(), AuditKind::SettingChanged, Some(&final_host_id), None, &format!("could not save host key: {e}"));
                }
                st.known_hosts.retain(|x| !(x.pattern == k.pattern && x.key_type == k.key_type));
                st.known_hosts.push(k);
            }
            st.audit.append(now(), AuditKind::Connect, Some(&final_host_id), None, "");
            let id = st.next_session;
            st.next_session += 1;
            st.sessions.insert(id, Live { session: Session::new(final_host_id.clone(), handle) });
            id
        };

        let _ = events.add(SessionEventDto::Started(id));
        let state = self.state.clone();
        let events = events.clone();
        rt().spawn(async move {
            while let Some(ev) = rx.recv().await {
                match ev {
                    Event::Output(bytes) => {
                        let mut guard = state.lock().unwrap_or_else(|e| e.into_inner());
                        let st = &mut *guard;
                        if let Some(l) = st.sessions.get_mut(&id) {
                            l.session.on_output(&bytes);
                            // Combined `cd`s seen via the prompt (cdtrack).
                            let ctx = RecordContext { incognito: l.session.incognito, prompt: None };
                            let host = l.session.host_id.clone();
                            for cmd in l.session.take_derived() {
                                let _ = st.history.record(&host, &cmd, now(), ctx);
                            }
                        }
                        drop(guard);
                        let _ = events.add(SessionEventDto::Output(bytes));
                    }
                    Event::Exit(code) => {
                        let _ = events.add(SessionEventDto::Exit(code));
                    }
                    Event::Closed => break,
                }
            }
            let mut st = state.lock().unwrap_or_else(|e| e.into_inner());
            if let Some(l) = st.sessions.remove(&id) {
                let host = l.session.host_id.clone();
                st.audit.append(now(), AuditKind::Disconnect, Some(&host), None, "");
            }
            let _ = events.add(SessionEventDto::Closed);
        });
        Ok(())
    }

    /// Keystrokes from the terminal view or key bar.
    #[frb(sync)]
    pub fn send_input(&self, session_id: u32, data: Vec<u8>) -> anyhow::Result<()> {
        let mut st = self.st();
        let st = &mut *st;
        let live = st.sessions.get_mut(&session_id).ok_or_else(|| anyhow::anyhow!("no session"))?;
        if let Some(cmd) = live.session.on_input(&data)? {
            if cmd.reliable {
                let host = live.session.host_id.clone();
                let ctx = RecordContext { incognito: live.session.incognito, prompt: Some(cmd.prompt) };
                let _ = st.history.record(&host, &cmd.line, now(), ctx);
            }
        }
        // Combined `cd`s from typed commands (cdtrack fallback mode).
        let ctx = RecordContext { incognito: live.session.incognito, prompt: None };
        let host = live.session.host_id.clone();
        for cmd in live.session.take_derived() {
            let _ = st.history.record(&host, &cmd, now(), ctx);
        }
        Ok(())
    }

    #[frb(sync)]
    pub fn resize(&self, session_id: u32, cols: u32, rows: u32) {
        if let Some(l) = self.st().sessions.get_mut(&session_id) {
            l.session.channel_mut().resize(cols, rows);
        }
    }

    #[frb(sync)]
    pub fn disconnect(&self, session_id: u32) {
        if let Some(l) = self.st().sessions.get_mut(&session_id) {
            l.session.channel_mut().close();
        }
    }

    #[frb(sync)]
    pub fn set_incognito(&self, session_id: u32, incognito: bool) {
        if let Some(l) = self.st().sessions.get_mut(&session_id) {
            l.session.incognito = incognito;
        }
    }

    /// Autocomplete suggestions for what's typed so far in a session.
    #[frb(sync)]
    pub fn suggest(&self, session_id: u32, limit: u32) -> Vec<SuggestionDto> {
        let st = self.st();
        let Some(live) = st.sessions.get(&session_id) else { return Vec::new() };
        let line = live.session.current_line();
        let host_id = live.session.host_id.clone();
        let hosts: Vec<Host> = st.ids_of(K_HOST).iter().filter_map(|id| st.host(id)).map(|h| st.model_host(&h)).collect();
        // History is shared across hosts: hosts in the same group or with the
        // same OS come first, then every other host's history.
        let mut related = hosts.iter().find(|h| h.id == host_id).map(|h| model::related_hosts(h, &hosts)).unwrap_or_default();
        for other in st.history.host_ids() {
            if other != host_id && !related.iter().any(|r| r == other) {
                related.push(other.to_string());
            }
        }
        let input = CompletionInput { host_id: &host_id, related_hosts: &related, line: &line, now: now(), limit: limit as usize };
        completion::suggest(&input, &st.snippets(), &st.history, completion::BUILTIN_SPECS, None)
            .into_iter()
            .map(|s| SuggestionDto {
                line: s.line,
                source: format!("{:?}", s.source),
                snippet_id: s.snippet_id,
            })
            .collect()
    }

    #[frb(sync)]
    pub fn accept_suggestion(&self, session_id: u32, line: String) -> anyhow::Result<()> {
        let mut st = self.st();
        let live = st.sessions.get_mut(&session_id).ok_or_else(|| anyhow::anyhow!("no session"))?;
        live.session.accept_suggestion(&line)?;
        Ok(())
    }

    /// Spec 3.5 C. Call with confirmed=false first; on NeedConfirmation run the
    /// biometric prompt and call again with confirmed=true.
    #[frb(sync)]
    pub fn type_password(
        &self,
        session_id: u32,
        credential_id: Option<String>,
        override_prompt: bool,
        confirmed: bool,
    ) -> anyhow::Result<InjectDto> {
        let mut st = self.st();
        let st = &mut *st;
        let host_id = st.sessions.get(&session_id).ok_or_else(|| anyhow::anyhow!("no session"))?.session.host_id.clone();
        let cred_id = match credential_id {
            Some(c) => c,
            None => {
                let h = st.host(&host_id).ok_or_else(|| anyhow::anyhow!("unknown host"))?;
                let r = model::resolve(&st.model_host(&h), &st.model_groups())?;
                r.identity_id.ok_or_else(|| anyhow::anyhow!("host has no stored password"))?
            }
        };
        let meta = st.cred_meta(&cred_id).ok_or_else(|| anyhow::anyhow!("unknown credential"))?;
        let vault = &st.vault;
        let live = st.sessions.get_mut(&session_id).ok_or_else(|| anyhow::anyhow!("no session"))?;
        let (decision, audit) = live.session.type_password(&meta, override_prompt, confirmed, now(), || {
            vault.secret(&cred_id).map_err(|e| std::io::Error::other(e.to_string()))
        })?;
        if let Some(a) = audit {
            st.audit.append(a.at_unix, AuditKind::CredentialTyped, Some(&a.host_id), Some(&a.credential_id), &format!("{:?}", a.prompt));
        }
        Ok(match decision {
            InjectDecision::Proceed => InjectDto::Proceeded,
            InjectDecision::NeedConfirmation => InjectDto::NeedConfirmation,
            InjectDecision::AskOverride => InjectDto::AskOverride,
            InjectDecision::Refuse(Refusal::NeverAutoType) => InjectDto::RefusedNeverAutoType,
            InjectDecision::Refuse(Refusal::WrongPromptKind) => InjectDto::RefusedOneTimeCode,
        })
    }

    // ----- audit

    pub fn export_audit_jsonl(&self) -> String {
        self.st().audit.to_jsonl()
    }

    /// Records a sync outcome in the audit log (counts and errors only).
    pub fn audit_sync(&self, detail: String) {
        self.st().audit.append(now(), AuditKind::Sync, None, None, &detail);
    }
}

/// Windows: protects bytes with DPAPI for the current Windows user (used to
/// keep the vault key for "Unlock with Windows Hello"). Errors elsewhere.
pub fn os_protect(data: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    crate::dpapi::protect(&data)
}

/// Windows: reverses [`os_protect`]. Errors elsewhere.
pub fn os_unprotect(data: Vec<u8>) -> anyhow::Result<Vec<u8>> {
    crate::dpapi::unprotect(&data)
}
