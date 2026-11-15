//! Transport-independent session logic.
//!
//! Sits between the terminal view and the SSH channel. It watches output to
//! know when the server is asking for hidden input, tracks what the user
//! types so finished commands can go to history, and performs the secure
//! "Type password" flow. The SSH crate supplies a [`Channel`]; tests use a
//! fake one.

use std::io;

use crate::cdtrack::CdTracker;
use crate::inject::{self, CredentialMeta, InjectAudit, InjectDecision, SecretBuf, SessionState};
use crate::prompt::{self, PromptKind};

/// Where bytes typed by the user (or by the app) are written.
pub trait Channel {
    fn write(&mut self, data: &[u8]) -> io::Result<()>;
}

/// How much recent output to keep for prompt detection.
const TAIL_CHARS: usize = 512;

/// A command the user finished typing (Enter pressed).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompletedCommand {
    pub line: String,
    /// Prompt that was on screen when typing started.
    pub prompt: PromptKind,
    /// False when the line was edited with cursor keys or history recall,
    /// so the tracked text may not match what the shell ran.
    pub reliable: bool,
}

pub struct Session<C: Channel> {
    pub host_id: String,
    pub incognito: bool,
    channel: C,
    tail: String,
    /// Raw bytes of the line being typed (UTF-8).
    line: Vec<u8>,
    line_prompt: PromptKind,
    line_reliable: bool,
    inject_state: SessionState,
    /// Bytes of an escape sequence typed by the user (arrow keys etc.).
    in_escape: bool,
    cd: CdTracker,
    /// Combined `cd` commands waiting to be added to history.
    derived: Vec<String>,
}

impl<C: Channel> Session<C> {
    pub fn new(host_id: impl Into<String>, channel: C) -> Self {
        Session {
            host_id: host_id.into(),
            incognito: false,
            channel,
            tail: String::new(),
            line: Vec::new(),
            line_prompt: PromptKind::None,
            line_reliable: true,
            inject_state: SessionState::default(),
            in_escape: false,
            cd: CdTracker::new(),
            derived: Vec::new(),
        }
    }

    pub fn channel_mut(&mut self) -> &mut C {
        &mut self.channel
    }

    /// Feed bytes received from the server (also rendered by the terminal view).
    pub fn on_output(&mut self, data: &[u8]) {
        self.tail.push_str(&String::from_utf8_lossy(data));
        let excess = self.tail.chars().count().saturating_sub(TAIL_CHARS);
        if excess > 0 {
            let cut = self.tail.char_indices().nth(excess).map(|(i, _)| i).unwrap_or(0);
            self.tail.drain(..cut);
        }
        if let Some(d) = self.cd.on_output(&self.tail, data) {
            self.derived.push(d);
        }
    }

    /// Combined `cd` commands to add to history (see [`crate::cdtrack`]).
    pub fn take_derived(&mut self) -> Vec<String> {
        std::mem::take(&mut self.derived)
    }

    /// Current directory, when known.
    pub fn cwd(&self) -> Option<&str> {
        self.cd.cwd()
    }

    /// The prompt currently on screen.
    pub fn current_prompt(&self) -> PromptKind {
        prompt::detect(&self.tail)
    }

    /// The line typed so far, for autocomplete. Empty while input is hidden.
    pub fn current_line(&self) -> String {
        if self.line_prompt.is_hidden() || self.current_prompt().is_hidden() {
            String::new()
        } else {
            String::from_utf8_lossy(&self.line).into_owned()
        }
    }

