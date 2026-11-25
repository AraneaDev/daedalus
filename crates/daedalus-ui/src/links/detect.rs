use super::{DetectedLink, is_safe_http_url};
use daedalus_proto::GridRow;
use std::path::{Path, PathBuf};

/// Detects an `http(s)://` URL or existing `<path>[:<line>[:<col>]]` at character column `col`
/// within `row_text`.
///
/// Relative file paths are resolved against `cwd` first, then `root_dir`, and must exist as
/// regular files on disk (`is_file()`).
pub fn detect_link_at(
    row_text: &str,
    col: usize,
    cwd: Option<&Path>,
    root_dir: Option<&Path>,
) -> Option<DetectedLink> {
    detect_at(row_text, col, Some((cwd, root_dir)))
}

pub(super) fn detect_at(
    row_text: &str,
    col: usize,
    file_context: Option<(Option<&Path>, Option<&Path>)>,
) -> Option<DetectedLink> {
    let (click_byte, clicked_ch) = row_text.char_indices().nth(col)?;
    if clicked_ch.is_whitespace() {
        return None;
    }

    let tok_start = row_text[..click_byte]
        .char_indices()
        .rev()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(idx, ch)| idx + ch.len_utf8())
        .unwrap_or(0);

    let tok_end = row_text[click_byte..]
        .char_indices()
        .find(|(_, ch)| ch.is_whitespace())
        .map(|(offset, _)| click_byte + offset)
        .unwrap_or(row_text.len());

    let token = &row_text[tok_start..tok_end];
    let rel_click = click_byte - tok_start;

    if let Some(url) = extract_url_at(token, rel_click) {
        return Some(DetectedLink::Url(url));
    }

    let (cwd, root_dir) = file_context?;
    extract_file_at(token, rel_click, cwd, root_dir)
}

/// Detects a link at cell column `col` in a protocol [`GridRow`].
pub fn detect_link_in_row(
    row: &GridRow,
    col: usize,
    cwd: Option<&Path>,
    root_dir: Option<&Path>,
) -> Option<DetectedLink> {
    row_link(row, col, Some((cwd, root_dir)))
}

/// Detects a web link in a remote pane without resolving paths on the client.
pub fn detect_url_in_row(row: &GridRow, col: usize) -> Option<DetectedLink> {
    row_link(row, col, None)
}

/// Resolve the visible portion of a logical line across terminal soft wraps.
pub fn browser_link_at(rows: &[GridRow], row: usize, col: usize) -> Option<DetectedLink> {
    let clicked = rows.get(row)?;
    clicked.cells.get(col)?;
    if let Some(span) = clicked
        .links
        .iter()
        .find(|span| (span.start as usize..span.end as usize).contains(&col))
    {
        return is_safe_http_url(&span.url).then(|| DetectedLink::Url(span.url.clone()));
    }
    if !clicked.wraps && (row == 0 || !rows[row - 1].wraps) {
        return detect_url_in_row(clicked, col);
    }
    let mut x = col;
    if clicked.cells[x].width == 0 {
        x = x.checked_sub(1)?;
    }
    let origin = (row, x);
    let mut y = row;
    let blank = |cell: &daedalus_proto::Cell| {
        cell.width != 0 && (cell.text.is_empty() || cell.text.chars().any(char::is_whitespace))
    };
    if blank(&rows[y].cells[x]) {
        return None;
    }
    // Walk only the clicked token. Hard newlines and word boundaries end it.
    for _ in 0..8192 {
        let previous = if x > 0 {
            (y, x - 1)
        } else if y > 0 && rows[y - 1].wraps {
            (y - 1, rows[y - 1].cells.len().checked_sub(1)?)
        } else {
            break;
        };
        if blank(&rows[previous.0].cells[previous.1]) {
            break;
        }
        (y, x) = previous;
    }
    let mut token = String::new();
    let mut click = None;
    loop {
        let cell = rows.get(y)?.cells.get(x)?;
        if blank(cell) {
            break;
        }
        if (y, x) == origin {
            click = Some(token.len());
        }
        if cell.width != 0 {
            token.push_str(&cell.text);
        }
        if token.len() > 8192 {
            return None;
        }
        x += 1;
        if x == rows[y].cells.len() {
            if !rows[y].wraps {
                break;
            }
            y += 1;
            x = 0;
        }
    }
    extract_url_at(&token, click?).map(DetectedLink::Url)
}

