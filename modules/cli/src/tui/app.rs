//! Pure view-model for the TUI, independent of terminal backends.

use std::collections::VecDeque;

/// Severity of a log line, mapped to colors by the renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    System,
    Info,
    Event,
    Approval,
    Error,
}

/// One scrollback entry.
#[derive(Debug, Clone)]
pub struct LogLine {
    pub kind: Kind,
    pub text: String,
}

/// An unanswered approval request.
#[derive(Debug, Clone)]
pub struct PendingApproval {
    pub request_id: String,
    pub detail: String,
}

/// The complete UI state; all transitions are pure and unit-testable.
pub struct AppModel {
    capacity: usize,
    pub log: VecDeque<LogLine>,
    /// Lines scrolled back from the bottom (0 = pinned to bottom).
    pub scroll: usize,
    pub input: String,
    history: Vec<String>,
    history_index: Option<usize>,
    pub runs: Vec<(String, String)>,
    pub pending: Vec<PendingApproval>,
    /// The workspace opened at startup, shown in the status bar.
    pub workspace: Option<String>,
    pub current_ws: Option<String>,
    pub auto_approve: bool,
    pub status_note: String,
}

impl AppModel {
    /// Creates a model with the given scrollback capacity.
    pub fn new(capacity: usize) -> Self {
        Self {
            capacity,
            log: VecDeque::new(),
            scroll: 0,
            input: String::new(),
            history: Vec::new(),
            history_index: None,
            runs: Vec::new(),
            pending: Vec::new(),
            workspace: None,
            current_ws: None,
            auto_approve: false,
            status_note: String::new(),
        }
    }

    /// Appends a log line, trimming overflow.
    pub fn push_log(&mut self, kind: Kind, text: impl Into<String>) {
        if self.log.len() == self.capacity {
            self.log.pop_front();
        }
        self.log.push_back(LogLine {
            kind,
            text: text.into(),
        });
    }

    /// Accepts the typed input; returns the command line when non-empty.
    pub fn submit_input(&mut self) -> Option<String> {
        let command = self.input.trim().to_string();
        self.input.clear();
        self.history_index = None;
        if command.is_empty() {
            return None;
        }
        if self.history.last().map(|last| last != &command).unwrap_or(true) {
            self.history.push(command.clone());
        }
        Some(command)
    }

    /// Steps back through command history.
    pub fn history_prev(&mut self) {
        if self.history.is_empty() {
            return;
        }
        let next = match self.history_index {
            None => self.history.len() - 1,
            Some(index) => index.saturating_sub(1),
        };
        self.history_index = Some(next);
        self.input = self.history[next].clone();
    }

    /// Steps forward through command history (past the end restores blank).
    pub fn history_next(&mut self) {
        let Some(index) = self.history_index else {
            return;
        };
        if index + 1 >= self.history.len() {
            self.history_index = None;
            self.input.clear();
        } else {
            self.history_index = Some(index + 1);
            self.input = self.history[index + 1].clone();
        }
    }

    /// Scrolls toward older entries. The upper bound is applied each frame by
    /// [`AppModel::clamp_scroll`] once the viewport height is known, so the
    /// backlog is never scrolled past a full screen of the oldest rows.
    pub fn scroll_up(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_add(amount);
    }

    /// Caps the scroll offset so the viewport stays full (`max` = maximum
    /// number of rows that may be skipped while the window still fills it).
    pub fn clamp_scroll(&mut self, max: usize) {
        self.scroll = self.scroll.min(max);
    }

    /// Scrolls toward newer entries; 0 pins to the bottom.
    pub fn scroll_down(&mut self, amount: usize) {
        self.scroll = self.scroll.saturating_sub(amount);
    }

    /// Records run status in the sidebar.
    pub fn set_run_status(&mut self, label: &str, status: &str) {
        if let Some(entry) = self.runs.iter_mut().find(|(name, _)| name == label) {
            entry.1 = status.to_string();
        } else {
            self.runs.push((label.to_string(), status.to_string()));
        }
    }

