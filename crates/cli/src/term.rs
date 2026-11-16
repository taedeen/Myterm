//! `myterm ssh`: MyTerm's client in your terminal.
//!
//! The terminal is put in raw mode and relayed to the SSH channel. The
//! bottom line is MyTerm's: the remote side is told the terminal is one row
//! shorter, and a scroll region keeps its output above that line, which
//! shows the host, autocomplete suggestions and messages.
//!
//! Keys (Ctrl+] then):  1-6 run a suggestion · p type the stored password
//! · . disconnect · ] send Ctrl+] · ? help

use std::io::Write;
use std::sync::Arc;
use std::time::Duration;

use anyhow::Result;
use crossterm::terminal;
use myterm_ffi::api::{AuthChoice, HostDto, InjectDto, MyTerm, SessionEventDto, SuggestionDto};
use myterm_ssh::HostKeyPrompt;
use myterm_core::knownhosts::Verdict;
use tokio::io::AsyncReadExt;
use tokio::signal::unix::{signal, SignalKind};
use tokio::sync::mpsc;

use crate::store::Store;

/// Host key questions, asked on the terminal before the session starts.
pub struct TtyPrompt {
    /// Non-interactive callers (`myterm run`) refuse unknown keys.
    pub interactive: bool,
}

#[async_trait::async_trait]
impl HostKeyPrompt for TtyPrompt {
    async fn confirm(&self, host: &str, port: u16, verdict: &Verdict) -> bool {
        let (what, fp) = match verdict {
            Verdict::Unknown { fingerprint } => ("Unknown host", fingerprint.clone()),
            Verdict::NewKeyType { fingerprint, .. } => ("New host key type", fingerprint.clone()),
            _ => return false,
        };
        if !self.interactive {
            eprintln!("{what} {host}:{port} ({fp}): trust it with `myterm ssh` first");
            return false;
        }
        let (host, what) = (host.to_string(), what.to_string());
        tokio::task::spawn_blocking(move || {
            eprintln!("\r\n{what}: {host}:{port}\r\nFingerprint: {fp}\r\nCheck it against the server before trusting it.");
            crate::prompt::yes("Trust this host key?", false).unwrap_or(false)
        })
        .await
        .unwrap_or(false)
    }
}

/// How to sign in: stored credential or certificate, else ask for the password.
pub fn auth_for(store: &Store, host: &HostDto, interactive: bool) -> Result<Option<AuthChoice>> {
    if host.use_certificate == Some(true) || store.engine.host_has_credential(host.id.clone()) {
        return Ok(Some(AuthChoice::Stored));
    }
    if !interactive {
        return Ok(None);
    }
    let pw = crate::prompt::secret(&format!("Password for {}@{}: ", if host.username.is_empty() { "root" } else { &host.username }, host.address))?;
    Ok(Some(AuthChoice::Password { password: pw.into_bytes() }))
}

/// Renews this device's certificate first when any hop signs in with one.
pub fn prepare_certificate(store: &Store, host: &HostDto) {
    let hosts = store.engine.list_hosts();
    let mut ids: Vec<String> = host.jump_chain.clone();
    ids.push(host.id.clone());
    let mut i = 0;
    let mut uses = false;
    while i < ids.len() {
        if let Some(h) = hosts.iter().find(|h| h.id == ids[i]) {
            uses |= h.use_certificate == Some(true);
            for j in &h.jump_chain {
                if !ids.contains(j) {
                    ids.push(j.clone());
                }
            }
        }
        i += 1;
    }
    if uses {
        if let Err(e) = crate::sync::renew_certificate(store, false, 2 * 3600) {
            eprintln!("Couldn't renew the SSH certificate: {e}");
        }
    }
}

struct RawGuard {
    rows: u16,
}

/// Keeps the last row for MyTerm's status line.
fn set_region(rows: u16) {
    let mut out = std::io::stdout();
    let _ = write!(out, "\x1b7\x1b[1;{}r\x1b8", rows.saturating_sub(1).max(1));
    let _ = out.flush();
}

impl RawGuard {
    fn enter(rows: u16) -> Result<RawGuard> {
        terminal::enable_raw_mode()?;
        // Scroll once so the line under the cursor isn't the status line.
        print!("\n\x1b[A");
        set_region(rows);
        Ok(RawGuard { rows })
    }
}

