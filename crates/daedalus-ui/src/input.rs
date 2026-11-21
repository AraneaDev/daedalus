use std::borrow::Cow;

use daedalus_proto::{FailureTrace, PaneRole, wrap_bracketed_paste};
use winit::keyboard::{Key, ModifiersState, NamedKey};

pub use crate::state::FocusDirection;

/// High-level UI action produced by translating a `winit` keyboard event.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UiAction {
    FocusRole(PaneRole),
    FocusDir(FocusDirection),
    ToggleZoom,
    ToggleSidebar,
    ToggleAgentPicker,
    NewWorkspace,
    CopySelection,
    PasteClipboard,
    ForwardFailureToAgent,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToBottom,
    SendBytes(Vec<u8>),
    PickerUp,
    PickerDown,
    PickerConfirm,
    PickerCancel,
    Ignore,
}

/// Translates a `winit` key press into either a workbench UI action, a picker modal
/// navigation event, or XTerm VT bytes to forward to the focused pane's PTY.
pub fn translate_key(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    picker_open: bool,
) -> UiAction {
    if picker_open {
        return translate_picker_key(key, mods);
    }

    // Never leak OS / window-manager Super shortcuts into the terminal.
    if mods.super_key() {
        return UiAction::Ignore;
    }

    let ctrl = mods.control_key();
    let shift = mods.shift_key();
    let alt = mods.alt_key();

    // Workbench chords
    if alt
        && !ctrl
        && !shift
        && let Key::Character(ch) = key
    {
        match ch.as_str() {
            "1" => return UiAction::FocusRole(PaneRole::Agent),
            "2" => return UiAction::FocusRole(PaneRole::Service),
            "3" => return UiAction::FocusRole(PaneRole::Runner),
            _ => {}
        }
    }

    if ctrl && shift && !alt {
        if let Key::Character(ch) = key {
            return match ch.to_ascii_lowercase().as_str() {
                "h" => UiAction::FocusDir(FocusDirection::Left),
                "j" => UiAction::FocusDir(FocusDirection::Down),
                "k" => UiAction::FocusDir(FocusDirection::Up),
                "l" => UiAction::FocusDir(FocusDirection::Right),
                "z" => UiAction::ToggleZoom,
                "b" => UiAction::ToggleSidebar,
                "p" => UiAction::ToggleAgentPicker,
                "n" => UiAction::NewWorkspace,
                "c" => UiAction::CopySelection,
                "v" => UiAction::PasteClipboard,
                "e" => UiAction::ForwardFailureToAgent,
                _ => UiAction::Ignore,
            };
        }
        return UiAction::Ignore;
    }

    if shift
        && !ctrl
        && !alt
        && let Key::Named(named) = key
    {
        match named {
            NamedKey::PageUp => return UiAction::ScrollPageUp,
            NamedKey::PageDown => return UiAction::ScrollPageDown,
            NamedKey::End => return UiAction::ScrollToBottom,
            _ => {}
        }
    }

    // Ctrl+<key> (without Shift/Alt) -> single ASCII control byte
    if ctrl && !shift && !alt {
        return match key {
            Key::Named(NamedKey::Space) => UiAction::SendBytes(vec![0x00]),
            Key::Character(ch) => match encode_ctrl_char(ch.as_str()) {
                Some(byte) => UiAction::SendBytes(vec![byte]),
                None => UiAction::Ignore,
            },
            _ => UiAction::Ignore,
        };
    }

    // Alt+<char> (without Ctrl) -> ESC + UTF-8 bytes
    if alt && !ctrl {
        return match key {
            Key::Character(ch) if !ch.is_empty() => {
                let mut bytes = Vec::with_capacity(1 + ch.len());
                bytes.push(0x1b);
                bytes.extend_from_slice(ch.as_bytes());
                UiAction::SendBytes(bytes)
            }
            Key::Named(NamedKey::Space) => UiAction::SendBytes(b"\x1b ".to_vec()),
            Key::Named(NamedKey::Enter) => UiAction::SendBytes(b"\x1b\r".to_vec()),
            Key::Named(NamedKey::Backspace) => UiAction::SendBytes(b"\x1b\x7f".to_vec()),
            _ => UiAction::Ignore,
        };
    }

    // Any other Ctrl+Alt combination is ignored rather than leaking raw text.
    if ctrl {
        return UiAction::Ignore;
    }

    // Standard XTerm VT named keys
    if let Key::Named(named) = key {
        let seq: Option<&'static [u8]> = match named {
            NamedKey::Enter => Some(b"\r"),
            NamedKey::Backspace => Some(b"\x7f"),
            NamedKey::Tab => Some(if shift { b"\x1b[Z" } else { b"\t" }),
            NamedKey::Escape => Some(b"\x1b"),
            NamedKey::Space => Some(b" "),
            NamedKey::ArrowUp => Some(b"\x1b[A"),
            NamedKey::ArrowDown => Some(b"\x1b[B"),
            NamedKey::ArrowRight => Some(b"\x1b[C"),
            NamedKey::ArrowLeft => Some(b"\x1b[D"),
            NamedKey::Home => Some(b"\x1b[H"),
            NamedKey::End => Some(b"\x1b[F"),
            NamedKey::Insert => Some(b"\x1b[2~"),
            NamedKey::Delete => Some(b"\x1b[3~"),
            NamedKey::PageUp => Some(b"\x1b[5~"),
            NamedKey::PageDown => Some(b"\x1b[6~"),
            _ => None,
        };
        return match seq {
            Some(bytes) => UiAction::SendBytes(bytes.to_vec()),
            None => UiAction::Ignore,
        };
    }

    // Printable UTF-8 text (prefer winit's composed `text` when non-control)
    if let Some(t) = text
        && !t.is_empty()
        && !t.chars().all(char::is_control)
    {
        return UiAction::SendBytes(t.as_bytes().to_vec());
    }

    if let Key::Character(ch) = key
        && !ch.is_empty()
    {
        return UiAction::SendBytes(ch.as_bytes().to_vec());
    }

    UiAction::Ignore
}

