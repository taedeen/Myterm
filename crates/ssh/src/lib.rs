//! MyTerm SSH transport on top of `russh`.
//!
//! Responsibilities:
//! - connect directly or through a jump-host chain (ProxyJump)
//! - verify host keys against MyTerm's known hosts (hard stop on change)
//! - authenticate with a password, a software private key, or a hardware
//!   key whose private part never leaves the device (spec 3.4.1)
//! - run an interactive shell with a PTY and stream output to the app
//!
//! NOTE: written against russh 0.54. The `russh` API has changed between
//! minor versions; CI builds this crate and is the source of truth.

use std::sync::Arc;

use async_trait::async_trait;
use myterm_core::inject::SecretBuf;
use myterm_core::knownhosts::{self, KnownHost, Verdict};
use myterm_core::session::Channel as CoreChannel;
use myterm_core::signer::{self, Protection};
use myterm_core::sshkey;
use russh::client::{self, Handle};
use russh::keys::ssh_key::{self, HashAlg};
use russh::keys::PrivateKeyWithHashAlg;
use russh::{ChannelMsg, CryptoVec};
use tokio::sync::{mpsc, Mutex};

#[derive(Debug, thiserror::Error)]
pub enum SshError {
    #[error("ssh: {0}")]
    Russh(#[from] russh::Error),
    #[error("host key for {host} was rejected: {reason}")]
    HostKeyRejected { host: String, reason: String },
    #[error("authentication failed for {0}")]
    AuthFailed(String),
    #[error("this host only accepts hardware keys; {0} key refused")]
    HardwareRequired(&'static str),
    #[error("hardware signer: {0}")]
    Signer(String),
    #[error("private key: {0}")]
    Key(String),
    #[error("no target to connect to")]
    NoTarget,
}

/// A hardware-backed P-256 key implemented in native code (Android Keystore,
/// Secure Enclave, TPM). `sign_der` may show a biometric prompt.
#[async_trait]
pub trait AsyncDerSigner: Send + Sync {
    fn public_sec1(&self) -> Vec<u8>;
    fn protection(&self) -> Protection;
    async fn sign_der(&self, data: Vec<u8>) -> Result<Vec<u8>, String>;
}

/// Asks the user to trust an unknown or new-type host key.
/// Never called for a *changed* key: that is always refused.
#[async_trait]
pub trait HostKeyPrompt: Send + Sync {
    async fn confirm(&self, host: &str, port: u16, verdict: &Verdict) -> bool;
}

pub enum Auth {
    Password(SecretBuf),
    /// OpenSSH private key text, optional passphrase.
    PrivateKey { openssh: SecretBuf, passphrase: Option<SecretBuf> },
    Hardware(Arc<dyn AsyncDerSigner>),
}

impl Auth {
    fn protection(&self) -> Protection {
        match self {
            Auth::Hardware(s) => s.protection(),
            _ => Protection::Software,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Auth::Password(_) => "password",
            Auth::PrivateKey { .. } => "software",
            Auth::Hardware(_) => "hardware",
        }
    }
}

pub struct Target {
    pub host: String,
    pub port: u16,
    pub user: String,
    pub auth: Auth,
    pub hardware_keys_only: bool,
}

pub struct ConnectOptions {
    /// Jump hosts in order, then the final host last.
    pub chain: Vec<Target>,
    pub known_hosts: Vec<KnownHost>,
    pub prompt: Arc<dyn HostKeyPrompt>,
    pub device_id: String,
    pub now_unix: u64,
    pub cols: u32,
    pub rows: u32,
    pub term: String,
}

#[derive(Debug)]
pub enum Event {
    Output(Vec<u8>),
    Exit(Option<u32>),
    Closed,
}

enum Command {
    Data(Vec<u8>),
    Resize(u32, u32),
    Close,
}

/// Handle used by the app to type into, resize and close a shell.
/// Implements [`myterm_core::session::Channel`], so it plugs into
/// [`myterm_core::session::Session`] directly.
#[derive(Clone)]
pub struct ShellHandle {
    tx: mpsc::UnboundedSender<Command>,
}

impl ShellHandle {
    pub fn resize(&self, cols: u32, rows: u32) {
        let _ = self.tx.send(Command::Resize(cols, rows));
    }

    pub fn close(&self) {
        let _ = self.tx.send(Command::Close);
    }
}

impl CoreChannel for ShellHandle {
    fn write(&mut self, data: &[u8]) -> std::io::Result<()> {
        self.tx
            .send(Command::Data(data.to_vec()))
            .map_err(|_| std::io::Error::new(std::io::ErrorKind::BrokenPipe, "session closed"))
    }
}

struct Verifier {
    host: String,
    port: u16,
    known: Arc<Mutex<Vec<KnownHost>>>,
    newly_approved: Arc<Mutex<Vec<KnownHost>>>,
    prompt: Arc<dyn HostKeyPrompt>,
    rejection: Arc<Mutex<Option<String>>>,
    device_id: String,
    now: u64,
}

impl client::Handler for Verifier {
    type Error = russh::Error;

    async fn check_server_key(&mut self, server_public_key: &ssh_key::PublicKey) -> Result<bool, Self::Error> {
        let blob = match server_public_key.to_bytes() {
            Ok(b) => b,
            Err(_) => return Ok(false),
        };
        let verdict = knownhosts::check(&self.known.lock().await, &self.host, self.port, &blob);
        let accept = match &verdict {
            Verdict::Trusted => true,
            Verdict::Changed { expected, presented } => {
                *self.rejection.lock().await =
                    Some(format!("HOST KEY CHANGED (expected {expected}, got {presented}). Possible interception."));
                false
            }
            Verdict::Unknown { .. } | Verdict::NewKeyType { .. } => {
                let ok = self.prompt.confirm(&self.host, self.port, &verdict).await;
                if ok {
                    let entry = KnownHost {
                        pattern: knownhosts::pattern(&self.host, self.port),
                        key_type: sshkey::key_type(&blob).unwrap_or_default(),
                        key_blob: blob,
                        approved_at: self.now,
                        approved_by_device: self.device_id.clone(),
                    };
                    self.known.lock().await.push(entry.clone());
                    self.newly_approved.lock().await.push(entry);
                } else {
                    *self.rejection.lock().await = Some("not trusted by user".into());
                }
                ok
            }
        };
        Ok(accept)
    }
}

/// Bridges a hardware signer into russh's public-key auth.
struct HardwareAuthSigner(Arc<dyn AsyncDerSigner>);

impl russh::Signer for HardwareAuthSigner {
    type Error = SshError;

    async fn auth_publickey_sign(
        &mut self,
        _key: &ssh_key::PublicKey,
        _hash_alg: Option<HashAlg>,
        mut to_sign: CryptoVec,
    ) -> Result<CryptoVec, Self::Error> {
        let der = self.0.sign_der(to_sign.to_vec()).await.map_err(SshError::Signer)?;
        let sig = sshkey::der_to_ssh_ecdsa_signature(&der).map_err(|e| SshError::Signer(e.to_string()))?;
        // russh expects the signed data with the SSH signature string appended,
        // matching its agent-client signer.
        let mut tail = Vec::with_capacity(sig.len() + 4);
        sshkey::put_string(&mut tail, &sig);
        to_sign.extend(&tail);
        Ok(to_sign)
    }
}

impl From<russh::SendError> for SshError {
    fn from(_: russh::SendError) -> Self {
        SshError::Russh(russh::Error::SendError)
    }
}

async fn authenticate(handle: &mut Handle<Verifier>, t: &Target) -> Result<(), SshError> {
    if signer::check_policy(t.hardware_keys_only, t.auth.protection()).is_err() {
        return Err(SshError::HardwareRequired(t.auth.kind()));
    }
    let ok = match &t.auth {
        Auth::Password(pw) => {
            let pw = String::from_utf8_lossy(pw.expose()).into_owned();
            // Many servers (PAM setups, TrueNAS, macOS) only offer the password
            // through keyboard-interactive, so fall back to it.
            handle.authenticate_password(&t.user, pw.clone()).await?.success()
                || keyboard_interactive_password(handle, &t.user, &pw).await?
        }
        Auth::PrivateKey { openssh, passphrase } => {
            let text = String::from_utf8_lossy(openssh.expose()).into_owned();
            let pass = passphrase.as_ref().map(|p| String::from_utf8_lossy(p.expose()).into_owned());
            let key = russh::keys::decode_secret_key(&text, pass.as_deref()).map_err(|e| SshError::Key(e.to_string()))?;
            let hash = handle.best_supported_rsa_hash().await?.flatten();
            handle
                .authenticate_publickey(&t.user, PrivateKeyWithHashAlg::new(Arc::new(key), hash))
                .await?
                .success()
        }
        Auth::Hardware(s) => {
            let blob = sshkey::ecdsa_p256_blob(&s.public_sec1()).map_err(|e| SshError::Key(e.to_string()))?;
            let public = ssh_key::PublicKey::from_bytes(&blob).map_err(|e| SshError::Key(e.to_string()))?;
            let mut signer = HardwareAuthSigner(s.clone());
            handle.authenticate_publickey_with(&t.user, public, None, &mut signer).await?.success()
        }
    };
    if ok {
        Ok(())
    } else {
        Err(SshError::AuthFailed(format!("{}@{}", t.user, t.host)))
    }
}

/// Answers keyboard-interactive prompts with the password. Prompts that don't
/// look like a password prompt (e.g. an OTP) get an empty answer, so the
/// server fails rather than receiving the password in the wrong place.
async fn keyboard_interactive_password(handle: &mut Handle<Verifier>, user: &str, pw: &str) -> Result<bool, SshError> {
    use russh::client::KeyboardInteractiveAuthResponse as R;
    let mut resp = handle.authenticate_keyboard_interactive_start(user, None::<String>).await?;
    // Servers usually ask once, then send an empty round; allow a few.
    for _ in 0..4 {
        match resp {
            R::Success => return Ok(true),
            R::InfoRequest { prompts, .. } => {
                let single = prompts.len() == 1;
                let answers = prompts
                    .iter()
                    .map(|p| {
                        let lower = p.prompt.to_lowercase();
                        let is_pw = lower.contains("password") || (lower.contains("passcode") && !lower.contains("one-time"));
                        if is_pw || (single && !p.echo && !lower.contains("code")) {
                            pw.to_string()
                        } else {
                            String::new()
                        }
                    })
                    .collect();
                resp = handle.authenticate_keyboard_interactive_respond(answers).await?;
            }
            _ => return Ok(false),
        }
    }
    Ok(false)
}

/// Opens an interactive shell. Output arrives on `events`; the returned
/// handle sends input. Also returns host keys approved during connect, for
/// the caller to persist and sync.
pub async fn open_shell(
    opts: ConnectOptions,
    events: mpsc::UnboundedSender<Event>,
) -> Result<(ShellHandle, Vec<KnownHost>), SshError> {
    if opts.chain.is_empty() {
        return Err(SshError::NoTarget);
    }
    let config = Arc::new(client::Config {
        inactivity_timeout: None,
        keepalive_interval: Some(std::time::Duration::from_secs(30)),
        keepalive_max: 4,
        ..Default::default()
    });
    let known = Arc::new(Mutex::new(opts.known_hosts));
    let newly_approved = Arc::new(Mutex::new(Vec::new()));

    let mut handles: Vec<Handle<Verifier>> = Vec::new();
    for (i, t) in opts.chain.iter().enumerate() {
        let rejection = Arc::new(Mutex::new(None));
        let verifier = Verifier {
            host: t.host.clone(),
            port: t.port,
            known: known.clone(),
            newly_approved: newly_approved.clone(),
            prompt: opts.prompt.clone(),
            rejection: rejection.clone(),
            device_id: opts.device_id.clone(),
            now: opts.now_unix,
        };
        let connected = if i == 0 {
            client::connect(config.clone(), (t.host.as_str(), t.port), verifier).await
        } else {
            // Tunnel through the previous hop (ProxyJump).
            let prev = handles.last().expect("previous hop exists");
            let ch = prev.channel_open_direct_tcpip(t.host.clone(), t.port as u32, "127.0.0.1", 0).await?;
            client::connect_stream(config.clone(), ch.into_stream(), verifier).await
        };
        let mut handle = match connected {
            Ok(h) => h,
            Err(e) => {
                if let Some(reason) = rejection.lock().await.take() {
                    return Err(SshError::HostKeyRejected { host: t.host.clone(), reason });
                }
                return Err(e.into());
            }
        };
        authenticate(&mut handle, t).await?;
        handles.push(handle);
    }

    let last = handles.last().expect("at least one hop");
    let mut channel = last.channel_open_session().await?;
    channel.request_pty(true, &opts.term, opts.cols, opts.rows, 0, 0, &[]).await?;
    channel.request_shell(true).await?;

    let (tx, mut rx) = mpsc::unbounded_channel::<Command>();
    tokio::spawn(async move {
        // Keep every hop alive for the life of the shell.
        let _hops = handles;
        loop {
            tokio::select! {
                msg = channel.wait() => match msg {
                    Some(ChannelMsg::Data { data }) => { let _ = events.send(Event::Output(data.to_vec())); }
                    Some(ChannelMsg::ExtendedData { data, .. }) => { let _ = events.send(Event::Output(data.to_vec())); }
                    Some(ChannelMsg::ExitStatus { exit_status }) => { let _ = events.send(Event::Exit(Some(exit_status))); }
                    Some(ChannelMsg::Eof) | Some(ChannelMsg::Close) | None => break,
                    Some(_) => {}
                },
                cmd = rx.recv() => match cmd {
                    Some(Command::Data(d)) => { if channel.data(&d[..]).await.is_err() { break; } }
                    Some(Command::Resize(c, r)) => { let _ = channel.window_change(c, r, 0, 0).await; }
                    Some(Command::Close) | None => { let _ = channel.eof().await; break; }
                },
            }
        }
        let _ = events.send(Event::Closed);
    });

    let approved = newly_approved.lock().await.clone();
    Ok((ShellHandle { tx }, approved))
}
