use std::path::PathBuf;

mod detect;
mod opener;

pub use detect::{browser_link_at, detect_link_at, detect_link_in_row, detect_url_in_row};
pub use opener::{
    build_open_command, default_system_opener, default_system_opener_for_os, open_link,
};

/// Target detected under a clicked terminal cell.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DetectedLink {
    Url(String),
    File {
        path: PathBuf,
        line: Option<u32>,
        col: Option<u32>,
    },
}

fn is_safe_http_url(url: &str) -> bool {
    let valid_scheme = url
        .get(..8)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("https://"))
        || url
            .get(..7)
            .is_some_and(|prefix| prefix.eq_ignore_ascii_case("http://"));

    valid_scheme && !url.chars().any(char::is_control) && !url.chars().any(char::is_whitespace)
}

#[cfg(test)]
mod tests {
    use super::detect::{canonicalize_existing_file, detect_at};
    use super::opener::{build_open_command_with_env, split_command_tokens_for_os};
    use super::*;
    use daedalus_proto::{Cell, CellFlags, Color, GridRow};
    use tempfile::tempdir;

    #[test]
    fn wrapped_and_labeled_links_use_the_displayed_rows() {
        let row = |text: &str, wraps| GridRow {
            row_index: 0,
            abs_row: 0,
            wraps,
            links: Vec::new(),
            cells: text
                .chars()
                .map(|ch| Cell {
                    text: ch.to_string(),
                    ..Default::default()
                })
                .collect(),
        };
        let mut rows = vec![row("See (https://", true), row("host/a), txt", false)];
        for (y, x) in [(0, 5), (1, 0), (1, 5)] {
            assert_eq!(
                browser_link_at(&rows, y, x),
                Some(DetectedLink::Url("https://host/a".into()))
            );
        }
        assert_eq!(browser_link_at(&rows, 1, 6), None);
        assert_eq!(browser_link_at(&rows, 0, 3), None);
        assert_eq!(browser_link_at(&rows[..1], 0, 5), None);
        rows[0].wraps = false;
        assert_eq!(browser_link_at(&rows, 1, 1), None);
        rows[1].links.push(daedalus_proto::types::LinkSpan {
            start: 8,
            end: 12,
            url: "https://different.example/".into(),
        });
        assert_eq!(
            browser_link_at(&rows, 1, 9),
            Some(DetectedLink::Url("https://different.example/".into()))
        );
        rows[1].links[0].url = "file:///tmp/test".into();
        assert_eq!(browser_link_at(&rows, 1, 9), None);
        assert_eq!(browser_link_at(&rows, 2, 0), None);
        assert_eq!(browser_link_at(&rows, 1, 80), None);
    }

    #[test]
    fn detects_urls_and_strips_trailing_punctuation_with_balanced_parens() {
        let row = "See (https://en.wikipedia.org/wiki/Rust_(programming_language)). and https://example.com/a,";
        let wiki_col = row.find("https://en").unwrap() + 10;
        assert_eq!(
            detect_link_at(row, wiki_col, None, None),
            Some(DetectedLink::Url(
                "https://en.wikipedia.org/wiki/Rust_(programming_language)".to_string()
            ))
        );

        let ex_col = row.find("https://example").unwrap() + 5;
        assert_eq!(
            detect_link_at(row, ex_col, None, None),
            Some(DetectedLink::Url("https://example.com/a".to_string()))
        );

        // Clicking on whitespace or leading prose returns None
        assert_eq!(detect_link_at(row, 0, None, None), None);
        assert_eq!(detect_link_at(row, 3, None, None), None);
    }

    #[test]
    fn detects_markdown_and_quoted_urls() {
        let row =
            r#"Check [docs](https://example.com/docs?x=1&y=2) or "http://localhost:8080/health";"#;
        let docs_col = row.find("https://").unwrap() + 8;
        assert_eq!(
            detect_link_at(row, docs_col, None, None),
            Some(DetectedLink::Url(
                "https://example.com/docs?x=1&y=2".to_string()
            ))
        );

        let local_col = row.find("http://").unwrap() + 7;
        assert_eq!(
            detect_link_at(row, local_col, None, None),
            Some(DetectedLink::Url(
                "http://localhost:8080/health".to_string()
            ))
        );
    }

