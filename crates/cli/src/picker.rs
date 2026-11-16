//! The host picker shown by a bare `myterm`: type to filter, arrows to move,
//! Enter to connect, Esc to quit.

use std::io::Write;

use anyhow::Result;
use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::{cursor, execute, queue, style, terminal};
use myterm_ffi::api::HostDto;

pub fn describe(h: &HostDto) -> String {
    let user = if h.username.is_empty() { String::new() } else { format!("{}@", h.username) };
    let port = h.port.filter(|p| *p != 22).map(|p| format!(":{p}")).unwrap_or_default();
    format!("{user}{}{port}", h.address)
}

fn matches(h: &HostDto, filter: &str) -> bool {
    let f = filter.to_lowercase();
    f.split_whitespace().all(|w| {
        h.label.to_lowercase().contains(w) || h.address.to_lowercase().contains(w) || h.tags.iter().any(|t| t.to_lowercase().contains(w))
    })
}

/// Returns the chosen host, or None if cancelled.
pub fn pick(hosts: &[HostDto]) -> Result<Option<HostDto>> {
    let mut filter = String::new();
    let mut sel = 0usize;
    let mut out = std::io::stderr();
    terminal::enable_raw_mode()?;
    execute!(out, terminal::EnterAlternateScreen, cursor::Hide)?;
    let result = (|| -> Result<Option<HostDto>> {
        loop {
            let shown: Vec<&HostDto> = hosts.iter().filter(|h| matches(h, &filter)).collect();
            sel = sel.min(shown.len().saturating_sub(1));
            let (cols, rows) = terminal::size().unwrap_or((80, 24));
            queue!(out, cursor::MoveTo(0, 0), terminal::Clear(terminal::ClearType::All))?;
            queue!(out, style::Print(format!("MyTerm: type to filter, Enter connects, Esc quits   {}", filter)))?;
            for (i, h) in shown.iter().take(rows.saturating_sub(2) as usize).enumerate() {
                let mark = if i == sel { "›" } else { " " };
                let tags = if h.tags.is_empty() { String::new() } else { format!("  [{}]", h.tags.join(", ")) };
                let line: String =
                    format!("{mark} {:<24} {}{tags}", h.label, describe(h)).chars().take(cols as usize).collect();
                queue!(out, cursor::MoveTo(0, i as u16 + 2))?;
                if i == sel {
                    queue!(out, style::SetAttribute(style::Attribute::Reverse), style::Print(line), style::SetAttribute(style::Attribute::Reset))?;
                } else {
                    queue!(out, style::Print(line))?;
                }
            }
            if shown.is_empty() {
                queue!(out, cursor::MoveTo(0, 2), style::Print("  No hosts match."))?;
            }
            out.flush()?;
            if let Event::Key(k) = event::read()? {
                if k.kind != KeyEventKind::Press {
                    continue;
                }
                match k.code {
                    KeyCode::Esc => return Ok(None),
                    KeyCode::Char('c') if k.modifiers.contains(KeyModifiers::CONTROL) => return Ok(None),
                    KeyCode::Enter => return Ok(shown.get(sel).map(|h| (*h).clone_dto())),
                    KeyCode::Up => sel = sel.saturating_sub(1),
                    KeyCode::Down => sel = (sel + 1).min(shown.len().saturating_sub(1)),
                    KeyCode::Backspace => {
                        filter.pop();
                    }
                    KeyCode::Char(c) => {
                        filter.push(c);
                        sel = 0;
                    }
                    _ => {}
                }
            }
        }
    })();
    let _ = execute!(out, cursor::Show, terminal::LeaveAlternateScreen);
    let _ = terminal::disable_raw_mode();
    result
}

/// HostDto has no Clone (it crosses into Dart); copy it field by field.
pub trait CloneDto {
    fn clone_dto(&self) -> HostDto;
}

impl CloneDto for HostDto {
    fn clone_dto(&self) -> HostDto {
        HostDto {
            id: self.id.clone(),
            label: self.label.clone(),
            address: self.address.clone(),
            port: self.port,
            username: self.username.clone(),
            group_id: self.group_id.clone(),
            credential_id: self.credential_id.clone(),
            hardware_key_id: self.hardware_key_id.clone(),
            tags: self.tags.clone(),
            os: self.os.clone(),
            keybar_profile_id: self.keybar_profile_id.clone(),
            hardware_keys_only: self.hardware_keys_only,
            sync_history: self.sync_history,
            jump_chain: self.jump_chain.clone(),
            use_certificate: self.use_certificate,
            persistent: self.persistent,
        }
    }
}