fn translate_picker_key(key: &Key, mods: ModifiersState) -> UiAction {
    match key {
        Key::Named(NamedKey::ArrowUp) => UiAction::PickerUp,
        Key::Named(NamedKey::ArrowDown) => UiAction::PickerDown,
        Key::Named(NamedKey::Enter) => UiAction::PickerConfirm,
        Key::Named(NamedKey::Escape) => UiAction::PickerCancel,
        Key::Character(ch)
            if !mods.control_key() && !mods.alt_key() && !mods.super_key() =>
        {
            if ch.eq_ignore_ascii_case("k") {
                UiAction::PickerUp
            } else if ch.eq_ignore_ascii_case("j") {
                UiAction::PickerDown
            } else {
                UiAction::Ignore
            }
        }
        _ => UiAction::Ignore,
    }
}

fn encode_ctrl_char(ch: &str) -> Option<u8> {
    let mut chars = ch.chars();
    let c = chars.next()?;
    if chars.next().is_some() || !c.is_ascii() {
        return None;
    }
    match c as u8 {
        b'a'..=b'z' => Some((c as u8) - b'a' + 1),
        b'A'..=b'Z' => Some((c as u8) - b'A' + 1),
        b'@' => Some(0x00),
        b'[' => Some(0x1b),
        b'\\' => Some(0x1c),
        b']' => Some(0x1d),
        b'^' => Some(0x1e),
        b'_' => Some(0x1f),
        _ => None,
    }
}

