use std::time::{Duration, Instant};

use super::terminal::TerminalCursorState;

pub(crate) const CURSOR_POSITION_SETTLE: Duration = Duration::from_millis(20);
const CURSOR_POSITION_MAX_HOLD: Duration = Duration::from_millis(100);

#[derive(Debug, Default)]
pub(crate) struct DecscusrTracker {
    state: DecscusrParseState,
    cursor_shape_overridden: bool,
}

#[derive(Debug, Default)]
enum DecscusrParseState {
    #[default]
    Ground,
    Escape,
    Csi {
        first_param: Option<u16>,
        collecting_first_param: bool,
        has_space_intermediate: bool,
    },
}

impl DecscusrTracker {
    pub(crate) fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            self.observe_byte(byte);
        }
    }

    fn observe_byte(&mut self, byte: u8) {
        match &mut self.state {
            DecscusrParseState::Ground => {
                if byte == 0x1b {
                    self.state = DecscusrParseState::Escape;
                }
            }
            DecscusrParseState::Escape => {
                self.state = if byte == b'[' {
                    DecscusrParseState::Csi {
                        first_param: None,
                        collecting_first_param: true,
                        has_space_intermediate: false,
                    }
                } else if byte == 0x1b {
                    DecscusrParseState::Escape
                } else {
                    DecscusrParseState::Ground
                };
            }
            DecscusrParseState::Csi {
                first_param,
                collecting_first_param,
                has_space_intermediate,
            } => {
                if byte == 0x1b {
                    self.state = DecscusrParseState::Escape;
                } else if byte.is_ascii_digit() && *collecting_first_param {
                    let digit = u16::from(byte - b'0');
                    *first_param = Some(first_param.unwrap_or(0).saturating_mul(10) + digit);
                } else if byte == b';' || byte == b':' {
                    *collecting_first_param = false;
                } else if byte == b' ' {
                    *has_space_intermediate = true;
                    *collecting_first_param = false;
                } else if (0x40..=0x7e).contains(&byte) {
                    if byte == b'q' && *has_space_intermediate {
                        let param = first_param.unwrap_or(0);
                        if param <= 6 {
                            self.cursor_shape_overridden = param != 0;
                        }
                    }
                    self.state = DecscusrParseState::Ground;
                } else if !(0x20..=0x3f).contains(&byte) {
                    self.state = DecscusrParseState::Ground;
                }
            }
        }
    }

    pub(crate) fn cursor_shape_overridden(&self) -> bool {
        self.cursor_shape_overridden
    }
}

#[derive(Debug, Default)]
pub(crate) struct CursorPositionSettleState {
    settled: Option<TerminalCursorState>,
    candidate: Option<TerminalCursorState>,
    pending_since: Option<Instant>,
    candidate_since: Option<Instant>,
    /// A different row or large column move can be a temporary redraw position.
    candidate_jump: bool,
}

impl CursorPositionSettleState {
    pub(crate) fn observe(&mut self, current: Option<TerminalCursorState>, now: Instant) {
        // A return to the caret's position or row ends the redraw hold, even
        // when typing advanced its column. Otherwise the accumulated typing
        // deadline can settle a later repair cell and anchor redraws there.
        if let (Some(candidate), Some(since)) = (self.candidate, self.candidate_since) {
            let expired = now.duration_since(since) >= self.candidate_hold();
            let restored = self.settled.zip(current).is_some_and(|(settled, current)| {
                settled.visible
                    && current.visible
                    && (candidate.visible || !expired)
                    && (same_cursor_position(settled, current)
                        || (candidate.y != settled.y && current.y == settled.y && !expired))
            });
            if restored {
                self.settle(current);
                return;
            }
            // Preserve an eligible caret before a later redraw moves it away.
            if expired {
                self.settle(Some(candidate));
            }
        }
        let Some(current) = current else {
            self.settle(None);
            return;
        };
        if !current.visible {
            // A PTY can briefly hide a stationary caret during a redraw.
            // Keep the last visible cell until the existing max hold expires.
            if self.candidate.is_some_and(|candidate| {
                !candidate.visible && same_cursor_position(candidate, current)
            }) {
                return;
            }
            if self.candidate.is_none()
                && self.settled.is_some_and(|settled| {
                    settled.visible && same_cursor_position(settled, current)
                })
            {
                self.candidate = Some(current);
                self.pending_since = Some(now);
                self.candidate_since = Some(now);
                return;
            }
            self.settle(Some(current));
            return;
        }
        if self.candidate.is_some_and(|candidate| !candidate.visible) {
            self.settle(self.settled);
        }
        let Some(settled) = self.settled else {
            self.settle(Some(current));
            return;
        };
        if same_cursor_position(settled, current) && settled.visible {
            self.settle(Some(current));
            return;
        }

        let Some(candidate) = self.candidate else {
            self.candidate = Some(current);
            self.pending_since = Some(now);
            self.candidate_since = Some(now);
            self.candidate_jump = is_jump(settled, current);
            return;
        };

        let pending_since = self.pending_since.unwrap_or(now);
        if now.duration_since(pending_since) >= CURSOR_POSITION_MAX_HOLD {
            self.settle(Some(current));
        } else {
            if !same_cursor_position(candidate, current) {
                self.candidate_since = Some(now);
                self.candidate_jump = is_jump(settled, current);
            }
            self.candidate = Some(current);
        }
    }