    /// Forward user keystrokes to the server and track the input line.
    /// Returns a command when Enter completes one.
    pub fn on_input(&mut self, data: &[u8]) -> io::Result<Option<CompletedCommand>> {
        self.channel.write(data)?;
        let mut done = None;
        for &b in data {
            if self.line.is_empty() && !self.in_escape {
                self.line_prompt = self.current_prompt();
            }
            if self.in_escape {
                // CSI/SS3 sequences end with a byte in '@'..='~' after '[' or 'O'.
                if (b'@'..=b'~').contains(&b) && b != b'[' && b != b'O' {
                    self.in_escape = false;
                }
                continue;
            }
            match b {
                b'\r' | b'\n' => {
                    let line = String::from_utf8_lossy(&std::mem::take(&mut self.line)).into_owned();
                    if let Some(d) = self.cd.on_enter(&line, self.line_reliable, self.line_prompt.is_hidden()) {
                        self.derived.push(d);
                    }
                    done = Some(CompletedCommand { line, prompt: self.line_prompt, reliable: self.line_reliable });
                    self.line_reliable = true;
                    self.line_prompt = PromptKind::None;
                }
                0x7f | 0x08 => {
                    // Remove one whole UTF-8 character.
                    while let Some(last) = self.line.pop() {
                        if last & 0xc0 != 0x80 {
                            break;
                        }
                    }
                }
                0x03 | 0x15 => {
                    // Ctrl+C / Ctrl+U clear the line.
                    self.line.clear();
                    self.line_reliable = true;
                }
                0x1b => {
                    self.in_escape = true;
                    self.line_reliable = false;
                }
                b'\t' | 0x12 => {
                    // Shell-side completion or reverse search: we can't see the result.
                    self.line_reliable = false;
                }
                0x20..=0x7e | 0x80..=0xff => self.line.push(b),
                _ => {}
            }
        }
        Ok(done)
    }

    /// Accepts an autocomplete suggestion: types the remaining characters.
    pub fn accept_suggestion(&mut self, full_line: &str) -> io::Result<()> {
        if let Some(rest) = full_line.as_bytes().strip_prefix(self.line.as_slice()) {
            let rest = rest.to_vec();
            self.on_input(&rest).map(|_| ())
        } else {
            // Clear the line (Ctrl+U) then type the suggestion.
            self.on_input(&[0x15])?;
            self.on_input(full_line.as_bytes()).map(|_| ())
        }
    }

    /// "Type password" (spec 3.5 C). `fetch_secret` is only called on `Proceed`,
    /// so the vault is not touched unless the secret will be sent.
    pub fn type_password(
        &mut self,
        cred: &CredentialMeta,
        override_prompt: bool,
        confirmed: bool,
        now: u64,
        fetch_secret: impl FnOnce() -> io::Result<SecretBuf>,
    ) -> io::Result<(InjectDecision, Option<InjectAudit>)> {
        let prompt = self.current_prompt();
        let decision = inject::decide(cred, prompt, self.inject_state, override_prompt, confirmed);
        if decision != InjectDecision::Proceed {
            return Ok((decision, None));
        }
        let secret = fetch_secret()?;
        let payload = inject::payload(&secret, cred.append_enter);
        // Written straight to the channel: never through on_input, so it is
        // never tracked as a command line.
        self.channel.write(payload.expose())?;
        drop(payload);
        drop(secret);
        if confirmed {
            self.inject_state.confirmed_this_session = true;
        }
        if cred.append_enter {
            self.line.clear();
            self.line_prompt = PromptKind::None;
            self.line_reliable = true;
        }
        Ok((
            decision,
            Some(InjectAudit { credential_id: cred.id.clone(), host_id: self.host_id.clone(), at_unix: now, prompt }),
        ))
    }
}

#[cfg(test)]
mod cd_tests {
    use super::*;

