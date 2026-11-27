use std::collections::HashSet;

use crate::state::ActiveOverlay;
use daedalus_proto::{PaneRole, WorkspaceId};

mod modal_keys;
mod terminal_keys;
pub use crate::bridge::terminal::format_failure_trace_prompt;
pub use modal_keys::filter_modal_text;
use winit::keyboard::{Key, ModifiersState, NamedKey, PhysicalKey};

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
    OpenSessions,
    ActionPaletteUp,
    ActionPaletteDown,
    ActionPaletteConfirm,
    ActionPaletteCancel,
    ToggleWorktreeModal,
    OpenCloseWorkspaceModal,
    CloseWorkspaceFromSidebar(WorkspaceId),
    CloseTerminal,
    ToggleSearch,
    NewWorkspace,
    WorkspaceSettings,
    SwitchWorkspaceNext,
    SwitchWorkspacePrev,
    SelectWorkspace(WorkspaceId),
    PreviewImage,
    CopySelection,
    PasteClipboard,
    ForwardFailureToAgent,
    OpenFailedRun(WorkspaceId),
    OpenNextFailedRun,
    RunTests,
    RunCargo(crate::assignments::CargoPreset),
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
    PickerAdd,
    PickerCancel,
    WorktreeModalText(String),
    WorktreeModalSetText(String),
    WorktreeModalBackspace,
    WorktreeModalConfirm,
    WorktreeModalCancel,
    CloseModalToggleCompose,
    CloseModalToggleWorktree,
    CloseModalToggleForce,
    CloseModalConfirm,
    CloseModalCancel,
    SearchText(String),
    PasteSearchQuery,
    SearchBackspace,
    SearchNext,
    SearchPrev,
    SearchToggleCase,
    SearchCancel,
    Ignore,
}

/// Keeps a held shortcut consumed even when its first press closes the active modal.
#[derive(Default)]
pub struct KeyRepeatGuard {
    consumed: HashSet<PhysicalKey>,
}

impl KeyRepeatGuard {
    pub fn press(&mut self, key: PhysicalKey, repeat: bool, action: UiAction) -> UiAction {
        if repeat && self.consumed.contains(&key) {
            return UiAction::Ignore;
        }
        let one_shot = matches!(
            action,
            UiAction::ToggleZoom
                | UiAction::ToggleSidebar
                | UiAction::ToggleAgentPicker
                | UiAction::ToggleActionPalette
                | UiAction::OpenSessions
                | UiAction::ActionPaletteConfirm
                | UiAction::ActionPaletteCancel
                | UiAction::ToggleWorktreeModal
                | UiAction::CloseTerminal
                | UiAction::OpenCloseWorkspaceModal
                | UiAction::ToggleSearch
                | UiAction::WorkspaceSettings
                | UiAction::NewWorkspace
                | UiAction::PreviewImage
                | UiAction::CopySelection
                | UiAction::PasteSearchQuery
                | UiAction::PasteClipboard
                | UiAction::RunTests
                | UiAction::OpenNextFailedRun
                | UiAction::ForwardFailureToAgent
                | UiAction::PickerAdd
                | UiAction::PickerConfirm
                | UiAction::PickerCancel
                | UiAction::WorktreeModalConfirm
                | UiAction::WorktreeModalCancel
                | UiAction::CloseModalToggleCompose
                | UiAction::CloseModalToggleWorktree
                | UiAction::CloseModalToggleForce
                | UiAction::CloseModalConfirm
                | UiAction::CloseModalCancel
                | UiAction::SearchCancel
        );
        if one_shot {
            self.consumed.insert(key);
            if repeat {
                return UiAction::Ignore;
            }
        }
        action
    }

    /// Claim form input until release, including when the action closes the dialog.
    pub fn form_press(&mut self, physical: PhysicalKey, logical: &Key, repeat: bool) -> bool {
        self.suppress(physical);
        !repeat || matches!(logical, Key::Named(NamedKey::Backspace))
    }

    /// Consume a press and its repeats until release.
    pub fn suppress(&mut self, key: PhysicalKey) {
        self.consumed.insert(key);
    }

