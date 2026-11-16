//! `myterm run`: one command on many hosts at once, output grouped per host.

use std::sync::Arc;
use std::time::Duration;

use myterm_ffi::api::{HostDto, SessionEventDto};
use tokio::sync::{mpsc, Semaphore};

use crate::store::Store;
use crate::term::{auth_for, prepare_certificate, TtyPrompt};

pub struct Outcome {
    pub host: String,
    pub code: Option<u32>,
    pub output: String,
    pub error: Option<String>,
}

/// Runs `command` on each host, at most `parallel` at a time.
pub async fn run(store: Arc<Store>, hosts: Vec<HostDto>, command: String, parallel: usize, timeout: u64) -> Vec<Outcome> {
    for h in &hosts {
        prepare_certificate(&store, h);
    }
    let sem = Arc::new(Semaphore::new(parallel.max(1)));
    let mut tasks = Vec::new();
    for h in hosts {
        let (store, sem, command) = (store.clone(), sem.clone(), command.clone());
        tasks.push(tokio::spawn(async move {
            let _permit = sem.acquire().await;
            one(&store, &h, command, timeout).await
        }));
    }
    let mut out = Vec::new();
    for t in tasks {
        if let Ok(o) = t.await {
            out.push(o);
        }
    }
    out
}

async fn one(store: &Store, h: &HostDto, command: String, timeout: u64) -> Outcome {
    let name = if h.label.is_empty() { h.address.clone() } else { h.label.clone() };
    let mut o = Outcome { host: name, code: None, output: String::new(), error: None };
    let auth = match auth_for(store, h, false) {
        Ok(Some(a)) => a,
        Ok(None) => {
            o.error = Some("no stored password, key or certificate: use `myterm ssh` for this host".into());
            return o;
        }
        Err(e) => {
            o.error = Some(e.to_string());
            return o;
        }
    };
    let (tx, mut rx) = mpsc::unbounded_channel::<SessionEventDto>();
    let events: myterm_ffi::native::EventFn = Arc::new(move |e| {
        let _ = tx.send(e);
    });
    if let Err(e) = store
        .engine
        .connect_native(
            h.id.clone(),
            200,
            50,
            auth,
            Some(command),
            events,
            Arc::new(TtyPrompt { interactive: false }),
            myterm_ffi::native::no_hardware_signer(),
        )
        .await
    {
        o.error = Some(e.to_string());
        return o;
    }
    let mut bytes = Vec::new();
    let mut session = None;
    let deadline = tokio::time::sleep(Duration::from_secs(timeout));
    tokio::pin!(deadline);
    loop {
        tokio::select! {
            ev = rx.recv() => match ev {
                Some(SessionEventDto::Started(id)) => session = Some(id),
                Some(SessionEventDto::Output(b)) => bytes.extend_from_slice(&b),
                Some(SessionEventDto::Exit(c)) => o.code = Some(c.unwrap_or(0)),
                Some(SessionEventDto::Error(e)) => { o.error = Some(e); break; }
                Some(SessionEventDto::Closed) | None => break,
            },
            _ = &mut deadline => {
                o.error = Some(format!("timed out after {timeout}s"));
                if let Some(id) = session { store.engine.disconnect(id); }
                break;
            }
        }
    }
    o.output = String::from_utf8_lossy(&bytes).replace("\r\n", "\n");
    o
}

/// Prints outcomes, one block per host. Returns how many failed.
pub fn print(outcomes: &[Outcome], prefix: bool) -> usize {
    let mut failed = 0;
    for o in outcomes {
        let ok = o.error.is_none() && o.code == Some(0);
        if !ok {
            failed += 1;
        }
        let status = match (&o.error, o.code) {
            (Some(e), _) => format!("error: {e}"),
            (None, Some(c)) => format!("exit {c}"),
            (None, None) => "no exit status".into(),
        };
        if prefix {
            for line in o.output.lines() {
                println!("{}: {line}", o.host);
            }
            println!("{}: [{status}]", o.host);
        } else {
            println!("── {} ({status}) ──", o.host);
            print!("{}", o.output);
            if !o.output.is_empty() && !o.output.ends_with('\n') {
                println!();
            }
        }
    }
    failed
}