    pub(crate) fn reported_cursor(
        &self,
        current: Option<TerminalCursorState>,
        now: Instant,
    ) -> Option<TerminalCursorState> {
        let current = current?;
        let Some(candidate) = self.candidate else {
            return Some(current);
        };
        let candidate_since = self.candidate_since.unwrap_or(now);
        let pending_since = self.pending_since.unwrap_or(now);
        if now.duration_since(candidate_since) >= self.candidate_hold()
            || now.duration_since(pending_since) >= CURSOR_POSITION_MAX_HOLD
        {
            return Some(TerminalCursorState {
                visible: current.visible && candidate.visible,
                shape: current.shape,
                color: current.color,
                ..candidate
            });
        }
        self.settled
            .map(|settled| TerminalCursorState {
                visible: settled.visible
                    && (current.visible
                        || (!candidate.visible && same_cursor_position(candidate, current))),
                shape: current.shape,
                color: current.color,
                ..settled
            })
            .or(Some(TerminalCursorState {
                visible: false,
                shape: current.shape,
                color: current.color,
                ..candidate
            }))
    }

    pub(crate) fn pending(&self) -> bool {
        self.candidate.is_some()
    }

    pub(crate) fn render_delay(&self) -> Option<Duration> {
        self.pending().then(|| self.candidate_hold())
    }

    fn candidate_hold(&self) -> Duration {
        if self.candidate_jump || self.candidate.is_some_and(|candidate| !candidate.visible) {
            CURSOR_POSITION_MAX_HOLD
        } else {
            CURSOR_POSITION_SETTLE
        }
    }

    fn settle(&mut self, cursor: Option<TerminalCursorState>) {
        *self = Self {
            settled: cursor,
            ..Self::default()
        };
    }
}

fn same_cursor_position(left: TerminalCursorState, right: TerminalCursorState) -> bool {
    left.x == right.x && left.y == right.y
}

