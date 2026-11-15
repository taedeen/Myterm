//! Working-directory tracking for combined `cd` suggestions (spec 3.3).
//!
//! After `cd docker_volumes` and later `cd MyJam`, history gains
//! `cd docker_volumes/MyJam`, so next time one suggestion gets you there.
//!
//! Two sources, best first:
//! 1. **The shell prompt.** Debian/Ubuntu-style prompts (`user@host:~/a/b$`)
//!    show the directory, so the tracker reads where the shell really is. This
//!    also covers Tab completion and history recall, where the typed text
//!    isn't what ran.
//! 2. **Typed `cd` commands**, when the prompt doesn't show a path. Only
//!    reliably-typed lines are used, and a `cd` that prints an error
//!    (`No such file or directory`) is ignored.
//!
//! Paths are kept as `~`, `~/a/b` or `/abs/path`. A combined suggestion is
//! made when a `cd` lands two or more levels below where the run of `cd`s
//! started. Runs that start at home are suggested relative to home
//! (`cd a/b`, as typed from a fresh login); others use the full path
//! (`cd /etc/nginx/sites-enabled`), which works from anywhere.

use crate::prompt::strip_ansi;

#[derive(Clone, Debug, Default)]
struct Awaiting {
    was_cd: bool,
    /// Target computed from the typed line (fallback mode only).
    typed_target: Option<String>,
    seen_newline: bool,
    error: bool,
}

#[derive(Clone, Debug)]
pub struct CdTracker {
    cwd: Option<String>,
    prev_cwd: Option<String>,
    /// Where the current run of `cd`s started.
    start: Option<String>,
    /// `user@host` from the first parsed prompt; other hosts (nested ssh) are ignored.
    prompt_host: Option<String>,
    prompt_mode: bool,
    awaiting: Option<Awaiting>,
}

impl Default for CdTracker {
    fn default() -> Self {
        // Login shells start in the home directory.
        CdTracker {
            cwd: Some("~".into()),
            prev_cwd: None,
            start: Some("~".into()),
            prompt_host: None,
            prompt_mode: false,
            awaiting: None,
        }
    }
}

const CD_ERRORS: &[&str] = &["No such file or directory", "Not a directory", "Permission denied", "can't cd", "too many arguments"];

/// Commands after which the typed-command fallback can no longer know the directory.
const CWD_UNKNOWABLE: &[&str] = &["su", "ssh", "mosh", "exit", "logout", "exec", "popd", "pushd", "telnet"];

impl CdTracker {
    pub fn new() -> Self {
        Self::default()
    }

    /// Current directory as far as the tracker knows (`~/a`, `/srv`), if known.
    pub fn cwd(&self) -> Option<&str> {
        self.cwd.as_deref()
    }

    /// Call when the user presses Enter. `reliable`: the tracked line matches
    /// what the shell ran. `hidden`: typed at a password prompt.
    /// Returns a combined `cd` command to add to history, if any.
    pub fn on_enter(&mut self, line: &str, reliable: bool, hidden: bool) -> Option<String> {
        let derived = if self.prompt_mode { None } else { self.commit_fallback() };
        if hidden {
            self.awaiting = None;
            return derived;
        }
        let words = split_words(line.trim());
        let first = words.first().map(String::as_str).unwrap_or("");
        let was_cd = first == "cd";
        let mut typed_target = None;
        if !self.prompt_mode {
            if was_cd && reliable && words.len() <= 2 {
                let arg = words.get(1).map(String::as_str).unwrap_or("~");
                typed_target = if arg == "-" {
                    self.prev_cwd.clone()
                } else {
                    self.cwd.as_deref().and_then(|c| resolve(c, arg))
                };
                if typed_target.is_none() {
                    self.forget();
                }
            } else if was_cd
                || line.contains("cd ")
                || CWD_UNKNOWABLE.contains(&first)
                || (first == "sudo" && words.iter().any(|w| w == "-i" || w == "-s" || w == "su"))
            {
                // Edited/compound cd, or a command that switches shells.
                self.forget();
            }
        }
        self.awaiting = Some(Awaiting { was_cd, typed_target, ..Default::default() });
        derived
    }

