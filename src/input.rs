//! Translates winit key events into the byte sequences a pty expects.

use winit::keyboard::{Key, ModifiersState, NamedKey};

/// xterm-style modifier parameter: 1 + bitmask of shift/alt/ctrl.
fn modifier_param(mods: ModifiersState) -> u8 {
    1 + u8::from(mods.shift_key())
        + 2 * u8::from(mods.alt_key())
        + 4 * u8::from(mods.control_key())
}

/// Builds `CSI <param> <suffix>`, inserting the modifier parameter only when a
/// modifier is actually held, since bare `CSI A` is what most programs expect.
fn csi(mods: ModifiersState, base: &str, suffix: char) -> Vec<u8> {
    let m = modifier_param(mods);
    if m == 1 {
        format!("\x1b[{base}{suffix}").into_bytes()
    } else {
        format!("\x1b[{base};{m}{suffix}").into_bytes()
    }
}

/// Arrow and Home/End keys switch between CSI and SS3 form depending on
/// whether the application has requested cursor-key application mode.
fn cursor_key(mods: ModifiersState, app_cursor: bool, suffix: char) -> Vec<u8> {
    let m = modifier_param(mods);
    if m == 1 {
        if app_cursor {
            format!("\x1bO{suffix}").into_bytes()
        } else {
            format!("\x1b[{suffix}").into_bytes()
        }
    } else {
        format!("\x1b[1;{m}{suffix}").into_bytes()
    }
}

fn tilde(mods: ModifiersState, number: u8) -> Vec<u8> {
    let m = modifier_param(mods);
    if m == 1 {
        format!("\x1b[{number}~").into_bytes()
    } else {
        format!("\x1b[{number};{m}~").into_bytes()
    }
}

/// Maps a printable character plus modifiers to the bytes a pty expects.
fn character(c: char, mods: ModifiersState) -> Option<Vec<u8>> {
    if mods.control_key() {
        // C0 control codes. Only the documented range maps cleanly.
        let code = match c {
            ' ' | '@' => Some(0x00),
            'a'..='z' => Some(c as u8 - b'a' + 1),
            'A'..='Z' => Some(c as u8 - b'A' + 1),
            '[' => Some(0x1b),
            '\\' => Some(0x1c),
            ']' => Some(0x1d),
            '^' => Some(0x1e),
            '_' | '/' | '-' => Some(0x1f),
            '?' => Some(0x7f),
            _ => None,
        };
        if let Some(code) = code {
            let mut out = Vec::with_capacity(2);
            if mods.alt_key() {
                out.push(0x1b);
            }
            out.push(code);
            return Some(out);
        }
        return None;
    }

    let mut out = Vec::with_capacity(5);
    if mods.alt_key() {
        // Alt is sent as an ESC prefix rather than by setting the high bit.
        out.push(0x1b);
    }
    let mut buf = [0u8; 4];
    out.extend_from_slice(c.encode_utf8(&mut buf).as_bytes());
    Some(out)
}

