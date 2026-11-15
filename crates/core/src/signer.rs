//! Hardware signer interface and host key policy (spec 3.4.1).
//!
//! Each platform implements [`HardwareSigner`] in native code
//! (Android Keystore, Secure Enclave, TPM/CNG, tpm2-pkcs11, libfido2) and
//! exposes it to the core through the FFI layer. The private key never
//! leaves the hardware: the core hands over bytes to sign and receives a
//! DER signature, which it converts to SSH format.

use std::fmt;

use crate::sshkey::{self, KeyError};

/// Where a private key lives, from strongest to weakest. Taken from platform
/// attestation where available.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Protection {
    Software,
    Tee,
    Tpm,
    SecurityKey,
    StrongBox,
    SecureEnclave,
}

impl Protection {
    pub fn is_hardware(self) -> bool {
        self != Protection::Software
    }

    pub fn label(self) -> &'static str {
        match self {
            Protection::Software => "Software",
            Protection::Tee => "TEE",
            Protection::Tpm => "TPM",
            Protection::SecurityKey => "Security key",
            Protection::StrongBox => "StrongBox",
            Protection::SecureEnclave => "Secure Enclave",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HardwareKeyInfo {
    pub key_id: String,
    pub protection: Protection,
    /// SEC1 uncompressed P-256 point (65 bytes).
    pub public_sec1: Vec<u8>,
    /// True if every signature requires biometrics / PIN / touch.
    pub user_presence_required: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub enum SignerError {
    UserCancelled,
    KeyInvalidated,
    Unavailable(String),
    Format(KeyError),
}

impl fmt::Display for SignerError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            SignerError::UserCancelled => write!(f, "authentication cancelled"),
            SignerError::KeyInvalidated => write!(f, "key invalidated (biometrics changed); create a new key"),
            SignerError::Unavailable(why) => write!(f, "hardware signer unavailable: {why}"),
            SignerError::Format(e) => write!(f, "{e}"),
        }
    }
}

impl std::error::Error for SignerError {}

impl From<KeyError> for SignerError {
    fn from(e: KeyError) -> Self {
        SignerError::Format(e)
    }
}

pub trait HardwareSigner: Send + Sync {
    fn info(&self) -> HardwareKeyInfo;
    /// Signs `data` with ECDSA P-256 / SHA-256 and returns a DER signature.
    /// May block while the OS shows a biometric prompt.
    fn sign_der(&self, data: &[u8]) -> Result<Vec<u8>, SignerError>;
}

/// SSH public key blob for a hardware key.
pub fn public_blob(signer: &dyn HardwareSigner) -> Result<Vec<u8>, SignerError> {
    Ok(sshkey::ecdsa_p256_blob(&signer.info().public_sec1)?)
}

/// Signs SSH userauth data and returns an SSH signature blob.
pub fn ssh_sign(signer: &dyn HardwareSigner, data: &[u8]) -> Result<Vec<u8>, SignerError> {
    let der = signer.sign_der(data)?;
    Ok(sshkey::der_to_ssh_ecdsa_signature(&der)?)
}

#[derive(Debug, PartialEq, Eq)]
pub enum PolicyViolation {
    HardwareRequired { offered: Protection },
}

/// Enforces a host's "hardware keys only" setting before a key is offered.
pub fn check_policy(hardware_keys_only: bool, offered: Protection) -> Result<(), PolicyViolation> {
    if hardware_keys_only && !offered.is_hardware() {
        return Err(PolicyViolation::HardwareRequired { offered });
    }
    Ok(())
}

/// Spec 3.4.1 step 6: warn until each host has at least two independent keys.
pub fn recovery_warning(enrolled_keys_on_host: usize) -> Option<&'static str> {
    match enrolled_keys_on_host {
        0 => Some("No key enrolled on this host yet."),
        1 => Some("Only one key can open this host. Enrol a second device or security key so losing this one doesn't lock you out."),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct FakeSigner;

    impl HardwareSigner for FakeSigner {
        fn info(&self) -> HardwareKeyInfo {
            let mut p = vec![0x04];
            p.extend([7u8; 64]);
            HardwareKeyInfo { key_id: "k".into(), protection: Protection::StrongBox, public_sec1: p, user_presence_required: true }
        }
        fn sign_der(&self, _data: &[u8]) -> Result<Vec<u8>, SignerError> {
            Ok(vec![0x30, 0x06, 0x02, 0x01, 0x05, 0x02, 0x01, 0x09])
        }
    }

    #[test]
    fn signs_through_trait() {
        let s = FakeSigner;
        assert_eq!(sshkey::key_type(&public_blob(&s).unwrap()).unwrap(), sshkey::ECDSA_P256);
        let sig = ssh_sign(&s, b"session").unwrap();
        assert!(sig.ends_with(&[0, 0, 0, 1, 5, 0, 0, 0, 1, 9]));
    }

    #[test]
    fn policy_and_recovery() {
        assert!(check_policy(true, Protection::StrongBox).is_ok());
        assert!(check_policy(false, Protection::Software).is_ok());
        assert_eq!(
            check_policy(true, Protection::Software),
            Err(PolicyViolation::HardwareRequired { offered: Protection::Software })
        );
        assert!(recovery_warning(1).is_some());
        assert!(recovery_warning(2).is_none());
        assert!(Protection::SecureEnclave > Protection::Software);
    }
}
