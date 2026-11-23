// Typed boundary for libghostty's C API, whose generic getters accept void*.
#include <ghostty/vt.h>
#include <stdlib.h>

typedef struct {
    uint16_t cols, rows, x, y;
    bool visible, alternate;
    size_t history;
} DdInfo;

typedef struct {
    uint8_t tag, r, g, b;
} DdColor;

typedef struct {
    DdColor fg, bg;
    uint16_t flags;
    uint8_t width;
    uint32_t scalars[256];
    size_t len;
} DdCell;

typedef struct {
    GhosttyTerminal vt;
    GhosttyTrackedGridRef anchor;
} DdTerminal;

enum { EVENT_REPLY, EVENT_PWD, EVENT_PROMPT, EVENT_INPUT, EVENT_OUTPUT, EVENT_END };
extern void dd_ghostty_event(void*, unsigned int, int32_t, const uint8_t*, size_t);

static void write_pty(GhosttyTerminal vt, void *ud, const uint8_t *p, size_t n) {
    (void)vt;
    dd_ghostty_event(ud, EVENT_REPLY, 0, p, n);
}

static void pwd(GhosttyTerminal vt, void *ud) {
    GhosttyString path;
    if (ghostty_terminal_get(vt, GHOSTTY_TERMINAL_DATA_PWD, &path) == GHOSTTY_SUCCESS && path.len)
        dd_ghostty_event(ud, EVENT_PWD, 0, path.ptr, path.len);
}

static void prompt(GhosttyTerminal vt, void *ud, const GhosttyTerminalSemanticPrompt *p) {
    (void)vt;
    unsigned event;
    switch (p->kind) {
    case GHOSTTY_SEMANTIC_PROMPT_PROMPT_START: event = EVENT_PROMPT; break;
    case GHOSTTY_SEMANTIC_PROMPT_INPUT_START: event = EVENT_INPUT; break;
    case GHOSTTY_SEMANTIC_PROMPT_OUTPUT_START: event = EVENT_OUTPUT; break;
    case GHOSTTY_SEMANTIC_PROMPT_COMMAND_END: event = EVENT_END; break;
    default: return;
    }
    dd_ghostty_event(ud, event, p->has_exit_code ? p->exit_code : 0, NULL, 0);
}

void dd_ghostty_free(DdTerminal *t) {
    if (t->anchor) ghostty_tracked_grid_ref_free(t->anchor);
    if (t->vt) ghostty_terminal_free(t->vt);
    free(t);
}

DdTerminal *dd_ghostty_new(uint16_t cols, uint16_t rows, size_t lines, void *user) {
    DdTerminal *t = calloc(1, sizeof(*t));
    if (!t) return NULL;
    size_t bytes = lines ? SIZE_MAX : 0;
    if (ghostty_terminal_new(NULL, &t->vt, cols, rows) != GHOSTTY_SUCCESS ||
        ghostty_terminal_set(t->vt, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_LINES, &lines) != GHOSTTY_SUCCESS ||
        ghostty_terminal_set(t->vt, GHOSTTY_TERMINAL_OPT_SCROLLBACK_MAX_BYTES, &bytes) != GHOSTTY_SUCCESS ||
        ghostty_terminal_set(t->vt, GHOSTTY_TERMINAL_OPT_USERDATA, user) != GHOSTTY_SUCCESS ||
        ghostty_terminal_set(t->vt, GHOSTTY_TERMINAL_OPT_PWD_CHANGED, (const void*)pwd) != GHOSTTY_SUCCESS ||
        ghostty_terminal_set(t->vt, GHOSTTY_TERMINAL_OPT_SEMANTIC_PROMPT, (const void*)prompt) != GHOSTTY_SUCCESS ||
        ghostty_terminal_set(t->vt, GHOSTTY_TERMINAL_OPT_WRITE_PTY, (const void*)write_pty) != GHOSTTY_SUCCESS) {
        dd_ghostty_free(t);
        return NULL;
    }
    return t;
}

void dd_ghostty_write(DdTerminal *t, const uint8_t *p, size_t n) {
    ghostty_terminal_vt_write(t->vt, p, n);
}

bool dd_ghostty_resize(DdTerminal *t, uint16_t cols, uint16_t rows) {
    return ghostty_terminal_resize(t->vt, cols, rows, 1, 1) == GHOSTTY_SUCCESS;
}

void dd_ghostty_info(DdTerminal *t, DdInfo *i) {
    GhosttyTerminalScreen screen = GHOSTTY_TERMINAL_SCREEN_PRIMARY;
    ghostty_terminal_get(t->vt, GHOSTTY_TERMINAL_DATA_ACTIVE_SCREEN, &screen);
    i->alternate = screen == GHOSTTY_TERMINAL_SCREEN_ALTERNATE;
    ghostty_terminal_get(t->vt, GHOSTTY_TERMINAL_DATA_COLS, &i->cols);
    ghostty_terminal_get(t->vt, GHOSTTY_TERMINAL_DATA_ROWS, &i->rows);
    ghostty_terminal_get(t->vt, GHOSTTY_TERMINAL_DATA_CURSOR_X, &i->x);
    ghostty_terminal_get(t->vt, GHOSTTY_TERMINAL_DATA_CURSOR_Y, &i->y);
    ghostty_terminal_get(t->vt, GHOSTTY_TERMINAL_DATA_CURSOR_VISIBLE, &i->visible);
    ghostty_terminal_get(t->vt, GHOSTTY_TERMINAL_DATA_SCROLLBACK_ROWS, &i->history);
}

