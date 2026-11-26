//! A filterable list to pick from: the base branch of a new issue branch, or
//! an existing branch to link to an issue.

use std::path::PathBuf;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Something a [`Picker`] lists.
pub trait Entry {
    /// The text the query is matched against.
    fn text(&self) -> &str;
}

/// A branch a new issue branch can start from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BaseCandidate {
    /// What is passed to `wt switch --base`: `main`, `origin/epic/x`, ...
    pub name: String,
    pub remote: bool,
    pub is_default: bool,
}

impl Entry for BaseCandidate {
    fn text(&self) -> &str {
        &self.name
    }
}

/// A local branch that can be linked to an issue.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BranchCandidate {
    pub name: String,
    /// The branch's worktree, when it has one.
    pub worktree: Option<PathBuf>,
    /// Whether that worktree is open in Herdr.
    pub open: bool,
    /// The issue the branch is linked to, if any.
    pub issue: Option<u64>,
}

impl Entry for BranchCandidate {
    fn text(&self) -> &str {
        &self.name
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerAction<T> {
    None,
    Pick(T),
    /// Text the user typed, for a picker that accepts it.
    Typed(String),
    Cancel,
}

#[derive(Debug, Clone)]
pub struct Picker<T> {
    pub entries: Vec<T>,
    pub query: String,
    cursor: usize,
    /// Whether enter can pick the typed text instead of an entry.
    accepts_typed: bool,
}

impl<T: Entry + Clone> Picker<T> {
    pub fn new(entries: Vec<T>) -> Self {
        Picker {
            entries,
            query: String::new(),
            cursor: 0,
            accepts_typed: false,
        }
    }

    /// Let enter pick the typed text when nothing matches it, and alt-enter
    /// pick it even when something does.
    pub fn accepting_typed(self) -> Self {
        Picker {
            accepts_typed: true,
            ..self
        }
    }

    pub fn accepts_typed(&self) -> bool {
        self.accepts_typed
    }

    /// Entries matching every whitespace-separated term of the query.
    pub fn filtered(&self) -> Vec<&T> {
        let terms: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        self.entries
            .iter()
            .filter(|e| {
                let text = e.text().to_lowercase();
                terms.iter().all(|term| text.contains(term))
            })
            .collect()
    }

    /// Index of the highlighted entry within `filtered()`.
    pub fn cursor(&self) -> usize {
        self.cursor.min(self.filtered().len().saturating_sub(1))
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> PickerAction<T> {
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

    /// The highlighted entry; or the typed text, when the picker accepts it
    /// and nothing matches or `force_typed` is set.
    fn pick(&self, force_typed: bool) -> PickerAction<T> {
        let typed = self.query.trim();
        let highlighted = self.filtered().get(self.cursor()).map(|&e| e.clone());
        let force_typed = force_typed && self.accepts_typed;
        match highlighted {
            Some(entry) if !force_typed => PickerAction::Pick(entry),
            _ if self.accepts_typed && !typed.is_empty() => PickerAction::Typed(typed.to_string()),
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

    pub fn branches() -> Vec<BranchCandidate> {
        vec![
            BranchCandidate {
                name: "login-timeout".into(),
                worktree: Some("/src/acme/app.login-timeout".into()),
                open: true,
                issue: None,
            },
            BranchCandidate {
                name: "search-accents".into(),
                worktree: Some("/src/acme/app.search-accents".into()),
                open: false,
                issue: Some(3),
            },
            BranchCandidate {
                name: "main".into(),
                worktree: Some("/src/acme/app".into()),
                open: true,
                issue: None,
            },
            BranchCandidate {
                name: "old-experiment".into(),
                worktree: None,
                open: false,
                issue: None,
            },
        ]
    }

    fn base_picker() -> Picker<BaseCandidate> {
        Picker::new(candidates()).accepting_typed()
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text<T: Entry + Clone + std::fmt::Debug + PartialEq>(
        picker: &mut Picker<T>,
        text: &str,
    ) {
        for c in text.chars() {
            assert_eq!(picker.handle_key(key(KeyCode::Char(c))), PickerAction::None);
        }
    }

    fn names<T: Entry + Clone>(picker: &Picker<T>) -> Vec<&str> {
        picker.filtered().iter().map(|e| e.text()).collect()
    }

    fn picked_name<T: Entry>(action: PickerAction<T>) -> Option<String> {
        match action {
            PickerAction::Pick(entry) => Some(entry.text().to_string()),
            _ => None,
        }
    }

    #[test]
    fn enter_picks_the_first_entry() {
        let mut picker = base_picker();
        assert_eq!(picker.cursor(), 0);
        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            PickerAction::Pick(candidates()[0].clone())
        );
    }

    #[test]
    fn typing_filters_and_letters_are_not_navigation() {
        let mut picker = base_picker();
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
        let mut picker = base_picker();
        type_text(&mut picker, "epic");
        picker.handle_key(key(KeyCode::Down));
        picker.handle_key(key(KeyCode::Down));
        assert_eq!(picker.cursor(), 1, "stops at the end");
        assert_eq!(
            picked_name(picker.handle_key(key(KeyCode::Enter))).as_deref(),
            Some("origin/epic/search-v2")
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
        let mut picker = base_picker();
        for _ in 0..3 {
            picker.handle_key(key(KeyCode::Down));
        }
        type_text(&mut picker, "login");
        assert_eq!(picker.cursor(), 0);
        assert_eq!(
            picked_name(picker.handle_key(key(KeyCode::Enter))).as_deref(),
            Some("feature/login")
        );
    }

    #[test]
    fn enter_without_matches_uses_the_typed_text() {
        let mut picker = base_picker();
        type_text(&mut picker, " pr:123 ");
        assert_eq!(names(&picker), Vec::<&str>::new());
        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            PickerAction::Typed("pr:123".into())
        );
    }

    #[test]
    fn alt_enter_forces_the_typed_text() {
        let mut picker = base_picker();
        type_text(&mut picker, "epic");
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            PickerAction::Typed("epic".into())
        );
    }

    #[test]
    fn enter_with_nothing_to_pick_does_nothing() {
        let mut picker = Picker::<BaseCandidate>::new(vec![]).accepting_typed();
        assert_eq!(picker.handle_key(key(KeyCode::Enter)), PickerAction::None);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            PickerAction::None
        );
    }

    #[test]
    fn ctrl_u_clears_and_esc_or_ctrl_c_cancel() {
        let mut picker = base_picker();
        type_text(&mut picker, "abc");
        picker.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(picker.query, "");
        assert_eq!(picker.handle_key(key(KeyCode::Esc)), PickerAction::Cancel);
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            PickerAction::Cancel
        );
    }

    #[test]
    fn a_picker_without_typed_text_only_picks_entries() {
        let mut picker = Picker::new(branches());
        assert!(!picker.accepts_typed());
        type_text(&mut picker, "search");
        assert_eq!(
            picker.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::ALT)),
            PickerAction::Pick(branches()[1].clone()),
            "alt-enter picks the highlighted entry"
        );
        type_text(&mut picker, "-nothing");
        assert_eq!(names(&picker), Vec::<&str>::new());
        assert_eq!(
            picker.handle_key(key(KeyCode::Enter)),
            PickerAction::None,
            "the typed text is never picked"
        );
    }
}

#[cfg(test)]
mod properties {
    use super::*;
    use crate::app::properties::key_event;
    use proptest::prelude::*;

