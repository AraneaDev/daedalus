use daedalus_proto::{CellFlags, Color, PaneId};
use daedalus_vt::{BuiltinVt, VtConfig, VtEngine};

#[test]
fn sgr_changes_apply_only_to_subsequent_characters() {
    let mut vt = BuiltinVt::new(VtConfig::default());
    vt.feed(b"\x1b[1;2;3;4;7;9;31;46mA\x1b[22;23;24;27;29;94;103mB");
    vt.feed(b"\x1b[38;5;140;48;5;225mC\x1b[38;2;1;2;3;48;2;4;5;6mD\x1b[39;49mE");
    let snapshot = vt.snapshot(PaneId::new(), 1);
    let cells = &snapshot.rows_data[0].cells;
    for (i, (fg, bg, flags)) in [
        (Color::Indexed(1), Color::Indexed(6), CellFlags::all()),
        (Color::Indexed(12), Color::Indexed(11), CellFlags::empty()),
        (Color::Indexed(140), Color::Indexed(225), CellFlags::empty()),
        (Color::Rgb(1, 2, 3), Color::Rgb(4, 5, 6), CellFlags::empty()),
        (Color::Default, Color::Default, CellFlags::empty()),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!((cells[i].fg, cells[i].bg, cells[i].flags), (fg, bg, flags));
    }
}

#[test]
fn cursor_motion_clamps_to_the_viewport_and_edits_only_the_addressed_cells() {
    let mut vt = BuiltinVt::new(VtConfig {
        cols: 12,
        rows: 3,
        scrollback_limit: 3,
    });
    vt.feed(b"a\tb\x08c\x1b[99B\x1b[99Cz\x1b[99A\x1b[99DL\x1b[3Gm");
    assert_eq!(vt.row_text_range(0, 0, None), "L m     c");
    assert_eq!(vt.row_text_range(2, 11, Some(20)), "z");
    assert_eq!(vt.row_text_range(2, 11, Some(5)), "");
    vt.feed(b"\x1b[?25l");
    assert!(!vt.cursor().visible);
    vt.feed(b"\x1b[?25h");
    assert!(vt.cursor().visible);
}

#[test]
fn scrolling_region_keeps_header_and_footer_while_erasing_display_to_cursor() {
    let mut vt = BuiltinVt::new(VtConfig {
        cols: 8,
        rows: 4,
        scrollback_limit: 4,
    });
    vt.feed(b"HEAD\r\nold\r\nnew\r\nFOOT\x1b[2;3r\x1b[3;1H\nnext");
    let rows = |vt: &BuiltinVt| {
        vt.snapshot(PaneId::new(), 1)
            .rows_data
            .into_iter()
            .map(|r| vt.row_text_range(r.abs_row, 0, None))
            .collect::<Vec<_>>()
    };
    assert_eq!(rows(&vt), ["HEAD", "new", "next", "FOOT"]);
    assert_eq!(vt.scrollback_len(), 0);
    vt.feed(b"\x1b[3;2H\x1b[1J");
    assert_eq!(rows(&vt), ["", "", "  xt", "FOOT"]);
}