fn row_link(
    row: &GridRow,
    col: usize,
    file_context: Option<(Option<&Path>, Option<&Path>)>,
) -> Option<DetectedLink> {
    if col >= row.cells.len() {
        return None;
    }

    let mut target_idx = col;
    if row.cells[target_idx].width == 0 && target_idx > 0 {
        target_idx -= 1;
    }

    let mut row_text = String::with_capacity(row.cells.len());
    let mut target_char_col = None;
    let mut char_count = 0usize;

    for (idx, cell) in row.cells.iter().enumerate() {
        if cell.width == 0 {
            continue;
        }
        if idx == target_idx {
            target_char_col = Some(char_count);
        }
        let text = if cell.text.is_empty() {
            " "
        } else {
            cell.text.as_str()
        };
        row_text.push_str(text);
        char_count += text.chars().count();
    }

    let char_col = target_char_col?;
    detect_at(&row_text, char_col, file_context)
}

fn is_url_stopper(ch: char) -> bool {
    matches!(ch, '<' | '>' | '"' | '\'' | '`')
}

fn trim_url_trailing_punctuation(candidate: &str) -> &str {
    let mut end = candidate.len();
    while end > 0 {
        let slice = &candidate[..end];
        let Some(last) = slice.chars().next_back() else {
            break;
        };
        let strip = match last {
            '.' | ',' | ';' | ':' | '!' | '?' | '\'' | '"' | '>' | '`' => true,
            ')' => slice.matches(')').count() > slice.matches('(').count(),
            ']' => slice.matches(']').count() > slice.matches('[').count(),
            '}' => slice.matches('}').count() > slice.matches('{').count(),
            _ => false,
        };
        if strip {
            end -= last.len_utf8();
        } else {
            break;
        }
    }
    &candidate[..end]
}

fn extract_url_at(token: &str, rel_click: usize) -> Option<String> {
    let mut search_from = 0usize;
    while search_from < token.len() {
        let rest = &token[search_from..];
        let next_https = rest.find("https://");
        let next_http = rest.find("http://");
        let rel_scheme = match (next_https, next_http) {
            (Some(a), Some(b)) => a.min(b),
            (Some(a), None) => a,
            (None, Some(b)) => b,
            (None, None) => break,
        };
        let scheme_start = search_from + rel_scheme;
        if scheme_start > rel_click {
            break;
        }

        let scheme_len = if token[scheme_start..].starts_with("https://") {
            "https://".len()
        } else {
            "http://".len()
        };

        let raw_end = token[scheme_start..]
            .char_indices()
            .find(|(_, ch)| is_url_stopper(*ch))
            .map(|(offset, _)| scheme_start + offset)
            .unwrap_or(token.len());

        let trimmed = trim_url_trailing_punctuation(&token[scheme_start..raw_end]);
        let trimmed_end = scheme_start + trimmed.len();

        if rel_click >= scheme_start
            && rel_click < trimmed_end
            && trimmed.len() > scheme_len
            && has_valid_url_host(&trimmed[scheme_len..])
        {
            return Some(trimmed.to_string());
        }

        search_from = raw_end.max(scheme_start + scheme_len);
    }

    None
}

fn has_valid_url_host(after_scheme: &str) -> bool {
    let host_part = after_scheme
        .split(['/', '?', '#'])
        .next()
        .unwrap_or_default();
    !host_part.is_empty() && !host_part.starts_with('.')
}

fn is_leading_wrapper(ch: char) -> bool {
    matches!(ch, '(' | '[' | '{' | '<' | '\'' | '"' | '`')
}

fn is_trailing_wrapper_or_punct(ch: char) -> bool {
    matches!(
        ch,
        ')' | ']' | '}' | '>' | '\'' | '"' | '`' | ',' | ';' | ':'
    )
}

