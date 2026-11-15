//! Hidden-input prompt detection (spec 3.3 privacy rules, 3.5 C step 3).
//!
//! An SSH client can't read the remote TTY's echo flag directly, so MyTerm
//! looks at the last line the server printed. When that line is a password,
//! sudo or passphrase prompt, typed input is treated as hidden: it is never
//! recorded to history or offered to autocomplete, and "Type password" may
//! send a secret without asking for an override.

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PromptKind {
    /// `[sudo] password for alice:`
    Sudo,
    /// `Enter passphrase for key '/home/a/.ssh/id_ed25519':`
    Passphrase,
    /// `Password:`, `alice@host's password:`, `Password for root:`
    Password,
    /// One-time code prompts (`Verification code:`). Hidden, but never auto-filled with a password.
    OneTimeCode,
    None,
}

impl PromptKind {
    /// Input typed at this prompt must not be recorded or completed.
    pub fn is_hidden(self) -> bool {
        self != PromptKind::None
    }

    /// A stored password may be typed here.
    pub fn accepts_password(self) -> bool {
        matches!(self, PromptKind::Sudo | PromptKind::Password | PromptKind::Passphrase)
    }
}

/// Removes ANSI escape sequences (CSI and OSC) and carriage returns.
pub fn strip_ansi(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut chars = s.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\x1b' => match chars.peek() {
                Some('[') => {
                    chars.next();
                    // CSI: parameters then one final byte in @..~
                    for n in chars.by_ref() {
                        if ('@'..='~').contains(&n) {
                            break;
                        }
                    }
                }
                Some(']') => {
                    chars.next();
                    // OSC: until BEL or ESC \
                    while let Some(n) = chars.next() {
                        if n == '\x07' {
                            break;
                        }
                        if n == '\x1b' && chars.peek() == Some(&'\\') {
                            chars.next();
                            break;
                        }
                    }
                }
                _ => {
                    chars.next();
                }
            },
            '\r' => {}
            _ => out.push(c),
        }
    }
    out
}

/// Classifies the prompt on the last line of terminal output.
pub fn detect(tail: &str) -> PromptKind {
    let clean = strip_ansi(tail);
    let line = clean.rsplit('\n').next().unwrap_or("").trim();
    if line.is_empty() || !line.ends_with(':') {
        return PromptKind::None;
    }
    let lower = line.to_lowercase();
    // A shell prompt like "user@host:~$" never ends in ':' after trimming, but a
    // long line of command output might; keep prompts short to avoid false hits.
    if lower.chars().count() > 160 {
        return PromptKind::None;
    }
    if lower.starts_with("[sudo] password for ") {
        PromptKind::Sudo
    } else if lower.contains("passphrase") {
        PromptKind::Passphrase
    } else if lower.contains("verification code")
        || lower.contains("one-time")
        || lower.contains("otp")
        || lower.contains("authenticator code")
    {
        PromptKind::OneTimeCode
    } else if lower.ends_with("password:")
        || lower.contains("password for ")
        || lower.ends_with("'s password:")
        || lower == "pin:"
    {
        PromptKind::Password
    } else {
        PromptKind::None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn recognises_common_prompts() {
        assert_eq!(detect("[sudo] password for taedeen: "), PromptKind::Sudo);
        assert_eq!(detect("root@192.168.0.19's password:"), PromptKind::Password);
        assert_eq!(detect("Password: "), PromptKind::Password);
        assert_eq!(detect("Enter passphrase for key '/home/t/.ssh/id_ed25519': "), PromptKind::Passphrase);
        assert_eq!(detect("Verification code: "), PromptKind::OneTimeCode);
        assert_eq!(detect("Enter password for postgres:"), PromptKind::Password);
    }

    #[test]
    fn ignores_shell_prompts_and_output() {
        assert_eq!(detect("taedeen@dbb:~/docker_volumes/MyJam$ "), PromptKind::None);
        assert_eq!(detect("password: changed successfully"), PromptKind::None);
        assert_eq!(detect(""), PromptKind::None);
        assert_eq!(detect("Name:"), PromptKind::None);
    }

    #[test]
    fn uses_last_line_and_strips_colour() {
        let out = "Last login: Sat\r\n\x1b[1;32mwelcome\x1b[0m\r\n\x1b[0m[sudo] password for t: ";
        assert_eq!(detect(out), PromptKind::Sudo);
        let osc = "\x1b]0;title\x07Password:";
        assert_eq!(detect(osc), PromptKind::Password);
    }

    #[test]
    fn otp_is_hidden_but_not_password() {
        assert!(PromptKind::OneTimeCode.is_hidden());
        assert!(!PromptKind::OneTimeCode.accepts_password());
        assert!(PromptKind::Sudo.accepts_password());
        assert!(!PromptKind::None.is_hidden());
    }
}
