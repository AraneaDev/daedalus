use daedalus_proto::{MouseTracking, TerminalModes};
use vte::Params;

pub(crate) fn apply_dec_mode(
    modes: &mut TerminalModes,
    parameters: &Params,
    intermediates: &[u8],
    ignore: bool,
    action: char,
) {
    if ignore || intermediates != b"?" || !matches!(action, 'h' | 'l') {
        return;
    }
    let enabled = action == 'h';
    for parameter in parameters {
        let [mode] = parameter else {
            continue;
        };
        let tracking = match mode {
            9 => MouseTracking::Press,
            1000 => MouseTracking::Button,
            1002 => MouseTracking::Drag,
            1003 => MouseTracking::Any,
            1006 => {
                modes.sgr_mouse = enabled;
                continue;
            }
            2004 => {
                modes.bracketed_paste = enabled;
                continue;
            }
            _ => continue,
        };
        if enabled {
            modes.mouse = tracking;
        } else {
            modes.mouse = MouseTracking::Off;
        }
    }
}

pub(crate) fn reset_on_ris(
    modes: &mut TerminalModes,
    intermediates: &[u8],
    ignore: bool,
    byte: u8,
) {
    if !ignore && intermediates.is_empty() && byte == b'c' {
        *modes = TerminalModes::default();
    }
}

// Ghostty exposes whether tracking is active but keeps the effective mouse
// encoding inside its encoder. Observe the requested tracking and format
// without reaching into its private VT structs.
#[cfg(feature = "ghostty-vt")]
pub(crate) struct ModeObserver {
    parser: vte::Parser,
    modes: TerminalModes,
}

#[cfg(feature = "ghostty-vt")]
impl ModeObserver {
    pub(crate) fn new() -> Self {
        Self {
            parser: vte::Parser::new(),
            modes: TerminalModes::default(),
        }
    }

    pub(crate) fn feed(&mut self, bytes: &[u8]) {
        struct ModePerformer<'a>(&'a mut TerminalModes);

        impl vte::Perform for ModePerformer<'_> {
            fn csi_dispatch(
                &mut self,
                parameters: &Params,
                intermediates: &[u8],
                ignore: bool,
                action: char,
            ) {
                apply_dec_mode(self.0, parameters, intermediates, ignore, action);
            }

            fn esc_dispatch(&mut self, intermediates: &[u8], ignore: bool, byte: u8) {
                reset_on_ris(self.0, intermediates, ignore, byte);
            }
        }

        self.parser
            .advance(&mut ModePerformer(&mut self.modes), bytes);
    }

    pub(crate) fn modes(&self) -> TerminalModes {
        self.modes
    }
}
