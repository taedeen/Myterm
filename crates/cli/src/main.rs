//! `myterm`: MyTerm on the Linux command line.
//!
//! Another device on your vault: the same hosts, history, snippets, known
//! hosts and SSH certificates as the apps, through the same sync server.

mod import;
mod keyholder;
mod picker;
mod prompt;
mod run;
mod store;
mod sync;
mod term;

use std::process::ExitCode;
use std::sync::Arc;

use anyhow::{anyhow, bail, Context, Result};
use clap::{Parser, Subcommand};
use myterm_ffi::api::HostDto;
use serde_json::{json, Value};

use picker::CloneDto;
use store::{Scope, Store};

#[derive(Parser)]
#[command(name = "myterm", version, about = "MyTerm on the Linux command line: your hosts, history and SSH certificates from the MyTerm vault")]
struct Cli {
    #[command(subcommand)]
    command: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Join your vault through the sync server (once per machine)
    Join {
        /// Sync server address, e.g. https://myterm.example.com (asked if left out;
        /// the one used last is offered)
        url: Option<String>,
        /// Vault name (asked if left out)
        #[arg(long)]
        name: Option<String>,
        /// What this machine takes: full, certs (hosts and certificates, no stored passwords) or hosts
        #[arg(long)]
        scope: Option<String>,
        /// Start a new vault on the server instead (when you have no MyTerm vault yet)
        #[arg(long)]
        create: bool,
    },
    /// Change a host's options
    Set {
        host: String,
        /// Sign in with this device's MyTerm certificate (on/off)
        #[arg(long)]
        certificate: Option<OnOff>,
        /// Keep the session running in tmux on the server (on/off)
        #[arg(long)]
        keep_session: Option<OnOff>,
        /// Jump host (a host label), or "none"
        #[arg(long)]
        via: Option<String>,
        #[arg(long)]
        user: Option<String>,
        #[arg(long)]
        port: Option<u16>,
        /// Comma-separated tags (replaces the existing ones)
        #[arg(long)]
        tags: Option<String>,
    },
    /// Unlock and keep the vault unlocked for a while (default 15 minutes idle)
    Unlock {
        #[arg(long, default_value_t = 15)]
        minutes: u64,
    },
    /// Lock now
    Lock,
    /// This machine's state: vault, sync, scope, certificate
    Status,
    /// Sync with the sync server now
    Sync,
    /// List hosts (optionally filtered by words)
    Hosts {
        filter: Vec<String>,
        #[arg(long)]
        json: bool,
    },
    /// Connect to a host, or run a command on it
    Ssh {
        host: String,
        /// Run in tmux on the server and reattach (overrides the host setting); NAME defaults to myterm-1
        #[arg(long, value_name = "NAME", num_args = 0..=1, default_missing_value = "myterm-1")]
        tmux: Option<String>,
        /// Don't use tmux even if the host has "Keep session running"
        #[arg(long)]
        no_tmux: bool,
        /// Command to run instead of a shell
        #[arg(last = true)]
        command: Vec<String>,
    },
    /// Run a command on many hosts in parallel, output grouped per host
    Run {
        command: String,
        /// Hosts (labels); or use --tag
        hosts: Vec<String>,
        #[arg(short, long)]
        tag: Vec<String>,
        #[arg(long, default_value_t = 8)]
        parallel: usize,
        /// Prefix every line with the host instead of grouping
        #[arg(long)]
        prefix: bool,
        #[arg(long, default_value_t = 120)]
        timeout: u64,
    },
    /// Open hosts in tmux panes with typing sent to all of them
    Multi {
        hosts: Vec<String>,
        #[arg(short, long)]
        tag: Vec<String>,
    },
    /// Import hosts from an OpenSSH config file
    Import {
        #[arg(default_value = "~/.ssh/config")]
        path: String,
    },
    /// Show, renew or install SSH certificate trust
    Cert {
        #[arg(long)]
        renew: bool,
        /// Make this machine's sshd trust MyTerm certificates (uses sudo)
        #[arg(long)]
        trust: bool,
    },
    /// Devices syncing with this vault
    Devices,
    /// Rename this device (as shown in the Devices list)
    Rename { name: String },
    /// Print this device's audit log (JSON lines)
    Audit,
    /// Shell completions: myterm completions bash > ~/.local/share/bash-completion/completions/myterm
    Completions { shell: clap_complete::Shell },
    #[command(name = "__keyholder", hide = true)]
    KeyHolder { device: String, idle: u64 },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum OnOff {
    On,
    Off,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    match real_main(cli) {
        Ok(code) => ExitCode::from(code),
        Err(e) => {
            eprintln!("myterm: {e:#}");
            ExitCode::from(1)
        }
    }
}

/// Runs async work, then drops the runtime without waiting for the
/// keyboard reader (a blocking read that only ends at the next key press).
fn block_on<F: std::future::Future>(f: F) -> F::Output {
    let rt = tokio::runtime::Builder::new_multi_thread().enable_all().build().expect("tokio runtime");
    let out = rt.block_on(f);
    rt.shutdown_background();
    out
}

/// Unlocks, keeping the key in the key holder for the next commands.
fn unlock(store: &Store) -> Result<()> {
    if store.engine.is_unlocked() {
        return Ok(());
    }
    if !store.has_vault() {
        bail!("this machine hasn't joined a vault yet: run `myterm join`");
    }
    if let Some(mk) = keyholder::fetch(&store.device_id()) {
        return store.engine.unlock_with_master_key(mk);
    }
    let password = prompt::secret("MyTerm master password: ")?;
    let mk = store.unwrap(&password)?;
    store.engine.unlock_with_master_key(mk.clone())?;
    let minutes: u64 = std::env::var("MYTERM_UNLOCK_MINUTES").ok().and_then(|v| v.parse().ok()).unwrap_or(15);
    if minutes > 0 {
        if let Err(e) = keyholder::start(&store.device_id(), &mk, minutes * 60) {
            eprintln!("(couldn't keep the vault unlocked: {e})");
        }
    }
    Ok(())
}

/// Syncs if the last sync was more than a minute ago; problems are warnings.
fn sync_quietly(store: &mut Store) {
    if store.str("syncUrl").is_none() {
        return;
    }
    let last = store.meta.get("syncLastAt").and_then(Value::as_u64).unwrap_or(0);
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_millis() as u64).unwrap_or(0);
    if now.saturating_sub(last) < 60_000 {
        return;
    }
    if let Err(e) = sync::sync_now(store) {
        eprintln!("(sync: {e})");
    }
}

