//! MyTerm vault (spec 6.1).
//!
//! - A random 256-bit **master key (MK)** encrypts everything.
//! - MK is wrapped twice: by a key derived from the master password with
//!   **Argon2id** (recovery, new devices) and by a platform hardware key
//!   (everyday biometric unlock; the wrapping happens in native code, which
//!   hands MK to [`Vault::unlock`] via [`MasterKey::from_bytes`]).
//! - Each field is sealed with **XChaCha20-Poly1305** under a per-record key
//!   derived from MK with HKDF-SHA256. Record kind, id and field name are
//!   bound as associated data, so ciphertext can't be moved between records.
//! - Sealed fields are stored in [`myterm_core::sync::SyncRecord`]s, so the
//!   sync server only ever sees ciphertext.

use std::collections::BTreeMap;
use std::fmt;

use argon2::{Algorithm, Argon2, Params, Version};
use chacha20poly1305::aead::{Aead, KeyInit, Payload};
use chacha20poly1305::{Key, XChaCha20Poly1305, XNonce};
use hkdf::Hkdf;
use myterm_core::inject::SecretBuf;
use myterm_core::sync::{self, Causality, SyncRecord};
use myterm_core::{DeviceId, Id};
use rand_core::{OsRng, RngCore};
use sha2::Sha256;
use zeroize::{Zeroize, Zeroizing};

const FORMAT_V1: u8 = 1;
const NONCE_LEN: usize = 24;

#[derive(Debug, PartialEq, Eq)]
pub enum VaultError {
    Locked,
    NotFound,
    /// Wrong key, wrong password, or tampered data. Deliberately vague.
    Decrypt,
    BadFormat,
    Kdf(String),
}

impl fmt::Display for VaultError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VaultError::Locked => write!(f, "vault is locked"),
            VaultError::NotFound => write!(f, "record or field not found"),
            VaultError::Decrypt => write!(f, "decryption failed"),
            VaultError::BadFormat => write!(f, "unrecognised data format"),
            VaultError::Kdf(e) => write!(f, "key derivation failed: {e}"),
        }
    }
}

impl std::error::Error for VaultError {}

/// Argon2id parameters, stored alongside the password-wrapped MK.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KdfParams {
    pub m_cost_kib: u32,
    pub t_cost: u32,
    pub p_cost: u32,
    pub salt: [u8; 16],
}

impl KdfParams {
    fn with_salt(m_cost_kib: u32, t_cost: u32, p_cost: u32) -> Self {
        let mut salt = [0u8; 16];
        OsRng.fill_bytes(&mut salt);
        KdfParams { m_cost_kib, t_cost, p_cost, salt }
    }

    /// Phones: 64 MiB, 3 passes, 1 lane.
    pub fn mobile() -> Self {
        Self::with_salt(64 * 1024, 3, 1)
    }

    /// Desktops: 256 MiB, 3 passes, 4 lanes.
    pub fn desktop() -> Self {
        Self::with_salt(256 * 1024, 3, 4)
    }
}

/// The vault master key. Zeroed on drop; never printed.
pub struct MasterKey(Zeroizing<[u8; 32]>);

impl fmt::Debug for MasterKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("MasterKey(<redacted>)")
    }
}

impl MasterKey {
    pub fn generate() -> Self {
        let mut k = Zeroizing::new([0u8; 32]);
        OsRng.fill_bytes(k.as_mut());
        MasterKey(k)
    }

    /// Used after native code unwraps MK with the hardware device key.
    pub fn from_bytes(mut bytes: [u8; 32]) -> Self {
        let mk = MasterKey(Zeroizing::new(bytes));
        bytes.zeroize();
        mk
    }

    /// Raw bytes for native code to wrap with the hardware device key.
    pub fn expose_for_wrapping(&self) -> &[u8; 32] {
        &self.0
    }

    /// 32 bytes derived from MK for a named purpose (HKDF-SHA256). Used for
    /// the sync vault id, the sync access token and sync record keys, so
    /// every device holding MK derives the same values.
    pub fn derive(&self, purpose: &str) -> Zeroizing<[u8; 32]> {
        let hk = Hkdf::<Sha256>::new(Some(b"myterm-derive-v1"), self.0.as_ref());
        let mut out = Zeroizing::new([0u8; 32]);
        hk.expand(purpose.as_bytes(), out.as_mut()).expect("32 bytes is a valid HKDF-SHA256 output length");
        out
    }