    /// Removes a finished run from the sidebar.
    pub fn remove_run(&mut self, label: &str) {
        self.runs.retain(|(name, _)| name != label);
    }

    /// Enqueues an approval request.
    pub fn push_approval(&mut self, request_id: String, detail: String) {
        self.pending.push(PendingApproval {
            request_id,
            detail,
        });
    }

    /// Takes the front approval request (the one shown by the modal).
    pub fn take_front_approval(&mut self) -> Option<PendingApproval> {
        if self.pending.is_empty() {
            None
        } else {
            Some(self.pending.remove(0))
        }
    }

    /// Whether the approval modal should render.
    pub fn has_modal(&self) -> bool {
        !self.pending.is_empty()
    }

    /// Visible log window ending at (len - scroll).
    pub fn visible_tail(&self, height: usize) -> impl Iterator<Item = &LogLine> {
        let end = self.log.len().saturating_sub(self.scroll);
        let start = end.saturating_sub(height);
        self.log.iter().skip(start).take(end - start)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn log_capacity_trims_oldest() {
        let mut app = AppModel::new(3);
        for index in 0..5 {
            app.push_log(Kind::Info, format!("line-{index}"));
        }
        assert_eq!(app.log.len(), 3);
        assert_eq!(app.log.front().unwrap().text, "line-2");
        assert_eq!(app.log.back().unwrap().text, "line-4");
    }

    #[test]
    fn input_history_navigation() {
        let mut app = AppModel::new(10);
        app.input = "one".into();
        assert_eq!(app.submit_input().as_deref(), Some("one"));
        app.input = "two".into();
        assert_eq!(app.submit_input().as_deref(), Some("two"));

        app.history_prev();
        assert_eq!(app.input, "two");
        app.history_prev();
        assert_eq!(app.input, "one");
        app.history_prev(); // stays at the oldest entry
        assert_eq!(app.input, "one");
        app.history_next();
        assert_eq!(app.input, "two");
        app.history_next(); // past newest restores blank input
        assert!(app.input.is_empty());
    }

    #[test]
    fn empty_input_is_not_submitted_or_recorded() {
        let mut app = AppModel::new(10);
        app.input = "   ".into();
        assert_eq!(app.submit_input(), None);
        app.submit_input();
        app.input = "run".into();
        app.submit_input();
        app.submit_input(); // duplicate of nothing pending
        assert!(app.history.iter().all(|entry| entry != "   "));
    }

    #[test]
    fn scroll_clamps_to_history_bounds() {
        let mut app = AppModel::new(10);
        for index in 0..6 {
            app.push_log(Kind::Info, format!("{index}"));
        }
        app.scroll_up(100);
        // The viewport clamps each frame; a 5-row pane can skip at most 5 rows.
        app.clamp_scroll(app.log.len().saturating_sub(5));
        assert_eq!(app.scroll, 1);
        app.clamp_scroll(5);
        assert_eq!(app.scroll, 1);
        app.scroll_down(4);
        assert_eq!(app.scroll, 0);
        app.scroll_down(10);
        assert_eq!(app.scroll, 0);
    }

    #[test]
    fn approvals_are_served_in_order() {
        let mut app = AppModel::new(10);
        assert!(!app.has_modal());
        app.push_approval("r1".into(), "cmd one".into());
        app.push_approval("r2".into(), "cmd two".into());
        assert!(app.has_modal());
        let first = app.take_front_approval().unwrap();
        assert_eq!(first.request_id, "r1");
        let second = app.take_front_approval().unwrap();
        assert_eq!(second.request_id, "r2");
        assert!(!app.has_modal());
    }

    #[test]
    fn run_statuses_update_and_clear() {
        let mut app = AppModel::new(10);
        app.set_run_status("bp-a", "running");
        app.set_run_status("bp-a", "completed");
        app.remove_run("bp-a");
        assert!(app.runs.is_empty());
    }
}
