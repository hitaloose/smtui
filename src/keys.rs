//! Parsing and matching of configurable key chords such as `ctrl-]`.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyChord {
    code: KeyCode,
    mods: KeyModifiers,
}

impl KeyChord {
    pub fn parse(spec: &str) -> Option<Self> {
        let spec = spec.trim().to_ascii_lowercase();
        // The key itself may be `-`, so split off modifiers from the left.
        let mut mods = KeyModifiers::NONE;
        let mut rest = spec.as_str();
        loop {
            let (m, tail) = match rest.split_once('-') {
                Some((m, tail)) if !tail.is_empty() => (m, tail),
                _ => break,
            };
            match m {
                "ctrl" | "c" => mods |= KeyModifiers::CONTROL,
                "alt" | "a" | "meta" | "m" => mods |= KeyModifiers::ALT,
                "shift" | "s" => mods |= KeyModifiers::SHIFT,
                _ => return None,
            }
            rest = tail;
        }
        let code = match rest {
            "space" => KeyCode::Char(' '),
            "esc" => KeyCode::Esc,
            "tab" => KeyCode::Tab,
            k if k.chars().count() == 1 => KeyCode::Char(k.chars().next()?),
            k if k.starts_with('f') => KeyCode::F(k[1..].parse().ok()?),
            _ => return None,
        };
        Some(Self { code, mods })
    }

    pub fn matches(&self, key: &KeyEvent) -> bool {
        let mods = key.modifiers - KeyModifiers::SHIFT;
        let want = self.mods - KeyModifiers::SHIFT;
        if mods != want {
            return false;
        }
        match (self.code, key.code) {
            (KeyCode::Char(a), KeyCode::Char(b)) => {
                a.eq_ignore_ascii_case(&b)
                    || (want.contains(KeyModifiers::CONTROL) && control_alias(a) == Some(b))
            }
            (a, b) => a == b,
        }
    }
}

/// Human-readable form of a chord spec: `ctrl-]` → `Ctrl-]`.
pub fn display(spec: &str) -> String {
    let spec = spec.trim();
    let mut parts: Vec<String> = Vec::new();
    let mut rest = spec;
    while let Some((m, tail)) = rest.split_once('-').filter(|(_, t)| !t.is_empty()) {
        parts.push(match m.to_ascii_lowercase().as_str() {
            "ctrl" | "c" => "Ctrl".into(),
            "alt" | "a" | "meta" | "m" => "Alt".into(),
            "shift" | "s" => "Shift".into(),
            other => other.to_string(),
        });
        rest = tail;
    }
    parts.push(if rest.len() > 1 { format!("{}{}", rest[..1].to_uppercase(), &rest[1..]) } else { rest.to_string() });
    parts.join("-")
}

/// Legacy terminals send Ctrl+\ ] ^ _ as 0x1c–0x1f, which crossterm reports as Ctrl+4–7.
fn control_alias(c: char) -> Option<char> {
    match c {
        '\\' => Some('4'),
        ']' => Some('5'),
        '^' => Some('6'),
        '_' => Some('7'),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    #[test]
    fn ctrl_bracket_matches_legacy_alias() {
        let k = KeyChord::parse("ctrl-]").unwrap();
        assert!(k.matches(&key(KeyCode::Char(']'), KeyModifiers::CONTROL)));
        assert!(k.matches(&key(KeyCode::Char('5'), KeyModifiers::CONTROL)));
        assert!(!k.matches(&key(KeyCode::Char(']'), KeyModifiers::NONE)));
    }

    #[test]
    fn displays_chords() {
        assert_eq!(display("ctrl-]"), "Ctrl-]");
        assert_eq!(display("alt-space"), "Alt-Space");
        assert_eq!(display("ctrl--"), "Ctrl--");
    }

    #[test]
    fn parses_named_keys() {
        assert!(KeyChord::parse("ctrl-space").is_some());
        assert!(KeyChord::parse("alt-f2").is_some());
        assert!(KeyChord::parse("ctrl--").is_some());
        assert!(KeyChord::parse("hyper-x").is_none());
    }
}
