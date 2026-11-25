use crate::{SemanticEvent, builtin::parse_osc7_file_uri};
use daedalus_proto::types::LinkSpan;
use daedalus_proto::{Cell, CellFlags, Color};
use std::{ffi::c_void, ptr::NonNull};

#[repr(C)]
#[derive(Default, Clone, Copy)]
pub(super) struct Info {
    pub cols: u16,
    pub rows: u16,
    pub x: u16,
    pub y: u16,
    pub visible: bool,
    pub alternate: bool,
    pub bracketed_paste: bool,
    pub history: usize,
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct NativeColor {
    tag: u8,
    r: u8,
    g: u8,
    b: u8,
}

impl NativeColor {
    fn to_color(self) -> Color {
        match self.tag {
            1 => Color::Indexed(self.r),
            2 => Color::Rgb(self.r, self.g, self.b),
            _ => Color::Default,
        }
    }
}

#[repr(C)]
struct NativeCell {
    fg: NativeColor,
    bg: NativeColor,
    flags: u16,
    width: u8,
    scalars: [u32; 256],
    len: usize,
}

unsafe extern "C" {
    fn dd_ghostty_new(cols: u16, rows: u16, lines: usize, user: *mut c_void) -> *mut c_void;
    fn dd_ghostty_row_flags(vt: *mut c_void, y: u32, wrap: *mut bool, links: *mut bool);
    fn dd_ghostty_link(vt: *mut c_void, x: u16, y: u32, buf: *mut u8, capacity: usize) -> usize;
    fn dd_ghostty_free(vt: *mut c_void);
    fn dd_ghostty_write(vt: *mut c_void, data: *const u8, len: usize);
    fn dd_ghostty_resize(vt: *mut c_void, cols: u16, rows: u16) -> bool;
    fn dd_ghostty_info(vt: *mut c_void, info: *mut Info);
    fn dd_ghostty_cell(vt: *mut c_void, x: u16, y: u32, cell: *mut NativeCell) -> bool;
    fn dd_ghostty_anchor(vt: *mut c_void, row: u32) -> bool;
    fn dd_ghostty_anchor_row(vt: *mut c_void, row: *mut u32) -> bool;
}

#[unsafe(no_mangle)]
extern "C" fn dd_ghostty_event(
    user: *mut c_void,
    kind: u32,
    code: i32,
    data: *const u8,
    len: usize,
) {
    let bytes = if len == 0 {
        &[]
    } else {
        // SAFETY: native.c forwards buffers owned by Ghostty for this synchronous callback.
        unsafe { std::slice::from_raw_parts(data, len) }
    };
    let event = match kind {
        0 => SemanticEvent::TerminalReply(bytes.to_vec()),
        1 => match std::str::from_utf8(bytes)
            .ok()
            .and_then(parse_osc7_file_uri)
        {
            Some(path) => SemanticEvent::CwdChanged(path),
            None => return,
        },
        2 => SemanticEvent::PromptStart,
        3 => SemanticEvent::CommandInputStart,
        4 => SemanticEvent::CommandExecuted,
        5 => SemanticEvent::CommandFinished { exit_code: code },
        6 => SemanticEvent::Title(crate::terminal_title(&[bytes])),
        7 => SemanticEvent::Attention,
        _ => return,
    };
    // SAFETY: the terminal owns this stable allocation and callbacks finish before write returns.
    unsafe { &mut *user.cast::<Vec<SemanticEvent>>() }.push(event);
}

pub(super) struct Terminal {
    handle: NonNull<c_void>,
    #[allow(
        clippy::box_collection,
        reason = "Ghostty retains a pointer to the Vec across actor moves"
    )]
    events: Box<Vec<SemanticEvent>>,
}

// SAFETY: each handle owns its terminal and is only accessed from one actor at a time.
// Ghostty has no thread affinity. The callback target moves with the handle via its Box.
unsafe impl Send for Terminal {}

impl Terminal {
    pub fn new(cols: u16, rows: u16, lines: usize) -> Self {
        let mut events = Box::<Vec<SemanticEvent>>::default();
        let user = std::ptr::from_mut(events.as_mut()).cast();
        // SAFETY: the callback target outlives the newly allocated opaque terminal.
        let raw = unsafe { dd_ghostty_new(cols.max(1), rows.max(1), lines, user) };
        Self {
            handle: NonNull::new(raw).expect("Ghostty terminal allocation failed"),
            events,
        }
    }