    #[derive(Default)]
    struct Sink;
    impl Channel for Sink {
        fn write(&mut self, _: &[u8]) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn session_derives_combined_cd_from_prompts() {
        let mut s = Session::new("dbb", Sink);
        s.on_output(b"t@dbb:~$ ");
        for (typed, out) in [("cd docker_volumes\r", "cd docker_volumes\r\nt@dbb:~/docker_volumes$ "), ("cd MyJam\r", "cd MyJam\r\nt@dbb:~/docker_volumes/MyJam$ ")] {
            // Echo arrives as the user types, then Enter, then the new prompt.
            s.on_input(typed.as_bytes()).unwrap();
            s.on_output(out.as_bytes());
        }
        assert_eq!(s.take_derived(), vec!["cd docker_volumes/MyJam".to_string()]);
        assert!(s.take_derived().is_empty());
        assert_eq!(s.cwd(), Some("~/docker_volumes/MyJam"));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::{HistoryStore, RecordContext};
    use crate::inject::ConfirmPolicy;

    #[derive(Default)]
    struct Fake(Vec<u8>);
    impl Channel for Fake {
        fn write(&mut self, data: &[u8]) -> io::Result<()> {
            self.0.extend_from_slice(data);
            Ok(())
        }
    }

    fn cred() -> CredentialMeta {
        CredentialMeta { id: "c".into(), name: "sudo".into(), confirm: ConfirmPolicy::OncePerSession, never_auto_type: false, append_enter: true }
    }

    #[test]
    fn tracks_commands_and_feeds_history() {
        let mut s = Session::new("dbb", Fake::default());
        let mut h = HistoryStore::new("phone");
        s.on_output(b"taedeen@dbb:~$ ");
        assert!(s.on_input(b"docker pz").unwrap().is_none());
        s.on_input(&[0x7f]).unwrap();
        assert_eq!(s.current_line(), "docker p");
        let cmd = s.on_input(b"s\r").unwrap().unwrap();
        assert_eq!(cmd, CompletedCommand { line: "docker ps".into(), prompt: PromptKind::None, reliable: true });
        h.record("dbb", &cmd.line, 1, RecordContext { incognito: s.incognito, prompt: Some(cmd.prompt) }).unwrap();
        assert_eq!(s.channel_mut().0, b"docker pz\x7fs\r");
    }

    #[test]
    fn hidden_input_is_not_tracked() {
        let mut s = Session::new("dbb", Fake::default());
        s.on_output(b"[sudo] password for taedeen: ");
        s.on_input(b"hunter").unwrap();
        assert_eq!(s.current_line(), "", "nothing exposed to autocomplete");
        let cmd = s.on_input(b"2\r").unwrap().unwrap();
        assert_eq!(cmd.prompt, PromptKind::Sudo);
        let mut h = HistoryStore::new("phone");
        assert!(h.record("dbb", &cmd.line, 1, RecordContext { incognito: false, prompt: Some(cmd.prompt) }).is_err());
    }

    #[test]
    fn arrow_keys_make_line_unreliable() {
        let mut s = Session::new("dbb", Fake::default());
        s.on_output(b"$ ");
        s.on_input(b"\x1b[A").unwrap();
        let cmd = s.on_input(b"\r").unwrap().unwrap();
        assert!(!cmd.reliable);
        let cmd = s.on_input(b"ls\r").unwrap().unwrap();
        assert!(cmd.reliable, "reset after Enter");
        assert_eq!(cmd.line, "ls");
    }

    #[test]
    fn accept_suggestion_types_remainder() {
        let mut s = Session::new("dbb", Fake::default());
        s.on_output(b"$ ");
        s.on_input("caf\u{e9} ".as_bytes()).unwrap();
        s.on_input(&[0x7f, 0x7f]).unwrap();
        assert_eq!(s.current_line(), "caf", "backspace removes a whole character");
        s.on_input(&[0x15]).unwrap();
        s.channel_mut().0.clear();
        s.on_input(b"docker l").unwrap();
        s.accept_suggestion("docker logs -f myjam").unwrap();
        assert_eq!(s.current_line(), "docker logs -f myjam");
        assert_eq!(s.channel_mut().0, b"docker logs -f myjam");
    }

    #[test]
    fn type_password_flow() {
        let mut s = Session::new("dbb", Fake::default());
        s.on_output(b"sudo apt update\r\n[sudo] password for taedeen: ");
        let mut fetched = 0;
        let (d, a) = s.type_password(&cred(), false, false, 9, || { fetched += 1; Ok(SecretBuf::new(b"pw".to_vec())) }).unwrap();
        assert_eq!(d, InjectDecision::NeedConfirmation);
        assert!(a.is_none());
        assert_eq!(fetched, 0, "vault not touched before confirmation");

        let (d, a) = s.type_password(&cred(), false, true, 9, || Ok(SecretBuf::new(b"pw".to_vec()))).unwrap();
        assert_eq!(d, InjectDecision::Proceed);
        let a = a.unwrap();
        assert_eq!((a.credential_id.as_str(), a.host_id.as_str(), a.prompt), ("c", "dbb", PromptKind::Sudo));
        assert_eq!(s.channel_mut().0, b"pw\r");

        // Once per session: second use needs no confirmation.
        s.on_output(b"\r\n[sudo] password for taedeen: ");
        let (d, _) = s.type_password(&cred(), false, false, 10, || Ok(SecretBuf::new(b"pw".to_vec()))).unwrap();
        assert_eq!(d, InjectDecision::Proceed);
    }

    #[test]
    fn tail_is_bounded() {
        let mut s = Session::new("h", Fake::default());
        s.on_output(&vec![b'x'; 5000]);
        s.on_output("\r\né Password:".as_bytes());
        assert!(s.tail.chars().count() <= TAIL_CHARS);
        assert_eq!(s.current_prompt(), PromptKind::Password);
    }
}