    /// Call with each chunk of server output and the updated output tail.
    /// Returns a combined `cd` command to add to history, if any.
    pub fn on_output(&mut self, tail: &str, data: &[u8]) -> Option<String> {
        if let Some(a) = self.awaiting.as_mut() {
            if data.contains(&b'\n') {
                a.seen_newline = true;
            }
            let text = String::from_utf8_lossy(data);
            if CD_ERRORS.iter().any(|e| text.contains(e)) {
                a.error = true;
            }
        }
        if self.awaiting.as_ref().is_some_and(|a| !a.seen_newline) {
            return None;
        }
        let (host, path) = parse_prompt(tail)?;
        self.prompt_mode = true;
        match &self.prompt_host {
            None => self.prompt_host = Some(host),
            Some(h) if *h != host => {
                self.awaiting = None;
                return None;
            }
            Some(_) => {}
        }
        let was_cd = self.awaiting.take().is_some_and(|a| a.was_cd);
        let Some(old) = self.cwd.clone() else {
            self.cwd = Some(path.clone());
            self.start = Some(path);
            return None;
        };
        if path == old {
            return None;
        }
        self.prev_cwd = Some(old.clone());
        self.cwd = Some(path.clone());
        if was_cd {
            self.moved(&old, &path)
        } else {
            self.start = Some(path);
            None
        }
    }

    fn forget(&mut self) {
        self.cwd = None;
        self.start = None;
    }

    fn commit_fallback(&mut self) -> Option<String> {
        let a = self.awaiting.take()?;
        if !a.was_cd || a.error {
            return None;
        }
        let target = a.typed_target?;
        let old = self.cwd.clone()?;
        if target == old {
            return None;
        }
        self.prev_cwd = Some(old.clone());
        self.cwd = Some(target.clone());
        self.moved(&old, &target)
    }

    fn moved(&mut self, old: &str, new: &str) -> Option<String> {
        let start = self.start.get_or_insert_with(|| old.to_string()).clone();
        match relative(&start, new) {
            Some(rel) if rel.split('/').count() >= 2 => {
                let target = if start == "~" { rel } else { new.to_string() };
                Some(format!("cd {}", quote(&target)))
            }
            Some(_) => None,
            None => {
                // Left the run's starting directory: start a new run here.
                self.start = Some(new.to_string());
                None
            }
        }
    }
}

/// Splits a command line into words, honouring simple single and double quotes.
fn split_words(line: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut cur = String::new();
    let mut quote: Option<char> = None;
    let mut has = false;
    for c in line.chars() {
        match (quote, c) {
            (Some(q), c) if c == q => quote = None,
            (Some(_), c) => cur.push(c),
            (None, '\'' | '"') => {
                quote = Some(c);
                has = true;
            }
            (None, c) if c.is_whitespace() => {
                if has || !cur.is_empty() {
                    words.push(std::mem::take(&mut cur));
                    has = false;
                }
            }
            (None, c) => cur.push(c),
        }
    }
    if has || !cur.is_empty() {
        words.push(cur);
    }
    words
}

fn quote(path: &str) -> String {
    if path.chars().all(|c| c.is_ascii_alphanumeric() || "/._-~+@:,".contains(c)) {
        path.to_string()
    } else {
        format!("'{}'", path.replace('\'', "'\\''"))
    }
}

/// Resolves `arg` against `cwd`. None when the result can't be known
/// (variables, globs, `..` above home).
pub fn resolve(cwd: &str, arg: &str) -> Option<String> {
    if arg.is_empty() || arg.contains(['$', '*', '?', '`', '[', '{']) {
        return None;
    }
    let (base, rest): (&str, &str) = if arg == "~" {
        ("~", "")
    } else if let Some(r) = arg.strip_prefix("~/") {
        ("~", r)
    } else if arg.starts_with('~') {
        return None; // ~otheruser
    } else if let Some(r) = arg.strip_prefix('/') {
        ("/", r)
    } else {
        (cwd, arg)
    };
    let (root, mut parts): (&str, Vec<&str>) = if base == "~" || base.starts_with("~/") {
        ("~", base.split('/').skip(1).collect())
    } else if base.starts_with('/') {
        ("/", base.split('/').filter(|s| !s.is_empty()).collect())
    } else {
        return None;
    };
    for seg in rest.split('/') {
        match seg {
            "" | "." => {}
            ".." => {
                if parts.pop().is_none() && root == "~" {
                    return None; // above home: absolute path unknown
                }
            }
            s => parts.push(s),
        }
    }
    Some(match (root, parts.is_empty()) {
        ("~", true) => "~".to_string(),
        ("~", false) => format!("~/{}", parts.join("/")),
        (_, true) => "/".to_string(),
        (_, false) => format!("/{}", parts.join("/")),
    })
}

