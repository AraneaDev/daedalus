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
    ToggleActionPalette,
    ActionPaletteUp,
    ActionPaletteDown,
    ActionPaletteConfirm,
    ActionPaletteCancel,
    ToggleWorktreeModal,
    OpenCloseWorkspaceModal,
    ToggleSearch,
    NewWorkspace,
    SwitchWorkspaceNext,
    SwitchWorkspacePrev,
    CopySelection,
    PasteClipboard,
    ForwardFailureToAgent,
    FontZoomIn,
    FontZoomOut,
    FontZoomReset,
    ScrollPageUp,
    ScrollPageDown,
    ScrollToBottom,
    SendBytes(Vec<u8>),
    PickerUp,
    PickerDown,
    PickerConfirm,
    PickerCancel,
    WorktreeModalText(String),
    WorktreeModalBackspace,
    WorktreeModalConfirm,
    WorktreeModalCancel,
    CloseModalToggleCompose,
    CloseModalToggleWorktree,
    CloseModalToggleForce,
    CloseModalConfirm,
    CloseModalCancel,
    SearchText(String),
    SearchBackspace,
    SearchNext,
    SearchPrev,
    SearchCancel,
    Ignore,
}

/// Filters a composed text / IME slice for modal inputs, stripping control characters
/// and optionally stripping whitespace (e.g. for Git branch names).
pub fn filter_modal_text(raw: &str, allow_whitespace: bool) -> Option<String> {
    let filtered: String = raw
        .chars()
        .filter(|ch| !ch.is_control() && (allow_whitespace || !ch.is_whitespace()))
        .collect();
    (!filtered.is_empty()).then_some(filtered)
}

/// Translates a `winit` key press into either a workbench UI action, a modal
/// navigation/input event, or XTerm VT bytes to forward to the focused pane's PTY.
#[allow(clippy::too_many_arguments)]
pub fn translate_key(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    picker_open: bool,
    worktree_modal_open: bool,
    close_modal_open: bool,
    search_open: bool,
    action_palette_open: bool,
) -> UiAction {
    if action_palette_open {
        return translate_palette_key(key, mods, cfg!(target_os = "macos"));
    }
    translate_key_for_platform(
        key,
        text,
        mods,
        picker_open,
        worktree_modal_open,
        close_modal_open,
        search_open,
        cfg!(target_os = "macos"),
    )
}