    pub fn release(&mut self, key: PhysicalKey) {
        self.consumed.remove(&key);
    }
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
    let overlay = if action_palette_open {
        Some(ActiveOverlay::ActionPalette)
    } else {
        legacy_overlay(
            picker_open,
            worktree_modal_open,
            close_modal_open,
            search_open,
        )
    };
    translate_overlay_key(key, text, mods, overlay, cfg!(target_os = "macos"))
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
    let overlay = legacy_overlay(
        picker_open,
        worktree_modal_open,
        close_modal_open,
        search_open,
    );
    translate_overlay_key(key, text, mods, overlay, is_macos)
}

fn legacy_overlay(
    picker: bool,
    worktree: bool,
    close: bool,
    search: bool,
) -> Option<ActiveOverlay> {
    [
        (picker, ActiveOverlay::AgentPicker),
        (worktree, ActiveOverlay::Worktree),
        (close, ActiveOverlay::CloseWorkspace),
        (search, ActiveOverlay::Search),
    ]
    .into_iter()
    .find_map(|(open, overlay)| open.then_some(overlay))
}

pub fn translate_overlay_key(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    overlay: Option<ActiveOverlay>,
    is_macos: bool,
) -> UiAction {
    if let Some(overlay) = overlay {
        return modal_keys::translate(overlay, key, text, mods, is_macos);
    }
    translate_workbench_shortcut(key, mods, is_macos)
        .unwrap_or_else(|| terminal_keys::encode(key, text, mods))
}

fn translate_workbench_shortcut(
    key: &Key,
    mods: ModifiersState,
    is_macos: bool,
) -> Option<UiAction> {
    let ctrl = mods.control_key();
    let shift = mods.shift_key();
    let alt = mods.alt_key();

    if mods.super_key() {
        if is_macos && !ctrl && !alt {
            return Some(translate_macos_cmd(key, shift));
        }
        // Never leak OS / window-manager Super shortcuts into the terminal.
        return Some(UiAction::Ignore);
    }

    // Workbench chords
    if alt
        && !ctrl
        && !shift
        && let Key::Character(ch) = key
    {
        match ch.as_str() {
            "1" => return Some(UiAction::FocusRole(PaneRole::Agent)),
            "2" => return Some(UiAction::FocusRole(PaneRole::Service)),
            "3" => return Some(UiAction::FocusRole(PaneRole::Runner)),
            _ => {}
        }
    }

    if ctrl && shift && !alt {
        if matches!(key, Key::Named(NamedKey::Tab)) {
            return Some(UiAction::SwitchWorkspacePrev);
        }
        if !is_macos && let Key::Character(ch) = key {
            return Some(translate_primary_shift_char(ch));
        }
        return Some(UiAction::Ignore);
    }

    if shift
        && !ctrl
        && !alt
        && let Key::Named(named) = key
    {
        match named {
            NamedKey::PageUp => return Some(UiAction::ScrollPageUp),
            NamedKey::PageDown => return Some(UiAction::ScrollPageDown),
            NamedKey::End => return Some(UiAction::ScrollToBottom),
            _ => {}
        }
    }

    if ctrl && !shift && !alt {
        return match key {
            Key::Named(NamedKey::Tab) => Some(UiAction::SwitchWorkspaceNext),
            Key::Character(ch) if !is_macos => match ch.as_str() {
                "=" | "+" => Some(UiAction::FontZoomIn),
                "-" => Some(UiAction::FontZoomOut),
                "0" => Some(UiAction::FontZoomReset),
                _ => None,
            },
            _ => None,
        };
    }
    None
}

/// Windows clipboard shortcuts in the terminal viewport.
pub fn windows_terminal_shortcut(key: &Key, mods: ModifiersState) -> Option<UiAction> {
    if mods != ModifiersState::CONTROL {
        return None;
    }
    let Key::Character(key) = key else {
        return None;
    };
    match key.as_str() {
        "c" | "C" => Some(UiAction::CopySelection),
        "v" | "V" => Some(UiAction::PasteClipboard),
        "." => Some(UiAction::SendBytes(vec![0x03])),
        _ => None,
    }
}

