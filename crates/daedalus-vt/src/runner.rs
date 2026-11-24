use daedalus_proto::RunnerInsight;
use std::time::Instant;
use vte::{Parser, Perform};

pub(crate) struct RunnerObserver {
    parser: Parser,
    lines: CargoLines,
    started: Option<Instant>,
}

#[derive(Default)]
struct CargoLines {
    insight: RunnerInsight,
    line: String,
    suite_passed: u32,
    suite_failed: u32,
}

impl Perform for CargoLines {
    fn print(&mut self, c: char) {
        if self.line.len() < 2048 {
            self.line.push(c);
        }
    }
    fn execute(&mut self, byte: u8) {
        if matches!(byte, b'\n' | b'\r') {
            self.complete_line();
        }
    }
}

impl CargoLines {
    fn complete_line(&mut self) {
        let line = self.line.trim();
        if let Some(name) = line
            .strip_prefix("Compiling ")
            .or_else(|| line.strip_prefix("Checking "))
        {
            self.insight.phase = format!(
                "{} {}",
                line.split_whitespace().next().unwrap_or(""),
                name.split_whitespace().next().unwrap_or("")
            );
        } else if let Some(summary) = line.strip_prefix("test result: ") {
            for field in summary
                .split_once('.')
                .map_or("", |(_, counts)| counts)
                .split(';')
            {
                let mut words = field.split_whitespace();
                if let Some(n) = words.next().and_then(|w| w.parse::<u32>().ok()) {
                    match words.next() {
                        Some("passed") => {
                            self.insight.passed = self
                                .insight
                                .passed
                                .saturating_sub(self.suite_passed)
                                .saturating_add(n)
                        }
                        Some("failed") => {
                            self.insight.failed = self
                                .insight
                                .failed
                                .saturating_sub(self.suite_failed)
                                .saturating_add(n)
                        }
                        _ => {}
                    }
                }
            }
            self.suite_passed = 0;
            self.suite_failed = 0;
        } else if line.starts_with("test ") && line.ends_with(" ... ok") {
            self.suite_passed = self.suite_passed.saturating_add(1);
            self.insight.passed = self.insight.passed.saturating_add(1);
            self.insight.phase = "Testing".into();
        } else if line.starts_with("test ") && line.ends_with(" ... FAILED") {
            self.suite_failed = self.suite_failed.saturating_add(1);
            self.insight.failed = self.insight.failed.saturating_add(1);
            self.insight.diagnostic.get_or_insert_with(|| line.into());
        } else if line.starts_with("error[") || line.starts_with("error:") {
            self.insight.diagnostic.get_or_insert_with(|| line.into());
        } else if line.starts_with("warning:") && !line.contains(" generated ") {
            self.insight.warnings = self.insight.warnings.saturating_add(1);
        }
        self.line.clear();
    }
}

impl RunnerObserver {
    pub fn new() -> Self {
        Self {
            parser: Parser::new(),
            lines: CargoLines::default(),
            started: Some(Instant::now()),
        }
    }
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.started.is_some() {
            self.parser.advance(&mut self.lines, bytes);
        }
    }
    pub fn snapshot(&self) -> RunnerInsight {
        let mut insight = self.lines.insight.clone();
        if let Some(start) = self.started {
            insight.elapsed_secs = start.elapsed().as_secs();
        }
        insight
    }
    pub fn finish(&mut self, code: Option<i32>) {
        if let Some(start) = self.started.take() {
            self.lines.complete_line();
            self.lines.insight.elapsed_secs = start.elapsed().as_secs();
            self.lines.insight.phase = match code {
                Some(0) => "Passed",
                Some(_) => "Failed",
                None => "Stopped",
            }
            .into();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn aggregates_suites_and_waits_for_exit() {
        let mut observer = RunnerObserver::new();
        for bytes in b"\x1b[32mCompiling foo\x1b[0m\r\ntest a ... ok\ntest result: ok. 1 passed; 0 failed;\ntest b ... FAILED\ntest result: FAILED. 0 passed; 1 failed;\n".chunks(3) { observer.feed(bytes); }
        let state = observer.snapshot();
        assert_eq!((state.passed, state.failed), (1, 1));
        assert_ne!(state.phase, "Passed");
        observer.finish(Some(1));
        assert_eq!(observer.snapshot().phase, "Failed");
    }
}
