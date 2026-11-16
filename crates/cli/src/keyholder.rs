//! `myterm unlock`: a small background process that holds the unlocked vault
//! key for a while, so each command doesn't ask for the master password.
//!
//! - It holds 32 bytes, locked in memory (`mlock`) and not dumpable.
//! - It answers on a socket in `$XDG_RUNTIME_DIR/myterm` (directory 0700,
//!   socket 0600), only to processes of the same user (`SO_PEERCRED`).
//! - It exits after the idle time (default 15 minutes since last use), or on
//!   `myterm lock`.

use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::{UnixListener, UnixStream};
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};

fn runtime_dir() -> PathBuf {
    let base = std::env::var("XDG_RUNTIME_DIR")
        .ok()
        .filter(|s| !s.is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(format!("/tmp/myterm-{}", unsafe { libc::getuid() })));
    base.join("myterm")
}

fn socket_path(device_id: &str) -> PathBuf {
    runtime_dir().join(format!("key-{}.sock", &device_id[..8.min(device_id.len())]))
}

fn connect(device_id: &str) -> Option<UnixStream> {
    let s = UnixStream::connect(socket_path(device_id)).ok()?;
    s.set_read_timeout(Some(Duration::from_secs(3))).ok()?;
    Some(s)
}

/// The vault key, if the key holder is running.
pub fn fetch(device_id: &str) -> Option<Vec<u8>> {
    let mut s = connect(device_id)?;
    s.write_all(b"GET\n").ok()?;
    let mut key = vec![0u8; 32];
    s.read_exact(&mut key).ok()?;
    Some(key)
}

/// Seconds until the key holder locks, if it is running.
pub fn status(device_id: &str) -> Option<u64> {
    let mut s = connect(device_id)?;
    s.write_all(b"PING\n").ok()?;
    let mut buf = String::new();
    s.read_to_string(&mut buf).ok()?;
    buf.trim().parse().ok()
}

pub fn lock(device_id: &str) -> bool {
    match connect(device_id) {
        Some(mut s) => s.write_all(b"LOCK\n").is_ok(),
        None => false,
    }
}

/// Starts the key holder with `key` for `idle` seconds of inactivity.
pub fn start(device_id: &str, key: &[u8], idle: u64) -> Result<()> {
    lock(device_id);
    let dir = runtime_dir();
    std::fs::create_dir_all(&dir)?;
    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
    let exe = std::env::current_exe()?;
    let mut child = Command::new(exe)
        .arg("__keyholder")
        .arg(device_id)
        .arg(idle.to_string())
        .stdin(Stdio::piped())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .context("starting the key holder")?;
    child.stdin.take().context("no stdin")?.write_all(key)?;
    // Wait until it answers, so the next command finds it.
    for _ in 0..50 {
        if status(device_id).is_some() {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(50));
    }
    bail!("the key holder didn't start")
}

fn peer_uid(s: &UnixStream) -> Option<u32> {
    let mut cred = libc::ucred { pid: 0, uid: 0, gid: 0 };
    let mut len = std::mem::size_of::<libc::ucred>() as libc::socklen_t;
    let r = unsafe {
        libc::getsockopt(
            s.as_raw_fd(),
            libc::SOL_SOCKET,
            libc::SO_PEERCRED,
            &mut cred as *mut libc::ucred as *mut libc::c_void,
            &mut len,
        )
    };
    (r == 0).then_some(cred.uid)
}

/// The key holder process itself (`myterm __keyholder <device> <idle>`).
pub fn serve(device_id: &str, idle: u64) -> Result<()> {
    unsafe {
        libc::setsid();
        libc::prctl(libc::PR_SET_DUMPABLE, 0, 0, 0, 0);
    }
    let mut key = Box::new([0u8; 32]);
    std::io::stdin().read_exact(&mut key[..])?;
    unsafe {
        libc::mlock(key.as_ptr() as *const libc::c_void, 32);
    }
    let path = socket_path(device_id);
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path)?;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    listener.set_nonblocking(true)?;
    let me = unsafe { libc::getuid() };
    let idle = Duration::from_secs(idle);
    let mut deadline = Instant::now() + idle;
    loop {
        if Instant::now() >= deadline {
            break;
        }
        match listener.accept() {
            Ok((mut s, _)) => {
                if peer_uid(&s) != Some(me) {
                    continue;
                }
                let _ = s.set_nonblocking(false);
                let _ = s.set_read_timeout(Some(Duration::from_secs(2)));
                let mut cmd = [0u8; 5];
                let n = s.read(&mut cmd).unwrap_or(0);
                match &cmd[..n] {
                    b"GET\n" => {
                        deadline = Instant::now() + idle;
                        let _ = s.write_all(&key[..]);
                    }
                    b"PING\n" => {
                        let left = deadline.saturating_duration_since(Instant::now()).as_secs();
                        let _ = s.write_all(left.to_string().as_bytes());
                    }
                    b"LOCK\n" => break,
                    _ => {}
                }
            }
            Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => std::thread::sleep(Duration::from_millis(100)),
            Err(_) => std::thread::sleep(Duration::from_millis(100)),
        }
    }
    for b in key.iter_mut() {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    let _ = std::fs::remove_file(&path);
    Ok(())
}