    pub fn info(&self) -> Info {
        let mut info = Info::default();
        // SAFETY: valid terminal and matching repr(C) output struct.
        unsafe { dd_ghostty_info(self.handle.as_ptr(), &mut info) };
        info
    }

    pub fn write(&mut self, bytes: &[u8]) {
        // SAFETY: the input slice remains valid for the whole synchronous write.
        unsafe { dd_ghostty_write(self.handle.as_ptr(), bytes.as_ptr(), bytes.len()) };
    }

    pub fn take_events(&mut self) -> Vec<SemanticEvent> {
        std::mem::take(&mut self.events)
    }

    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
        // SAFETY: handle is owned and valid, and dimensions are positive.
        unsafe { dd_ghostty_resize(self.handle.as_ptr(), cols.max(1), rows.max(1)) }
    }

    pub fn set_anchor(&mut self, row: u32) -> bool {
        // SAFETY: the native terminal owns and releases its tracked reference.
        unsafe { dd_ghostty_anchor(self.handle.as_ptr(), row) }
    }

    pub fn anchor(&self) -> Option<u32> {
        let mut row = 0;
        // SAFETY: the row out-pointer is writable for the lifetime of the call.
        unsafe { dd_ghostty_anchor_row(self.handle.as_ptr(), &mut row) }.then_some(row)
    }

    pub fn row_links(&self, y: u32, cols: u16) -> (bool, Vec<LinkSpan>) {
        let (mut wrap, mut has_links) = (false, false);
        // SAFETY: both output pointers remain valid for this synchronous call.
        unsafe { dd_ghostty_row_flags(self.handle.as_ptr(), y, &mut wrap, &mut has_links) };
        let mut spans: Vec<LinkSpan> = Vec::new();
        if has_links {
            let mut buf = [0; LinkSpan::MAX_URI_BYTES];
            for x in 0..cols {
                // SAFETY: native writes at most buf.len() bytes and retains no pointer.
                let len = unsafe {
                    dd_ghostty_link(self.handle.as_ptr(), x, y, buf.as_mut_ptr(), buf.len())
                };
                let Some(url) = buf
                    .get(..len)
                    .and_then(|b| std::str::from_utf8(b).ok())
                    .filter(|s| !s.is_empty())
                else {
                    continue;
                };
                if let Some(span) = spans.last_mut()
                    && span.end == x
                    && span.url == url
                {
                    span.end += 1;
                } else if spans.len() < LinkSpan::MAX_PER_ROW {
                    spans.push(LinkSpan {
                        start: x,
                        end: x + 1,
                        url: url.to_owned(),
                    });
                } else {
                    break;
                }
            }
        }
        (wrap, spans)
    }

    pub fn cell(&self, x: u16, y: u32) -> Cell {
        let mut out = NativeCell {
            fg: NativeColor::default(),
            bg: NativeColor::default(),
            flags: 0,
            width: 1,
            scalars: [0; 256],
            len: 0,
        };
        // SAFETY: native.c checks coordinates and writes at most 256 codepoints to this C layout.
        if !unsafe { dd_ghostty_cell(self.handle.as_ptr(), x, y, &mut out) } {
            return Cell::default();
        }
        let mut text = String::new();
        for &point in &out.scalars[..out.len] {
            let ch = char::from_u32(point).unwrap_or(char::REPLACEMENT_CHARACTER);
            if text.len() + ch.len_utf8() > 256 {
                break;
            }
            text.push(ch);
        }
        if text.is_empty() && out.width > 0 {
            text.push(' ');
        }
        Cell {
            text,
            fg: out.fg.to_color(),
            bg: out.bg.to_color(),
            flags: CellFlags::from_bits_truncate(out.flags),
            width: out.width,
        }
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        // SAFETY: unique allocation from dd_ghostty_new; Rust drops the callback target afterward.
        unsafe { dd_ghostty_free(self.handle.as_ptr()) };
    }
}
