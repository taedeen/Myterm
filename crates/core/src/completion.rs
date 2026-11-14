//! Suggestion engine (spec 3.3).
//!
//! Sources, highest priority first: snippets; this host's history (all
//! devices); related hosts' history (same group or OS); bundled command specs.
//! Remote-aware completion (PATH executables, file paths) is fed in by the
//! SSH layer through [`RemoteHints`].

use std::collections::HashMap;

use crate::history::HistoryStore;
use crate::Id;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Snippet {
    pub id: Id,
    pub name: String,
    /// Body; may contain `{{param}}` placeholders.
    pub body: String,
}

impl Snippet {
    /// Parameter names in order of first appearance.
    pub fn params(&self) -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        let mut rest = self.body.as_str();
        while let Some(start) = rest.find("{{") {
            let after = &rest[start + 2..];
            let Some(end) = after.find("}}") else { break };
            let name = after[..end].trim().to_string();
            if !name.is_empty() && !out.contains(&name) {
                out.push(name);
            }
            rest = &after[end + 2..];
        }
        out
    }

    /// Fills placeholders; unknown parameters are left as-is.
    pub fn render(&self, values: &HashMap<String, String>) -> String {
        let mut s = self.body.clone();
        for (k, v) in values {
            s = s.replace(&format!("{{{{{k}}}}}"), v);
        }
        s
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Source {
    Snippet,
    HostHistory,
    RelatedHistory,
    Spec,
    Remote,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Suggestion {
    /// The full command line the suggestion would produce.
    pub line: String,
    pub source: Source,
    pub score: f64,
    /// Snippet id when `source == Snippet` (UI prompts for parameters).
    pub snippet_id: Option<Id>,
}

pub struct CommandSpec {
    pub name: &'static str,
    pub subcommands: &'static [&'static str],
    pub flags: &'static [&'static str],
}

/// Small bundled library; the full one ships as data with the app.
pub static BUILTIN_SPECS: &[CommandSpec] = &[
    CommandSpec { name: "git", subcommands: &["add", "branch", "checkout", "clone", "commit", "diff", "fetch", "log", "pull", "push", "rebase", "restore", "stash", "status", "switch"], flags: &["--help", "--version"] },
    CommandSpec { name: "docker", subcommands: &["build", "compose", "exec", "images", "inspect", "logs", "ps", "pull", "restart", "rm", "rmi", "run", "start", "stats", "stop", "system"], flags: &["--help", "--version"] },
    CommandSpec { name: "systemctl", subcommands: &["daemon-reload", "disable", "enable", "is-active", "list-units", "reload", "restart", "start", "status", "stop"], flags: &["--failed", "--now", "--user", "--no-pager"] },
    CommandSpec { name: "journalctl", subcommands: &[], flags: &["-u", "-f", "-n", "-b", "--since", "--until", "--no-pager", "-xe"] },
    CommandSpec { name: "apt", subcommands: &["autoremove", "full-upgrade", "install", "list", "purge", "remove", "search", "show", "update", "upgrade"], flags: &["-y", "--no-install-recommends"] },
    CommandSpec { name: "kubectl", subcommands: &["apply", "config", "delete", "describe", "exec", "get", "logs", "port-forward", "rollout", "scale"], flags: &["-n", "-A", "-o", "-f", "--context"] },
    CommandSpec { name: "zpool", subcommands: &["clear", "iostat", "list", "scrub", "status"], flags: &["-v", "-x"] },
    CommandSpec { name: "tmux", subcommands: &["attach", "kill-session", "ls", "new", "new-session"], flags: &["-s", "-t"] },
];

/// Executables and paths fetched from the host over the side channel.
#[derive(Clone, Debug, Default)]
pub struct RemoteHints {
    pub executables: Vec<String>,
    /// Entries of the directory the current token points into.
    pub paths: Vec<String>,
}

pub struct CompletionInput<'a> {
    pub host_id: &'a str,
    pub related_hosts: &'a [Id],
    pub line: &'a str,
    pub now: u64,
    pub limit: usize,
}

const W_SNIPPET: f64 = 4.0;
const W_HOST: f64 = 2.0;
const W_RELATED: f64 = 0.75;
const W_SPEC: f64 = 0.5;
const W_REMOTE: f64 = 0.4;

