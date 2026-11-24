/// The review edits an argv vector on the daemon's platform, which may differ from the UI host.
#[derive(Clone, Copy)]
pub(super) enum CommandLine {
    Windows,
    Posix,
}

impl CommandLine {
    pub fn for_platform(platform: &str) -> Self {
        if platform == "windows" {
            Self::Windows
        } else {
            Self::Posix
        }
    }

    pub fn format(self, argv: &[String]) -> String {
        match self {
            Self::Posix => shell_words::join(argv),
            Self::Windows => argv
                .iter()
                .map(|arg| quote_windows(arg))
                .collect::<Vec<_>>()
                .join(" "),
        }
    }

    pub fn parse(self, text: &str) -> Result<Vec<String>, ()> {
        match self {
            Self::Posix => shell_words::split(text).map_err(|_| ()),
            Self::Windows => parse_windows(text),
        }
    }
}

// Windows CRT quoting doubles backslashes before a closing quote, and escapes literal quotes.
fn quote_windows(arg: &str) -> String {
    if !arg.is_empty() && !arg.contains([' ', '\t', '"']) {
        return arg.to_owned();
    }
    let mut result = String::from("\"");
    let mut slashes = 0;
    for ch in arg.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        let count = if ch == '"' { 2 * slashes + 1 } else { slashes };
        result.extend(std::iter::repeat_n('\\', count));
        result.push(ch);
        slashes = 0;
    }
    result.extend(std::iter::repeat_n('\\', 2 * slashes));
    result.push('"');
    result
}

fn parse_windows(text: &str) -> Result<Vec<String>, ()> {
    let mut chars = text.chars().peekable();
    let mut args = Vec::new();
    while chars.peek().is_some() {
        if chars.next_if(|c| matches!(c, ' ' | '\t')).is_some() {
            continue;
        }
        let mut word = String::new();
        let mut quoted = false;
        while chars.peek().is_some() {
            let mut slashes = 0;
            while chars.next_if_eq(&'\\').is_some() {
                slashes += 1;
            }
            if chars.next_if_eq(&'"').is_some() {
                word.extend(std::iter::repeat_n('\\', slashes / 2));
                if slashes % 2 == 1 || (quoted && chars.next_if_eq(&'"').is_some()) {
                    word.push('"');
                } else {
                    quoted = !quoted;
                }
            } else {
                word.extend(std::iter::repeat_n('\\', slashes));
                match chars.next() {
                    None => break,
                    Some(' ' | '\t') if !quoted => break,
                    Some(ch) => word.push(ch),
                }
            }
        }
        if quoted {
            return Err(());
        }
        args.push(word);
    }
    Ok(args)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_arguments_round_trip() {
        let args = [
            "", "O'Brien", "tool.exe", "a b", "a\tb", r#"a\"b"#, r#"a b\"#, r#"a""b"#,
        ];
        let args = args.map(str::to_owned);
        let syntax = CommandLine::Windows;
        assert_eq!(syntax.parse(&syntax.format(&args)).unwrap(), args);
        assert!(syntax.parse("\"unfinished").is_err());
    }
}