    #[test]
    fn detects_file_line_col_resolving_cwd_then_root_and_verifying_existence() {
        let root = tempdir().unwrap();
        let sub = root.path().join("crates/ui");
        std::fs::create_dir_all(&sub).unwrap();
        std::fs::create_dir_all(root.path().join("docs")).unwrap();

        let cwd_file = sub.join("main.rs");
        let root_file = root.path().join("docs/guide.md");
        std::fs::write(&cwd_file, "fn main() {}\n").unwrap();
        std::fs::write(&root_file, "# Guide\n").unwrap();

        // Relative to cwd with line + col
        let row1 = "   --> main.rs:42:13";
        let col1 = row1.find("main.rs").unwrap() + 2;
        assert_eq!(
            detect_link_at(row1, col1, Some(&sub), Some(root.path())),
            Some(DetectedLink::File {
                path: canonicalize_existing_file(&cwd_file),
                line: Some(42),
                col: Some(13),
            })
        );

        // Fallback to root_dir when not found in cwd
        let row2 = "See docs/guide.md:10, for details";
        let col2 = row2.find("docs/guide.md").unwrap() + 5;
        assert_eq!(
            detect_link_at(row2, col2, Some(&sub), Some(root.path())),
            Some(DetectedLink::File {
                path: canonicalize_existing_file(&root_file),
                line: Some(10),
                col: None,
            })
        );

        // Ripgrep-style inline snippet `path:line:code`
        let row3 = "main.rs:7:fn main() {}";
        assert_eq!(
            detect_link_at(row3, 3, Some(&sub), Some(root.path())),
            Some(DetectedLink::File {
                path: canonicalize_existing_file(&cwd_file),
                line: Some(7),
                col: None,
            })
        );

        // Non-existent file returns None
        let missing = "   --> nonexistent.rs:12:1";
        assert_eq!(
            detect_link_at(missing, 10, Some(&sub), Some(root.path())),
            None
        );
    }

