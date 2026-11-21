use daedalus_proto::AgentState;
use std::time::{Duration, Instant};

const MAX_TAIL_BYTES: usize = 2048;
const APPROVAL_WINDOW_BYTES: usize = 384;
const APPROVAL_WINDOW_LINES: usize = 4;
const QUIET_IDLE_DURATION: Duration = Duration::from_millis(1500);

const APPROVAL_PATTERNS: &[&str] = &[
    "(y/n)",
    "[y/n]",
    "[y/n/a]",
    "allow?",
    "approve",
    "do you want to proceed",
    "run this command",
    "confirm?",
    "press enter to continue",
    "allow once",
    "permission?",
    "permission required",
    "grant permission",
];

const SPINNER_GLYPHS: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];
const THINKING_MARKERS: &[&str] = &["thinking", "working", "analyzing", "generating"];

/// Strips ANSI CSI (`ESC [ ... <final>`), OSC/DCS/PM/APC (`ESC ] ... BEL/ST`),
/// and other 7-bit ESC sequences from raw PTY output while preserving UTF-8 text.
pub fn strip_ansi_csi_osc(bytes: &[u8]) -> String {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == 0x1b {
            i += 1;
            if i >= bytes.len() {
                break;
            }
            match bytes[i] {
                // CSI: ESC [ ... <0x40..=0x7E>
                b'[' => {
                    i += 1;
                    while i < bytes.len() {
                        let b = bytes[i];
                        i += 1;
                        if (0x40..=0x7e).contains(&b) {
                            break;
                        }
                    }
                }
                // OSC and string-terminated controls: ESC ] / P / X / ^ / _ ... (BEL or ESC \)
                b']' | b'P' | b'X' | b'^' | b'_' => {
                    i += 1;
                    while i < bytes.len() {
                        if bytes[i] == 0x07 {
                            i += 1;
                            break;
                        }
                        if bytes[i] == 0x1b && i + 1 < bytes.len() && bytes[i + 1] == b'\\' {
                            i += 2;
                            break;
                        }
                        i += 1;
                    }
                }
                // Charset designation / 3-byte ESC sequences
                b'(' | b')' | b'*' | b'+' | b'#' => {
                    i = (i + 2).min(bytes.len());
                }
                // Other 2-byte ESC sequences
                _ => {
                    i += 1;
                }
            }
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn contains_approval_pattern(text: &str) -> bool {
    let lower = text.to_ascii_lowercase();
    APPROVAL_PATTERNS.iter().any(|pat| lower.contains(pat))
}

fn contains_spinner_or_thinking(text: &str) -> bool {
    if text.chars().any(|c| SPINNER_GLYPHS.contains(&c)) {
        return true;
    }
    let lower = text.to_ascii_lowercase();
    THINKING_MARKERS.iter().any(|marker| lower.contains(marker))
}

fn recent_tail_window(tail: &str) -> &str {
    let mut start = tail.len().saturating_sub(APPROVAL_WINDOW_BYTES);
    while start < tail.len() && !tail.is_char_boundary(start) {
        start += 1;
    }
    let slice = &tail[start..];
    let mut newline_count = 0;
    for (idx, ch) in slice.char_indices().rev() {
        if ch == '\n' {
            newline_count += 1;
            if newline_count >= APPROVAL_WINDOW_LINES {
                return &slice[idx + 1..];
            }
        }
    }
    slice
}

/// Pure, debounced state machine detecting [`AgentState`] for a `PaneRole::Agent` PTY stream.
#[derive(Debug, Clone)]
pub struct AgentStateDetector {
    state: AgentState,
    tail: String,
    last_output: Instant,
    override_state: Option<(AgentState, Instant)>,
}

impl Default for AgentStateDetector {
    fn default() -> Self {
        Self::new()
    }
}

impl AgentStateDetector {
    /// Creates a new detector starting in [`AgentState::Idle`].
    pub fn new() -> Self {
        Self::with_now(Instant::now())
    }

    /// Creates a new detector with an explicit initial timestamp (useful for deterministic tests).
    pub fn with_now(now: Instant) -> Self {
        Self {
            state: AgentState::Idle,
            tail: String::new(),
            last_output: now,
            override_state: None,
        }
    }

    /// Returns the current detected or overridden [`AgentState`].
    pub fn state(&self) -> AgentState {
        self.state
    }

    fn transition(&mut self, next: AgentState) -> Option<AgentState> {
        if self.state != next {
            self.state = next;
            Some(next)
        } else {
            None
        }
    }

    fn push_tail(&mut self, chunk: &str) {
        self.tail.push_str(chunk);
        if self.tail.len() > MAX_TAIL_BYTES {
            let mut cut = self.tail.len() - MAX_TAIL_BYTES;
            while cut < self.tail.len() && !self.tail.is_char_boundary(cut) {
                cut += 1;
            }
            self.tail.drain(..cut);
        }
    }

    /// Observes raw PTY output bytes at `now`, returning `Some(new_state)` only on transitions.
    pub fn observe_output(&mut self, bytes: &[u8], now: Instant) -> Option<AgentState> {
        let clean = strip_ansi_csi_osc(bytes);
        if clean.is_empty() {
            return None;
        }

        self.last_output = now;

        let clean_has_approval = contains_approval_pattern(&clean);
        let clean_has_active_marker = contains_spinner_or_thinking(&clean);
        let clean_has_substantive = clean.chars().any(|c| !c.is_whitespace());

        // Once the agent resumes spinning/thinking after an approval prompt, clear the old
        // tail window so the previous prompt does not keep re-triggering WaitingForApproval.
        if self.state == AgentState::WaitingForApproval
            && clean_has_active_marker
            && !clean_has_approval
        {
            self.tail.clear();
        }
        self.push_tail(&clean);

        if let Some((_, expires_at)) = self.override_state {
            if now < expires_at {
                return None;
            }
            self.override_state = None;
        }

        let window = recent_tail_window(&self.tail);
        let next_state = if contains_approval_pattern(window) {
            AgentState::WaitingForApproval
        } else if clean_has_active_marker || clean_has_substantive {
            AgentState::Thinking
        } else {
            return None;
        };

        self.transition(next_state)
    }

    /// Advances quiet-time and override expiration checks at `now`.
    pub fn observe_tick(&mut self, now: Instant) -> Option<AgentState> {
        if let Some((_, expires_at)) = self.override_state {
            if now < expires_at {
                return None;
            }
            self.override_state = None;
        }

        if self.state == AgentState::Thinking
            && now.saturating_duration_since(self.last_output) >= QUIET_IDLE_DURATION
        {
            return self.transition(AgentState::Idle);
        }
        None
    }

    /// Updates state on agent process exit (`0` -> `Idle`, non-zero -> `Error`).
    pub fn observe_exit(&mut self, exit_code: Option<i32>) -> Option<AgentState> {
        self.override_state = None;
        let next = match exit_code {
            Some(0) | None => AgentState::Idle,
            Some(_) => AgentState::Error,
        };
        self.transition(next)
    }

    /// Applies an explicit state override for `ttl`, returning `Some(state)` if it caused a transition.
    pub fn set_override(
        &mut self,
        state: AgentState,
        now: Instant,
        ttl: Duration,
    ) -> Option<AgentState> {
        let expires_at = now.checked_add(ttl).unwrap_or(now);
        self.override_state = Some((state, expires_at));
        if state != AgentState::WaitingForApproval {
            self.tail.clear();
        }
        self.transition(state)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strips_ansi_csi_osc_and_preserves_utf8_spinners() {
        let raw = b"\x1b]0;agent title\x07\x1b[38;5;214m\xe2\xa0\x8b Thinking...\x1b[0m\x1b]8;;https://example.com\x1b\\link\x1b]8;;\x1b\\";
        assert_eq!(strip_ansi_csi_osc(raw), "⠋ Thinking...link");
    }

    #[test]
    fn transitions_thinking_to_idle_after_quiet_tick() {
        let t0 = Instant::now();
        let mut det = AgentStateDetector::with_now(t0);
        assert_eq!(det.state(), AgentState::Idle);

        // Whitespace / empty ANSI alone keeps Idle
        assert_eq!(det.observe_output(b"\x1b[2K\r\n", t0), None);
        assert_eq!(det.state(), AgentState::Idle);

        // Spinner or streaming output transitions Idle -> Thinking
        assert_eq!(
            det.observe_output("⠋ Analyzing codebase...".as_bytes(), t0),
            Some(AgentState::Thinking)
        );
        // Repeated output while already Thinking emits no duplicate transition
        assert_eq!(
            det.observe_output(b"more tokens streaming", t0 + Duration::from_millis(500)),
            None
        );

        // Quiet for < 1.5s stays Thinking
        assert_eq!(det.observe_tick(t0 + Duration::from_millis(1999)), None);
        assert_eq!(det.state(), AgentState::Thinking);

        // Quiet >= 1.5s transitions Thinking -> Idle
        assert_eq!(
            det.observe_tick(t0 + Duration::from_millis(2000)),
            Some(AgentState::Idle)
        );
        assert_eq!(det.state(), AgentState::Idle);
    }

    #[test]
    fn detects_approval_prompts_across_chunks_and_does_not_idle_on_tick() {
        let t0 = Instant::now();
        let mut det = AgentStateDetector::with_now(t0);

        // Split prompt across two chunks
        assert_eq!(
            det.observe_output(b"Do you want ", t0),
            Some(AgentState::Thinking)
        );
        assert_eq!(
            det.observe_output(b"to proceed? [Y/n]", t0 + Duration::from_millis(50)),
            Some(AgentState::WaitingForApproval)
        );

        // WaitingForApproval does NOT time out to Idle on quiet tick
        assert_eq!(det.observe_tick(t0 + Duration::from_secs(10)), None);
        assert_eq!(det.state(), AgentState::WaitingForApproval);

        // Resuming spinner transitions back to Thinking
        assert_eq!(
            det.observe_output("⠙ Working...".as_bytes(), t0 + Duration::from_secs(11)),
            Some(AgentState::Thinking)
        );
    }

    #[test]
    fn detects_all_configured_approval_phrases_case_insensitively() {
        let t0 = Instant::now();
        for phrase in [
            "Continue (y/n)",
            "Apply patch [y/n]",
            "Run tool [y/n/a]",
            "Allow? yes/no",
            "Please APPROVE this edit",
            "Do you want to proceed?",
            "Run this command?",
            "Confirm? [enter]",
            "Press Enter to continue",
            "Allow once for this session",
        ] {
            let mut det = AgentStateDetector::with_now(t0);
            assert_eq!(
                det.observe_output(phrase.as_bytes(), t0),
                Some(AgentState::WaitingForApproval),
                "expected WaitingForApproval for phrase: {phrase}"
            );
        }
    }

    #[test]
    fn exit_transitions_to_error_on_nonzero_and_idle_on_zero() {
        let t0 = Instant::now();
        let mut det = AgentStateDetector::with_now(t0);
        det.observe_output(b"working...", t0);

        assert_eq!(det.observe_exit(Some(1)), Some(AgentState::Error));
        assert_eq!(det.observe_exit(Some(1)), None);
        assert_eq!(det.observe_exit(Some(0)), Some(AgentState::Idle));
        assert_eq!(det.observe_exit(Some(0)), None);
    }

    #[test]
    fn explicit_override_takes_precedence_until_expiry() {
        let t0 = Instant::now();
        let mut det = AgentStateDetector::with_now(t0);

        assert_eq!(
            det.set_override(AgentState::WaitingForApproval, t0, Duration::from_secs(30)),
            Some(AgentState::WaitingForApproval)
        );
        // Same override again emits no transition
        assert_eq!(
            det.set_override(AgentState::WaitingForApproval, t0, Duration::from_secs(30)),
            None
        );

        // Heuristic output while override is active must not overwrite the override
        assert_eq!(
            det.observe_output(b"analyzing files...", t0 + Duration::from_secs(1)),
            None
        );
        assert_eq!(det.state(), AgentState::WaitingForApproval);

        // Tick before expiry keeps override
        assert_eq!(det.observe_tick(t0 + Duration::from_secs(29)), None);
        assert_eq!(det.state(), AgentState::WaitingForApproval);

        // After TTL expires, heuristics resume on next output
        assert_eq!(
            det.observe_output(b"analyzing files...", t0 + Duration::from_secs(31)),
            Some(AgentState::Thinking)
        );
        assert_eq!(det.state(), AgentState::Thinking);
    }

    #[test]
    fn tail_buffer_stays_bounded_and_utf8_safe() {
        let t0 = Instant::now();
        let mut det = AgentStateDetector::with_now(t0);
        let chunk = "⠋".repeat(1500); // 4500 bytes (> MAX_TAIL_BYTES)
        det.observe_output(chunk.as_bytes(), t0);
        assert!(det.tail.len() <= MAX_TAIL_BYTES);
        assert!(std::str::from_utf8(det.tail.as_bytes()).is_ok());
    }
}