    fn candidate() -> impl Strategy<Value = BaseCandidate> {
        ("[a-z][a-z/-]{0,11}", any::<bool>(), any::<bool>()).prop_map(
            |(name, remote, is_default)| BaseCandidate {
                name,
                remote,
                is_default,
            },
        )
    }

    proptest! {
        #[test]
        fn keys_keep_the_cursor_in_bounds_and_picks_are_usable(
            candidates in prop::collection::vec(candidate(), 0..8),
            accepts_typed in any::<bool>(),
            keys in prop::collection::vec(key_event(), 0..40),
        ) {
            let mut picker = Picker::new(candidates.clone());
            if accepts_typed {
                picker = picker.accepting_typed();
            }
            for key in keys {
                match picker.handle_key(key) {
                    PickerAction::Pick(entry) => prop_assert!(candidates.contains(&entry)),
                    PickerAction::Typed(text) => {
                        prop_assert!(accepts_typed);
                        prop_assert!(!text.is_empty());
                        prop_assert_eq!(text.trim(), text.as_str());
                    }
                    PickerAction::None | PickerAction::Cancel => {}
                }
                let len = picker.filtered().len();
                let in_bounds = if len == 0 { picker.cursor() == 0 } else { picker.cursor() < len };
                prop_assert!(in_bounds, "cursor {} of {}", picker.cursor(), len);
            }
        }
    }
}
