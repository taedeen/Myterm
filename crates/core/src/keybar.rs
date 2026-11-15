//! Key-bar profiles and shortcut encoding (spec 3.5 A).
//!
//! A key-bar key carries an action. Key sequences are written the way users
//! read them, e.g. `ctrl+B, %` (tmux split) or `esc, :wq, enter`, and are
//! encoded to the bytes sent down the SSH channel.

use std::fmt;

use crate::Id;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum KeyAction {
    /// A key sequence such as `ctrl+B, %`.
    Sequence(String),
    /// Literal text typed as-is (no newline unless included).
    Text(String),
    /// Run a saved snippet.
    Snippet(Id),
    /// Secure "Type password" (spec 3.5 C). `None` = host identity's password.
    TypePassword { credential_id: Option<Id> },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Key {
    pub label: String,
    pub action: KeyAction,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct KeybarProfile {
    pub id: Id,
    pub name: String,
    pub rows: Vec<Vec<Key>>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum KeyParseError {
    Empty,
    BadCtrl(String),
}

impl fmt::Display for KeyParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            KeyParseError::Empty => write!(f, "empty key sequence"),
            KeyParseError::BadCtrl(s) => write!(f, "unsupported ctrl combination: {s}"),
        }
    }
}

impl std::error::Error for KeyParseError {}

/// Encodes one element of a sequence (between commas).
fn encode_element(el: &str, out: &mut Vec<u8>) -> Result<(), KeyParseError> {
    let lower = el.to_ascii_lowercase();
    if let Some(rest) = lower.strip_prefix("ctrl+") {
        // ctrl+<char>: ASCII control code. ctrl+space / ctrl+@ = NUL.
        let ch = match rest {
            "space" => '@',
            _ if rest.chars().count() == 1 => rest.chars().next().unwrap(),
            _ => return Err(KeyParseError::BadCtrl(el.to_string())),
        };
        let upper = ch.to_ascii_uppercase();
        if !('@'..='_').contains(&upper) && upper != '?' {
            return Err(KeyParseError::BadCtrl(el.to_string()));
        }
        out.push(if upper == '?' { 0x7f } else { (upper as u8) & 0x1f });
        return Ok(());
    }
    if let Some(rest) = lower.strip_prefix("alt+") {
        // Meta: ESC prefix, then the key itself.
        out.push(0x1b);
        return encode_element(&el[el.len() - rest.len()..], out);
    }
    let named: Option<&[u8]> = match lower.as_str() {
        "esc" | "escape" => Some(b"\x1b"),
        "tab" => Some(b"\t"),
        "enter" | "return" => Some(b"\r"),
        "backspace" => Some(b"\x7f"),
        "up" | "\u{2191}" => Some(b"\x1b[A"),
        "down" | "\u{2193}" => Some(b"\x1b[B"),
        "right" | "\u{2192}" => Some(b"\x1b[C"),
        "left" | "\u{2190}" => Some(b"\x1b[D"),
        "home" => Some(b"\x1b[H"),
        "end" => Some(b"\x1b[F"),
        "pgup" => Some(b"\x1b[5~"),
        "pgdn" => Some(b"\x1b[6~"),
        "del" | "delete" => Some(b"\x1b[3~"),
        _ => None,
    };
    match named {
        Some(bytes) => out.extend_from_slice(bytes),
        None => out.extend_from_slice(el.as_bytes()),
    }
    Ok(())
}

/// Encodes a human-readable sequence like `ctrl+B, "` into bytes.
///
/// Elements are separated by `", "` (comma + space), so a bare `,` is still
/// usable as a literal key.
pub fn encode_sequence(seq: &str) -> Result<Vec<u8>, KeyParseError> {
    if seq.is_empty() {
        return Err(KeyParseError::Empty);
    }
    let mut out = Vec::new();
    for el in seq.split(", ") {
        if el.is_empty() {
            return Err(KeyParseError::Empty);
        }
        encode_element(el, &mut out)?;
    }
    Ok(out)
}