/// Returns the bytes to write to the pty for a key press, or `None` when the
/// key has no pty representation (modifiers on their own, unhandled keys).
///
/// `text` is winit's pre-composed text for the event, which is what makes dead
/// keys and IME-composed input work; it is preferred over the logical key
/// whenever no control/alt modifier is in play.
pub fn encode(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    app_cursor: bool,
) -> Option<Vec<u8>> {
    match key {
        Key::Named(named) => Some(match named {
            NamedKey::Enter => {
                if mods.alt_key() || mods.shift_key() {
                    b"\x1b\r".to_vec()
                } else {
                    b"\r".to_vec()
                }
            },
            NamedKey::Backspace => {
                if mods.super_key() {
                    b"\x15".to_vec() // Cmd+Backspace line rubout (Ctrl+U)
                } else if mods.control_key() {
                    b"\x17".to_vec() // Ctrl+Backspace word rubout (Ctrl+W)
                } else if mods.alt_key() {
                    b"\x1b\x7f".to_vec()
                } else {
                    b"\x7f".to_vec()
                }
            },
            NamedKey::Tab => {
                if mods.shift_key() {
                    b"\x1b[Z".to_vec()
                } else {
                    b"\t".to_vec()
                }
            },
            NamedKey::Escape => b"\x1b".to_vec(),
            NamedKey::ArrowUp => cursor_key(mods, app_cursor, 'A'),
            NamedKey::ArrowDown => cursor_key(mods, app_cursor, 'B'),
            NamedKey::ArrowRight => {
                if mods.super_key() {
                    b"\x05".to_vec() // Cmd+Right: end of line (Ctrl+E)
                } else {
                    cursor_key(mods, app_cursor, 'C')
                }
            },
            NamedKey::ArrowLeft => {
                if mods.super_key() {
                    b"\x01".to_vec() // Cmd+Left: start of line (Ctrl+A)
                } else {
                    cursor_key(mods, app_cursor, 'D')
                }
            },
            NamedKey::Home => cursor_key(mods, app_cursor, 'H'),
            NamedKey::End => cursor_key(mods, app_cursor, 'F'),
            NamedKey::Insert => tilde(mods, 2),
            NamedKey::Delete => tilde(mods, 3),
            NamedKey::PageUp => tilde(mods, 5),
            NamedKey::PageDown => tilde(mods, 6),
            NamedKey::F1 => csi(mods, "1", 'P'),
            NamedKey::F2 => csi(mods, "1", 'Q'),
            NamedKey::F3 => csi(mods, "1", 'R'),
            NamedKey::F4 => csi(mods, "1", 'S'),
            NamedKey::F5 => tilde(mods, 15),
            NamedKey::F6 => tilde(mods, 17),
            NamedKey::F7 => tilde(mods, 18),
            NamedKey::F8 => tilde(mods, 19),
            NamedKey::F9 => tilde(mods, 20),
            NamedKey::F10 => tilde(mods, 21),
            NamedKey::F11 => tilde(mods, 23),
            NamedKey::F12 => tilde(mods, 24),
            NamedKey::Space => return character(' ', mods),
            _ => return None,
        }),
        Key::Character(s) => {
            let c = s.chars().next()?;
            // Prefer winit's composed text so IME and dead keys work, but not
            // when ctrl/alt are held, since then the key identity is what
            // matters rather than the glyph it would have produced.
            if !mods.control_key() && !mods.alt_key() {
                if let Some(text) = text.filter(|t| !t.is_empty()) {
                    return Some(text.as_bytes().to_vec());
                }
            }
            character(c, mods)
        },
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use winit::keyboard::SmolStr;

    fn ctrl() -> ModifiersState {
        ModifiersState::CONTROL
    }

    fn key(c: &str) -> Key {
        Key::Character(SmolStr::new(c))
    }

    /// Line-editing keys belong to the shell's own readline, not to this app.
    /// These assertions exist to catch a future shortcut silently swallowing
    /// one of them.
    #[test]
    fn readline_control_codes_pass_through() {
        for (ch, code) in [
            ("a", 0x01), // start of line
            ("e", 0x05), // end of line
            ("w", 0x17), // delete word backward
            ("u", 0x15), // clear to start
            ("k", 0x0b), // clear to end
            ("r", 0x12), // reverse history search
            ("c", 0x03), // interrupt
            ("d", 0x04), // EOF
            ("l", 0x0c), // clear
            ("z", 0x1a), // suspend
        ] {
            let out = encode(&key(ch), None, ctrl(), false).expect(ch);
            assert_eq!(out, vec![code], "ctrl+{ch} encoded wrong");
        }
    }

    #[test]
    fn tab_and_shift_tab_encode_for_completion() {
        assert_eq!(
            encode(&Key::Named(NamedKey::Tab), None, ModifiersState::empty(), false),
            Some(b"\t".to_vec())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::Tab), None, ModifiersState::SHIFT, false),
            Some(b"\x1b[Z".to_vec())
        );
    }

    #[test]
    fn history_arrows_encode_both_modes() {
        let none = ModifiersState::empty();
        // Normal mode: CSI. Application cursor mode: SS3.
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowUp), None, none, false),
            Some(b"\x1b[A".to_vec())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowUp), None, none, true),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowDown), None, none, false),
            Some(b"\x1b[B".to_vec())
        );
    }

    #[test]
    fn modified_arrows_carry_an_xterm_modifier_parameter() {
        // Alt+Left is word-back in many shells; the parameter must be 1+2.
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowLeft), None, ModifiersState::ALT, false),
            Some(b"\x1b[1;3D".to_vec())
        );
    }

    #[test]
    fn composed_text_is_preferred_over_the_logical_key() {
        // What makes dead keys and IME work.
        let out = encode(&key("e"), Some("é"), ModifiersState::empty(), false);
        assert_eq!(out, Some("é".as_bytes().to_vec()));
    }

    #[test]
    fn alt_prefixes_with_escape_rather_than_setting_the_high_bit() {
        assert_eq!(
            encode(&key("b"), Some("b"), ModifiersState::ALT, false),
            Some(vec![0x1b, b'b'])
        );
    }

    #[test]
    fn ctrl_slash_and_underscore_encode_undo() {
        assert_eq!(
            encode(&key("/"), None, ctrl(), false),
            Some(vec![0x1f])
        );
        assert_eq!(
            encode(&key("_"), None, ctrl(), false),
            Some(vec![0x1f])
        );
    }

    #[test]
    fn enter_with_alt_or_shift_encodes_newline() {
        assert_eq!(
            encode(&Key::Named(NamedKey::Enter), None, ModifiersState::SHIFT, false),
            Some(b"\x1b\r".to_vec())
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::Enter), None, ModifiersState::ALT, false),
            Some(b"\x1b\r".to_vec())
        );
    }

    #[test]
    fn ctrl_backspace_encodes_word_rubout() {
        assert_eq!(
            encode(&Key::Named(NamedKey::Backspace), None, ctrl(), false),
            Some(b"\x17".to_vec())
        );
    }

    #[test]
    fn cmd_left_right_and_backspace_encode_line_navigation() {
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowLeft), None, ModifiersState::SUPER, false),
            Some(vec![0x01])
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::ArrowRight), None, ModifiersState::SUPER, false),
            Some(vec![0x05])
        );
        assert_eq!(
            encode(&Key::Named(NamedKey::Backspace), None, ModifiersState::SUPER, false),
            Some(vec![0x15])
        );
    }
}
