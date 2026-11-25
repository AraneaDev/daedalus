use super::{UiAction, is_primary_shift_char};
use crate::state::ActiveOverlay;
use winit::keyboard::{Key, ModifiersState, NamedKey};

/// Filters composed modal text, stripping control characters
/// and optionally stripping whitespace (e.g. for Git branch names).
pub fn filter_modal_text(raw: &str, allow_whitespace: bool) -> Option<String> {
    let filtered: String = raw
        .chars()
        .filter(|ch| !ch.is_control() && (allow_whitespace || !ch.is_whitespace()))
        .collect();
    (!filtered.is_empty()).then_some(filtered)
}

pub(super) fn translate(
    overlay: ActiveOverlay,
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    is_macos: bool,
) -> UiAction {
    match overlay {
        ActiveOverlay::AgentPicker => translate_picker_key(key, mods, is_macos),
        ActiveOverlay::Worktree => translate_worktree_modal_key(key, text, mods, is_macos),
        ActiveOverlay::CloseWorkspace => translate_close_modal_key(key, mods),
        ActiveOverlay::Search => translate_search_key(key, text, mods, is_macos),
        ActiveOverlay::ActionPalette => translate_palette_key(key, mods, is_macos),
        ActiveOverlay::FailureRecipients
        | ActiveOverlay::SetupReview
        | ActiveOverlay::WorkspaceForm
        | ActiveOverlay::Settings => UiAction::Ignore,
    }
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

pub(super) fn translate_palette_key(key: &Key, mods: ModifiersState, is_macos: bool) -> UiAction {
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
        Key::Named(NamedKey::Enter) if mods.shift_key() => UiAction::PickerAdd,
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

pub(super) fn translate_search_key(
    key: &Key,
    text: Option<&str>,
    mods: ModifiersState,
    is_macos: bool,
) -> UiAction {
    if is_search_toggle_chord(key, mods, is_macos) {
        return UiAction::ToggleSearch;
    }

    let primary = if is_macos {
        ModifiersState::SUPER
    } else {
        ModifiersState::CONTROL
    };
    if mods.difference(ModifiersState::SHIFT) == primary
        && matches!(key, Key::Character(ch) if ch.eq_ignore_ascii_case("v"))
    {
        return UiAction::PasteSearchQuery;
    }

    if mods.control_key() || mods.alt_key() || mods.super_key() {
        return UiAction::Ignore;
    }

    match key {
        Key::Named(NamedKey::Escape) => UiAction::SearchCancel,
        Key::Named(NamedKey::F4) => UiAction::SearchToggleCase,
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
