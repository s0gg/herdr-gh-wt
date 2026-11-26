//! Choosing the base (parent) branch for a new issue branch.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseCandidate {
    /// What is passed to `wt switch --base`: `main`, `origin/epic/x`, ...
    pub name: String,
    pub remote: bool,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction {
    None,
    Pick(String),
    Cancel,
}

#[derive(Debug, Clone)]
pub struct BasePicker {
    pub candidates: Vec<BaseCandidate>,
    pub query: String,
    cursor: usize,
}

impl BasePicker {
    pub fn new(candidates: Vec<BaseCandidate>) -> Self {
        BasePicker {
            candidates,
            query: String::new(),
            cursor: 0,
        }
    }

    /// Candidates matching every whitespace-separated term of the query.
    pub fn filtered(&self) -> Vec<&BaseCandidate> {
        let terms: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.candidates
            .iter()
            .filter(|c| {
                let name = c.name.to_lowercase();
                terms.iter().all(|term| name.contains(term))
            })
            .collect()
    }

    /// Index of the highlighted entry within `filtered()`.
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.filtered().len().saturating_sub(1))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return PickerAction::Cancel,
            KeyCode::Char('c') if ctrl => return PickerAction::Cancel,
            KeyCode::Enter => return self.pick(key.modifiers.contains(KeyModifiers::ALT)),
            KeyCode::Down => self.move_by(1),
            KeyCode::Char('n') if ctrl => self.move_by(1),
            KeyCode::Up => self.move_by(-1),
            KeyCode::Char('p') if ctrl => self.move_by(-1),
            KeyCode::Char('u') if ctrl => self.edit(String::clear),
            KeyCode::Backspace => self.edit(|q| {
                q.pop();
            }),
            KeyCode::Char(c) if !ctrl => self.edit(|q| q.push(c)),
            _ => {}
        }
        PickerAction::None
    }

    /// The highlighted candidate, or the typed text when nothing matches or
    /// when `force_typed` is set.
    fn pick(&self, force_typed: bool) -> PickerAction {
        let typed = self.query.trim();
        let highlighted = self.filtered().get(self.cursor()).map(|c| c.name.clone());
        match highlighted {
            Some(name) if !force_typed => PickerAction::Pick(name),
            _ if !typed.is_empty() => PickerAction::Pick(typed.to_string()),
            _ => PickerAction::None,
        }
    }

    fn move_by(&mut self, delta: isize) {
        let last = self.filtered().len().saturating_sub(1);
        self.cursor = self.cursor().saturating_add_signed(delta).min(last);
    }

    fn edit(&mut self, change: impl FnOnce(&mut String)) {
        change(&mut self.query);
        self.cursor = self.cursor();
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn candidates() -> Vec<BaseCandidate> {
        let c = |name: &str, remote, is_default| BaseCandidate {
            name: name.into(),
            remote,
            is_default,
        };
        vec![
            c("main", false, true),
            c("epic/search", false, false),
            c("feature/login", false, false),
            c("origin/epic/search-v2", true, false),
        ]
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(picker: &mut BasePicker, text: &str) {
        for c in text.chars() {
            assert_eq!(picker.handle_key(key(KeyCode::Char(c))), PickerAction::None);
        }
    }

    fn names(picker: &BasePicker) -> Vec<&str> {
        picker.filtered().iter().map(|c| c.name.as_str()).collect()
    }

    #[test]
    fn enter_picks_the_default_branch_first() {
        let mut picker = BasePicker::new(candidates());
        assert_eq!(picker.cursor(), 0);
        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            PickerAction::Pick("main".into())
        );
    }

    #[test]
    fn typing_filters_and_letters_are_not_navigation() {
        let mut picker = BasePicker::new(candidates());
        type_text(&mut picker, "EPIC jk");
        assert!(names(&picker).is_empty(), "j and k are part of the query");
        picker.handle_key(key(KeyCode::Backspace));
        picker.handle_key(key(KeyCode::Backspace));
        picker.handle_key(key(KeyCode::Backspace));
        assert_eq!(picker.query, "EPIC");
        assert_eq!(names(&picker), ["epic/search", "origin/epic/search-v2"]);
    }

    #[test]
    fn arrows_move_within_the_filtered_list() {
        let mut picker = BasePicker::new(candidates());
        type_text(&mut picker, "epic");
        picker.handle_key(key(KeyCode::Down));
        picker.handle_key(key(KeyCode::Down));
        assert_eq!(picker.cursor(), 1, "stops at the end");
        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            PickerAction::Pick("origin/epic/search-v2".into())
        );
        picker.handle_key(key(KeyCode::Up));
        picker.handle_key(key(KeyCode::Up));
        assert_eq!(picker.cursor(), 0, "stops at the start");
        picker.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::CONTROL));
        assert_eq!(picker.cursor(), 1, "ctrl-n moves down");
        picker.handle_key(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::CONTROL));
        assert_eq!(picker.cursor(), 0, "ctrl-p moves up");
    }

    #[test]
    fn the_cursor_is_clamped_when_the_query_narrows_the_list() {
        let mut picker = BasePicker::new(candidates());
        for _ in 0..3 {
            picker.handle_key(key(KeyCode::Down));
        }
        type_text(&mut picker, "login");
        assert_eq!(picker.cursor(), 0);
        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            PickerAction::Pick("feature/login".into())
        );
    }

    #[test]
    fn enter_without_matches_uses_the_typed_text() {
        let mut picker = BasePicker::new(candidates());
        type_text(&mut picker, " pr:123 ");
        assert!(names(&picker).is_empty());
        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            PickerAction::Pick("pr:123".into())
        );
    }

    #[test]
    fn alt_enter_forces_the_typed_text() {
        let mut picker = BasePicker::new(candidates());
        type_text(&mut picker, "epic");
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            PickerAction::Pick("epic".into())
        );
    }

    #[test]
    fn enter_with_nothing_to_pick_does_nothing() {
        let mut picker = BasePicker::new(vec![]);
        assert_eq!(picker.handle_key(key(KeyCode::Enter)), PickerAction::None);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            PickerAction::None
        );
    }

    #[test]
    fn ctrl_u_clears_and_esc_or_ctrl_c_cancel() {
        let mut picker = BasePicker::new(candidates());
        type_text(&mut picker, "abc");
        picker.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(picker.query, "");
        assert_eq!(picker.handle_key(key(KeyCode::Esc)), PickerAction::Cancel);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            PickerAction::Cancel
        );
    }
}