/// `path` relative to `base` if `path` is strictly below it.
fn relative(base: &str, path: &str) -> Option<String> {
    let prefix = if base == "/" { "/".to_string() } else { format!("{base}/") };
    path.strip_prefix(&prefix).filter(|r| !r.is_empty()).map(str::to_string)
}

/// Parses Debian/Ubuntu-style prompts: `user@host:PATH$ ` or `user@host:PATH# `.
/// Returns (`user@host`, path) when PATH is `~...` or `/...`.
pub fn parse_prompt(tail: &str) -> Option<(String, String)> {
    let clean = strip_ansi(tail);
    let line = clean.rsplit('\n').next()?.trim_end();
    let body = line.strip_suffix('$').or_else(|| line.strip_suffix('#'))?;
    // Allow a leading "(venv) " or similar before user@host.
    let body = body.rsplit(' ').next()?;
    let (who, path) = body.split_once(':')?;
    let (user, host) = who.split_once('@')?;
    let valid = |s: &str| !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || "-_.".contains(c));
    if !valid(user) || !valid(host) || !(path.starts_with('~') || path.starts_with('/')) {
        return None;
    }
    let path = path.trim_end_matches('/');
    Some((who.to_string(), if path.is_empty() { "/".to_string() } else { path.to_string() }))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Simulates a shell session: each step is (typed line, output after Enter).
    fn run(t: &mut CdTracker, steps: &[(&str, &str)], start_prompt: &str) -> Vec<String> {
        let mut out = Vec::new();
        let mut tail = start_prompt.to_string();
        if let Some(d) = t.on_output(&tail, start_prompt.as_bytes()) {
            out.push(d);
        }
        for (line, output) in steps {
            tail.push_str(line);
            if let Some(d) = t.on_enter(line, true, false) {
                out.push(d);
            }
            tail.push_str(output);
            if let Some(d) = t.on_output(&tail, output.as_bytes()) {
                out.push(d);
            }
        }
        // A final Enter commits fallback-mode moves.
        if let Some(d) = t.on_enter("", true, false) {
            out.push(d);
        }
        out
    }

    #[test]
    fn resolves_paths() {
        assert_eq!(resolve("~", "docker_volumes").as_deref(), Some("~/docker_volumes"));
        assert_eq!(resolve("~/a/b", "../c/./d").as_deref(), Some("~/a/c/d"));
        assert_eq!(resolve("~/a", "/etc/nginx/").as_deref(), Some("/etc/nginx"));
        assert_eq!(resolve("/etc", "..").as_deref(), Some("/"));
        assert_eq!(resolve("/", "..").as_deref(), Some("/"));
        assert_eq!(resolve("/srv", "~").as_deref(), Some("~"));
        assert_eq!(resolve("/srv", "~/x").as_deref(), Some("~/x"));
        assert_eq!(resolve("~", ".."), None, "above home is unknown");
        assert_eq!(resolve("~", "$HOME/x"), None);
        assert_eq!(resolve("~", "~bob"), None);
    }

    #[test]
    fn parses_debian_prompts() {
        assert_eq!(
            parse_prompt("\x1b[01;32malice@server1\x1b[00m:\x1b[01;34m~/docker_volumes/MyJam\x1b[00m$ "),
            Some(("alice@server1".into(), "~/docker_volumes/MyJam".into()))
        );
        assert_eq!(parse_prompt("root@web1:/etc/nginx# "), Some(("root@web1".into(), "/etc/nginx".into())));
        assert_eq!(parse_prompt("root@web1:/# "), Some(("root@web1".into(), "/".into())));
        assert_eq!(parse_prompt("(venv) t@server1:~$ "), Some(("t@server1".into(), "~".into())));
        assert_eq!(parse_prompt("[t@rhel MyJam]$ "), None, "RHEL prompt has no full path");
        assert_eq!(parse_prompt("t@server1:~$ ls"), None, "typing after the prompt");
        assert_eq!(parse_prompt("Password: "), None);
    }

    #[test]
    fn prompt_mode_combines_cds_even_with_ls_between() {
        let mut t = CdTracker::new();
        let got = run(
            &mut t,
            &[
                ("cd docker_volumes", "\r\nt@server1:~/docker_volumes$ "),
                ("ls", "\r\nMyJam  frigate\r\nt@server1:~/docker_volumes$ "),
                ("cd MyJam", "\r\nt@server1:~/docker_volumes/MyJam$ "),
                ("cd config", "\r\nt@server1:~/docker_volumes/MyJam/config$ "),
            ],
            "t@server1:~$ ",
        );
        assert_eq!(got, vec!["cd docker_volumes/MyJam", "cd docker_volumes/MyJam/config"]);
        assert_eq!(t.cwd(), Some("~/docker_volumes/MyJam/config"));
    }

    #[test]
    fn prompt_mode_sees_tab_completed_paths() {
        // The typed text "cd doc" was completed by the shell; the prompt tells the truth.
        let mut t = CdTracker::new();
        let mut tail = String::from("t@server1:~$ ");
        t.on_output(&tail.clone(), tail.clone().as_bytes());
        t.on_enter("cd doc", false, false);
        tail.push_str("cd docker_volumes/\r\nt@server1:~/docker_volumes$ ");
        assert_eq!(t.on_output(&tail, b"\r\nt@server1:~/docker_volumes$ "), None);
        t.on_enter("cd My", false, false);
        tail.push_str("cd MyJam/\r\nt@server1:~/docker_volumes/MyJam$ ");
        assert_eq!(t.on_output(&tail, b"\r\nt@server1:~/docker_volumes/MyJam$ ").as_deref(), Some("cd docker_volumes/MyJam"));
    }

    #[test]
    fn absolute_runs_use_full_paths_and_leaving_resets() {
        let mut t = CdTracker::new();
        let got = run(
            &mut t,
            &[
                ("cd /etc", "\r\nr@b:/etc# "),
                ("cd nginx", "\r\nr@b:/etc/nginx# "),
                ("cd sites-enabled", "\r\nr@b:/etc/nginx/sites-enabled# "),
                ("cd /var/log", "\r\nr@b:/var/log# "),
                ("cd nginx", "\r\nr@b:/var/log/nginx# "),
            ],
            "r@b:~# ",
        );
        assert_eq!(got, vec!["cd /etc/nginx/sites-enabled"]);
    }

    #[test]
    fn prompt_mode_ignores_other_hosts_and_non_cd_moves() {
        let mut t = CdTracker::new();
        let got = run(
            &mut t,
            &[
                ("cd a", "\r\nt@server1:~/a$ "),
                ("ssh nas", "\r\nt@nas:~$ "),
                ("cd x", "\r\nt@nas:~/x$ "),
                ("cd y", "\r\nt@nas:~/x/y$ "),
                ("exit", "\r\nt@server1:~/a$ "),
                ("cd b", "\r\nt@server1:~/a/b$ "),
            ],
            "t@server1:~$ ",
        );
        assert_eq!(got, vec!["cd a/b"], "nested host ignored, home run continues");
    }

    #[test]
    fn fallback_mode_tracks_typed_cds() {
        // Prompt without a path: rely on the typed commands.
        let mut t = CdTracker::new();
        let got = run(
            &mut t,
            &[
                ("cd docker_volumes", "\r\n$ "),
                ("ls", "\r\nMyJam\r\n$ "),
                ("cd MyJam", "\r\n$ "),
                ("cd nope", "\r\nbash: cd: nope: No such file or directory\r\n$ "),
                ("cd 'My Dir'", "\r\n$ "),
            ],
            "$ ",
        );
        assert_eq!(got, vec!["cd docker_volumes/MyJam", "cd 'docker_volumes/MyJam/My Dir'"]);
    }

    #[test]
    fn fallback_mode_gives_up_when_it_cannot_know() {
        let mut t = CdTracker::new();
        let got = run(&mut t, &[("cd a", "\r\n$ "), ("su -", "\r\n# "), ("cd b", "\r\n# "), ("cd c", "\r\n# ")], "$ ");
        assert!(got.is_empty(), "{got:?}");
        let mut t = CdTracker::new();
        t.on_enter("cd a", true, false);
        t.on_enter("cd doc", false, false); // Tab-completed: unknown
        t.on_enter("cd b", true, false);
        assert_eq!(t.on_enter("", true, false), None);
    }

    #[test]
    fn cd_dash_and_hidden_input() {
        let mut t = CdTracker::new();
        let got = run(&mut t, &[("cd a", "\r\n$ "), ("cd b", "\r\n$ "), ("cd -", "\r\n$ "), ("cd c", "\r\n$ ")], "$ ");
        assert_eq!(got, vec!["cd a/b", "cd a/c"]);
        let mut t = CdTracker::new();
        assert_eq!(t.on_enter("cd secret", true, true), None);
        assert_eq!(t.on_enter("", true, false), None);
        assert_eq!(t.cwd(), Some("~"));
    }
}