/// Formats a `FailureTrace` into the diagnostic prompt defined in spec §6.1 and
/// wraps it with `daedalus_proto::wrap_bracketed_paste` (which strips embedded
/// breakout sequences).
pub fn format_failure_trace_prompt(trace: &FailureTrace) -> Vec<u8> {
    let command = trace
        .command
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .unwrap_or("unknown");

    let cwd: Cow<'_, str> = trace
        .cwd
        .as_ref()
        .map(|p| p.to_string_lossy())
        .filter(|s| !s.is_empty())
        .unwrap_or(Cow::Borrowed("unknown"));

    let output = trace.output.trim();

    let prompt = format!(
        "The test/build runner failed. Please diagnose and fix:\n\
         Command: {command}\n\
         Exit code: {exit_code}\n\
         CWD: {cwd}\n\
         \n\
         Output:\n\
         {output}",
        exit_code = trace.exit_code,
    );

    wrap_bracketed_paste(prompt.as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use daedalus_proto::PaneId;
    use std::path::PathBuf;

    fn char_key(s: &str) -> Key {
        Key::Character(s.into())
    }

    fn named_key(n: NamedKey) -> Key {
        Key::Named(n)
    }

    #[test]
    fn alt_number_chords_focus_semantic_roles() {
        let alt = ModifiersState::ALT;
        assert_eq!(
            translate_key(&char_key("1"), None, alt, false),
            UiAction::FocusRole(PaneRole::Agent)
        );
        assert_eq!(
            translate_key(&char_key("2"), None, alt, false),
            UiAction::FocusRole(PaneRole::Service)
        );
        assert_eq!(
            translate_key(&char_key("3"), None, alt, false),
            UiAction::FocusRole(PaneRole::Runner)
        );
    }

    #[test]
    fn ctrl_shift_chords_dispatch_all_workbench_actions() {
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;

        let cases = [
            ("H", UiAction::FocusDir(FocusDirection::Left)),
            ("h", UiAction::FocusDir(FocusDirection::Left)),
            ("J", UiAction::FocusDir(FocusDirection::Down)),
            ("K", UiAction::FocusDir(FocusDirection::Up)),
            ("L", UiAction::FocusDir(FocusDirection::Right)),
            ("Z", UiAction::ToggleZoom),
            ("B", UiAction::ToggleSidebar),
            ("P", UiAction::ToggleAgentPicker),
            ("N", UiAction::NewWorkspace),
            ("C", UiAction::CopySelection),
            ("V", UiAction::PasteClipboard),
            ("E", UiAction::ForwardFailureToAgent),
        ];

        for (ch, expected) in cases {
            assert_eq!(
                translate_key(&char_key(ch), Some(ch), cs, false),
                expected,
                "Ctrl+Shift+{ch}"
            );
        }

        // Unbound Ctrl+Shift letter should be ignored, not leaked as text
        assert_eq!(
            translate_key(&char_key("Q"), Some("Q"), cs, false),
            UiAction::Ignore
        );
    }

    #[test]
    fn shift_scroll_chords_dispatch_scroll_actions() {
        let shift = ModifiersState::SHIFT;
        assert_eq!(
            translate_key(&named_key(NamedKey::PageUp), None, shift, false),
            UiAction::ScrollPageUp
        );
        assert_eq!(
            translate_key(&named_key(NamedKey::PageDown), None, shift, false),
            UiAction::ScrollPageDown
        );
        assert_eq!(
            translate_key(&named_key(NamedKey::End), None, shift, false),
            UiAction::ScrollToBottom
        );
    }

    #[test]
    fn vt_encodes_ctrl_letters_and_ctrl_c() {
        let ctrl = ModifiersState::CONTROL;
        assert_eq!(
            translate_key(&char_key("a"), Some("\x01"), ctrl, false),
            UiAction::SendBytes(vec![0x01])
        );
        assert_eq!(
            translate_key(&char_key("c"), Some("\x03"), ctrl, false),
            UiAction::SendBytes(vec![0x03])
        );
        assert_eq!(
            translate_key(&char_key("d"), None, ctrl, false),
            UiAction::SendBytes(vec![0x04])
        );
        assert_eq!(
            translate_key(&char_key("z"), None, ctrl, false),
            UiAction::SendBytes(vec![0x1a])
        );
    }

    #[test]
    fn vt_encodes_named_navigation_and_control_keys() {
        let none = ModifiersState::empty();
        let cases = [
            (NamedKey::Enter, b"\r".as_slice()),
            (NamedKey::Backspace, b"\x7f"),
            (NamedKey::Tab, b"\t"),
            (NamedKey::Escape, b"\x1b"),
            (NamedKey::Space, b" "),
            (NamedKey::ArrowUp, b"\x1b[A"),
            (NamedKey::ArrowDown, b"\x1b[B"),
            (NamedKey::ArrowRight, b"\x1b[C"),
            (NamedKey::ArrowLeft, b"\x1b[D"),
            (NamedKey::Home, b"\x1b[H"),
            (NamedKey::End, b"\x1b[F"),
            (NamedKey::Delete, b"\x1b[3~"),
            (NamedKey::PageUp, b"\x1b[5~"),
            (NamedKey::PageDown, b"\x1b[6~"),
        ];

        for (named, expected) in cases {
            assert_eq!(
                translate_key(&named_key(named), None, none, false),
                UiAction::SendBytes(expected.to_vec()),
                "NamedKey::{named:?}"
            );
        }

        // Shift+Tab sends backtab CSI Z
        assert_eq!(
            translate_key(&named_key(NamedKey::Tab), None, ModifiersState::SHIFT, false),
            UiAction::SendBytes(b"\x1b[Z".to_vec())
        );

        // Bare modifier keys are ignored
        assert_eq!(
            translate_key(&named_key(NamedKey::Shift), None, ModifiersState::SHIFT, false),
            UiAction::Ignore
        );
    }

    #[test]
    fn vt_encodes_alt_chords_and_utf8_text() {
        let alt = ModifiersState::ALT;
        assert_eq!(
            translate_key(&char_key("b"), None, alt, false),
            UiAction::SendBytes(b"\x1bb".to_vec())
        );
        assert_eq!(
            translate_key(&char_key("é"), None, alt, false),
            UiAction::SendBytes("\x1bé".as_bytes().to_vec())
        );

        let none = ModifiersState::empty();
        assert_eq!(
            translate_key(&char_key("x"), Some("x"), none, false),
            UiAction::SendBytes(b"x".to_vec())
        );
        assert_eq!(
            translate_key(&char_key("λ"), Some("λ"), none, false),
            UiAction::SendBytes("λ".as_bytes().to_vec())
        );
        assert_eq!(
            translate_key(&char_key("🚀"), None, none, false),
            UiAction::SendBytes("🚀".as_bytes().to_vec())
        );
        // Super-modified keys are never leaked to the PTY
        assert_eq!(
            translate_key(&char_key("l"), Some("l"), ModifiersState::SUPER, false),
            UiAction::Ignore
        );
    }

    #[test]
    fn picker_modal_captures_navigation_and_swallows_other_keys() {
        let none = ModifiersState::empty();
        assert_eq!(
            translate_key(&named_key(NamedKey::ArrowUp), None, none, true),
            UiAction::PickerUp
        );
        assert_eq!(
            translate_key(&char_key("k"), Some("k"), none, true),
            UiAction::PickerUp
        );
        assert_eq!(
            translate_key(&char_key("K"), Some("K"), ModifiersState::SHIFT, true),
            UiAction::PickerUp
        );
        assert_eq!(
            translate_key(&named_key(NamedKey::ArrowDown), None, none, true),
            UiAction::PickerDown
        );
        assert_eq!(
            translate_key(&char_key("j"), Some("j"), none, true),
            UiAction::PickerDown
        );
        assert_eq!(
            translate_key(&char_key("J"), Some("J"), ModifiersState::SHIFT, true),
            UiAction::PickerDown
        );
        assert_eq!(
            translate_key(&named_key(NamedKey::Enter), Some("\r"), none, true),
            UiAction::PickerConfirm
        );
        assert_eq!(
            translate_key(&named_key(NamedKey::Escape), Some("\x1b"), none, true),
            UiAction::PickerCancel
        );

        // Normal text and workbench chords are swallowed while picker is open
        assert_eq!(
            translate_key(&char_key("a"), Some("a"), none, true),
            UiAction::Ignore
        );
        assert_eq!(
            translate_key(&char_key("1"), None, ModifiersState::ALT, true),
            UiAction::Ignore
        );
        assert_eq!(
            translate_key(
                &char_key("Z"),
                Some("Z"),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                true
            ),
            UiAction::Ignore
        );
        assert_eq!(
            translate_key(&char_key("k"), None, ModifiersState::CONTROL, true),
            UiAction::Ignore
        );
    }

    #[test]
    fn format_failure_trace_prompt_matches_spec_and_sanitizes_breakouts() {
        let trace = FailureTrace {
            pane_id: PaneId::new(),
            command: Some("cargo test -p daedalus-ui".to_string()),
            exit_code: 101,
            cwd: Some(PathBuf::from("/home/tim/daedalus")),
            output: "\n  error[E0425]: cannot find value `x`\n  \n".to_string(),
            captured_at_ms: 1_700_000_000_000,
        };

        let bytes = format_failure_trace_prompt(&trace);
        let expected = "\x1b[200~The test/build runner failed. Please diagnose and fix:\n\
                        Command: cargo test -p daedalus-ui\n\
                        Exit code: 101\n\
                        CWD: /home/tim/daedalus\n\
                        \n\
                        Output:\n\
                        error[E0425]: cannot find value `x`\x1b[201~";
        assert_eq!(String::from_utf8(bytes).unwrap(), expected);

        // Missing command / cwd fall back to "unknown" and embedded breakout markers are stripped
        let malicious = FailureTrace {
            pane_id: PaneId::new(),
            command: None,
            exit_code: 1,
            cwd: None,
            output: "boom\x1b[201~rm -rf /\x1b[\x1b[201~201~done".to_string(),
            captured_at_ms: 0,
        };

        let sanitized = format_failure_trace_prompt(&malicious);
        assert!(sanitized.starts_with(b"\x1b[200~"));
        assert!(sanitized.ends_with(b"\x1b[201~"));
        let inner = std::str::from_utf8(&sanitized[6..sanitized.len() - 6]).unwrap();
        assert!(inner.contains("Command: unknown\n"));
        assert!(inner.contains("Exit code: 1\n"));
        assert!(inner.contains("CWD: unknown\n"));
        assert!(inner.ends_with("Output:\nboomrm -rf /done"));
        assert!(!inner.contains("\x1b[201~"));
        assert!(!inner.contains("\x1b[200~"));
    }
}