/// Platform-parameterized variant of [`translate_key`].
#[allow(clippy::too_many_arguments)]
pub fn translate_key_for_platform(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    picker_open: bool,
    worktree_modal_open: bool,
    close_modal_open: bool,
    search_open: bool,
    is_macos: bool,
) -> UiAction {
    if picker_open {
        return translate_picker_key(key, mods, is_macos);
    }
    if worktree_modal_open {
        return translate_worktree_modal_key(key, text, mods, is_macos);
    }
    if close_modal_open {
        return translate_close_modal_key(key, mods);
    }
    if search_open {
        return translate_search_key(key, text, mods, is_macos);
    }

    let ctrl = mods.control_key();
    let shift = mods.shift_key();
    let alt = mods.alt_key();

    if mods.super_key() {
        if is_macos && !ctrl && !alt {
            return translate_macos_cmd(key, shift);
        }
        // Never leak OS / window-manager Super shortcuts into the terminal.
        return UiAction::Ignore;
    }

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
        if matches!(key, Key::Named(NamedKey::Tab)) {
            return UiAction::SwitchWorkspacePrev;
        }
        if !is_macos && let Key::Character(ch) = key {
            return translate_primary_shift_char(ch);
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

    // Ctrl+<key> (without Shift/Alt) -> workspace cycling, font zoom chords (Linux), or single ASCII control byte
    if ctrl && !shift && !alt {
        return match key {
            Key::Named(NamedKey::Tab) => UiAction::SwitchWorkspaceNext,
            Key::Named(NamedKey::Space) => UiAction::SendBytes(vec![0x00]),
            Key::Character(ch) => {
                if !is_macos {
                    match ch.as_str() {
                        "=" | "+" => return UiAction::FontZoomIn,
                        "-" => return UiAction::FontZoomOut,
                        "0" => return UiAction::FontZoomReset,
                        _ => {}
                    }
                }
                match encode_ctrl_char(ch) {
                    Some(byte) => UiAction::SendBytes(vec![byte]),
                    None => UiAction::Ignore,
                }
            }
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

fn translate_primary_shift_char(ch: &str) -> UiAction {
    match ch.to_ascii_lowercase().as_str() {
        "h" => UiAction::FocusDir(FocusDirection::Left),
        "j" => UiAction::FocusDir(FocusDirection::Down),
        "k" => UiAction::FocusDir(FocusDirection::Up),
        "l" => UiAction::FocusDir(FocusDirection::Right),
        "z" => UiAction::ToggleZoom,
        "b" => UiAction::ToggleSidebar,
        "a" => UiAction::ToggleActionPalette,
        "p" => UiAction::ToggleAgentPicker,
        "w" => UiAction::ToggleWorktreeModal,
        "d" => UiAction::OpenCloseWorkspaceModal,
        "f" => UiAction::ToggleSearch,
        "n" => UiAction::NewWorkspace,
        "c" => UiAction::CopySelection,
        "v" => UiAction::PasteClipboard,
        "e" => UiAction::ForwardFailureToAgent,
        "=" | "+" => UiAction::FontZoomIn,
        _ => UiAction::Ignore,
    }
}

fn translate_macos_cmd(key: &Key, shift: bool) -> UiAction {
    let Key::Character(ch) = key else {
        return UiAction::Ignore;
    };
    if shift {
        return translate_primary_shift_char(ch);
    }
    match ch.to_ascii_lowercase().as_str() {
        "c" => UiAction::CopySelection,
        "v" => UiAction::PasteClipboard,
        "f" => UiAction::ToggleSearch,
        "=" | "+" => UiAction::FontZoomIn,
        "-" => UiAction::FontZoomOut,
        "0" => UiAction::FontZoomReset,
        "1" => UiAction::FocusRole(PaneRole::Agent),
        "2" => UiAction::FocusRole(PaneRole::Service),
        "3" => UiAction::FocusRole(PaneRole::Runner),
        _ => UiAction::Ignore,
    }
}

fn is_primary_shift_char(key: &Key, mods: ModifiersState, target: &str, is_macos: bool) -> bool {
    let primary_only = if is_macos {
        mods.super_key() && !mods.control_key()
    } else {
        mods.control_key() && !mods.super_key()
    };
    primary_only
        && mods.shift_key()
        && !mods.alt_key()
        && matches!(key, Key::Character(ch) if ch.eq_ignore_ascii_case(target))
}

fn is_search_toggle_chord(key: &Key, mods: ModifiersState, is_macos: bool) -> bool {
    let Key::Character(ch) = key else {
        return false;
    };
    if !ch.eq_ignore_ascii_case("f") || mods.alt_key() {
        return false;
    }
    if is_macos {
        mods.super_key() && !mods.control_key()
    } else {
        mods.control_key() && mods.shift_key() && !mods.super_key()
    }
}

fn translate_palette_key(key: &Key, mods: ModifiersState, is_macos: bool) -> UiAction {
    if is_primary_shift_char(key, mods, "a", is_macos) {
        return UiAction::ToggleActionPalette;
    }
    match key {
        Key::Named(NamedKey::ArrowUp) => UiAction::ActionPaletteUp,
        Key::Named(NamedKey::ArrowDown) => UiAction::ActionPaletteDown,
        Key::Named(NamedKey::Enter) => UiAction::ActionPaletteConfirm,
        Key::Named(NamedKey::Escape) => UiAction::ActionPaletteCancel,
        _ => UiAction::Ignore,
    }
}

fn translate_picker_key(key: &Key, mods: ModifiersState, is_macos: bool) -> UiAction {
    if is_primary_shift_char(key, mods, "p", is_macos) {
        return UiAction::ToggleAgentPicker;
    }

    match key {
        Key::Named(NamedKey::ArrowUp) => UiAction::PickerUp,
        Key::Named(NamedKey::ArrowDown) => UiAction::PickerDown,
        Key::Named(NamedKey::Enter) => UiAction::PickerConfirm,
        Key::Named(NamedKey::Escape) => UiAction::PickerCancel,
        Key::Character(ch) if !mods.control_key() && !mods.alt_key() && !mods.super_key() => {
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

fn translate_worktree_modal_key(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    is_macos: bool,
) -> UiAction {
    if is_primary_shift_char(key, mods, "w", is_macos) {
        return UiAction::ToggleWorktreeModal;
    }

    if mods.control_key() || mods.alt_key() || mods.super_key() {
        return UiAction::Ignore;
    }

    match key {
        Key::Named(NamedKey::Enter) => UiAction::WorktreeModalConfirm,
        Key::Named(NamedKey::Escape) => UiAction::WorktreeModalCancel,
        Key::Named(NamedKey::Backspace) => UiAction::WorktreeModalBackspace,
        Key::Named(_) => UiAction::Ignore,
        _ => {
            let candidate = text.filter(|t| !t.is_empty()).or(match key {
                Key::Character(ch) => Some(ch.as_str()),
                _ => None,
            });
            match candidate.and_then(|raw| filter_modal_text(raw, false)) {
                Some(cleaned) => UiAction::WorktreeModalText(cleaned),
                None => UiAction::Ignore,
            }
        }
    }
}

fn translate_close_modal_key(key: &Key, mods: ModifiersState) -> UiAction {
    if mods.control_key() || mods.alt_key() || mods.super_key() {
        return UiAction::Ignore;
    }
    match key {
        Key::Named(NamedKey::Enter) => UiAction::CloseModalConfirm,
        Key::Named(NamedKey::Escape) => UiAction::CloseModalCancel,
        Key::Character(ch) => {
            if ch.eq_ignore_ascii_case("c") {
                UiAction::CloseModalToggleCompose
            } else if ch.eq_ignore_ascii_case("w") {
                UiAction::CloseModalToggleWorktree
            } else if ch.eq_ignore_ascii_case("f") {
                UiAction::CloseModalToggleForce
            } else {
                UiAction::Ignore
            }
        }
        _ => UiAction::Ignore,
    }
}

fn translate_search_key(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    is_macos: bool,
) -> UiAction {
    if is_search_toggle_chord(key, mods, is_macos) {
        return UiAction::ToggleSearch;
    }

    if mods.control_key() || mods.alt_key() || mods.super_key() {
        return UiAction::Ignore;
    }

    match key {
        Key::Named(NamedKey::Escape) => UiAction::SearchCancel,
        Key::Named(NamedKey::Enter) => {
            if mods.shift_key() {
                UiAction::SearchPrev
            } else {
                UiAction::SearchNext
            }
        }
        Key::Named(NamedKey::Backspace) => UiAction::SearchBackspace,
        Key::Named(NamedKey::Space) => UiAction::SearchText(" ".to_string()),
        Key::Named(_) => UiAction::Ignore,
        _ => {
            let candidate = text.filter(|t| !t.is_empty()).or(match key {
                Key::Character(ch) => Some(ch.as_str()),
                _ => None,
            });
            match candidate.and_then(|raw| filter_modal_text(raw, true)) {
                Some(cleaned) => UiAction::SearchText(cleaned),
                None => UiAction::Ignore,
            }
        }
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

    let mut prompt = format!(
        "The test/build runner failed. Please diagnose and fix:\n\
         Command: {command}\n\
         Exit code: {exit_code}\n\
         CWD: {cwd}",
        exit_code = trace.exit_code,
    );

    if let Some(branch) = trace
        .git_branch
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        prompt.push_str(&format!("\nBranch: {branch}"));
    }

    if let Some(diff_stat) = trace
        .git_diff_stat
        .as_deref()
        .map(str::trim)
        .filter(|s| !s.is_empty())
    {
        prompt.push_str(&format!("\n\nGit diff summary:\n{diff_stat}"));
    }

    prompt.push_str(&format!("\n\nOutput:\n{output}"));

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

    fn t(key: &Key, text: Option<&str>, mods: ModifiersState) -> UiAction {
        translate_key_for_platform(key, text, mods, false, false, false, false, false)
    }

    fn t_macos(key: &Key, text: Option<&str>, mods: ModifiersState) -> UiAction {
        translate_key_for_platform(key, text, mods, false, false, false, false, true)
    }

    #[test]
    fn palette_uses_platform_chord_and_captures_keys() {
        let key = char_key("A");
        for (macos, primary) in [
            (true, ModifiersState::SUPER),
            (false, ModifiersState::CONTROL),
        ] {
            let chord = primary | ModifiersState::SHIFT;
            assert_eq!(
                translate_key_for_platform(
                    &key,
                    Some("A"),
                    chord,
                    false,
                    false,
                    false,
                    false,
                    macos
                ),
                UiAction::ToggleActionPalette
            );
            assert_eq!(
                translate_palette_key(&key, chord, macos),
                UiAction::ToggleActionPalette
            );
        }
        for (key, expected) in [
            (NamedKey::ArrowUp, UiAction::ActionPaletteUp),
            (NamedKey::ArrowDown, UiAction::ActionPaletteDown),
            (NamedKey::Enter, UiAction::ActionPaletteConfirm),
            (NamedKey::Escape, UiAction::ActionPaletteCancel),
        ] {
            assert_eq!(
                translate_palette_key(&named_key(key), ModifiersState::empty(), false),
                expected
            );
        }
        assert_eq!(
            translate_palette_key(&char_key("x"), ModifiersState::empty(), false),
            UiAction::Ignore
        );
    }

    #[test]
    fn alt_number_chords_focus_semantic_roles() {
        let alt = ModifiersState::ALT;
        assert_eq!(
            t(&char_key("1"), None, alt),
            UiAction::FocusRole(PaneRole::Agent)
        );
        assert_eq!(
            t(&char_key("2"), None, alt),
            UiAction::FocusRole(PaneRole::Service)
        );
        assert_eq!(
            t(&char_key("3"), None, alt),
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
            ("W", UiAction::ToggleWorktreeModal),
            ("w", UiAction::ToggleWorktreeModal),
            ("D", UiAction::OpenCloseWorkspaceModal),
            ("d", UiAction::OpenCloseWorkspaceModal),
            ("F", UiAction::ToggleSearch),
            ("f", UiAction::ToggleSearch),
            ("N", UiAction::NewWorkspace),
            ("C", UiAction::CopySelection),
            ("V", UiAction::PasteClipboard),
            ("E", UiAction::ForwardFailureToAgent),
        ];

        for (ch, expected) in cases {
            assert_eq!(t(&char_key(ch), Some(ch), cs), expected, "Ctrl+Shift+{ch}");
        }

        // Ctrl+Tab / Ctrl+Shift+Tab cycle workspaces
        assert_eq!(
            t(&named_key(NamedKey::Tab), None, ModifiersState::CONTROL),
            UiAction::SwitchWorkspaceNext
        );
        assert_eq!(
            t(&named_key(NamedKey::Tab), None, cs),
            UiAction::SwitchWorkspacePrev
        );

        // Unbound Ctrl+Shift letter should be ignored, not leaked as text
        assert_eq!(t(&char_key("Q"), Some("Q"), cs), UiAction::Ignore);
    }

    #[test]
    fn font_zoom_chords_dispatch_in_out_and_reset() {
        let ctrl = ModifiersState::CONTROL;
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;

        assert_eq!(t(&char_key("="), None, ctrl), UiAction::FontZoomIn);
        assert_eq!(t(&char_key("+"), None, ctrl), UiAction::FontZoomIn);
        assert_eq!(t(&char_key("+"), Some("+"), cs), UiAction::FontZoomIn);
        assert_eq!(t(&char_key("="), None, cs), UiAction::FontZoomIn);
        assert_eq!(t(&char_key("-"), None, ctrl), UiAction::FontZoomOut);
        assert_eq!(t(&char_key("0"), None, ctrl), UiAction::FontZoomReset);
    }

    #[test]
    fn shift_scroll_chords_dispatch_scroll_actions() {
        let shift = ModifiersState::SHIFT;
        assert_eq!(
            t(&named_key(NamedKey::PageUp), None, shift),
            UiAction::ScrollPageUp
        );
        assert_eq!(
            t(&named_key(NamedKey::PageDown), None, shift),
            UiAction::ScrollPageDown
        );
        assert_eq!(
            t(&named_key(NamedKey::End), None, shift),
            UiAction::ScrollToBottom
        );
    }

    #[test]
    fn vt_encodes_ctrl_letters_and_ctrl_c() {
        let ctrl = ModifiersState::CONTROL;
        assert_eq!(
            t(&char_key("a"), Some("\x01"), ctrl),
            UiAction::SendBytes(vec![0x01])
        );
        assert_eq!(
            t(&char_key("c"), Some("\x03"), ctrl),
            UiAction::SendBytes(vec![0x03])
        );
        assert_eq!(
            t(&char_key("d"), None, ctrl),
            UiAction::SendBytes(vec![0x04])
        );
        assert_eq!(
            t(&char_key("z"), None, ctrl),
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
                t(&named_key(named), None, none),
                UiAction::SendBytes(expected.to_vec()),
                "NamedKey::{named:?}"
            );
        }

        // Shift+Tab sends backtab CSI Z
        assert_eq!(
            t(&named_key(NamedKey::Tab), None, ModifiersState::SHIFT),
            UiAction::SendBytes(b"\x1b[Z".to_vec())
        );

        // Bare modifier keys are ignored
        assert_eq!(
            t(&named_key(NamedKey::Shift), None, ModifiersState::SHIFT),
            UiAction::Ignore
        );
    }

    #[test]
    fn vt_encodes_alt_chords_and_utf8_text() {
        let alt = ModifiersState::ALT;
        assert_eq!(
            t(&char_key("b"), None, alt),
            UiAction::SendBytes(b"\x1bb".to_vec())
        );
        assert_eq!(
            t(&char_key("é"), None, alt),
            UiAction::SendBytes("\x1bé".as_bytes().to_vec())
        );

        let none = ModifiersState::empty();
        assert_eq!(
            t(&char_key("x"), Some("x"), none),
            UiAction::SendBytes(b"x".to_vec())
        );
        assert_eq!(
            t(&char_key("λ"), Some("λ"), none),
            UiAction::SendBytes("λ".as_bytes().to_vec())
        );
        assert_eq!(
            t(&char_key("🚀"), None, none),
            UiAction::SendBytes("🚀".as_bytes().to_vec())
        );
        // Super-modified keys are never leaked to the PTY
        assert_eq!(
            t(&char_key("l"), Some("l"), ModifiersState::SUPER),
            UiAction::Ignore
        );
    }

    fn t_linux_mode(
        key: &Key,
        text: Option<&str>,
        mods: ModifiersState,
        picker_open: bool,
        worktree_modal_open: bool,
        close_modal_open: bool,
        search_open: bool,
    ) -> UiAction {
        translate_key_for_platform(
            key,
            text,
            mods,
            picker_open,
            worktree_modal_open,
            close_modal_open,
            search_open,
            false,
        )
    }

    #[test]
    fn picker_modal_captures_navigation_and_swallows_other_keys() {
        let none = ModifiersState::empty();
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::ArrowUp),
                None,
                none,
                true,
                false,
                false,
                false
            ),
            UiAction::PickerUp
        );
        assert_eq!(
            t_linux_mode(&char_key("k"), Some("k"), none, true, false, false, false),
            UiAction::PickerUp
        );
        assert_eq!(
            t_linux_mode(
                &char_key("K"),
                Some("K"),
                ModifiersState::SHIFT,
                true,
                false,
                false,
                false
            ),
            UiAction::PickerUp
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::ArrowDown),
                None,
                none,
                true,
                false,
                false,
                false
            ),
            UiAction::PickerDown
        );
        assert_eq!(
            t_linux_mode(&char_key("j"), Some("j"), none, true, false, false, false),
            UiAction::PickerDown
        );
        assert_eq!(
            t_linux_mode(
                &char_key("J"),
                Some("J"),
                ModifiersState::SHIFT,
                true,
                false,
                false,
                false
            ),
            UiAction::PickerDown
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Enter),
                Some("\r"),
                none,
                true,
                false,
                false,
                false
            ),
            UiAction::PickerConfirm
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Escape),
                Some("\x1b"),
                none,
                true,
                false,
                false,
                false
            ),
            UiAction::PickerCancel
        );

        // Normal text and workbench chords are swallowed while picker is open
        assert_eq!(
            t_linux_mode(&char_key("a"), Some("a"), none, true, false, false, false),
            UiAction::Ignore
        );
        assert_eq!(
            t_linux_mode(
                &char_key("1"),
                None,
                ModifiersState::ALT,
                true,
                false,
                false,
                false
            ),
            UiAction::Ignore
        );
        assert_eq!(
            t_linux_mode(
                &char_key("Z"),
                Some("Z"),
                ModifiersState::CONTROL | ModifiersState::SHIFT,
                true,
                false,
                false,
                false
            ),
            UiAction::Ignore
        );
        assert_eq!(
            t_linux_mode(
                &char_key("k"),
                None,
                ModifiersState::CONTROL,
                true,
                false,
                false,
                false
            ),
            UiAction::Ignore
        );
    }

    #[test]
    fn ctrl_shift_p_toggles_picker_closed_when_already_open() {
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;
        assert_eq!(
            t_linux_mode(&char_key("P"), Some("P"), cs, true, false, false, false),
            UiAction::ToggleAgentPicker
        );
        assert_eq!(
            t_linux_mode(&char_key("p"), Some("p"), cs, true, false, false, false),
            UiAction::ToggleAgentPicker
        );
        assert_eq!(
            t_linux_mode(
                &char_key("P"),
                None,
                cs | ModifiersState::ALT,
                true,
                false,
                false,
                false
            ),
            UiAction::Ignore
        );
        assert_eq!(
            t_linux_mode(
                &char_key("p"),
                None,
                ModifiersState::CONTROL,
                true,
                false,
                false,
                false
            ),
            UiAction::Ignore
        );
    }

    #[test]
    fn worktree_modal_captures_branch_typing_ime_submit_cancel_and_toggle() {
        let none = ModifiersState::empty();
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;

        assert_eq!(
            t_linux_mode(&char_key("f"), Some("f"), none, false, true, false, false),
            UiAction::WorktreeModalText("f".to_string())
        );
        assert_eq!(
            t_linux_mode(&char_key("/"), Some("/"), none, false, true, false, false),
            UiAction::WorktreeModalText("/".to_string())
        );
        assert_eq!(
            t_linux_mode(
                &char_key("A"),
                Some("A"),
                ModifiersState::SHIFT,
                false,
                true,
                false,
                false
            ),
            UiAction::WorktreeModalText("A".to_string())
        );
        // Full multi-char IME / dead-key composition string, stripping whitespace & control chars
        assert_eq!(
            t_linux_mode(
                &char_key("é"),
                Some("feat/日本語\n "),
                none,
                false,
                true,
                false,
                false
            ),
            UiAction::WorktreeModalText("feat/日本語".to_string())
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Backspace),
                None,
                none,
                false,
                true,
                false,
                false
            ),
            UiAction::WorktreeModalBackspace
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Enter),
                Some("\r"),
                none,
                false,
                true,
                false,
                false
            ),
            UiAction::WorktreeModalConfirm
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Escape),
                Some("\x1b"),
                none,
                false,
                true,
                false,
                false
            ),
            UiAction::WorktreeModalCancel
        );
        assert_eq!(
            t_linux_mode(&char_key("W"), Some("W"), cs, false, true, false, false),
            UiAction::ToggleWorktreeModal
        );

        // Space, control, and alt chords are swallowed while worktree modal is open
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Space),
                Some(" "),
                none,
                false,
                true,
                false,
                false
            ),
            UiAction::Ignore
        );
        assert_eq!(
            t_linux_mode(
                &char_key("c"),
                None,
                ModifiersState::CONTROL,
                false,
                true,
                false,
                false
            ),
            UiAction::Ignore
        );
        assert_eq!(
            t_linux_mode(
                &char_key("1"),
                None,
                ModifiersState::ALT,
                false,
                true,
                false,
                false
            ),
            UiAction::Ignore
        );
    }

    #[test]
    fn close_modal_captures_toggles_confirm_cancel_and_swallows_others() {
        let none = ModifiersState::empty();
        assert_eq!(
            t_linux_mode(&char_key("c"), Some("c"), none, false, false, true, false),
            UiAction::CloseModalToggleCompose
        );
        assert_eq!(
            t_linux_mode(
                &char_key("C"),
                Some("C"),
                ModifiersState::SHIFT,
                false,
                false,
                true,
                false
            ),
            UiAction::CloseModalToggleCompose
        );
        assert_eq!(
            t_linux_mode(&char_key("w"), Some("w"), none, false, false, true, false),
            UiAction::CloseModalToggleWorktree
        );
        assert_eq!(
            t_linux_mode(&char_key("f"), Some("f"), none, false, false, true, false),
            UiAction::CloseModalToggleForce
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Enter),
                Some("\r"),
                none,
                false,
                false,
                true,
                false
            ),
            UiAction::CloseModalConfirm
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Escape),
                Some("\x1b"),
                none,
                false,
                false,
                true,
                false
            ),
            UiAction::CloseModalCancel
        );

        // Other keys & modifier chords are swallowed while close modal is open
        assert_eq!(
            t_linux_mode(&char_key("a"), Some("a"), none, false, false, true, false),
            UiAction::Ignore
        );
        assert_eq!(
            t_linux_mode(
                &char_key("c"),
                None,
                ModifiersState::CONTROL,
                false,
                false,
                true,
                false
            ),
            UiAction::Ignore
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Tab),
                None,
                ModifiersState::CONTROL,
                false,
                false,
                true,
                false
            ),
            UiAction::Ignore
        );
    }

    #[test]
    fn search_mode_captures_query_ime_navigation_and_cancel() {
        let none = ModifiersState::empty();
        let shift = ModifiersState::SHIFT;
        let cs = ModifiersState::CONTROL | ModifiersState::SHIFT;

        assert_eq!(
            t_linux_mode(&char_key("e"), Some("e"), none, false, false, false, true),
            UiAction::SearchText("e".to_string())
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Space),
                Some(" "),
                none,
                false,
                false,
                false,
                true
            ),
            UiAction::SearchText(" ".to_string())
        );
        // Multi-char IME composition preserves spaces and filters control chars
        assert_eq!(
            t_linux_mode(
                &char_key("x"),
                Some("foo bar\x07"),
                none,
                false,
                false,
                false,
                true
            ),
            UiAction::SearchText("foo bar".to_string())
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Backspace),
                None,
                none,
                false,
                false,
                false,
                true
            ),
            UiAction::SearchBackspace
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Enter),
                Some("\r"),
                none,
                false,
                false,
                false,
                true
            ),
            UiAction::SearchNext
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Enter),
                Some("\r"),
                shift,
                false,
                false,
                false,
                true
            ),
            UiAction::SearchPrev
        );
        assert_eq!(
            t_linux_mode(
                &named_key(NamedKey::Escape),
                Some("\x1b"),
                none,
                false,
                false,
                false,
                true
            ),
            UiAction::SearchCancel
        );
        assert_eq!(
            t_linux_mode(&char_key("F"), Some("F"), cs, false, false, false, true),
            UiAction::ToggleSearch
        );
        // Ctrl+other is swallowed rather than typed
        assert_eq!(
            t_linux_mode(
                &char_key("c"),
                None,
                ModifiersState::CONTROL,
                false,
                false,
                false,
                true
            ),
            UiAction::Ignore
        );
    }

    #[test]
    fn macos_cmd_chords_and_terminal_ctrl_bytes() {
        let cmd = ModifiersState::SUPER;
        let cmd_shift = ModifiersState::SUPER | ModifiersState::SHIFT;
        let ctrl = ModifiersState::CONTROL;
        let ctrl_shift = ModifiersState::CONTROL | ModifiersState::SHIFT;
        let alt = ModifiersState::ALT;

        // Cmd clipboard, search, zoom, role focus
        assert_eq!(
            t_macos(&char_key("c"), Some("c"), cmd),
            UiAction::CopySelection
        );
        assert_eq!(
            t_macos(&char_key("C"), Some("C"), cmd_shift),
            UiAction::CopySelection
        );
        assert_eq!(
            t_macos(&char_key("v"), Some("v"), cmd),
            UiAction::PasteClipboard
        );
        assert_eq!(
            t_macos(&char_key("V"), Some("V"), cmd_shift),
            UiAction::PasteClipboard
        );
        assert_eq!(
            t_macos(&char_key("f"), Some("f"), cmd),
            UiAction::ToggleSearch
        );
        assert_eq!(
            t_macos(&char_key("F"), Some("F"), cmd_shift),
            UiAction::ToggleSearch
        );
        assert_eq!(t_macos(&char_key("="), None, cmd), UiAction::FontZoomIn);
        assert_eq!(
            t_macos(&char_key("+"), Some("+"), cmd),
            UiAction::FontZoomIn
        );
        assert_eq!(
            t_macos(&char_key("+"), Some("+"), cmd_shift),
            UiAction::FontZoomIn
        );
        assert_eq!(t_macos(&char_key("-"), None, cmd), UiAction::FontZoomOut);
        assert_eq!(t_macos(&char_key("0"), None, cmd), UiAction::FontZoomReset);
        assert_eq!(
            t_macos(&char_key("1"), None, cmd),
            UiAction::FocusRole(PaneRole::Agent)
        );
        assert_eq!(
            t_macos(&char_key("2"), None, cmd),
            UiAction::FocusRole(PaneRole::Service)
        );
        assert_eq!(
            t_macos(&char_key("3"), None, cmd),
            UiAction::FocusRole(PaneRole::Runner)
        );

        // Alt+1/2/3 also stays functional on macOS
        assert_eq!(
            t_macos(&char_key("1"), None, alt),
            UiAction::FocusRole(PaneRole::Agent)
        );
        assert_eq!(
            t_macos(&char_key("2"), None, alt),
            UiAction::FocusRole(PaneRole::Service)
        );
        assert_eq!(
            t_macos(&char_key("3"), None, alt),
            UiAction::FocusRole(PaneRole::Runner)
        );

        // Cmd+Shift workbench actions
        assert_eq!(
            t_macos(&char_key("P"), Some("P"), cmd_shift),
            UiAction::ToggleAgentPicker
        );
        assert_eq!(
            t_macos(&char_key("E"), Some("E"), cmd_shift),
            UiAction::ForwardFailureToAgent
        );
        assert_eq!(
            t_macos(&char_key("W"), Some("W"), cmd_shift),
            UiAction::ToggleWorktreeModal
        );
        assert_eq!(
            t_macos(&char_key("D"), Some("D"), cmd_shift),
            UiAction::OpenCloseWorkspaceModal
        );
        assert_eq!(
            t_macos(&char_key("B"), Some("B"), cmd_shift),
            UiAction::ToggleSidebar
        );
        assert_eq!(
            t_macos(&char_key("Z"), Some("Z"), cmd_shift),
            UiAction::ToggleZoom
        );

        // Ctrl+Tab / Ctrl+Shift+Tab cycle workspaces on macOS
        assert_eq!(
            t_macos(&named_key(NamedKey::Tab), None, ctrl),
            UiAction::SwitchWorkspaceNext
        );
        assert_eq!(
            t_macos(&named_key(NamedKey::Tab), None, ctrl_shift),
            UiAction::SwitchWorkspacePrev
        );

        // Crucially, Ctrl+<letter> without Cmd still sends raw PTY control bytes on macOS
        assert_eq!(
            t_macos(&char_key("c"), Some("\x03"), ctrl),
            UiAction::SendBytes(vec![0x03])
        );
        assert_eq!(
            t_macos(&char_key("d"), None, ctrl),
            UiAction::SendBytes(vec![0x04])
        );
        assert_eq!(
            t_macos(&char_key("z"), None, ctrl),
            UiAction::SendBytes(vec![0x1a])
        );

        // Unbound Cmd combinations are ignored rather than leaked to PTY
        assert_eq!(t_macos(&char_key("q"), Some("q"), cmd), UiAction::Ignore);
        assert_eq!(t_macos(&char_key("l"), Some("l"), cmd), UiAction::Ignore);
        assert_eq!(
            t_macos(&char_key("Q"), Some("Q"), cmd_shift),
            UiAction::Ignore
        );
        assert_eq!(t_macos(&char_key("c"), None, cmd | ctrl), UiAction::Ignore);
        assert_eq!(t_macos(&char_key("c"), None, cmd | alt), UiAction::Ignore);
        assert_eq!(
            t_macos(&char_key("C"), Some("C"), ctrl_shift),
            UiAction::Ignore
        );

        // Modal toggle chords also respect macOS Cmd shortcuts
        assert_eq!(
            translate_key_for_platform(
                &char_key("p"),
                Some("p"),
                cmd_shift,
                true,
                false,
                false,
                false,
                true
            ),
            UiAction::ToggleAgentPicker
        );
        assert_eq!(
            translate_key_for_platform(
                &char_key("w"),
                Some("w"),
                cmd_shift,
                false,
                true,
                false,
                false,
                true
            ),
            UiAction::ToggleWorktreeModal
        );
        assert_eq!(
            translate_key_for_platform(
                &char_key("f"),
                Some("f"),
                cmd,
                false,
                false,
                false,
                true,
                true
            ),
            UiAction::ToggleSearch
        );
        assert_eq!(
            translate_key_for_platform(
                &char_key("F"),
                Some("F"),
                cmd_shift,
                false,
                false,
                false,
                true,
                true
            ),
            UiAction::ToggleSearch
        );

        // Platform wrapper matches cfg!(target_os = "macos")
        assert_eq!(
            translate_key(
                &char_key("c"),
                Some("c"),
                cmd,
                false,
                false,
                false,
                false,
                false
            ),
            translate_key_for_platform(
                &char_key("c"),
                Some("c"),
                cmd,
                false,
                false,
                false,
                false,
                cfg!(target_os = "macos"),
            )
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
            git_branch: None,
            git_diff_stat: None,
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

        // Populated git_branch & git_diff_stat are included and sanitized against breakout markers
        let enriched = FailureTrace {
            pane_id: PaneId::new(),
            command: Some("cargo test".to_string()),
            exit_code: 1,
            cwd: Some(PathBuf::from("/tmp/wt")),
            output: "test failed".to_string(),
            captured_at_ms: 1_700_000_000_000,
            git_branch: Some("feat/v0.0.3\x1b[201~".to_string()),
            git_diff_stat: Some(" src/lib.rs | 4 ++--\x1b[201~\n 1 file changed".to_string()),
        };
        let enriched_bytes = format_failure_trace_prompt(&enriched);
        let enriched_inner =
            std::str::from_utf8(&enriched_bytes[6..enriched_bytes.len() - 6]).unwrap();
        assert!(enriched_inner.contains("Branch: feat/v0.0.3\n"));
        assert!(
            enriched_inner
                .contains("Git diff summary:\nsrc/lib.rs | 4 ++--\n 1 file changed\n\nOutput:\n")
        );
        assert!(!enriched_inner.contains("\x1b[201~"));

        // Whitespace-only branch/diff_stat are omitted
        let blank_git = FailureTrace {
            git_branch: Some("   ".to_string()),
            git_diff_stat: Some(" \n\t ".to_string()),
            ..trace
        };
        let blank_bytes = format_failure_trace_prompt(&blank_git);
        let blank_str = String::from_utf8(blank_bytes).unwrap();
        assert!(!blank_str.contains("Branch:"));
        assert!(!blank_str.contains("Git diff summary:"));

        // Missing command / cwd fall back to "unknown" and embedded breakout markers are stripped
        let malicious = FailureTrace {
            pane_id: PaneId::new(),
            command: None,
            exit_code: 1,
            cwd: None,
            output: "boom\x1b[201~rm -rf /\x1b[\x1b[201~201~done".to_string(),
            captured_at_ms: 0,
            git_branch: None,
            git_diff_stat: None,
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