fn is_jump(settled: TerminalCursorState, current: TerminalCursorState) -> bool {
    current.y != settled.y || current.x.abs_diff(settled.x) > 2
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cursor(x: u16, y: u16, visible: bool, shape: u8) -> TerminalCursorState {
        TerminalCursorState {
            x,
            y,
            visible,
            shape,
            color: None,
        }
    }

    #[test]
    fn cursor_settle_holds_position_change_until_quiet_window() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(cursor(20, 5, true, 0)), now + Duration::from_millis(1));

        let reported = settle
            .reported_cursor(Some(cursor(20, 5, true, 0)), now + Duration::from_millis(2))
            .unwrap();

        assert_eq!((reported.x, reported.y), (1, 0));
    }

    #[test]
    fn cursor_settle_adopts_position_change_after_quiet_window() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(cursor(2, 0, true, 0)), now + Duration::from_millis(1));

        let reported = settle
            .reported_cursor(
                Some(cursor(2, 0, true, 0)),
                now + CURSOR_POSITION_SETTLE + Duration::from_millis(1),
            )
            .unwrap();

        assert_eq!((reported.x, reported.y), (2, 0));
    }

    #[test]
    fn cursor_settle_caps_continuous_position_changes_from_first_pending_time() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(cursor(2, 0, true, 0)), now + Duration::from_millis(1));
        for ms in (11..=91).step_by(10) {
            settle.observe(
                Some(cursor(ms as u16, 0, true, 0)),
                now + Duration::from_millis(ms),
            );
        }
        settle.observe(
            Some(cursor(3, 0, true, 0)),
            now + CURSOR_POSITION_MAX_HOLD + Duration::from_millis(1),
        );

        assert!(!settle.pending());
        assert_eq!(
            settle.reported_cursor(
                Some(cursor(3, 0, true, 0)),
                now + CURSOR_POSITION_MAX_HOLD + Duration::from_millis(2),
            ),
            Some(cursor(3, 0, true, 0))
        );
    }

    #[test]
    fn cursor_settle_keeps_render_read_pure() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(cursor(2, 0, true, 0)), now + Duration::from_millis(1));

        assert!(settle.pending());
        let _ = settle.reported_cursor(
            Some(cursor(2, 0, true, 0)),
            now + CURSOR_POSITION_SETTLE + Duration::from_millis(1),
        );

        assert!(settle.pending());
    }

    #[test]
    fn cursor_settle_passes_appearance_through_while_position_is_held() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 2)), now);
        settle.observe(Some(cursor(2, 0, true, 6)), now + Duration::from_millis(1));

        let color = Some(crate::terminal_theme::RgbColor { r: 1, g: 2, b: 3 });
        let current = TerminalCursorState {
            color,
            ..cursor(2, 0, true, 6)
        };
        let reported = settle
            .reported_cursor(Some(current), now + Duration::from_millis(2))
            .unwrap();

        assert_eq!((reported.x, reported.y, reported.shape), (1, 0, 6));
        assert_eq!(reported.color, color);
    }

    #[test]
    fn cursor_settle_passes_shape_through_after_quiet_window() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 2)), now);
        settle.observe(Some(cursor(2, 0, true, 2)), now + Duration::from_millis(1));

        let reported = settle
            .reported_cursor(
                Some(cursor(2, 0, true, 6)),
                now + CURSOR_POSITION_SETTLE + Duration::from_millis(1),
            )
            .unwrap();

        assert_eq!((reported.x, reported.y, reported.shape), (2, 0, 6));
    }

    #[test]
    fn cursor_settle_ignores_short_same_position_hides() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(cursor(1, 0, true, 0)), now);
        settle.observe(Some(cursor(1, 0, false, 0)), now + Duration::from_millis(1));

        assert_eq!(
            settle.reported_cursor(Some(cursor(1, 0, false, 0)), now + Duration::from_millis(2)),
            Some(cursor(1, 0, true, 0))
        );
        settle.observe(
            Some(cursor(1, 0, false, 0)),
            now + Duration::from_millis(50),
        );
        assert_eq!(
            settle.reported_cursor(
                Some(cursor(1, 0, false, 0)),
                now + Duration::from_millis(90)
            ),
            Some(cursor(1, 0, true, 0))
        );
        assert!(
            !settle
                .reported_cursor(
                    Some(cursor(2, 0, false, 0)),
                    now + Duration::from_millis(90)
                )
                .unwrap()
                .visible
        );
        settle.observe(Some(cursor(1, 0, true, 0)), now + Duration::from_millis(91));
        assert_eq!(
            settle.reported_cursor(Some(cursor(1, 0, true, 0)), now + Duration::from_millis(92)),
            Some(cursor(1, 0, true, 0))
        );
        assert!(!settle.pending());
    }

    #[test]
    fn cursor_settle_hides_after_deadline_and_waits_to_reveal() {
        let now = Instant::now();
        let mut settle = CursorPositionSettleState::default();
        let visible = cursor(1, 0, true, 0);
        let hidden = cursor(1, 0, false, 0);
        settle.observe(Some(visible), now);
        settle.observe(Some(hidden), now + Duration::from_millis(1));
        assert_eq!(settle.render_delay(), Some(CURSOR_POSITION_MAX_HOLD));
        assert_eq!(
            settle.reported_cursor(Some(hidden), now + Duration::from_millis(100)),
            Some(visible)
        );
        assert_eq!(
            settle.reported_cursor(Some(hidden), now + Duration::from_millis(101)),
            Some(hidden)
        );

        // The pure read above exposes expiry without changing the state.
        settle.observe(Some(visible), now + Duration::from_millis(102));
        assert_eq!(
            settle.reported_cursor(Some(visible), now + Duration::from_millis(103)),
            Some(hidden)
        );
        assert_eq!(
            settle.reported_cursor(Some(visible), now + Duration::from_millis(122)),
            Some(visible)
        );
    }

    #[test]
    fn cursor_settle_hides_immediately_outside_stationary_caret() {
        let now = Instant::now();
        let visible = cursor(1, 0, true, 0);
        let hidden = cursor(2, 0, false, 0);
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(visible), now);
        settle.observe(Some(hidden), now + Duration::from_millis(1));
        assert_eq!(
            settle.reported_cursor(Some(hidden), now + Duration::from_millis(2)),
            Some(hidden)
        );

        settle.observe(None, now + Duration::from_millis(3));
        assert_eq!(
            settle.reported_cursor(None, now + Duration::from_millis(4)),
            None
        );

        for hide_at in [visible, cursor(2, 0, true, 0)] {
            let mut settle = CursorPositionSettleState::default();
            settle.observe(Some(visible), now);
            settle.observe(Some(cursor(2, 0, true, 0)), now + Duration::from_millis(1));
            let hidden = TerminalCursorState {
                visible: false,
                ..hide_at
            };
            settle.observe(Some(hidden), now + Duration::from_millis(2));
            assert_eq!(
                settle.reported_cursor(Some(hidden), now + Duration::from_millis(3)),
                Some(hidden)
            );
        }

        // Once the pending move has expired, its destination is the caret to retain.
        let mut settle = CursorPositionSettleState::default();
        settle.observe(Some(visible), now);
        settle.observe(Some(cursor(2, 0, true, 0)), now + Duration::from_millis(1));
        settle.observe(Some(hidden), now + Duration::from_millis(22));
        assert_eq!(
            settle.reported_cursor(Some(hidden), now + Duration::from_millis(23)),
            Some(cursor(2, 0, true, 0))
        );
    }
}
