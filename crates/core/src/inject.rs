//! Secure "Type password" (spec 3.5 C).
//!
//! The UI never receives a secret. It asks the core what to do, shows the
//! confirmation the core asks for, and then the core writes the secret
//! straight to the SSH channel from a [`SecretBuf`] that wipes itself.

use std::fmt;
use std::sync::atomic::{compiler_fence, Ordering};

use crate::prompt::PromptKind;
use crate::Id;

/// Bytes that are zeroed when dropped and never printed.
pub struct SecretBuf(Vec<u8>);

impl SecretBuf {
    pub fn new(bytes: Vec<u8>) -> Self {
        SecretBuf(bytes)
    }

    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    fn wipe(&mut self) {
        for b in self.0.iter_mut() {
            // Volatile so the compiler can't drop the "dead" store.
            unsafe { std::ptr::write_volatile(b, 0) };
        }
        compiler_fence(Ordering::SeqCst);
        self.0.clear();
    }
}

impl Drop for SecretBuf {
    fn drop(&mut self) {
        // Also wipe spare capacity left over from earlier growth.
        let cap = self.0.capacity();
        self.0.resize(cap, 0);
        self.wipe();
    }
}

impl fmt::Debug for SecretBuf {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SecretBuf(<{} bytes redacted>)", self.0.len())
    }
}

/// When to ask for biometrics / PIN before typing a credential.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ConfirmPolicy {
    #[default]
    Always,
    OncePerSession,
    Never,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CredentialMeta {
    pub id: Id,
    pub name: String,
    pub confirm: ConfirmPolicy,
    /// High-value credential: may be viewed (with auth) but never auto-typed.
    pub never_auto_type: bool,
    pub append_enter: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct SessionState {
    /// The user already confirmed this credential in this session.
    pub confirmed_this_session: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Refusal {
    NeverAutoType,
    /// One-time-code prompt: a password is the wrong secret.
    WrongPromptKind,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum InjectDecision {
    Refuse(Refusal),
    /// No password prompt detected: UI asks "No password prompt detected, type anyway?"
    AskOverride,
    /// UI must run biometric / PIN confirmation, then call again with `confirmed = true`.
    NeedConfirmation,
    Proceed,
}

/// Decides the next step for a "Type password" press.
///
/// `override_prompt`: the user already answered "type anyway".
/// `confirmed`: the user just passed biometric / PIN confirmation.
pub fn decide(
    cred: &CredentialMeta,
    prompt: PromptKind,
    session: SessionState,
    override_prompt: bool,
    confirmed: bool,
) -> InjectDecision {
    if cred.never_auto_type {
        return InjectDecision::Refuse(Refusal::NeverAutoType);
    }
    if prompt == PromptKind::OneTimeCode {
        return InjectDecision::Refuse(Refusal::WrongPromptKind);
    }
    if !prompt.accepts_password() && !override_prompt {
        return InjectDecision::AskOverride;
    }
    let needs_confirm = match cred.confirm {
        ConfirmPolicy::Always => true,
        ConfirmPolicy::OncePerSession => !session.confirmed_this_session,
        ConfirmPolicy::Never => false,
    };
    if needs_confirm && !confirmed {
        return InjectDecision::NeedConfirmation;
    }
    InjectDecision::Proceed
}

/// Builds the bytes to send: the secret plus an optional carriage return.
pub fn payload(secret: &SecretBuf, append_enter: bool) -> SecretBuf {
    let mut v = Vec::with_capacity(secret.len() + 1);
    v.extend_from_slice(secret.expose());
    if append_enter {
        v.push(b'\r');
    }
    SecretBuf::new(v)
}

/// What the audit log records for a typed credential: never the value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct InjectAudit {
    pub credential_id: Id,
    pub host_id: Id,
    pub at_unix: u64,
    pub prompt: PromptKind,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cred(confirm: ConfirmPolicy) -> CredentialMeta {
        CredentialMeta { id: "c1".into(), name: "sudo".into(), confirm, never_auto_type: false, append_enter: true }
    }

    #[test]
    fn full_flow_at_sudo_prompt() {
        let c = cred(ConfirmPolicy::Always);
        let s = SessionState::default();
        assert_eq!(decide(&c, PromptKind::Sudo, s, false, false), InjectDecision::NeedConfirmation);
        assert_eq!(decide(&c, PromptKind::Sudo, s, false, true), InjectDecision::Proceed);
    }

    #[test]
    fn asks_override_without_prompt() {
        let c = cred(ConfirmPolicy::Never);
        let s = SessionState::default();
        assert_eq!(decide(&c, PromptKind::None, s, false, false), InjectDecision::AskOverride);
        assert_eq!(decide(&c, PromptKind::None, s, true, false), InjectDecision::Proceed);
    }

    #[test]
    fn once_per_session() {
        let c = cred(ConfirmPolicy::OncePerSession);
        let fresh = SessionState::default();
        let done = SessionState { confirmed_this_session: true };
        assert_eq!(decide(&c, PromptKind::Password, fresh, false, false), InjectDecision::NeedConfirmation);
        assert_eq!(decide(&c, PromptKind::Password, done, false, false), InjectDecision::Proceed);
    }

    #[test]
    fn refusals() {
        let mut c = cred(ConfirmPolicy::Never);
        let s = SessionState::default();
        assert_eq!(
            decide(&c, PromptKind::OneTimeCode, s, true, true),
            InjectDecision::Refuse(Refusal::WrongPromptKind)
        );
        c.never_auto_type = true;
        assert_eq!(
            decide(&c, PromptKind::Sudo, s, true, true),
            InjectDecision::Refuse(Refusal::NeverAutoType)
        );
    }

    #[test]
    fn payload_and_redaction() {
        let s = SecretBuf::new(b"hunter2".to_vec());
        let p = payload(&s, true);
        assert_eq!(p.expose(), b"hunter2\r");
        assert_eq!(format!("{p:?}"), "SecretBuf(<8 bytes redacted>)");
        assert_eq!(payload(&s, false).expose(), b"hunter2");
    }
}
