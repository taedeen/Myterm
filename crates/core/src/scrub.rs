//! Secret-pattern filter for command history (spec 3.3, 8.2).
//!
//! Runs on the device before a command enters history or the sync log.
//! It errs towards dropping: losing one suggestion is cheap, syncing a
//! secret to every device is not. No regex dependency; simple token rules.

/// Why a command was dropped (for the debug view, never with the value).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SecretReason {
    SecretFlag,
    SecretAssignment,
    InlineDbPassword,
    CredentialUrl,
    KnownTokenFormat,
    PasswordOnStdin,
    Sshpass,
}

const SECRET_FLAGS: &[&str] = &[
    "--password", "--passwd", "--pass", "--token", "--secret", "--api-key", "--apikey",
    "--access-key", "--secret-key", "--private-key", "--client-secret", "--auth",
];

const SECRET_NAME_PARTS: &[&str] = &["PASSWORD", "PASSWD", "SECRET", "TOKEN", "API_KEY", "APIKEY", "PRIVATE_KEY", "ACCESS_KEY", "CREDENTIAL"];

const DB_CLIENTS: &[&str] = &["mysql", "mysqldump", "mariadb", "mysqladmin"];

const TOKEN_PREFIXES: &[&str] = &[
    "ghp_", "gho_", "ghu_", "ghs_", "ghr_", "github_pat_", // GitHub
    "glpat-",                                               // GitLab
    "xoxb-", "xoxp-", "xoxa-", "xapp-",                    // Slack
    "sk_live_", "rk_live_",                                 // Stripe
    "AIza",                                                 // Google API key
];

fn is_aws_key_id(tok: &str) -> bool {
    (tok.starts_with("AKIA") || tok.starts_with("ASIA"))
        && tok.len() == 20
        && tok.chars().all(|c| c.is_ascii_uppercase() || c.is_ascii_digit())
}

fn is_jwt(tok: &str) -> bool {
    let parts: Vec<&str> = tok.split('.').collect();
    parts.len() == 3
        && tok.starts_with("eyJ")
        && parts.iter().all(|p| p.len() >= 8 && p.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_'))
}

fn has_credential_url(tok: &str) -> bool {
    // scheme://user:pass@host
    if let Some(idx) = tok.find("://") {
        let rest = &tok[idx + 3..];
        if let Some(at) = rest.find('@') {
            let userinfo = &rest[..at];
            return userinfo.contains(':') && !userinfo.ends_with(':');
        }
    }
    false
}

fn strip_quotes(s: &str) -> &str {
    s.trim_matches(|c| c == '"' || c == '\'')
}

/// Returns `Some(reason)` if the command looks like it contains a secret.
pub fn secret_reason(cmd: &str) -> Option<SecretReason> {
    let tokens: Vec<&str> = cmd.split_whitespace().collect();
    if tokens.is_empty() {
        return None;
    }
    let lower = cmd.to_lowercase();

    if (lower.contains("sudo -s") && tokens.contains(&"-S")) || lower.contains("sudo --stdin") {
        return Some(SecretReason::PasswordOnStdin);
    }
    if lower.contains("--password-stdin") && lower.contains("echo ") {
        return Some(SecretReason::PasswordOnStdin);
    }

    for (i, raw) in tokens.iter().enumerate() {
        let tok = strip_quotes(raw);
        let tl = tok.to_lowercase();

        // --password=value or --password value
        for flag in SECRET_FLAGS {
            if tl == *flag && i + 1 < tokens.len() {
                return Some(SecretReason::SecretFlag);
            }
            if tl.starts_with(flag) && tl[flag.len()..].starts_with('=') && tl.len() > flag.len() + 1 {
                return Some(SecretReason::SecretFlag);
            }
        }

        // NAME=value where NAME looks secret (export, inline env, docker -e).
        if let Some(eq) = tok.find('=') {
            let name = tok[..eq].trim_start_matches('-').to_ascii_uppercase();
            let value = &tok[eq + 1..];
            let name_is_ident = !name.is_empty() && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if name_is_ident && !value.is_empty() && SECRET_NAME_PARTS.iter().any(|p| name.contains(p)) {
                return Some(SecretReason::SecretAssignment);
            }
        }

        if has_credential_url(tok) {
            return Some(SecretReason::CredentialUrl);
        }
        if is_aws_key_id(tok) || is_jwt(tok) || TOKEN_PREFIXES.iter().any(|p| tok.starts_with(p) && tok.len() > p.len() + 10) {
            return Some(SecretReason::KnownTokenFormat);
        }
    }

    // mysql -pSecret (password glued to -p). Plain "-p" prompts, which is fine.
    let base = |t: &str| t.rsplit('/').next().unwrap_or(t).to_string();
    if tokens.iter().any(|t| DB_CLIENTS.contains(&base(t).as_str()))
        && tokens.iter().any(|t| t.starts_with("-p") && t.len() > 2 && !t.starts_with("--"))
    {
        return Some(SecretReason::InlineDbPassword);
    }

    if base(tokens[0]) == "sshpass" && tokens.iter().any(|t| t.starts_with("-p")) {
        return Some(SecretReason::Sshpass);
    }
    None
}

pub fn is_secret(cmd: &str) -> bool {
    secret_reason(cmd).is_some()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn drops_secrets() {
        let cases = [
            ("mysql -u root -pS3cret db", SecretReason::InlineDbPassword),
            ("export AWS_SECRET_ACCESS_KEY=abc123", SecretReason::SecretAssignment),
            ("DB_PASSWORD=hunter2 ./run.sh", SecretReason::SecretAssignment),
            ("docker run -e API_TOKEN=xyz image", SecretReason::SecretAssignment),
            ("restic --password=letmein backup /", SecretReason::SecretFlag),
            ("vault login --token abcd", SecretReason::SecretFlag),
            ("git clone https://bob:pw@github.com/x/y", SecretReason::CredentialUrl),
            ("curl -H 'Authorization: Bearer eyJhbGciOiJI.eyJzdWIiOiIx.SflKxwRJSMeKKF2QT4'", SecretReason::KnownTokenFormat),
            ("echo AKIAIOSFODNN7EXAMPLE", SecretReason::KnownTokenFormat),
            ("gh auth login --with-token ghp_abcdefghijklmnop1234", SecretReason::KnownTokenFormat),
            ("echo pw | sudo -S apt update", SecretReason::PasswordOnStdin),
            ("echo pw | docker login --password-stdin", SecretReason::PasswordOnStdin),
            ("sshpass -p pw ssh host", SecretReason::Sshpass),
        ];
        for (cmd, reason) in cases {
            assert_eq!(secret_reason(cmd), Some(reason), "{cmd}");
        }
    }

    #[test]
    fn keeps_ordinary_commands() {
        for cmd in [
            "docker ps -a",
            "docker logs -f myjam",
            "mysql -u root -p db",
            "systemctl restart nginx",
            "cd ~/docker_volumes/MyJam",
            "ssh -p 2222 host",
            "export PATH=$PATH:/opt/bin",
            "git clone https://github.com/x/y",
            "passwd",
            "grep -r TOKEN src/",
            "ls --password-file",
        ] {
            assert_eq!(secret_reason(cmd), None, "{cmd}");
        }
    }
}