fn extract_file_at(
    token: &str,
    rel_click: usize,
    cwd: Option<&Path>,
    root_dir: Option<&Path>,
) -> Option<DetectedLink> {
    let mut start = 0usize;
    for (idx, ch) in token.char_indices() {
        if is_leading_wrapper(ch) {
            start = idx + ch.len_utf8();
        } else {
            break;
        }
    }

    let mut end = token.len();
    while end > start {
        let Some(last) = token[start..end].chars().next_back() else {
            break;
        };
        if is_trailing_wrapper_or_punct(last) {
            end -= last.len_utf8();
        } else {
            break;
        }
    }

    if start >= end || rel_click < start || rel_click >= end {
        return None;
    }

    let candidate = &token[start..end];
    parse_file_candidate(candidate, cwd, root_dir).or_else(|| {
        candidate
            .strip_suffix('.')
            .and_then(|stripped| parse_file_candidate(stripped, cwd, root_dir))
    })
}

fn parse_positive_u32(s: &str) -> Option<u32> {
    if s.is_empty() || !s.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    s.parse::<u32>().ok().filter(|&n| n > 0)
}

fn parse_file_candidate(
    candidate: &str,
    cwd: Option<&Path>,
    root_dir: Option<&Path>,
) -> Option<DetectedLink> {
    // 1. `<path>:<line>:<col>`
    if let Some((prefix, col_str)) = candidate.rsplit_once(':')
        && let Some(col_num) = parse_positive_u32(col_str)
        && let Some((path_str, line_str)) = prefix.rsplit_once(':')
        && let Some(line_num) = parse_positive_u32(line_str)
        && let Some(path) = resolve_existing_file(path_str, cwd, root_dir)
    {
        return Some(DetectedLink::File {
            path,
            line: Some(line_num),
            col: Some(col_num),
        });
    }

    // 2. `<path>:<line>`
    if let Some((path_str, line_str)) = candidate.rsplit_once(':')
        && let Some(line_num) = parse_positive_u32(line_str)
        && let Some(path) = resolve_existing_file(path_str, cwd, root_dir)
    {
        return Some(DetectedLink::File {
            path,
            line: Some(line_num),
            col: None,
        });
    }

    // 3. `<path>:<line>[:<col>]:<inline_text>` (e.g. ripgrep/grep output without spaces)
    for (colon_idx, _) in candidate.match_indices(':') {
        let path_str = &candidate[..colon_idx];
        let rest = &candidate[colon_idx + 1..];
        let mut parts = rest.split(':');
        let Some(line_str) = parts.next() else {
            continue;
        };
        let Some(line_num) = parse_positive_u32(line_str) else {
            continue;
        };
        let col_num = parts.next().and_then(parse_positive_u32);
        if let Some(path) = resolve_existing_file(path_str, cwd, root_dir) {
            return Some(DetectedLink::File {
                path,
                line: Some(line_num),
                col: col_num,
            });
        }
    }

    // 4. Plain `<path>`
    resolve_existing_file(candidate, cwd, root_dir).map(|path| DetectedLink::File {
        path,
        line: None,
        col: None,
    })
}

pub(super) fn canonicalize_existing_file(path: &Path) -> PathBuf {
    daedalus_proto::strip_windows_verbatim_prefix(
        std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf()),
    )
}

fn resolve_existing_file(
    raw_path: &str,
    cwd: Option<&Path>,
    root_dir: Option<&Path>,
) -> Option<PathBuf> {
    if raw_path.is_empty() {
        return None;
    }

    let path = Path::new(raw_path);
    if path.is_absolute() {
        return path.is_file().then(|| canonicalize_existing_file(path));
    }

    if let Some(cwd) = cwd {
        let joined = cwd.join(path);
        if joined.is_file() {
            return Some(canonicalize_existing_file(&joined));
        }
    }

    if let Some(root) = root_dir
        && cwd != Some(root)
    {
        let joined = root.join(path);
        if joined.is_file() {
            return Some(canonicalize_existing_file(&joined));
        }
    }

    None
}