    fn record_key(&self, kind: &str, id: &str) -> Zeroizing<[u8; 32]> {
        let hk = Hkdf::<Sha256>::new(Some(b"myterm-record-v1"), self.0.as_ref());
        let mut info = Vec::with_capacity(kind.len() + id.len() + 1);
        info.extend_from_slice(kind.as_bytes());
        info.push(0);
        info.extend_from_slice(id.as_bytes());
        let mut out = Zeroizing::new([0u8; 32]);
        hk.expand(&info, out.as_mut()).expect("32 bytes is a valid HKDF-SHA256 output length");
        out
    }

    fn aad(kind: &str, id: &str, field: &str) -> Vec<u8> {
        [kind.as_bytes(), id.as_bytes(), field.as_bytes()].join(&0u8)
    }

    /// Encrypts one field: `version || nonce || ciphertext+tag`.
    pub fn seal(&self, kind: &str, id: &str, field: &str, plaintext: &[u8]) -> Vec<u8> {
        let key = self.record_key(kind, id);
        seal_with(&key, &Self::aad(kind, id, field), plaintext)
    }

    pub fn open(&self, kind: &str, id: &str, field: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        let key = self.record_key(kind, id);
        open_with(&key, &Self::aad(kind, id, field), sealed)
    }
}

fn seal_with(key: &[u8; 32], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let mut nonce = [0u8; NONCE_LEN];
    OsRng.fill_bytes(&mut nonce);
    let ct = cipher
        .encrypt(XNonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .expect("XChaCha20-Poly1305 encryption cannot fail for in-memory buffers");
    let mut out = Vec::with_capacity(1 + NONCE_LEN + ct.len());
    out.push(FORMAT_V1);
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

fn open_with(key: &[u8; 32], aad: &[u8], sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, VaultError> {
    if sealed.len() < 1 + NONCE_LEN + 16 || sealed[0] != FORMAT_V1 {
        return Err(VaultError::BadFormat);
    }
    let cipher = XChaCha20Poly1305::new(Key::from_slice(key));
    let nonce = XNonce::from_slice(&sealed[1..1 + NONCE_LEN]);
    cipher
        .decrypt(nonce, Payload { msg: &sealed[1 + NONCE_LEN..], aad })
        .map(Zeroizing::new)
        .map_err(|_| VaultError::Decrypt)
}

/// Derives the password key-encryption key with Argon2id.
pub fn derive_password_key(password: &[u8], p: &KdfParams) -> Result<Zeroizing<[u8; 32]>, VaultError> {
    let params = Params::new(p.m_cost_kib, p.t_cost, p.p_cost, Some(32)).map_err(|e| VaultError::Kdf(e.to_string()))?;
    let argon = Argon2::new(Algorithm::Argon2id, Version::V0x13, params);
    let mut out = Zeroizing::new([0u8; 32]);
    argon.hash_password_into(password, &p.salt, out.as_mut()).map_err(|e| VaultError::Kdf(e.to_string()))?;
    Ok(out)
}

/// MK wrapped under a key-encryption key, with a label naming the wrapper.
pub fn wrap_master_key(mk: &MasterKey, kek: &[u8; 32], label: &str) -> Vec<u8> {
    seal_with(kek, format!("myterm-mk-v1|{label}").as_bytes(), mk.0.as_ref())
}

pub fn unwrap_master_key(wrapped: &[u8], kek: &[u8; 32], label: &str) -> Result<MasterKey, VaultError> {
    let raw = open_with(kek, format!("myterm-mk-v1|{label}").as_bytes(), wrapped)?;
    let bytes: [u8; 32] = raw.as_slice().try_into().map_err(|_| VaultError::BadFormat)?;
    Ok(MasterKey::from_bytes(bytes))
}

/// Field name holding a credential's secret (password, private key, passphrase).
pub const SECRET_FIELD: &str = "secret";

pub struct Vault {
    pub device_id: DeviceId,
    mk: Option<MasterKey>,
    records: BTreeMap<Id, SyncRecord>,
}

impl Vault {
    pub fn new(device_id: impl Into<DeviceId>) -> Self {
        Vault { device_id: device_id.into(), mk: None, records: BTreeMap::new() }
    }

    pub fn unlock(&mut self, mk: MasterKey) {
        self.mk = Some(mk);
    }

    /// Drops MK (zeroed). Records stay, as ciphertext.
    pub fn lock(&mut self) {
        self.mk = None;
    }

    pub fn is_unlocked(&self) -> bool {
        self.mk.is_some()
    }

    fn mk(&self) -> Result<&MasterKey, VaultError> {
        self.mk.as_ref().ok_or(VaultError::Locked)
    }

    /// See [`MasterKey::derive`].
    pub fn derive(&self, purpose: &str) -> Result<Zeroizing<[u8; 32]>, VaultError> {
        Ok(self.mk()?.derive(purpose))
    }

    /// Encrypts a whole blob (e.g. a record for upload) under MK.
    pub fn seal_blob(&self, purpose: &str, plaintext: &[u8]) -> Result<Vec<u8>, VaultError> {
        Ok(self.mk()?.seal("blob", purpose, "", plaintext))
    }

    pub fn open_blob(&self, purpose: &str, sealed: &[u8]) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        self.mk()?.open("blob", purpose, "", sealed)
    }

    /// Re-encrypts every record under `new` and switches to it. Used when this
    /// device joins another device's vault, so its existing hosts and
    /// credentials carry over. All-or-nothing: on error nothing changes.
    pub fn rekey(&mut self, new: MasterKey) -> Result<(), VaultError> {
        let old = self.mk()?;
        let mut records = self.records.clone();
        for r in records.values_mut() {
            for (name, f) in r.fields.iter_mut() {
                let plain = old.open(&r.kind, &r.id, name, &f.value)?;
                f.value = new.seal(&r.kind, &r.id, name, &plain);
            }
        }
        self.records = records;
        self.mk = Some(new);
        Ok(())
    }

    pub fn put(&mut self, kind: &str, id: &str, field: &str, plaintext: &[u8], now: u64) -> Result<(), VaultError> {
        let sealed = self.mk()?.seal(kind, id, field, plaintext);
        let device = self.device_id.clone();
        self.records
            .entry(id.to_string())
            .or_insert_with(|| SyncRecord::new(id, kind))
            .set(field, sealed, now, &device);
        Ok(())
    }

    pub fn get(&self, id: &str, field: &str) -> Result<Zeroizing<Vec<u8>>, VaultError> {
        let mk = self.mk()?;
        let rec = self.records.get(id).filter(|r| !r.is_deleted()).ok_or(VaultError::NotFound)?;
        let f = rec.fields.get(field).ok_or(VaultError::NotFound)?;
        mk.open(&rec.kind, &rec.id, field, &f.value)
    }

    /// A credential's secret as a self-wiping buffer for "Type password".
    pub fn secret(&self, credential_id: &str) -> Result<SecretBuf, VaultError> {
        let v = self.get(credential_id, SECRET_FIELD)?;
        Ok(SecretBuf::new(v.to_vec()))
    }

    pub fn delete(&mut self, id: &str, now: u64) -> Result<(), VaultError> {
        let device = self.device_id.clone();
        self.records.get_mut(id).ok_or(VaultError::NotFound)?.delete(now, &device);
        Ok(())
    }

    /// Keep a credential off sync (spec 8.1 "this device only").
    pub fn set_device_only(&mut self, id: &str, device_only: bool) -> Result<(), VaultError> {
        self.records.get_mut(id).ok_or(VaultError::NotFound)?.device_only = device_only;
        Ok(())
    }

    /// Records to upload (ciphertext only, excludes device-only ones).
    pub fn outbound(&self) -> Vec<&SyncRecord> {
        sync::outbound(self.records.values())
    }

    /// Applies a record received from sync. Works while locked: it's ciphertext.
    pub fn merge_remote(&mut self, remote: &SyncRecord) -> Causality {
        match self.records.get_mut(&remote.id) {
            Some(local) if local.device_only => Causality::Equal,
            Some(local) => local.merge(remote),
            None => {
                self.records.insert(remote.id.clone(), remote.clone());
                Causality::Newer
            }
        }
    }

    /// Every record, for persistence to local storage.
    pub fn records(&self) -> impl Iterator<Item = &SyncRecord> {
        self.records.values()
    }

    pub fn load(&mut self, records: impl IntoIterator<Item = SyncRecord>) {
        for r in records {
            self.records.insert(r.id.clone(), r);
        }
    }
}

#[cfg(test)]
mod sync_tests {
    use super::*;
    use myterm_core::codec;

    /// What the app does on each sync: seal every outbound record.
    fn collect(v: &Vault) -> Vec<Vec<u8>> {
        v.outbound()
            .into_iter()
            .map(|r| v.seal_blob("sync-record", &codec::encode_records(std::iter::once(r))).unwrap())
            .collect()
    }

    fn apply(v: &mut Vault, blobs: &[Vec<u8>]) {
        for b in blobs {
            let plain = v.open_blob("sync-record", b).unwrap();
            for r in codec::decode_records(&plain).unwrap() {
                v.merge_remote(&r);
            }
        }
    }

    #[test]
    fn two_devices_join_and_converge() {
        let password_kdf = KdfParams { m_cost_kib: 64, t_cost: 1, p_cost: 1, salt: [3; 16] };
        // Phone creates the vault and some data.
        let mk = MasterKey::generate();
        let wrapped = wrap_master_key(&mk, &derive_password_key(b"correct horse", &password_kdf).unwrap(), "password");
        let mut phone = Vault::new("phone");
        phone.unlock(mk);
        phone.put("host", "server1", "address", b"192.0.2.17", 10).unwrap();
        phone.put("credential", "pw", SECRET_FIELD, b"s3cret", 10).unwrap();
        phone.put("audit", "audit-phone", "data", b"local only", 10).unwrap();
        phone.set_device_only("audit-phone", true).unwrap();

        // Desk already had its own vault with a host, then joins with the password.
        let mut desk = Vault::new("desk");
        desk.unlock(MasterKey::generate());
        desk.put("host", "nas", "address", b"192.0.2.59", 5).unwrap();
        let joined = unwrap_master_key(&wrapped, &derive_password_key(b"correct horse", &password_kdf).unwrap(), "password").unwrap();
        assert_eq!(*joined.derive("sync-auth"), *phone.derive("sync-auth").unwrap(), "same vault token");
        desk.rekey(joined).unwrap();

        // Exchange.
        let from_phone = collect(&phone);
        assert_eq!(from_phone.len(), 2, "device-only audit not uploaded");
        apply(&mut desk, &from_phone);
        apply(&mut phone, &collect(&desk));

        for v in [&phone, &desk] {
            assert_eq!(&*v.get("server1", "address").unwrap(), b"192.0.2.17");
            assert_eq!(&*v.get("nas", "address").unwrap(), b"192.0.2.59", "desk's own host kept");
            assert_eq!(v.secret("pw").unwrap().expose(), b"s3cret");
        }

        // Concurrent edits to different fields both survive; newer edit of the same field wins.
        phone.put("host", "server1", "label", b"server1", 20).unwrap();
        desk.put("host", "server1", "address", b"192.0.2.18", 21).unwrap();
        let (p, d) = (collect(&phone), collect(&desk));
        apply(&mut desk, &p);
        apply(&mut phone, &d);
        for v in [&phone, &desk] {
            assert_eq!(&*v.get("server1", "label").unwrap(), b"server1");
            assert_eq!(&*v.get("server1", "address").unwrap(), b"192.0.2.18");
        }
        assert!(desk.get("audit-phone", "data").is_err(), "audit never left the phone");

        // A blob from a different vault is refused.
        let mut stranger = Vault::new("x");
        stranger.unlock(MasterKey::generate());
        stranger.put("host", "evil", "address", b"1.2.3.4", 1).unwrap();
        let bad = collect(&stranger);
        assert!(desk.open_blob("sync-record", &bad[0]).is_err());
    }

    #[test]
    fn derive_is_deterministic_per_key_and_purpose() {
        let mk = MasterKey::from_bytes([7; 32]);
        let same = MasterKey::from_bytes([7; 32]);
        let other = MasterKey::from_bytes([8; 32]);
        assert_eq!(*mk.derive("sync-auth"), *same.derive("sync-auth"));
        assert_ne!(*mk.derive("sync-auth"), *mk.derive("sync-vault-id"));
        assert_ne!(*mk.derive("sync-auth"), *other.derive("sync-auth"));
    }

    #[test]
    fn blobs_roundtrip_and_bind_purpose() {
        let mut v = Vault::new("phone");
        v.unlock(MasterKey::generate());
        let sealed = v.seal_blob("sync-record", b"hello").unwrap();
        assert_eq!(&*v.open_blob("sync-record", &sealed).unwrap(), b"hello");
        assert!(v.open_blob("other", &sealed).is_err());
        v.lock();
        assert!(matches!(v.seal_blob("x", b"y"), Err(VaultError::Locked)));
    }

    #[test]
    fn rekey_keeps_data_readable_under_the_new_key_only() {
        let mut v = Vault::new("phone");
        v.unlock(MasterKey::from_bytes([1; 32]));
        v.put("host", "h1", "address", b"192.0.2.17", 1).unwrap();
        v.put("credential", "c1", SECRET_FIELD, b"hunter2", 1).unwrap();
        let stamp_before = v.records().find(|r| r.id == "h1").unwrap().fields["address"].stamp.clone();

        v.rekey(MasterKey::from_bytes([2; 32])).unwrap();
        assert_eq!(&*v.get("h1", "address").unwrap(), b"192.0.2.17");
        assert_eq!(v.secret("c1").unwrap().expose(), b"hunter2");
        assert_eq!(v.records().find(|r| r.id == "h1").unwrap().fields["address"].stamp, stamp_before, "stamps kept");

        let mut old = Vault::new("x");
        old.load(v.records().cloned());
        old.unlock(MasterKey::from_bytes([1; 32]));
        assert!(old.get("h1", "address").is_err(), "old key no longer opens it");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fast_kdf() -> KdfParams {
        KdfParams { m_cost_kib: 64, t_cost: 1, p_cost: 1, salt: [9; 16] }
    }

    #[test]
    fn password_wrap_roundtrip() {
        let mk = MasterKey::generate();
        let kek = derive_password_key(b"correct horse", &fast_kdf()).unwrap();
        let wrapped = wrap_master_key(&mk, &kek, "password");
        let back = unwrap_master_key(&wrapped, &kek, "password").unwrap();
        assert_eq!(back.expose_for_wrapping(), mk.expose_for_wrapping());

        let wrong = derive_password_key(b"wrong horse", &fast_kdf()).unwrap();
        assert_eq!(unwrap_master_key(&wrapped, &wrong, "password").unwrap_err(), VaultError::Decrypt);
        assert_eq!(unwrap_master_key(&wrapped, &kek, "device").unwrap_err(), VaultError::Decrypt, "label bound");
    }

    #[test]
    fn fields_are_bound_to_their_record() {
        let mk = MasterKey::generate();
        let sealed = mk.seal("credential", "c1", "secret", b"hunter2");
        assert_eq!(mk.open("credential", "c1", "secret", &sealed).unwrap().as_slice(), b"hunter2");
        assert_eq!(mk.open("credential", "c2", "secret", &sealed).unwrap_err(), VaultError::Decrypt);
        assert_eq!(mk.open("credential", "c1", "username", &sealed).unwrap_err(), VaultError::Decrypt);
        let mut tampered = sealed.clone();
        *tampered.last_mut().unwrap() ^= 1;
        assert_eq!(mk.open("credential", "c1", "secret", &tampered).unwrap_err(), VaultError::Decrypt);
        assert_ne!(sealed, mk.seal("credential", "c1", "secret", b"hunter2"), "random nonce");
    }

    #[test]
    fn vault_lock_and_sync() {
        let mk_bytes = *MasterKey::generate().expose_for_wrapping();
        let mut phone = Vault::new("phone");
        phone.unlock(MasterKey::from_bytes(mk_bytes));
        phone.put("credential", "sudo", SECRET_FIELD, b"pw1", 10).unwrap();
        phone.put("credential", "root", SECRET_FIELD, b"pw-root", 10).unwrap();
        phone.set_device_only("root", true).unwrap();

        // Desk receives ciphertext while locked, then unlocks with the same MK.
        let mut desk = Vault::new("desk");
        for r in phone.outbound() {
            desk.merge_remote(r);
        }
        assert_eq!(desk.secret("sudo").unwrap_err(), VaultError::Locked);
        desk.unlock(MasterKey::from_bytes(mk_bytes));
        assert_eq!(desk.secret("sudo").unwrap().expose(), b"pw1");
        assert_eq!(desk.secret("root").unwrap_err(), VaultError::NotFound, "device-only never synced");

        // Ciphertext on the wire doesn't contain the plaintext.
        let wire = &phone.outbound()[0].fields[SECRET_FIELD].value;
        assert!(!wire.windows(3).any(|w| w == b"pw1"));

        phone.lock();
        assert!(!phone.is_unlocked());
        assert_eq!(phone.get("sudo", SECRET_FIELD).unwrap_err(), VaultError::Locked);
    }
}
