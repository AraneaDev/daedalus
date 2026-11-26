use super::DaedalusApp;
use daedalus_proto::{MouseTracking, PaneId, PaneRole, PaneStatus, TerminalModes};
use daedalus_ui::bridge::UiCommand;
use daedalus_ui::{layout::HitTarget, state::PaneViewState};
use std::time::Instant;
use winit::keyboard::ModifiersState;

#[derive(Clone, Copy)]
pub(super) struct TerminalPress {
    pane_id: PaneId,
    modes: TerminalModes,
    cell: (u16, u16),
}

#[derive(Clone, Copy)]
enum TerminalMouseEvent {
    Press,
    Release,
    Motion { pressed: bool },
}

fn terminal_mouse_report(
    modes: TerminalModes,
    event: TerminalMouseEvent,
    modifiers: ModifiersState,
    (col, row): (u16, u16),
) -> Option<Vec<u8>> {
    use TerminalMouseEvent::{Motion, Press, Release};
    let button = match (modes.mouse, event) {
        (MouseTracking::Off, _) => return None,
        (_, Press) => 0,
        (MouseTracking::Press, Release) => return None,
        (_, Release) if modes.sgr_mouse => 0,
        (_, Release) => 3,
        (MouseTracking::Drag | MouseTracking::Any, Motion { pressed: true }) => 32,
        (MouseTracking::Any, Motion { pressed: false }) => 35,
        (_, Motion { .. }) => return None,
    };
    let modifiers = if modes.mouse == MouseTracking::Press {
        0
    } else {
        (u8::from(modifiers.shift_key()) * 4)
            | (u8::from(modifiers.alt_key()) * 8)
            | (u8::from(modifiers.control_key()) * 16)
    };
    let code = button | modifiers;
    let x = u32::from(col) + 1;
    let y = u32::from(row) + 1;
    if modes.sgr_mouse {
        let end = if matches!(event, Release) { 'm' } else { 'M' };
        Some(format!("\x1b[<{code};{x};{y}{end}").into_bytes())
    } else {
        Some(vec![
            b'\x1b',
            b'[',
            b'M',
            code + 32,
            u8::try_from(x + 32).ok()?,
            u8::try_from(y + 32).ok()?,
        ])
    }
}

fn live_terminal_modes(pane: &PaneViewState) -> Option<TerminalModes> {
    (pane.scroll_offset == 0
        && pane.requested_scroll_offset == 0
        && !matches!(pane.summary.status, PaneStatus::Exited { .. }))
    .then_some(pane.terminal_modes)
}

fn is_link_click_modifier_for_platform(mods: ModifiersState, is_macos: bool) -> bool {
    if is_macos {
        mods.super_key() && !mods.control_key() && !mods.alt_key()
    } else {
        mods.control_key() && !mods.super_key() && !mods.alt_key()
    }
}

fn is_link_click_modifier(mods: ModifiersState) -> bool {
    is_link_click_modifier_for_platform(mods, cfg!(target_os = "macos"))
}

impl DaedalusApp {
    pub(super) fn handle_terminal_press(&mut self, role: PaneRole, col: u16, row: u16) {
        self.state.focus_clicked_pane(role);
        if is_link_click_modifier(self.modifiers) {
            self.open_clicked_link(role, col, row);
            self.request_redraw();
            return;
        }
        if let Some(pane_id) = self.state.pane_id_for_role(role)
            && let Some(pane) = self.state.panes.get_mut(&pane_id)
        {
            let cell = (col, row);
            if !self.modifiers.shift_key()
                && let Some(modes) = live_terminal_modes(pane)
                && let Some(data) =
                    terminal_mouse_report(modes, TerminalMouseEvent::Press, self.modifiers, cell)
            {
                self.pointer.last_click = None;
                self.pointer.terminal_hover = None;
                if self.send_command(UiCommand::SendInput { pane_id, data }) {
                    self.pointer.terminal_press = Some(TerminalPress {
                        pane_id,
                        modes,
                        cell,
                    });
                }
            } else {
                self.pointer.select_at(pane, col, row, Instant::now());
            }
        }
        self.request_redraw();
    }

