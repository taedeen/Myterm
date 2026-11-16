//! Questions on the terminal (`/dev/tty`, so they work with pipes too).

use std::io::{BufRead, BufReader, Write};

use anyhow::{Context, Result};

/// Reads a secret with echo off. Never from arguments or the environment.
pub fn secret(prompt: &str) -> Result<String> {
    rpassword::prompt_password(prompt).context("reading from the terminal")
}

/// Asks a question; Enter gives `default`.
pub fn ask(prompt: &str, default: &str) -> Result<String> {
    let tty = std::fs::OpenOptions::new().read(true).write(true).open("/dev/tty").context("no terminal to ask on")?;
    let mut out = tty.try_clone()?;
    if default.is_empty() {
        write!(out, "{prompt}: ")?;
    } else {
        write!(out, "{prompt} [{default}]: ")?;
    }
    out.flush()?;
    let mut line = String::new();
    BufReader::new(tty).read_line(&mut line)?;
    let line = line.trim();
    Ok(if line.is_empty() { default.to_string() } else { line.to_string() })
}

pub fn yes(prompt: &str, default_yes: bool) -> Result<bool> {
    let a = ask(&format!("{prompt} {}", if default_yes { "(Y/n)" } else { "(y/N)" }), "")?;
    Ok(match a.to_lowercase().as_str() {
        "" => default_yes,
        "y" | "yes" => true,
        _ => false,
    })
}