fn find_host(hosts: &[HostDto], name: &str) -> Result<HostDto> {
    let n = name.to_lowercase();
    if let Some(h) = hosts.iter().find(|h| h.label.to_lowercase() == n || h.address.to_lowercase() == n || h.id == name) {
        return Ok(h.clone_dto());
    }
    let partial: Vec<&HostDto> = hosts.iter().filter(|h| h.label.to_lowercase().contains(&n)).collect();
    match partial.len() {
        1 => Ok(partial[0].clone_dto()),
        0 => bail!("no host called {name} (see `myterm hosts`)"),
        _ => bail!("{name} matches {}", partial.iter().map(|h| h.label.as_str()).collect::<Vec<_>>().join(", ")),
    }
}

fn select_hosts(hosts: &[HostDto], names: &[String], tags: &[String]) -> Result<Vec<HostDto>> {
    let mut out: Vec<HostDto> = Vec::new();
    for n in names {
        let h = find_host(hosts, n)?;
        if !out.iter().any(|o| o.id == h.id) {
            out.push(h);
        }
    }
    for t in tags {
        let t = t.to_lowercase();
        for h in hosts.iter().filter(|h| h.tags.iter().any(|x| x.to_lowercase() == t)) {
            if !out.iter().any(|o| o.id == h.id) {
                out.push(h.clone_dto());
            }
        }
    }
    if out.is_empty() {
        bail!("no hosts selected: name some, or use --tag");
    }
    Ok(out)
}

fn expand_home(p: &str) -> String {
    match p.strip_prefix("~/") {
        Some(rest) => format!("{}/{rest}", std::env::var("HOME").unwrap_or_default()),
        None => p.to_string(),
    }
}