impl Drop for RawGuard {
    fn drop(&mut self) {
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b7\x1b[r\x1b[{};1H\x1b[2K\x1b8", self.rows);
        let _ = out.flush();
        let _ = terminal::disable_raw_mode();
    }
}

struct Status {
    host: String,
    message: Option<String>,
    suggestions: Vec<SuggestionDto>,
    shown: String,
}

impl Status {
    fn draw(&mut self, cols: u16, rows: u16, force: bool) {
        let mut text = format!(" myterm │ {} │ ", self.host);
        if let Some(m) = &self.message {
            text.push_str(m);
        } else if self.suggestions.is_empty() {
            text.push_str("Ctrl+] ? for keys");
        } else {
            for (i, s) in self.suggestions.iter().enumerate() {
                text.push_str(&format!("{} {}   ", i + 1, s.line));
            }
        }
        let line: String = text.chars().take(cols as usize).collect();
        if !force && line == self.shown {
            return;
        }
        self.shown = line.clone();
        let pad = (cols as usize).saturating_sub(line.chars().count());
        let mut out = std::io::stdout();
        let _ = write!(out, "\x1b7\x1b[{rows};1H\x1b[0;7m{line}{}\x1b[0m\x1b8", " ".repeat(pad));
        let _ = out.flush();
    }
}

/// Runs an interactive session until it ends. Returns the exit code.
pub async fn interactive(store: &Store, host: &HostDto, tmux_name: Option<String>) -> Result<i32> {
    let persistent = tmux_name.is_some();
    let engine: &MyTerm = &store.engine;
    let mut auth = match auth_for(store, host, true)? {
        Some(a) => a,
        None => unreachable!(),
    };
    prepare_certificate(store, host);
    let tmux = tmux_name.as_deref().map(myterm_ffi::api::tmux_command);

    // One stdin reader for the whole run (reconnects included).
    let (in_tx, mut in_rx) = mpsc::unbounded_channel::<Vec<u8>>();
    tokio::spawn(async move {
        let mut stdin = tokio::io::stdin();
        let mut buf = [0u8; 4096];
        loop {
            match stdin.read(&mut buf).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if in_tx.send(buf[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut winch = signal(SignalKind::window_change())?;

    let mut attempt = 0u32;
    let label = if host.label.is_empty() { host.address.clone() } else { host.label.clone() };
    loop {
        let (cols, rows) = terminal::size().unwrap_or((80, 24));
        let (tx, mut rx) = mpsc::unbounded_channel::<SessionEventDto>();
        let events: myterm_ffi::native::EventFn = Arc::new(move |e| {
            let _ = tx.send(e);
        });
        let sign = myterm_ffi::native::no_hardware_signer();
        engine
            .connect_native(
                host.id.clone(),
                cols as u32,
                rows.saturating_sub(1).max(1) as u32,
                clone_auth(&auth),
                tmux.clone(),
                events,
                Arc::new(TtyPrompt { interactive: true }),
                sign,
            )
            .await?;

        // Wait for Started or an error (host key prompts happen here).
        let id = loop {
            match rx.recv().await {
                Some(SessionEventDto::Started(id)) => break Some(id),
                Some(SessionEventDto::Error(e)) => {
                    eprintln!("{label}: {e}");
                    break None;
                }
                Some(_) => {}
                None => break None,
            }
        };
        let Some(id) = id else {
            if persistent && attempt > 0 && attempt < 30 {
                attempt += 1;
                let wait = [1u64, 2, 5, 10, 20, 30][(attempt as usize - 1).min(5)];
                eprintln!("Reconnecting in {wait}s (attempt {attempt})…");
                tokio::time::sleep(Duration::from_secs(wait)).await;
                continue;
            }
            return Ok(255);
        };
        let mut guard = RawGuard::enter(rows)?;
        let mut status = Status { host: label.clone(), message: None, suggestions: Vec::new(), shown: String::new() };
        let (mut cols, mut rows) = (cols, rows);
        status.draw(cols, rows, true);
        let mut prefix = false;
        let mut exited: Option<u32> = None;
        let mut user_closed = false;
        let mut out = std::io::stdout();

        let dropped = loop {
            tokio::select! {
                ev = rx.recv() => match ev {
                    Some(SessionEventDto::Output(bytes)) => {
                        let _ = out.write_all(&bytes);
                        let _ = out.flush();
                        // Programs that reset the scroll region (reset, some
                        // full-screen apps) would scroll over the status line.
                        if contains(&bytes, b"\x1b[r") || contains(&bytes, b"\x1b[?1049") || contains(&bytes, b"\x1bc") {
                            set_region(rows);
                            status.draw(cols, rows, true);
                        }
                    }
                    Some(SessionEventDto::Exit(code)) => exited = Some(code.unwrap_or(0)),
                    Some(SessionEventDto::Error(e)) => { status.message = Some(e); status.draw(cols, rows, true); }
                    Some(SessionEventDto::Started(_)) => {}
                    Some(SessionEventDto::Closed) | None => break exited.is_none() && !user_closed,
                },
                Some(bytes) = in_rx.recv() => {
                    let mut plain = Vec::with_capacity(bytes.len());
                    for &b in &bytes {
                        if prefix {
                            prefix = false;
                            status.message = None;
                            match b {
                                b'1'..=b'6' => {
                                    let n = (b - b'1') as usize;
                                    if let Some(line) = status.suggestions.get(n).map(|s| s.line.clone()) {
                                        let _ = engine.send_input(id, std::mem::take(&mut plain));
                                        if engine.accept_suggestion(id, line.clone()).is_ok() && !line.contains("{{") {
                                            let _ = engine.send_input(id, vec![b'\r']);
                                        }
                                    }
                                }
                                b'p' | b'P' => {
                                    let _ = engine.send_input(id, std::mem::take(&mut plain));
                                    status.message = Some(type_password(engine, id, b == b'P'));
                                }
                                b'.' => { user_closed = true; engine.disconnect(id); }
                                0x1d => plain.push(0x1d),
                                _ => status.message = Some("Ctrl+] then: 1-6 run a suggestion · p type password (P: anyway) · . disconnect · ] send Ctrl+]".into()),
                            }
                        } else if b == 0x1d {
                            prefix = true;
                            status.message = Some("Ctrl+] … (1-6 · p · . · ?)".into());
                        } else {
                            plain.push(b);
                        }
                    }
                    if !plain.is_empty() {
                        let _ = engine.send_input(id, plain);
                    }
                    status.suggestions = engine.suggest(id, 6);
                    status.draw(cols, rows, false);
                },
                _ = winch.recv() => {
                    if let Ok((c, r)) = terminal::size() {
                        (cols, rows) = (c, r);
                        guard.rows = rows;
                        set_region(rows);
                        engine.resize(id, cols as u32, rows.saturating_sub(1).max(1) as u32);
                        status.draw(cols, rows, true);
                    }
                }
            }
        };
        drop(guard);
        let _ = store.save();
        if dropped && persistent {
            attempt = 1;
            eprintln!("\r\nConnection to {label} lost; reconnecting…");
            auth = clone_auth(&auth);
            tokio::time::sleep(Duration::from_secs(1)).await;
            continue;
        }
        if dropped {
            eprintln!("\r\nConnection to {label} closed.");
        }
        return Ok(exited.map(|c| c as i32).unwrap_or(if dropped { 255 } else { 0 }));
    }
}

fn type_password(engine: &MyTerm, id: u32, anyway: bool) -> String {
    match engine.type_password(id, None, anyway, true) {
        Ok(InjectDto::Proceeded) => "Password typed".into(),
        Ok(InjectDto::AskOverride) => "No password prompt seen: Ctrl+] P types it anyway".into(),
        Ok(InjectDto::NeedConfirmation) => "Needs confirmation".into(),
        Ok(InjectDto::RefusedNeverAutoType) => "This credential is set never to be typed".into(),
        Ok(InjectDto::RefusedOneTimeCode) => "That prompt wants a one-time code, not the password".into(),
        Err(e) => format!("{e}"),
    }
}

fn clone_auth(a: &AuthChoice) -> AuthChoice {
    match a {
        AuthChoice::Stored => AuthChoice::Stored,
        AuthChoice::Password { password } => AuthChoice::Password { password: password.clone() },
        AuthChoice::Hardware { key_id, public_sec1, protection } => AuthChoice::Hardware {
            key_id: key_id.clone(),
            public_sec1: public_sec1.clone(),
            protection: protection.clone(),
        },
    }
}

fn contains(hay: &[u8], needle: &[u8]) -> bool {
    hay.windows(needle.len()).any(|w| w == needle)
}