pub fn suggest(
    input: &CompletionInput,
    snippets: &[Snippet],
    history: &HistoryStore,
    specs: &[CommandSpec],
    remote: Option<&RemoteHints>,
) -> Vec<Suggestion> {
    let line = input.line;
    if line.trim().is_empty() {
        return Vec::new();
    }
    let mut best: HashMap<String, Suggestion> = HashMap::new();
    let mut offer = |line_out: String, source: Source, score: f64, snippet_id: Option<Id>| {
        if line_out == line {
            return;
        }
        let keep = best.get(&line_out).is_none_or(|s| score > s.score);
        if keep {
            best.insert(line_out.clone(), Suggestion { line: line_out, source, score, snippet_id });
        }
    };

    // 1. Snippets: match on name or body prefix.
    let lower = line.to_lowercase();
    for s in snippets {
        if s.body.starts_with(line) || s.name.to_lowercase().starts_with(&lower) {
            offer(s.body.clone(), Source::Snippet, W_SNIPPET, Some(s.id.clone()));
        }
    }

    // 2 and 3. History prefix matches.
    for (cmd, e) in history.entries(input.host_id) {
        if cmd.starts_with(line) {
            offer(cmd.to_string(), Source::HostHistory, W_HOST + HistoryStore::score(e, input.now), None);
        }
    }
    for host in input.related_hosts {
        for (cmd, e) in history.entries(host) {
            if cmd.starts_with(line) {
                offer(cmd.to_string(), Source::RelatedHistory, W_RELATED * HistoryStore::score(e, input.now), None);
            }
        }
    }

    // 4. Specs complete the current token.
    let ends_with_space = line.ends_with(' ');
    let tokens: Vec<&str> = line.split_whitespace().collect();
    let (prefix_tokens, partial) = if ends_with_space {
        (tokens.as_slice(), "")
    } else {
        (&tokens[..tokens.len() - 1], *tokens.last().unwrap())
    };
    let head = line[..line.len() - partial.len()].to_string();
    let mut complete_token = |cands: &mut dyn Iterator<Item = &str>, source: Source, weight: f64| {
        for c in cands {
            if c.starts_with(partial) && c != partial {
                offer(format!("{head}{c}"), source, weight, None);
            }
        }
    };
    if prefix_tokens.is_empty() {
        complete_token(&mut specs.iter().map(|s| s.name), Source::Spec, W_SPEC);
        if let Some(r) = remote {
            complete_token(&mut r.executables.iter().map(String::as_str), Source::Remote, W_REMOTE);
        }
    } else if let Some(spec) = specs.iter().find(|s| s.name == prefix_tokens[0]) {
        if partial.starts_with('-') {
            complete_token(&mut spec.flags.iter().copied(), Source::Spec, W_SPEC);
        } else if prefix_tokens.len() == 1 {
            complete_token(&mut spec.subcommands.iter().copied(), Source::Spec, W_SPEC);
        }
    }
    if !prefix_tokens.is_empty() && !partial.starts_with('-') {
        if let Some(r) = remote {
            complete_token(&mut r.paths.iter().map(String::as_str), Source::Remote, W_REMOTE);
        }
    }

    let mut out: Vec<Suggestion> = best.into_values().collect();
    out.sort_by(|a, b| b.score.partial_cmp(&a.score).unwrap_or(std::cmp::Ordering::Equal).then_with(|| a.line.cmp(&b.line)));
    out.truncate(input.limit);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::RecordContext;

    fn hist() -> HistoryStore {
        let mut h = HistoryStore::new("desk");
        for _ in 0..5 {
            h.record("server1", "docker logs -f myjam", 1_000, RecordContext::default()).unwrap();
        }
        h.record("server1", "docker ps -a", 1_000, RecordContext::default()).unwrap();
        h.record("web1", "docker compose up -d", 1_000, RecordContext::default()).unwrap();
        h
    }

    fn input<'a>(line: &'a str, related: &'a [Id]) -> CompletionInput<'a> {
        CompletionInput { host_id: "server1", related_hosts: related, line, now: 1_000, limit: 10 }
    }

    #[test]
    fn ranks_snippet_then_host_then_related_then_spec() {
        let snippets = vec![Snippet { id: "s1".into(), name: "restart jam".into(), body: "docker restart myjam".into() }];
        let related = vec!["web1".to_string()];
        let out = suggest(&input("docker ", &related), &snippets, &hist(), BUILTIN_SPECS, None);
        let order: Vec<(Source, &str)> = out.iter().map(|s| (s.source, s.line.as_str())).collect();
        assert_eq!(order[0], (Source::Snippet, "docker restart myjam"));
        assert_eq!(order[1], (Source::HostHistory, "docker logs -f myjam"), "more runs ranks higher");
        assert_eq!(order[2], (Source::HostHistory, "docker ps -a"));
        assert_eq!(order[3], (Source::RelatedHistory, "docker compose up -d"));
        assert!(order[4..].iter().all(|(s, _)| *s == Source::Spec));
    }

    #[test]
    fn spec_tokens_and_flags() {
        let h = HistoryStore::new("d");
        let lines = |l: &str| -> Vec<String> {
            suggest(&input(l, &[]), &[], &h, BUILTIN_SPECS, None).into_iter().map(|s| s.line).collect()
        };
        assert_eq!(lines("syst"), vec!["systemctl"]);
        assert!(lines("systemctl re").contains(&"systemctl restart".to_string()));
        assert!(lines("systemctl re").contains(&"systemctl reload".to_string()));
        assert_eq!(lines("journalctl --no"), vec!["journalctl --no-pager"]);
        assert!(lines("").is_empty());
    }

    #[test]
    fn remote_hints_and_dedupe() {
        let h = HistoryStore::new("d");
        let r = RemoteHints { executables: vec!["docker".into(), "dockerd".into()], paths: vec!["docker_volumes/".into()] };
        let out = suggest(&input("dock", &[]), &[], &h, BUILTIN_SPECS, Some(&r));
        let lines: Vec<&str> = out.iter().map(|s| s.line.as_str()).collect();
        assert_eq!(lines.iter().filter(|l| **l == "docker").count(), 1, "deduplicated");
        assert!(lines.contains(&"dockerd"));
        let out = suggest(&input("cd dock", &[]), &[], &h, BUILTIN_SPECS, Some(&r));
        assert_eq!(out[0].line, "cd docker_volumes/");
    }

    #[test]
    fn snippet_params() {
        let s = Snippet { id: "s".into(), name: "logs".into(), body: "docker logs -f {{container}} --tail {{ n }} {{container}}".into() };
        assert_eq!(s.params(), vec!["container", "n"]);
        let v = HashMap::from([("container".to_string(), "myjam".to_string())]);
        assert_eq!(s.render(&v), "docker logs -f myjam --tail {{ n }} myjam");
    }
}
