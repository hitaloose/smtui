//! Translation of crossterm key and mouse events into the bytes a terminal program expects.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind};
use vt100::{MouseProtocolEncoding, MouseProtocolMode};

/// xterm modifier parameter: 1 + shift(1) + alt(2) + ctrl(4).
fn modifier_param(mods: KeyModifiers) -> u8 {
    let mut p = 1;
    if mods.contains(KeyModifiers::SHIFT) {
        p += 1;
    }
    if mods.contains(KeyModifiers::ALT) {
        p += 2;
    }
    if mods.contains(KeyModifiers::CONTROL) {
        p += 4;
    }
    p
}

pub fn encode_key(key: &KeyEvent, application_cursor: bool) -> Vec<u8> {
    let mods = key.modifiers;
    let alt = mods.contains(KeyModifiers::ALT);
    let ctrl = mods.contains(KeyModifiers::CONTROL);
    let param = modifier_param(mods);

    // CSI sequences for cursor-like keys: `ESC [ 1 ; m X` with modifiers, else `ESC [ X` / `ESC O X`.
    let cursor_key = |c: char| -> Vec<u8> {
        if param > 1 {
            format!("\x1b[1;{param}{c}").into_bytes()
        } else if application_cursor {
            format!("\x1bO{c}").into_bytes()
        } else {
            format!("\x1b[{c}").into_bytes()
        }
    };
    let tilde = |n: u8| -> Vec<u8> {
        if param > 1 { format!("\x1b[{n};{param}~").into_bytes() } else { format!("\x1b[{n}~").into_bytes() }
    };

    let mut out = match key.code {
        KeyCode::Char(c) if ctrl => match control_byte(c) {
            Some(b) => vec![b],
            None => c.to_string().into_bytes(),
        },
        KeyCode::Char(c) => c.to_string().into_bytes(),
        KeyCode::Enter => vec![b'\r'],
        KeyCode::Tab => vec![b'\t'],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Backspace => vec![if ctrl { 0x08 } else { 0x7f }],
        KeyCode::Esc => vec![0x1b],
        KeyCode::Up => cursor_key('A'),
        KeyCode::Down => cursor_key('B'),
        KeyCode::Right => cursor_key('C'),
        KeyCode::Left => cursor_key('D'),
        KeyCode::Home => cursor_key('H'),
        KeyCode::End => cursor_key('F'),
        KeyCode::Insert => tilde(2),
        KeyCode::Delete => tilde(3),
        KeyCode::PageUp => tilde(5),
        KeyCode::PageDown => tilde(6),
        KeyCode::F(n @ 1..=4) => {
            let c = (b'P' + n - 1) as char;
            if param > 1 { format!("\x1b[1;{param}{c}").into_bytes() } else { format!("\x1bO{c}").into_bytes() }
        }
        KeyCode::F(n @ 5..=12) => tilde([15, 17, 18, 19, 20, 21, 23, 24][(n - 5) as usize]),
        _ => return Vec::new(),
    };
    // Alt prefixes plain characters with ESC (modified CSI keys already carry it).
    if alt && matches!(key.code, KeyCode::Char(_) | KeyCode::Enter | KeyCode::Backspace | KeyCode::Esc) {
        out.insert(0, 0x1b);
    }
    out
}

fn control_byte(c: char) -> Option<u8> {
    match c {
        'a'..='z' => Some(c as u8 - b'a' + 1),
        'A'..='Z' => Some(c as u8 - b'A' + 1),
        ' ' | '@' | '2' => Some(0),
        '[' | '3' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '-' | '7' => Some(0x1f),
        '8' | '?' => Some(0x7f),
        _ => None,
    }
}

/// Encodes a mouse event for the program, honoring the protocol it enabled.
/// `x`, `y` are 0-based cell coordinates inside the terminal area.
pub fn encode_mouse(
    ev: &MouseEvent,
    x: u16,
    y: u16,
    mode: MouseProtocolMode,
    encoding: MouseProtocolEncoding,
) -> Option<Vec<u8>> {
    let button_code = |b: MouseButton| match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    };
    let (mut code, release) = match ev.kind {
        MouseEventKind::Down(b) => (button_code(b), false),
        MouseEventKind::Up(b) => {
            if mode == MouseProtocolMode::Press {
                return None;
            }
            (button_code(b), true)
        }
        MouseEventKind::Drag(b) => {
            if !matches!(mode, MouseProtocolMode::ButtonMotion | MouseProtocolMode::AnyMotion) {
                return None;
            }
            (button_code(b) + 32, false)
        }
        MouseEventKind::Moved => {
            if mode != MouseProtocolMode::AnyMotion {
                return None;
            }
            (3 + 32, false)
        }
        MouseEventKind::ScrollUp => (64, false),
        MouseEventKind::ScrollDown => (65, false),
        MouseEventKind::ScrollLeft => (66, false),
        MouseEventKind::ScrollRight => (67, false),
    };
    if mode == MouseProtocolMode::None {
        return None;
    }
    if ev.modifiers.contains(KeyModifiers::SHIFT) {
        code += 4;
    }
    if ev.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if ev.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    let (cx, cy) = (x as u32 + 1, y as u32 + 1);
    match encoding {
        MouseProtocolEncoding::Sgr => {
            let end = if release { 'm' } else { 'M' };
            Some(format!("\x1b[<{code};{cx};{cy}{end}").into_bytes())
        }
        MouseProtocolEncoding::Default | MouseProtocolEncoding::Utf8 => {
            // Legacy encoding has no release button: release is reported as button 3.
            let code = if release { 3 + (code & !3) } else { code };
            let enc = |v: u32| -> Vec<u8> {
                let v = v + 32;
                if encoding == MouseProtocolEncoding::Utf8 {
                    char::from_u32(v).map(|c| c.to_string().into_bytes()).unwrap_or_default()
                } else {
                    vec![v.min(255) as u8]
                }
            };
            let mut out = b"\x1b[M".to_vec();
            out.extend(enc(code as u32));
            out.extend(enc(cx));
            out.extend(enc(cy));
            Some(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn k(code: KeyCode, mods: KeyModifiers) -> Vec<u8> {
        encode_key(&KeyEvent::new(code, mods), false)
    }

    #[test]
    fn encodes_keys() {
        assert_eq!(k(KeyCode::Char('c'), KeyModifiers::CONTROL), vec![3]);
        assert_eq!(k(KeyCode::Char('x'), KeyModifiers::ALT), b"\x1bx");
        assert_eq!(k(KeyCode::Up, KeyModifiers::NONE), b"\x1b[A");
        assert_eq!(encode_key(&KeyEvent::new(KeyCode::Up, KeyModifiers::NONE), true), b"\x1bOA");
        assert_eq!(k(KeyCode::Right, KeyModifiers::CONTROL), b"\x1b[1;5C");
        assert_eq!(k(KeyCode::F(5), KeyModifiers::NONE), b"\x1b[15~");
        assert_eq!(k(KeyCode::Backspace, KeyModifiers::NONE), vec![0x7f]);
    }

    #[test]
    fn encodes_sgr_mouse() {
        let ev = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 0,
            row: 0,
            modifiers: KeyModifiers::NONE,
        };
        let out = encode_mouse(&ev, 4, 2, MouseProtocolMode::PressRelease, MouseProtocolEncoding::Sgr);
        assert_eq!(out.unwrap(), b"\x1b[<0;5;3M");
        assert!(encode_mouse(&ev, 4, 2, MouseProtocolMode::None, MouseProtocolEncoding::Sgr).is_none());
    }
}