    #[test]
    fn remote_link_detection_ignores_existing_absolute_paths() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let path = file.path().to_str().unwrap();
        assert!(matches!(
            detect_link_at(path, 0, None, None),
            Some(DetectedLink::File { .. })
        ));
        assert_eq!(detect_at(path, 0, None), None);
    }

    #[test]
    fn detects_link_in_grid_row_with_wide_chars() {
        let cells = vec![
            Cell {
                text: "界".to_string(),
                fg: Color::Default,
                bg: Color::Default,
                flags: CellFlags::empty(),
                width: 2,
            },
            Cell {
                text: String::new(),
                fg: Color::Default,
                bg: Color::Default,
                flags: CellFlags::empty(),
                width: 0,
            },
            Cell::default(),
            Cell {
                text: "h".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "t".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "t".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "p".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "s".to_string(),
                ..Cell::default()
            },
            Cell {
                text: ":".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "/".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "/".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "a".to_string(),
                ..Cell::default()
            },
            Cell {
                text: ".".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "c".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "o".to_string(),
                ..Cell::default()
            },
            Cell {
                text: "m".to_string(),
                ..Cell::default()
            },
        ];
        let row = GridRow {
            links: Vec::new(),
            wraps: false,
            row_index: 0,
            abs_row: 0,
            cells,
        };
        assert_eq!(
            detect_url_in_row(&row, 5),
            detect_link_in_row(&row, 5, None, None),
        );
        assert_eq!(
            detect_link_in_row(&row, 5, None, None),
            Some(DetectedLink::Url("https://a.com".to_string()))
        );
    }

    #[test]
    fn build_open_command_handles_urls_and_rejects_unsafe_schemes() {
        let ok = DetectedLink::Url("https://example.com/a?x=1&y=$(whoami);id".to_string());
        assert_eq!(
            build_open_command_with_env(&ok, None, None, None, false, false),
            Some((
                "xdg-open".to_string(),
                vec!["https://example.com/a?x=1&y=$(whoami);id".to_string()]
            ))
        );
        assert_eq!(
            build_open_command_with_env(&ok, None, None, None, true, false),
            Some((
                "open".to_string(),
                vec!["https://example.com/a?x=1&y=$(whoami);id".to_string()]
            ))
        );
        assert_eq!(
            build_open_command_with_env(&ok, None, None, None, false, true),
            Some((
                "explorer.exe".to_string(),
                vec!["https://example.com/a?x=1&y=$(whoami);id".to_string()]
            ))
        );
        assert_eq!(
            build_open_command(&ok, None),
            Some((
                default_system_opener_for_os(
                    cfg!(target_os = "macos"),
                    cfg!(target_os = "windows")
                )
                .to_string(),
                vec!["https://example.com/a?x=1&y=$(whoami);id".to_string()]
            ))
        );

        let bad = DetectedLink::Url("javascript:alert(1)".to_string());
        assert_eq!(
            build_open_command_with_env(&bad, None, None, None, false, false),
            None
        );
        assert_eq!(
            build_open_command_with_env(&bad, None, None, None, true, false),
            None
        );
        assert_eq!(
            build_open_command_with_env(&bad, None, None, None, false, true),
            None
        );
    }

    #[test]
    fn build_open_command_substitutes_placeholders_and_falls_back_to_env_or_system_opener() {
        assert_eq!(default_system_opener(false), "xdg-open");
        assert_eq!(default_system_opener(true), "open");
        assert_eq!(default_system_opener_for_os(false, false), "xdg-open");
        assert_eq!(default_system_opener_for_os(true, false), "open");
        assert_eq!(default_system_opener_for_os(false, true), "explorer.exe");

        let link = DetectedLink::File {
            path: PathBuf::from("/tmp/my project/main.rs"),
            line: Some(42),
            col: Some(7),
        };

        // Explicit template with placeholders
        assert_eq!(
            build_open_command_with_env(
                &link,
                Some("code --goto {path}:{line}:{col}"),
                None,
                None,
                false,
                false
            ),
            Some((
                "code".to_string(),
                vec![
                    "--goto".to_string(),
                    "/tmp/my project/main.rs:42:7".to_string()
                ]
            ))
        );

        // Quoted binary path + placeholders without {col}
        assert_eq!(
            build_open_command_with_env(
                &link,
                Some("\"/opt/my editor/nvim\" +{line} {path}"),
                None,
                None,
                true,
                false
            ),
            Some((
                "/opt/my editor/nvim".to_string(),
                vec!["+42".to_string(), "/tmp/my project/main.rs".to_string()]
            ))
        );

        // $VISUAL fallback without placeholders (nvim -> +line path)
        assert_eq!(
            build_open_command_with_env(&link, None, Some("nvim"), Some("nano"), false, false),
            Some((
                "nvim".to_string(),
                vec!["+42".to_string(), "/tmp/my project/main.rs".to_string()]
            ))
        );

        // $EDITOR fallback without placeholders (code -> --goto path:line:col)
        assert_eq!(
            build_open_command_with_env(&link, None, None, Some("code"), true, false),
            Some((
                "code".to_string(),
                vec![
                    "--goto".to_string(),
                    "/tmp/my project/main.rs:42:7".to_string()
                ]
            ))
        );

        // Windows `.cmd` / `.exe` editor wrapper suffix still matches editor presets
        assert_eq!(
            build_open_command_with_env(&link, None, None, Some("code.cmd"), false, true),
            Some((
                "code.cmd".to_string(),
                vec![
                    "--goto".to_string(),
                    "/tmp/my project/main.rs:42:7".to_string()
                ]
            ))
        );

        // No editor configured -> platform opener fallback
        assert_eq!(
            build_open_command_with_env(&link, None, None, None, false, false),
            Some((
                "xdg-open".to_string(),
                vec!["/tmp/my project/main.rs".to_string()]
            ))
        );
        assert_eq!(
            build_open_command_with_env(&link, None, None, None, true, false),
            Some((
                "open".to_string(),
                vec!["/tmp/my project/main.rs".to_string()]
            ))
        );
        assert_eq!(
            build_open_command_with_env(&link, None, None, None, false, true),
            Some((
                "explorer.exe".to_string(),
                vec!["/tmp/my project/main.rs".to_string()]
            ))
        );
    }

    #[test]
    fn windows_editor_command_preserves_backslashes() {
        let link = DetectedLink::File {
            path: PathBuf::from(r"C:\Users\tim\repo\main.rs"),
            line: Some(18),
            col: Some(3),
        };

        assert_eq!(
            build_open_command_with_env(&link, Some(r"C:\tools\nvim.exe"), None, None, false, true,),
            Some((
                r"C:\tools\nvim.exe".to_string(),
                vec!["+18".to_string(), r"C:\Users\tim\repo\main.rs".to_string()]
            ))
        );

        assert_eq!(
            split_command_tokens_for_os(
                r#"C:\tools\nvim.exe --server \\.\pipe\nvim +{line} {path}"#,
                true,
            ),
            vec![
                r"C:\tools\nvim.exe".to_string(),
                "--server".to_string(),
                r"\\.\pipe\nvim".to_string(),
                "+{line}".to_string(),
                "{path}".to_string(),
            ]
        );

        assert_eq!(
            split_command_tokens_for_os(
                r#""C:\Program Files\Neovim\bin\nvim.exe" "\\server\share\init.lua""#,
                true,
            ),
            vec![
                r"C:\Program Files\Neovim\bin\nvim.exe".to_string(),
                r"\\server\share\init.lua".to_string(),
            ]
        );
    }
}