fn real_main(cli: Cli) -> Result<u8> {
    if let Some(Cmd::KeyHolder { device, idle }) = &cli.command {
        keyholder::serve(device, *idle)?;
        return Ok(0);
    }
    if let Some(Cmd::Completions { shell }) = &cli.command {
        use clap::CommandFactory;
        clap_complete::generate(*shell, &mut Cli::command(), "myterm", &mut std::io::stdout());
        return Ok(0);
    }
    let mut store = Store::open()?;
    match cli.command {
        None => {
            unlock(&store)?;
            sync_quietly(&mut store);
            let mut hosts = store.engine.list_hosts();
            hosts.sort_by_key(|h| h.label.to_lowercase());
            if hosts.is_empty() {
                bail!("no hosts yet: add some in the app, or run `myterm import`");
            }
            let Some(host) = picker::pick(&hosts)? else { return Ok(0) };
            connect(&mut store, &host, None, false)
        }
        Some(Cmd::Join { url, name, scope, create }) => {
            if store.has_vault() && !prompt::yes("This machine has already joined a vault. Join again?", false)? {
                return Ok(0);
            }
            let url = match url {
                Some(u) => u,
                None => {
                    let last = store.str("syncUrl").unwrap_or_default();
                    let q = if last.is_empty() {
                        format!("Sync server address (e.g. {})", sync::EXAMPLE_URL)
                    } else {
                        "Sync server address".to_string()
                    };
                    prompt::ask(&q, &last)?
                }
            };
            let url = sync::normalise_url(&url)?;
            println!("Joining your MyTerm vault through {url}.");
            let token = prompt::secret("Server token (MYTERM_SYNC_TOKEN on the sync server): ")?;
            let name = match name {
                Some(n) => n,
                None => prompt::ask("Vault name (as shown in the app's Sync settings)", "")?,
            };
            let scope = match scope {
                Some(s) => Scope::parse(&s).ok_or_else(|| anyhow!("--scope is full, certs or hosts"))?,
                None => {
                    println!("What should this machine take from the vault?");
                    for (i, s) in [Scope::Full, Scope::Certs, Scope::Hosts].iter().enumerate() {
                        println!("  {}. {:<5}  {}", i + 1, s.as_str(), s.describe());
                    }
                    match prompt::ask("Choose", "2")?.as_str() {
                        "1" | "full" => Scope::Full,
                        "3" | "hosts" => Scope::Hosts,
                        _ => Scope::Certs,
                    }
                }
            };
            let password = prompt::secret("Master password: ")?;
            store.meta.insert("scope".into(), json!(scope.as_str()));
            let mk = if create {
                if password.chars().count() < 12 {
                    bail!("use a master password of at least 12 characters");
                }
                if prompt::secret("Master password again: ")? != password {
                    bail!("the passwords don't match");
                }
                sync::create(&mut store, &url, &token, &name, &password)?
            } else {
                sync::join(&mut store, &url, &token, &name, &password)?
            };
            let summary = sync::sync_now(&mut store)?;
            let _ = keyholder::start(&store.device_id(), &mk, 15 * 60);
            println!("Joined vault {name} as {} ({}). Sync: {summary}.", store.device_name(), scope.as_str());
            println!("{} hosts. Try `myterm` to pick one.", store.engine.list_hosts().len());
            if scope != Scope::Hosts {
                match sync::renew_certificate(&store, false, 2 * 3600) {
                    Ok(_) => println!("SSH certificate issued."),
                    Err(e) => println!("No SSH certificate: {e}"),
                }
            }
            Ok(0)
        }
        Some(Cmd::Set { host, certificate, keep_session, via, user, port, tags }) => {
            unlock(&store)?;
            let hosts = store.engine.list_hosts();
            let mut h = find_host(&hosts, &host)?;
            let on = |v: OnOff| matches!(v, OnOff::On);
            if let Some(v) = certificate {
                h.use_certificate = Some(on(v));
            }
            if let Some(v) = keep_session {
                h.persistent = Some(on(v));
            }
            if let Some(v) = via {
                h.jump_chain = if v == "none" { vec![] } else { vec![find_host(&hosts, &v)?.id] };
            }
            if let Some(u) = user {
                h.username = u;
            }
            if let Some(p) = port {
                h.port = Some(p);
            }
            if let Some(t) = tags {
                h.tags = t.split(',').map(|x| x.trim().to_string()).filter(|x| !x.is_empty()).collect();
            }
            let label = h.label.clone();
            store.engine.upsert_host(h)?;
            store.save()?;
            sync_quietly(&mut store);
            println!("Updated {label}.");
            Ok(0)
        }
        Some(Cmd::Unlock { minutes }) => {
            keyholder::lock(&store.device_id());
            let password = prompt::secret("MyTerm master password: ")?;
            let mk = store.unwrap(&password)?;
            keyholder::start(&store.device_id(), &mk, minutes * 60)?;
            println!("Unlocked for {minutes} minutes of inactivity. `myterm lock` locks now.");
            Ok(0)
        }
        Some(Cmd::Lock) => {
            println!("{}", if keyholder::lock(&store.device_id()) { "Locked." } else { "Already locked." });
            Ok(0)
        }
        Some(Cmd::Status) => {
            println!("Device:      {} ({})", store.device_name(), &store.device_id()[..8]);
            println!("Vault:       {}", if store.has_vault() { store.str("syncVaultName").unwrap_or_else(|| "joined".into()) } else { "not joined: run `myterm join`".into() });
            println!("Sync server: {}", store.str("syncUrl").unwrap_or_else(|| "-".into()));
            println!("Scope:       {} ({})", store.scope().as_str(), store.scope().describe());
            match keyholder::status(&store.device_id()) {
                Some(s) => println!("Unlocked:    yes, locks after {} min idle", s / 60 + 1),
                None => println!("Unlocked:    no"),
            }
            if store.engine.is_unlocked() || keyholder::status(&store.device_id()).is_some() {
                unlock(&store)?;
                match store.engine.ssh_certificate() {
                    Some(c) => println!("Certificate: {} for {}, until {}", c.key_id, c.principals.join(","), fmt_time(c.valid_before)),
                    None => println!("Certificate: none"),
                }
                println!("Hosts:       {}", store.engine.list_hosts().len());
            }
            Ok(0)
        }
        Some(Cmd::Sync) => {
            unlock(&store)?;
            println!("Synced: {}", sync::sync_now(&mut store)?);
            Ok(0)
        }
        Some(Cmd::Hosts { filter, json }) => {
            unlock(&store)?;
            sync_quietly(&mut store);
            let all = store.engine.list_hosts();
            let words = filter.join(" ").to_lowercase();
            let mut hosts: Vec<&HostDto> = all
                .iter()
                .filter(|h| {
                    words.split_whitespace().all(|w| {
                        h.label.to_lowercase().contains(w) || h.address.to_lowercase().contains(w) || h.tags.iter().any(|t| t.to_lowercase().contains(w))
                    })
                })
                .collect();
            hosts.sort_by_key(|h| h.label.to_lowercase());
            let label_of = |id: &str| all.iter().find(|h| h.id == id).map(|h| h.label.clone()).unwrap_or_else(|| id.to_string());
            if json {
                let v: Vec<Value> = hosts
                    .iter()
                    .map(|h| {
                        json!({"id": h.id, "label": h.label, "address": h.address, "port": h.port.unwrap_or(22),
                               "user": h.username, "tags": h.tags, "via": h.jump_chain.iter().map(|j| label_of(j)).collect::<Vec<_>>(),
                               "certificate": h.use_certificate == Some(true), "keep_session": h.persistent == Some(true)})
                    })
                    .collect();
                println!("{}", serde_json::to_string_pretty(&v)?);
            } else {
                for h in hosts {
                    let via = if h.jump_chain.is_empty() { String::new() } else { format!(" via {}", h.jump_chain.iter().map(|j| label_of(j)).collect::<Vec<_>>().join(",")) };
                    let auth = if h.use_certificate == Some(true) {
                        "certificate"
                    } else if store.engine.host_has_credential(h.id.clone()) {
                        "saved"
                    } else {
                        "ask"
                    };
                    let tags = if h.tags.is_empty() { String::new() } else { format!(" [{}]", h.tags.join(", ")) };
                    println!("{:<20} {:<32} {:<11}{via}{tags}", h.label, picker::describe(h), auth);
                }
            }
            Ok(0)
        }
        Some(Cmd::Ssh { host, tmux, no_tmux, command }) => {
            unlock(&store)?;
            sync_quietly(&mut store);
            let h = find_host(&store.engine.list_hosts(), &host)?;
            if !command.is_empty() {
                let store = Arc::new(store);
                let outcomes = block_on(run::run(store.clone(), vec![h], command.join(" "), 1, 3600));
                let o = &outcomes[0];
                print!("{}", o.output);
                if let Some(e) = &o.error {
                    eprintln!("myterm: {e}");
                    return Ok(255);
                }
                let _ = store.save();
                return Ok(o.code.unwrap_or(0).min(255) as u8);
            }
            let tmux = if no_tmux { None } else { tmux.or_else(|| (h.persistent == Some(true)).then(|| "myterm-1".to_string())) };
            connect(&mut store, &h, tmux, true)
        }
        Some(Cmd::Run { command, hosts, tag, parallel, prefix, timeout }) => {
            unlock(&store)?;
            sync_quietly(&mut store);
            let selected = select_hosts(&store.engine.list_hosts(), &hosts, &tag)?;
            let store = Arc::new(store);
            let outcomes = block_on(run::run(store.clone(), selected, command, parallel, timeout));
            let failed = run::print(&outcomes, prefix);
            let _ = store.save();
            Ok(if failed == 0 { 0 } else { 1 })
        }
        Some(Cmd::Multi { hosts, tag }) => {
            unlock(&store)?; // starts the key holder, so the panes don't each ask
            let selected = select_hosts(&store.engine.list_hosts(), &hosts, &tag)?;
            multi(&selected)
        }
        Some(Cmd::Import { path }) => {
            unlock(&store)?;
            let path = expand_home(&path);
            let text = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
            let entries = import::parse(&text);
            let (added, updated) = import::apply(&store.engine, &entries)?;
            store.save()?;
            println!("Imported from {path}: {added} added, {updated} updated (tag \"imported\").");
            println!("Keys stay in ~/.ssh; set a password, key or certificate for each host in the app.");
            sync_quietly(&mut store);
            Ok(0)
        }
        Some(Cmd::Cert { renew, trust }) => {
            unlock(&store)?;
            if trust {
                return trust_ca(&store);
            }
            if renew {
                sync::renew_certificate(&store, true, 0)?;
            }
            match store.engine.ssh_certificate() {
                Some(c) => {
                    println!("Certificate {} for {}", c.key_id, c.principals.join(", "));
                    println!("Valid until {}", fmt_time(c.valid_before));
                }
                None => println!("No certificate yet: `myterm cert --renew` gets one."),
            }
            Ok(0)
        }
        Some(Cmd::Devices) => {
            unlock(&store)?;
            let me = store.device_id();
            for d in sync::devices(&store)? {
                let id = d["device_id"].as_str().unwrap_or("");
                println!(
                    "{:<32} {:<10} last synced {}{}",
                    d["name"].as_str().filter(|s| !s.is_empty()).unwrap_or("(unnamed)"),
                    &id[..8.min(id.len())],
                    fmt_time(d["last_seen"].as_u64().unwrap_or(0)),
                    if id == me { "  (this machine)" } else { "" }
                );
            }
            println!("Forget or revoke devices in the app: Settings > Sync > Devices.");
            Ok(0)
        }
        Some(Cmd::Rename { name }) => {
            store.meta.insert("deviceName".into(), json!(name.trim()));
            store.write_meta()?;
            println!("This device is now \"{}\" (the Devices list updates at the next sync).", name.trim());
            Ok(0)
        }
        Some(Cmd::Audit) => {
            unlock(&store)?;
            print!("{}", store.engine.export_audit_jsonl());
            Ok(0)
        }
        Some(Cmd::KeyHolder { .. }) | Some(Cmd::Completions { .. }) => unreachable!(),
    }
}