bool dd_ghostty_anchor(DdTerminal *t, uint32_t row) {
    GhosttyPoint p = {.tag = GHOSTTY_POINT_TAG_SCREEN, .value.coordinate = {.x = 0, .y = row}};
    if (t->anchor) return ghostty_tracked_grid_ref_set(t->anchor, t->vt, p) == GHOSTTY_SUCCESS;
    return ghostty_terminal_grid_ref_track(t->vt, p, &t->anchor) == GHOSTTY_SUCCESS;
}

bool dd_ghostty_anchor_row(DdTerminal *t, uint32_t *row) {
    GhosttyPointCoordinate p;
    if (!t->anchor || ghostty_tracked_grid_ref_point(t->anchor, GHOSTTY_POINT_TAG_SCREEN, &p) != GHOSTTY_SUCCESS)
        return false;
    *row = p.y;
    return true;
}

static DdColor color(GhosttyStyleColor c) {
    switch (c.tag) {
    case GHOSTTY_STYLE_COLOR_RGB: return (DdColor){2, c.value.rgb.r, c.value.rgb.g, c.value.rgb.b};
    case GHOSTTY_STYLE_COLOR_PALETTE: return (DdColor){1, c.value.palette, 0, 0};
    default: return (DdColor){0};
    }
}

bool dd_ghostty_cell(DdTerminal *t, uint16_t x, uint32_t y, DdCell *out) {
    GhosttyGridRef ref = {0};
    GhosttyPoint p = {.tag = GHOSTTY_POINT_TAG_SCREEN, .value.coordinate = {.x = x, .y = y}};
    GhosttyCell cell;
    GhosttyCellWide wide;
    GhosttyStyle style;
    if (ghostty_terminal_grid_ref(t->vt, p, &ref) != GHOSTTY_SUCCESS ||
        ghostty_grid_ref_cell(&ref, &cell) != GHOSTTY_SUCCESS ||
        ghostty_cell_get(cell, GHOSTTY_CELL_DATA_WIDE, &wide) != GHOSTTY_SUCCESS ||
        ghostty_grid_ref_style(&ref, &style) != GHOSTTY_SUCCESS) return false;
    out->width = wide == GHOSTTY_CELL_WIDE_WIDE ? 2 : wide == GHOSTTY_CELL_WIDE_SPACER_TAIL ? 0 : 1;
    out->fg = color(style.fg_color);
    out->bg = color(style.bg_color);
    // Ghostty can store the background directly in an otherwise empty cell.
    GhosttyCellContentTag content;
    if (ghostty_cell_get(cell, GHOSTTY_CELL_DATA_CONTENT_TAG, &content) != GHOSTTY_SUCCESS)
        return false;
    if (content == GHOSTTY_CELL_CONTENT_BG_COLOR_RGB) {
        GhosttyColorRgb rgb;
        if (ghostty_cell_get(cell, GHOSTTY_CELL_DATA_COLOR_RGB, &rgb) != GHOSTTY_SUCCESS) return false;
        out->bg = (DdColor){2, rgb.r, rgb.g, rgb.b};
    } else if (content == GHOSTTY_CELL_CONTENT_BG_COLOR_PALETTE) {
        GhosttyColorPaletteIndex index;
        if (ghostty_cell_get(cell, GHOSTTY_CELL_DATA_COLOR_PALETTE, &index) != GHOSTTY_SUCCESS) return false;
        out->bg = (DdColor){1, index, 0, 0};
    }
    out->flags = ((style.flags & GHOSTTY_STYLE_FLAG_BOLD) ? 1 : 0) |
        ((style.flags & GHOSTTY_STYLE_FLAG_ITALIC) ? 2 : 0) |
        (GHOSTTY_STYLE_UNDERLINE(style) ? 4 : 0) |
        ((style.flags & GHOSTTY_STYLE_FLAG_INVERSE) ? 8 : 0) |
        ((style.flags & GHOSTTY_STYLE_FLAG_FAINT) ? 16 : 0) |
        ((style.flags & GHOSTTY_STYLE_FLAG_STRIKETHROUGH) ? 32 : 0);
    GhosttyResult r = ghostty_grid_ref_graphemes(&ref, out->scalars, 256, &out->len);
    if (r == GHOSTTY_OUT_OF_SPACE) {
        // Keep the base character when an overlong cluster exceeds the IPC cell budget.
        out->len = 1;
        return ghostty_cell_get(cell, GHOSTTY_CELL_DATA_CODEPOINT, out->scalars) == GHOSTTY_SUCCESS;
    }
    return r == GHOSTTY_SUCCESS;
}