fn seq(label: &str, s: &str) -> Key {
    Key { label: label.into(), action: KeyAction::Sequence(s.into()) }
}

/// Built-in profiles shipped with the app; users can copy and edit them.
pub fn builtin_profiles() -> Vec<KeybarProfile> {
    let password = Key { label: "\u{1F511}".into(), action: KeyAction::TypePassword { credential_id: None } };
    let basic_row = vec![
        password.clone(),
        seq("ctrl", "ctrl+C"),
        seq("esc", "esc"),
        seq("tab", "tab"),
        seq("\u{2191}", "up"),
        seq("\u{2193}", "down"),
        seq("\u{2190}", "left"),
        seq("\u{2192}", "right"),
        seq("|", "|"),
        seq("~", "~"),
        seq("/", "/"),
        seq("-", "-"),
    ];
    vec![
        KeybarProfile { id: "builtin-basic".into(), name: "Basic".into(), rows: vec![basic_row.clone()] },
        KeybarProfile {
            id: "builtin-tmux".into(),
            name: "tmux".into(),
            rows: vec![
                vec![
                    seq("\u{2191}", "up"),
                    seq("\u{2193}", "down"),
                    seq("\u{2192}", "right"),
                    seq("ctrl+B, %", "ctrl+B, %"),
                    seq("ctrl+B, \"", "ctrl+B, \""),
                    seq("ctrl+B, O", "ctrl+B, o"),
                    seq("ctrl+B, C", "ctrl+B, c"),
                    seq("ctrl+B, N", "ctrl+B, n"),
                    seq("ctrl+B, D", "ctrl+B, d"),
                ],
                basic_row.clone(),
            ],
        },
        KeybarProfile {
            id: "builtin-vim".into(),
            name: "vim".into(),
            rows: vec![
                vec![
                    seq("esc", "esc"),
                    seq(":w", "esc, :w, enter"),
                    seq(":wq", "esc, :wq, enter"),
                    seq(":q!", "esc, :q!, enter"),
                    seq("/", "/"),
                    seq("u", "esc, u"),
                    seq("dd", "esc, dd"),
                ],
                basic_row,
            ],
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tmux_shortcuts() {
        assert_eq!(encode_sequence("ctrl+B, %").unwrap(), b"\x02%");
        assert_eq!(encode_sequence("ctrl+B, \"").unwrap(), b"\x02\"");
        assert_eq!(encode_sequence("ctrl+b, o").unwrap(), b"\x02o", "case of the key itself is kept");
    }

    #[test]
    fn named_keys_and_text() {
        assert_eq!(encode_sequence("esc, :wq, enter").unwrap(), b"\x1b:wq\r");
        assert_eq!(encode_sequence("up").unwrap(), b"\x1b[A");
        assert_eq!(encode_sequence("ctrl+C").unwrap(), b"\x03");
        assert_eq!(encode_sequence("ctrl+space").unwrap(), b"\x00");
        assert_eq!(encode_sequence("ctrl+?").unwrap(), b"\x7f");
        assert_eq!(encode_sequence("alt+f").unwrap(), b"\x1bf");
        assert_eq!(encode_sequence(",").unwrap(), b",", "bare comma is a literal key");
    }

    #[test]
    fn rejects_bad_input() {
        assert_eq!(encode_sequence(""), Err(KeyParseError::Empty));
        assert!(matches!(encode_sequence("ctrl+F1"), Err(KeyParseError::BadCtrl(_))));
        assert!(matches!(encode_sequence("ctrl+1"), Err(KeyParseError::BadCtrl(_))));
    }

    #[test]
    fn builtin_profiles_all_encode() {
        for p in builtin_profiles() {
            for row in &p.rows {
                for key in row {
                    if let KeyAction::Sequence(s) = &key.action {
                        encode_sequence(s).unwrap_or_else(|e| panic!("{} / {}: {e}", p.name, key.label));
                    }
                }
            }
        }
        let basic = &builtin_profiles()[0];
        assert!(matches!(basic.rows[0][0].action, KeyAction::TypePassword { credential_id: None }));
    }
}