fn connect(store: &mut Store, host: &HostDto, tmux: Option<String>, _named: bool) -> Result<u8> {
    let tmux = tmux.or_else(|| (host.persistent == Some(true)).then(|| "myterm-1".to_string()));
    let code = block_on(term::interactive(store, host, tmux))?;
    let _ = store.save();
    sync_quietly(store);
    Ok(code.clamp(0, 255) as u8)
}

fn multi(hosts: &[HostDto]) -> Result<u8> {
    use std::process::Command;
    if Command::new("tmux").arg("-V").output().is_err() {
        bail!("`myterm multi` uses tmux on this machine: install it (sudo apt install tmux)");
    }
    let exe = std::env::current_exe()?.to_string_lossy().to_string();
    let cmd = |h: &HostDto| format!("'{exe}' ssh '{}'", h.label.replace('\'', ""));
    let inside = std::env::var("TMUX").is_ok();
    let session = format!("myterm-{}", std::process::id());
    let first = &hosts[0];
    let status = if inside {
        Command::new("tmux").args(["new-window", "-n", "myterm", &cmd(first)]).status()?
    } else {
        Command::new("tmux").args(["new-session", "-d", "-s", &session, "-n", "myterm", &cmd(first)]).status()?
    };
    if !status.success() {
        bail!("tmux couldn't open a window");
    }
    let target = if inside { "myterm".to_string() } else { format!("{session}:myterm") };
    for h in &hosts[1..] {
        Command::new("tmux").args(["split-window", "-t", &target, &cmd(h)]).status()?;
        Command::new("tmux").args(["select-layout", "-t", &target, "tiled"]).status()?;
    }
    Command::new("tmux").args(["set-window-option", "-t", &target, "synchronize-panes", "on"]).status()?;
    println!("Typing goes to all {} panes. Turn it off: Ctrl+b then :setw synchronize-panes off", hosts.len());
    if !inside {
        Command::new("tmux").args(["attach", "-t", &session]).status()?;
    }
    Ok(0)
}