    pub(super) fn release_terminal_mouse(&mut self) {
        if let Some(press) = self.pointer.terminal_press.take()
            && let Some(pane) = self.state.panes.get(&press.pane_id)
            && !matches!(pane.summary.status, PaneStatus::Exited { .. })
            && pane.terminal_modes.mouse != MouseTracking::Off
            && pane.terminal_modes.sgr_mouse == press.modes.sgr_mouse
            && let Some(data) = terminal_mouse_report(
                press.modes,
                TerminalMouseEvent::Release,
                self.modifiers,
                press.cell,
            )
        {
            self.send_command(UiCommand::SendInput {
                pane_id: press.pane_id,
                data,
            });
        }
    }

    pub(super) fn report_terminal_motion(&mut self, target: Option<HitTarget>) {
        let Some(HitTarget::PaneViewport { role, col, row }) = target else {
            self.pointer.terminal_hover = None;
            return;
        };
        let Some(pane) = self.state.pane_for_role(role) else {
            return;
        };
        let pane_id = pane.summary.id;
        let Some(modes) = live_terminal_modes(pane) else {
            return;
        };
        let cell = (col, row);
        let pressed = if let Some(press) = &mut self.pointer.terminal_press {
            if press.pane_id != pane_id
                || press.cell == cell
                || (!press.modes.sgr_mouse && (col >= 223 || row >= 223))
            {
                return;
            }
            press.cell = cell;
            true
        } else {
            if self.pointer.terminal_hover == Some((pane_id, col, row))
                || modes.mouse != MouseTracking::Any
                || self.modifiers.shift_key()
            {
                return;
            }
            self.pointer.terminal_hover = Some((pane_id, col, row));
            false
        };
        if let Some(data) = terminal_mouse_report(
            modes,
            TerminalMouseEvent::Motion { pressed },
            self.modifiers,
            cell,
        ) {
            self.send_command(UiCommand::SendInput { pane_id, data });
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn terminal_mouse_formats_legacy_clicks_and_sgr_motion() {
        use TerminalMouseEvent::{Motion, Press, Release};
        let mut modes = TerminalModes {
            mouse: MouseTracking::Button,
            ..TerminalModes::default()
        };
        let modifiers = ModifiersState::empty();
        let cell = (1, 2);
        assert_eq!(
            terminal_mouse_report(modes, Press, modifiers, cell).as_deref(),
            Some(&b"\x1b[M \"#"[..])
        );
        assert_eq!(
            terminal_mouse_report(modes, Release, modifiers, cell).as_deref(),
            Some(&b"\x1b[M#\"#"[..])
        );
        assert!(terminal_mouse_report(modes, Motion { pressed: true }, modifiers, cell).is_none());
        assert!(terminal_mouse_report(modes, Press, modifiers, (223, 0)).is_none());
        modes.mouse = MouseTracking::Drag;
        modes.sgr_mouse = true;
        assert_eq!(
            terminal_mouse_report(modes, Motion { pressed: true }, modifiers, cell).as_deref(),
            Some(&b"\x1b[<32;2;3M"[..])
        );
        assert!(terminal_mouse_report(modes, Motion { pressed: false }, modifiers, cell).is_none());
    }

    #[test]
    fn link_click_modifier_matches_platform_convention() {
        let ctrl = ModifiersState::CONTROL;
        let cmd = ModifiersState::SUPER;

        // Linux uses Ctrl+Click
        assert!(is_link_click_modifier_for_platform(ctrl, false));
        assert!(!is_link_click_modifier_for_platform(cmd, false));
        assert!(!is_link_click_modifier_for_platform(
            ctrl | ModifiersState::ALT,
            false
        ));

        // macOS uses Cmd+Click
        assert!(is_link_click_modifier_for_platform(cmd, true));
        assert!(!is_link_click_modifier_for_platform(ctrl, true));
        assert!(!is_link_click_modifier_for_platform(
            cmd | ModifiersState::ALT,
            true
        ));

        let expected_current = if cfg!(target_os = "macos") {
            is_link_click_modifier(cmd) && !is_link_click_modifier(ctrl)
        } else {
            is_link_click_modifier(ctrl) && !is_link_click_modifier(cmd)
        };
        assert!(expected_current);
    }
}