fn translate_primary_shift_char(ch: &str) -> UiAction {
    match ch.to_ascii_lowercase().as_str() {
        "h" => UiAction::FocusDir(FocusDirection::Left),
        "j" => UiAction::FocusDir(FocusDirection::Down),
        "k" => UiAction::FocusDir(FocusDirection::Up),
        "l" => UiAction::FocusDir(FocusDirection::Right),
        "z" => UiAction::ToggleZoom,
        "i" => UiAction::PreviewImage,
        "b" => UiAction::ToggleSidebar,
        "a" => UiAction::ToggleActionPalette,
        "u" => UiAction::OpenSessions,
        "p" => UiAction::ToggleAgentPicker,
        "w" => UiAction::ToggleWorktreeModal,
        "d" => UiAction::OpenCloseWorkspaceModal,
        "x" => UiAction::CloseTerminal,
        "f" => UiAction::ToggleSearch,
        "n" => UiAction::NewWorkspace,
        "s" => UiAction::WorkspaceSettings,
        "c" => UiAction::CopySelection,
        "v" => UiAction::PasteClipboard,
        "e" => UiAction::ForwardFailureToAgent,
        "r" => UiAction::OpenNextFailedRun,
        "t" => UiAction::RunTests,
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

pub fn is_primary_shift_char(
    key: &Key,
    mods: ModifiersState,
    target: &str,
    is_macos: bool,
) -> bool {
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

#[cfg(test)]
mod tests {
    use super::modal_keys::{translate_palette_key, translate_search_key};
    use super::*;

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
    fn windows_clipboard_shortcuts_are_separate_from_interrupt() {
        let control = ModifiersState::CONTROL;
        let shortcut = |key, mods| windows_terminal_shortcut(&char_key(key), mods);
        assert_eq!(shortcut("c", control), Some(UiAction::CopySelection));
        assert_eq!(shortcut("v", control), Some(UiAction::PasteClipboard));
        assert_eq!(shortcut(".", control), Some(UiAction::SendBytes(vec![3])));
        assert_eq!(shortcut("d", control), None);
        assert_eq!(shortcut("c", control | ModifiersState::SHIFT), None);
        assert_eq!(shortcut("v", control | ModifiersState::ALT), None);
        assert_eq!(shortcut("c", ModifiersState::empty()), None);
    }

    #[test]
    fn run_tests_uses_the_platform_workbench_modifier() {
        let key = char_key("T");
        assert_eq!(
            t(&key, None, ModifiersState::CONTROL | ModifiersState::SHIFT),
            UiAction::RunTests
        );
        assert_eq!(
            t_macos(&key, None, ModifiersState::SUPER | ModifiersState::SHIFT),
            UiAction::RunTests
        );
    }

    #[test]
    fn open_next_failed_run_uses_the_platform_workbench_shortcut() {
        let key = char_key("R");
        assert_eq!(
            t(&key, None, ModifiersState::CONTROL | ModifiersState::SHIFT),
            UiAction::OpenNextFailedRun
        );
        assert_eq!(
            t_macos(&key, None, ModifiersState::SUPER | ModifiersState::SHIFT),
            UiAction::OpenNextFailedRun
        );
    }

    #[test]
    fn ctrl_arrow_keys_forward_modified_cursor_sequences_to_the_terminal() {
        for (key, sequence) in [
            (NamedKey::ArrowLeft, b"\x1b[1;5D"),
            (NamedKey::ArrowRight, b"\x1b[1;5C"),
            (NamedKey::ArrowUp, b"\x1b[1;5A"),
            (NamedKey::ArrowDown, b"\x1b[1;5B"),
        ] {
            for is_macos in [false, true] {
                let action = translate_key_for_platform(
                    &named_key(key),
                    None,
                    ModifiersState::CONTROL,
                    false,
                    false,
                    false,
                    false,
                    is_macos,
                );
                assert_eq!(action, UiAction::SendBytes(sequence.to_vec()));
            }
        }
    }

    #[test]
    fn submitting_or_dismissing_a_form_keeps_held_keys_out_of_the_terminal() {
        use winit::keyboard::KeyCode;
        let mut guard = KeyRepeatGuard::default();
        for (code, key, byte) in [
            (KeyCode::Enter, NamedKey::Enter, 13),
            (KeyCode::Escape, NamedKey::Escape, 27),
        ] {
            let physical = PhysicalKey::Code(code);
            assert!(guard.form_press(physical, &Key::Named(key), false));
            assert!(!guard.form_press(physical, &Key::Named(key), true));
            assert_eq!(
                guard.press(physical, true, UiAction::SendBytes(vec![byte])),
                UiAction::Ignore
            );
            guard.release(physical);
            assert_eq!(
                guard.press(physical, false, UiAction::SendBytes(vec![byte])),
                UiAction::SendBytes(vec![byte])
            );
        }
        assert!(guard.form_press(
            PhysicalKey::Code(KeyCode::Backspace),
            &Key::Named(NamedKey::Backspace),
            true
        ));
    }

    #[test]
    fn held_shortcuts_run_once_and_modal_confirmation_stays_consumed_until_release() {
        use winit::keyboard::KeyCode;
        let mut guard = KeyRepeatGuard::default();
        let n = PhysicalKey::Code(KeyCode::KeyN);
        assert_eq!(
            guard.press(n, false, UiAction::NewWorkspace),
            UiAction::NewWorkspace
        );
        assert_eq!(
            guard.press(n, true, UiAction::NewWorkspace),
            UiAction::Ignore
        );
        assert_eq!(
            guard.press(n, true, UiAction::SendBytes(b"n".to_vec())),
            UiAction::Ignore
        );
        let enter = PhysicalKey::Code(KeyCode::Enter);
        assert_eq!(
            guard.press(enter, false, UiAction::ActionPaletteConfirm),
            UiAction::ActionPaletteConfirm
        );
        assert_eq!(
            guard.press(enter, true, UiAction::SendBytes(vec![13])),
            UiAction::Ignore
        );
        guard.release(enter);
        assert_eq!(
            guard.press(enter, false, UiAction::SendBytes(vec![13])),
            UiAction::SendBytes(vec![13])
        );
        assert_eq!(
            guard.press(enter, true, UiAction::SendBytes(vec![13])),
            UiAction::SendBytes(vec![13])
        );
        let down = PhysicalKey::Code(KeyCode::ArrowDown);
        guard.suppress(down);
        assert_eq!(
            guard.press(down, true, UiAction::PickerDown),
            UiAction::Ignore
        );
        guard.release(down);
        assert_eq!(
            guard.press(down, true, UiAction::PickerDown),
            UiAction::PickerDown
        );
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
            ("I", UiAction::PreviewImage),
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
            ("X", UiAction::CloseTerminal),
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
    fn shift_arrows_forward_xterm_modifier_sequences() {
        for (key, expected) in [
            (NamedKey::ArrowUp, b"\x1b[1;2A"),
            (NamedKey::ArrowDown, b"\x1b[1;2B"),
            (NamedKey::ArrowRight, b"\x1b[1;2C"),
            (NamedKey::ArrowLeft, b"\x1b[1;2D"),
        ] {
            assert_eq!(
                t(&named_key(key), None, ModifiersState::SHIFT),
                UiAction::SendBytes(expected.to_vec())
            );
        }
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
    fn search_paste_uses_the_query_on_each_platform() {
        for (macos, primary) in [
            (true, ModifiersState::SUPER),
            (false, ModifiersState::CONTROL),
        ] {
            for mods in [primary, primary | ModifiersState::SHIFT] {
                assert_eq!(
                    translate_search_key(&char_key("V"), None, mods, macos),
                    UiAction::PasteSearchQuery
                );
            }
            assert_eq!(
                translate_search_key(&char_key("v"), None, primary | ModifiersState::ALT, macos),
                UiAction::Ignore
            );
        }
    }

    #[test]
    fn search_mode_captures_query_ime_navigation_and_cancel() {
        assert_eq!(
            translate_search_key(
                &named_key(NamedKey::F4),
                None,
                ModifiersState::empty(),
                false
            ),
            UiAction::SearchToggleCase
        );
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
}