fn trust_ca(store: &Store) -> Result<u8> {
    let ca = sync::ca_public_key(store)?;
    if !ca.starts_with("ssh-") || ca.contains('\'') || ca.contains('\n') {
        bail!("unexpected CA key from the server");
    }
    println!("Making this machine's sshd trust MyTerm certificates (CA: {}…).", &ca[..40.min(ca.len())]);
    let script = format!(
        "set -e\n\
         printf '%s\\n' '{ca}' > /etc/ssh/myterm_user_ca.pub\n\
         chmod 644 /etc/ssh/myterm_user_ca.pub\n\
         if [ -d /etc/ssh/sshd_config.d ] && grep -qiE '^Include /etc/ssh/sshd_config.d' /etc/ssh/sshd_config; then\n\
           echo 'TrustedUserCAKeys /etc/ssh/myterm_user_ca.pub' > /etc/ssh/sshd_config.d/myterm-ca.conf\n\
         elif ! grep -q '^TrustedUserCAKeys /etc/ssh/myterm_user_ca.pub' /etc/ssh/sshd_config; then\n\
           echo 'TrustedUserCAKeys /etc/ssh/myterm_user_ca.pub' >> /etc/ssh/sshd_config\n\
         fi\n\
         sshd -t\n\
         systemctl reload ssh 2>/dev/null || systemctl reload sshd\n"
    );
    let status = std::process::Command::new("sudo").args(["sh", "-c", &script]).status()?;
    if !status.success() {
        bail!("that didn't finish; nothing was reloaded if sshd -t failed");
    }
    println!("Done: sshd accepts MyTerm certificates for the login names the sync server signs for.");
    Ok(0)
}

fn fmt_time(unix: u64) -> String {
    if unix == 0 {
        return "never".into();
    }
    // Local time via libc, to avoid a date dependency.
    let t = unix as libc::time_t;
    let mut tm: libc::tm = unsafe { std::mem::zeroed() };
    unsafe { libc::localtime_r(&t, &mut tm) };
    format!("{:04}-{:02}-{:02} {:02}:{:02}", tm.tm_year + 1900, tm.tm_mon + 1, tm.tm_mday, tm.tm_hour, tm.tm_min)
}
