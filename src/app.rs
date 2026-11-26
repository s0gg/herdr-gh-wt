//! Dashboard state and keyboard handling, independent of rendering.

use std::time::SystemTime;

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::github::{Dashboard, Item, Kind, Reviewer};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Section {
    Reviews,
    Issues,
}

impl Section {
    fn index(self) -> usize {
        match self {
            Section::Reviews => 0,
            Section::Issues => 1,
        }
    }

    fn other(self) -> Section {
        match self {
            Section::Reviews => Section::Issues,
            Section::Issues => Section::Reviews,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Normal,
    /// Typing a free-text filter.
    Search,
    /// Choosing a repository; `cursor` 0 is "all repositories".
    RepoPicker {
        cursor: usize,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Load {
    Loading,
    Loaded,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    None,
    Quit,
    Refresh,
    Open(Item),
    /// Open an issue in a new branch created from a base branch the user picks.
    OpenWithBase(Item),
    Browse(Item),
}

#[derive(Debug, Clone)]
pub struct App {
    pub dashboard: Dashboard,
    pub load: Load,
    pub section: Section,
    selected: [usize; 2],
    /// Only show items of this repository (`owner/name`).
    pub repo: Option<String>,
    /// Whitespace-separated terms that must all match.
    pub query: String,
    pub mode: Mode,
    pub fetched_at: Option<SystemTime>,
    /// A one-off message shown in the footer until the next key press.
    pub notice: Option<String>,
}

impl Default for App {
    fn default() -> Self {
        Self::new()
    }
}

impl App {
    pub fn new() -> Self {
        App {
            dashboard: Dashboard::default(),
            load: Load::Loading,
            section: Section::Reviews,
            selected: [0, 0],
            repo: None,
            query: String::new(),
            mode: Mode::Normal,
            fetched_at: None,
            notice: None,
        }
    }

    /// Show freshly fetched data, keeping the filters and the cursor where possible.
    pub fn set_dashboard(&mut self, dashboard: Dashboard, fetched_at: SystemTime) {
        let previous = [Section::Reviews, Section::Issues].map(|section| {
            self.visible(section)
                .get(self.selected_index(section))
                .map(|i| i.url.clone())
        });
        self.dashboard = dashboard;
        self.load = Load::Loaded;
        self.fetched_at = Some(fetched_at);
        if let Some(repo) = &self.repo
            && !self.all_items().any(|i| &i.repo == repo)
        {
            self.repo = None;
        }
        for section in [Section::Reviews, Section::Issues] {
            let index = previous[section.index()]
                .as_ref()
                .and_then(|url| self.visible(section).iter().position(|i| &i.url == url))
                .unwrap_or(0);
            self.selected[section.index()] = index;
        }
        self.filters_changed();
    }

    pub fn set_error(&mut self, message: String) {
        self.load = Load::Failed(message);
    }

    fn all_items(&self) -> impl Iterator<Item = &Item> {
        self.dashboard
            .review_requests
            .iter()
            .chain(&self.dashboard.assigned_issues)
    }

    /// The items of `section` that pass the repository filter and the query.
    pub fn visible(&self, section: Section) -> Vec<&Item> {
        let items = match section {
            Section::Reviews => &self.dashboard.review_requests,
            Section::Issues => &self.dashboard.assigned_issues,
        };
        let terms: Vec<String> = self
            .query
            .split_whitespace()
            .map(str::to_lowercase)
            .collect();
        items
            .iter()
            .filter(|i| self.repo.as_ref().is_none_or(|repo| &i.repo == repo))
            .filter(|i| {
                if terms.is_empty() {
                    return true;
                }
                let haystack = format!(
                    "{} #{} {} {} {} {}",
                    i.repo,
                    i.number,
                    i.title,
                    i.author.as_deref().unwrap_or_default(),
                    i.labels.join(" "),
                    i.head_ref.as_deref().unwrap_or_default(),
                )
                .to_lowercase();
                terms.iter().all(|term| haystack.contains(term))
            })
            .collect()
    }

    /// Index of the cursor within `visible(section)`.
    pub fn selected_index(&self, section: Section) -> usize {
        let len = self.visible(section).len();
        self.selected[section.index()].min(len.saturating_sub(1))
    }

    pub fn selected_item(&self) -> Option<&Item> {
        self.visible(self.section)
            .get(self.selected_index(self.section))
            .copied()
    }

    /// Every repository with items, with its item count, sorted by name.
    pub fn repos(&self) -> Vec<(String, usize)> {
        let mut counts = std::collections::BTreeMap::<&str, usize>::new();
        for item in self.all_items() {
            *counts.entry(item.repo.as_str()).or_default() += 1;
        }
        counts
            .into_iter()
            .map(|(repo, n)| (repo.to_string(), n))
            .collect()
    }

    /// Re-clamp the cursors after the visible items changed, and leave a
    /// section that became empty.
    fn filters_changed(&mut self) {
        for section in [Section::Reviews, Section::Issues] {
            self.selected[section.index()] = self.selected_index(section);
        }
        if self.visible(self.section).is_empty() && !self.visible(self.section.other()).is_empty() {
            self.section = self.section.other();
        }
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> Action {
        self.notice = None;
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('c') {
            return Action::Quit;
        }
        match self.mode.clone() {
            Mode::Normal => self.normal_key(key),
            Mode::Search => {
                self.search_key(key);
                Action::None
            }
            Mode::RepoPicker { cursor } => {
                self.picker_key(key, cursor);
                Action::None
            }
        }
    }

    fn normal_key(&mut self, key: KeyEvent) -> Action {
        match key.code {
            KeyCode::Char('q') => return Action::Quit,
            KeyCode::Esc if self.query.is_empty() && self.repo.is_none() => return Action::Quit,
            KeyCode::Esc => {
                self.query.clear();
                self.repo = None;
                self.filters_changed();
            }
            KeyCode::Char('R') | KeyCode::F(5) => return Action::Refresh,
            KeyCode::Down | KeyCode::Char('j') => self.move_down(),
            KeyCode::Up | KeyCode::Char('k') => self.move_up(),
            KeyCode::Home | KeyCode::Char('g') => self.selected[self.section.index()] = 0,
            KeyCode::End | KeyCode::Char('G') => {
                self.selected[self.section.index()] =
                    self.visible(self.section).len().saturating_sub(1);
            }
            KeyCode::Tab | KeyCode::BackTab => self.section = self.section.other(),
            KeyCode::Char('/') => self.mode = Mode::Search,
            KeyCode::Char('r') => {
                let cursor = self
                    .repo
                    .as_ref()
                    .and_then(|repo| self.repos().iter().position(|(r, _)| r == repo))
                    .map_or(0, |i| i + 1);
                self.mode = Mode::RepoPicker { cursor };
            }
            KeyCode::Enter => {
                if let Some(item) = self.selected_item() {
                    return Action::Open(item.clone());
                }
            }
            KeyCode::Char('o') => {
                if let Some(item) = self.selected_item() {
                    return Action::Browse(item.clone());
                }
            }
            KeyCode::Char('b') => match self.selected_item() {
                Some(item) if item.kind == Kind::Issue => {
                    return Action::OpenWithBase(item.clone());
                }
                Some(_) => {
                    self.notice = Some(
                        "A base branch only applies to issues; pull requests use their own branch"
                            .to_string(),
                    );
                }
                None => {}
            },
            _ => {}
        }
        Action::None
    }

    fn move_down(&mut self) {
        let index = self.selected_index(self.section);
        if index + 1 < self.visible(self.section).len() {
            self.selected[self.section.index()] = index + 1;
        } else if self.section == Section::Reviews && !self.visible(Section::Issues).is_empty() {
            self.section = Section::Issues;
            self.selected[Section::Issues.index()] = 0;
        }
    }

    fn move_up(&mut self) {
        let index = self.selected_index(self.section);
        if index > 0 {
            self.selected[self.section.index()] = index - 1;
        } else if self.section == Section::Issues && !self.visible(Section::Reviews).is_empty() {
            self.section = Section::Reviews;
            self.selected[Section::Reviews.index()] = self.visible(Section::Reviews).len() - 1;
        }
    }

    fn search_key(&mut self, key: KeyEvent) {
        match key.code {
            KeyCode::Enter => self.mode = Mode::Normal,
            KeyCode::Esc => {
                self.query.clear();
                self.mode = Mode::Normal;
            }
            KeyCode::Backspace => {
                self.query.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.clear();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(c);
            }
            _ => return,
        }
        self.filters_changed();
    }

    fn picker_key(&mut self, key: KeyEvent, cursor: usize) {
        let last = self.repos().len();
        match key.code {
            KeyCode::Down | KeyCode::Char('j') => {
                self.mode = Mode::RepoPicker {
                    cursor: (cursor + 1).min(last),
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.mode = Mode::RepoPicker {
                    cursor: cursor.saturating_sub(1),
                }
            }
            KeyCode::Enter => {
                self.repo = cursor
                    .checked_sub(1)
                    .and_then(|i| self.repos().get(i).map(|(r, _)| r.clone()));
                self.mode = Mode::Normal;
                self.filters_changed();
            }
            KeyCode::Esc | KeyCode::Char('q' | 'r') => self.mode = Mode::Normal,
            _ => {}
        }
    }
}

/// Who a pull request's review was requested from, as far as you are
/// concerned: `you` for a direct request, and the requested teams.
pub fn requested_via(item: &Item, viewer: &str) -> String {
    let direct = item
        .reviewers
        .iter()
        .any(|r| matches!(r, Reviewer::User(login) if login == viewer));
    let teams = item.reviewers.iter().filter_map(|r| match r {
        Reviewer::Team { org, slug } => Some(format!("@{org}/{slug}")),
        Reviewer::User(_) => None,
    });
    direct
        .then(|| "you".to_string())
        .into_iter()
        .chain(teams)
        .collect::<Vec<_>>()
        .join(", ")
}

/// Compact age of a GitHub timestamp: `now`, `5m`, `3h`, `2d`, `3w`, `4mo`,
/// `2y`; empty when the timestamp can't be read.
pub fn age(timestamp: &str, now: SystemTime) -> String {
    parse_timestamp(timestamp).map_or_else(String::new, |then| age_of(then, now))
}

/// Compact time from `then` to `now`; `now` when `then` is in the future.
pub fn age_of(then: SystemTime, now: SystemTime) -> String {
    let secs = now.duration_since(then).map_or(0, |d| d.as_secs());
    let (minutes, hours, days) = (secs / 60, secs / 3600, secs / 86_400);
    match () {
        () if secs < 60 => "now".to_string(),
        () if hours < 1 => format!("{minutes}m"),
        () if days < 1 => format!("{hours}h"),
        () if days < 7 => format!("{days}d"),
        () if days < 30 => format!("{}w", days / 7),
        () if days < 365 => format!("{}mo", days / 30),
        () => format!("{}y", days / 365),
    }
}

/// Parse a UTC timestamp as GitHub returns it, `2024-05-01T12:34:56Z`
/// (optionally with fractional seconds). Dates before 1970 are rejected.
pub fn parse_timestamp(text: &str) -> Option<SystemTime> {
    let rest = text.strip_suffix('Z')?;
    let (main, fraction) = match rest.split_once('.') {
        Some((main, fraction)) => (main, Some(fraction)),
        None => (rest, None),
    };
    let b = main.as_bytes();
    if b.len() != 19
        || b[4] != b'-'
        || b[7] != b'-'
        || b[10] != b'T'
        || b[13] != b':'
        || b[16] != b':'
    {
        return None;
    }
    let number = |range: std::ops::Range<usize>| -> Option<u64> {
        let digits = &main[range];
        digits
            .bytes()
            .all(|c| c.is_ascii_digit())
            .then(|| digits.parse().ok())?
    };
    let (year, month, day) = (number(0..4)?, number(5..7)?, number(8..10)?);
    let (hour, minute, second) = (number(11..13)?, number(14..16)?, number(17..19)?);
    if year < 1970 || !(1..=12).contains(&month) || day == 0 || day > days_in_month(year, month) {
        return None;
    }
    if hour > 23 || minute > 59 || second > 60 {
        return None;
    }

    let nanos = match fraction {
        None => 0,
        Some(f) if !f.is_empty() && f.bytes().all(|c| c.is_ascii_digit()) => {
            // Keep the first nine digits: nanosecond precision.
            let digits: String = f.chars().chain(std::iter::repeat('0')).take(9).collect();
            digits.parse().ok()?
        }
        Some(_) => return None,
    };

    let days = days_since_epoch(year, month, day);
    let secs = days * 86_400 + hour * 3600 + minute * 60 + second;
    Some(SystemTime::UNIX_EPOCH + std::time::Duration::new(secs, nanos))
}

fn is_leap_year(year: u64) -> bool {
    year.is_multiple_of(4) && (!year.is_multiple_of(100) || year.is_multiple_of(400))
}

fn days_in_month(year: u64, month: u64) -> u64 {
    match month {
        2 if is_leap_year(year) => 29,
        2 => 28,
        4 | 6 | 9 | 11 => 30,
        _ => 31,
    }
}

/// Days from 1970-01-01 to the given date (Howard Hinnant's algorithm,
/// for dates from 1970 on).
fn days_since_epoch(year: u64, month: u64, day: u64) -> u64 {
    // Count years from March, so the leap day ends the year.
    let year = if month <= 2 { year - 1 } else { year };
    let era = year / 400;
    let year_of_era = year - era * 400;
    let month_from_march = (month + 9) % 12;
    let day_of_year = (153 * month_from_march + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    era * 146_097 + day_of_era - 719_468
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn item(kind: Kind, repo: &str, number: u64, title: &str) -> Item {
        Item {
            kind,
            host: "github.com".into(),
            repo: repo.into(),
            number,
            title: title.into(),
            url: format!(
                "https://github.com/{repo}/{}/{number}",
                if kind == Kind::Issue {
                    "issues"
                } else {
                    "pull"
                }
            ),
            author: Some("alice".into()),
            updated_at: "2026-11-26T00:00:00Z".into(),
            labels: vec!["bug".into()],
            body: String::new(),
            is_draft: false,
            head_ref: (kind == Kind::PullRequest).then(|| format!("branch-{number}")),
            reviewers: vec![],
        }
    }

    pub fn sample() -> Dashboard {
        Dashboard {
            viewer: "octocat".into(),
            review_requests: vec![
                item(Kind::PullRequest, "acme/app", 1, "Add login"),
                item(Kind::PullRequest, "acme/lib", 2, "Fix parser"),
                item(Kind::PullRequest, "acme/app", 3, "Refactor auth"),
            ],
            assigned_issues: vec![
                item(Kind::Issue, "acme/lib", 10, "Parser crashes"),
                item(Kind::Issue, "acme/app", 11, "Login is slow"),
            ],
        }
    }

    fn loaded() -> App {
        let mut app = App::new();
        app.set_dashboard(sample(), SystemTime::UNIX_EPOCH);
        app
    }

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn press(app: &mut App, codes: &[KeyCode]) -> Action {
        let mut last = Action::None;
        for code in codes {
            last = app.handle_key(key(*code));
        }
        last
    }

    fn type_text(app: &mut App, text: &str) {
        for c in text.chars() {
            app.handle_key(key(KeyCode::Char(c)));
        }
    }

    fn numbers(app: &App, section: Section) -> Vec<u64> {
        app.visible(section).iter().map(|i| i.number).collect()
    }

    #[test]
    fn starts_loading_on_the_review_section() {
        let app = App::new();
        assert_eq!(app.load, Load::Loading);
        assert_eq!(app.section, Section::Reviews);
        assert_eq!(app.selected_item(), None);
    }

    #[test]
    fn loaded_data_selects_the_first_review_request() {
        let app = loaded();
        assert_eq!(app.load, Load::Loaded);
        assert_eq!(app.selected_item().unwrap().number, 1);
        assert_eq!(app.fetched_at, Some(SystemTime::UNIX_EPOCH));
    }

    #[test]
    fn errors_are_kept_until_the_next_load() {
        let mut app = App::new();
        app.set_error("boom".into());
        assert_eq!(app.load, Load::Failed("boom".into()));
        app.set_dashboard(sample(), SystemTime::UNIX_EPOCH);
        assert_eq!(app.load, Load::Loaded);
    }

    #[test]
    fn down_and_up_move_through_both_sections() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Char('j'), KeyCode::Down]);
        assert_eq!(app.selected_item().unwrap().number, 3);
        press(&mut app, &[KeyCode::Char('j')]);
        assert_eq!(app.section, Section::Issues);
        assert_eq!(app.selected_item().unwrap().number, 10);
        press(&mut app, &[KeyCode::Char('j'), KeyCode::Char('j')]);
        assert_eq!(app.selected_item().unwrap().number, 11, "stops at the end");
        press(&mut app, &[KeyCode::Char('k'), KeyCode::Up]);
        assert_eq!(app.section, Section::Reviews);
        assert_eq!(
            app.selected_item().unwrap().number,
            3,
            "enters the previous section at its end"
        );
        press(
            &mut app,
            &[KeyCode::Char('k'), KeyCode::Char('k'), KeyCode::Char('k')],
        );
        assert_eq!(app.selected_item().unwrap().number, 1, "stops at the start");
    }

    #[test]
    fn tab_switches_sections_and_remembers_each_cursor() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Char('j'), KeyCode::Tab]);
        assert_eq!(app.section, Section::Issues);
        assert_eq!(app.selected_item().unwrap().number, 10);
        press(&mut app, &[KeyCode::BackTab]);
        assert_eq!(app.section, Section::Reviews);
        assert_eq!(app.selected_item().unwrap().number, 2);
    }

    #[test]
    fn home_and_end_jump_within_the_section() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Char('G')]);
        assert_eq!(app.selected_item().unwrap().number, 3);
        press(&mut app, &[KeyCode::Char('g')]);
        assert_eq!(app.selected_item().unwrap().number, 1);
    }

    #[test]
    fn moving_skips_an_empty_section() {
        let mut app = loaded();
        app.repo = Some("acme/lib".into());
        app.dashboard
            .review_requests
            .retain(|i| i.repo != "acme/lib");
        assert_eq!(app.visible(Section::Reviews), Vec::<&Item>::new());
        press(&mut app, &[KeyCode::Char('j')]);
        assert_eq!(app.section, Section::Issues);
        assert_eq!(app.selected_item().unwrap().number, 10);
    }

    #[test]
    fn enter_opens_and_o_browses_the_selection() {
        let mut app = loaded();
        assert!(matches!(press(&mut app, &[KeyCode::Enter]), Action::Open(i) if i.number == 1));
        assert!(
            matches!(press(&mut app, &[KeyCode::Char('o')]), Action::Browse(i) if i.number == 1)
        );
    }

    #[test]
    fn b_opens_an_issue_from_a_chosen_base() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Tab]);
        assert!(
            matches!(press(&mut app, &[KeyCode::Char('b')]), Action::OpenWithBase(i) if i.number == 10)
        );
    }

    #[test]
    fn b_on_a_pull_request_explains_itself() {
        let mut app = loaded();
        assert_eq!(press(&mut app, &[KeyCode::Char('b')]), Action::None);
        assert!(
            app.notice.as_deref().is_some_and(|n| n.contains("issue")),
            "{:?}",
            app.notice
        );
    }

    #[test]
    fn enter_without_items_does_nothing() {
        let mut app = App::new();
        assert_eq!(press(&mut app, &[KeyCode::Enter]), Action::None);
    }

    #[test]
    fn quit_and_refresh_keys() {
        let mut app = loaded();
        assert_eq!(press(&mut app, &[KeyCode::Char('R')]), Action::Refresh);
        assert_eq!(press(&mut app, &[KeyCode::Char('q')]), Action::Quit);
        assert_eq!(
            app.handle_key(KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL)),
            Action::Quit
        );
        assert_eq!(press(&mut app, &[KeyCode::Esc]), Action::Quit);
    }

    #[test]
    fn esc_clears_active_filters_before_quitting() {
        let mut app = loaded();
        app.repo = Some("acme/app".into());
        app.query = "login".into();
        assert_eq!(press(&mut app, &[KeyCode::Esc]), Action::None);
        assert_eq!(app.repo, None);
        assert_eq!(app.query, "");
        assert_eq!(press(&mut app, &[KeyCode::Esc]), Action::Quit);
    }

    #[test]
    fn repos_are_listed_with_counts() {
        assert_eq!(
            loaded().repos(),
            vec![("acme/app".to_string(), 3), ("acme/lib".to_string(), 2)]
        );
    }

    #[test]
    fn repo_picker_filters_both_sections() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Char('r')]);
        assert_eq!(app.mode, Mode::RepoPicker { cursor: 0 });
        press(
            &mut app,
            &[KeyCode::Down, KeyCode::Char('j'), KeyCode::Enter],
        );
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.repo.as_deref(), Some("acme/lib"));
        assert_eq!(numbers(&app, Section::Reviews), vec![2]);
        assert_eq!(numbers(&app, Section::Issues), vec![10]);
        assert_eq!(app.selected_item().unwrap().number, 2);
    }

    #[test]
    fn repo_picker_starts_on_the_active_repo_and_can_reset() {
        let mut app = loaded();
        app.repo = Some("acme/lib".into());
        press(&mut app, &[KeyCode::Char('r')]);
        assert_eq!(app.mode, Mode::RepoPicker { cursor: 2 });
        press(
            &mut app,
            &[KeyCode::Up, KeyCode::Up, KeyCode::Up, KeyCode::Enter],
        );
        assert_eq!(app.repo, None, "the first entry shows every repository");
    }

    #[test]
    fn repo_picker_can_be_cancelled() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Char('r'), KeyCode::Down, KeyCode::Esc]);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.repo, None);
        assert_eq!(
            press(&mut app, &[KeyCode::Char('r'), KeyCode::Char('q')]),
            Action::None,
            "q closes the picker"
        );
        assert_eq!(app.mode, Mode::Normal);
    }

    #[test]
    fn search_filters_live_and_matches_all_terms() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Char('/')]);
        assert_eq!(app.mode, Mode::Search);
        type_text(&mut app, "LOGIN app");
        assert_eq!(app.query, "LOGIN app");
        assert_eq!(numbers(&app, Section::Reviews), vec![1]);
        assert_eq!(numbers(&app, Section::Issues), vec![11]);
        press(&mut app, &[KeyCode::Enter]);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.query, "LOGIN app", "enter keeps the filter");
    }

    #[test]
    fn search_matches_numbers_authors_and_labels() {
        let mut app = loaded();
        app.query = "#10".into();
        assert_eq!(numbers(&app, Section::Issues), vec![10]);
        assert_eq!(numbers(&app, Section::Reviews), Vec::<u64>::new());
        app.query = "alice bug".into();
        assert_eq!(numbers(&app, Section::Reviews), vec![1, 2, 3]);
    }

    #[test]
    fn search_editing_keys() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Char('/')]);
        type_text(&mut app, "abc");
        press(&mut app, &[KeyCode::Backspace]);
        assert_eq!(app.query, "ab");
        app.handle_key(KeyEvent::new(KeyCode::Char('u'), KeyModifiers::CONTROL));
        assert_eq!(app.query, "");
        type_text(&mut app, "xyz");
        press(&mut app, &[KeyCode::Esc]);
        assert_eq!(app.mode, Mode::Normal);
        assert_eq!(app.query, "", "esc discards the search");
    }

    #[test]
    fn typing_q_in_search_does_not_quit() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Char('/')]);
        assert_eq!(press(&mut app, &[KeyCode::Char('q')]), Action::None);
        assert_eq!(app.query, "q");
    }

    #[test]
    fn cursor_is_clamped_when_the_filter_shrinks_the_list() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Char('G')]);
        assert_eq!(app.selected_item().unwrap().number, 3);
        press(&mut app, &[KeyCode::Char('/')]);
        type_text(&mut app, "parser");
        assert_eq!(app.selected_item().unwrap().number, 2);
    }

    #[test]
    fn reloading_keeps_the_cursor_on_the_same_item() {
        let mut app = loaded();
        press(&mut app, &[KeyCode::Char('j')]);
        let mut fresh = sample();
        fresh
            .review_requests
            .insert(0, item(Kind::PullRequest, "acme/app", 99, "New"));
        app.set_dashboard(fresh, SystemTime::UNIX_EPOCH);
        assert_eq!(app.selected_item().unwrap().number, 2);
    }

    #[test]
    fn reloading_drops_a_repo_filter_with_no_items_left() {
        let mut app = loaded();
        app.repo = Some("acme/gone".into());
        app.set_dashboard(sample(), SystemTime::UNIX_EPOCH);
        assert_eq!(app.repo, None);
        app.repo = Some("acme/lib".into());
        app.set_dashboard(sample(), SystemTime::UNIX_EPOCH);
        assert_eq!(app.repo.as_deref(), Some("acme/lib"));
    }

    #[test]
    fn notices_are_cleared_by_the_next_key() {
        let mut app = loaded();
        app.notice = Some("Opening in browser".into());
        press(&mut app, &[KeyCode::Char('j')]);
        assert_eq!(app.notice, None);
    }

    #[test]
    fn requested_via_names_you_and_teams() {
        let mut pr = item(Kind::PullRequest, "acme/app", 1, "x");
        pr.reviewers = vec![
            Reviewer::User("someone".into()),
            Reviewer::Team {
                org: "acme".into(),
                slug: "core".into(),
            },
            Reviewer::User("octocat".into()),
            Reviewer::Team {
                org: "acme".into(),
                slug: "web".into(),
            },
        ];
        assert_eq!(requested_via(&pr, "octocat"), "you, @acme/core, @acme/web");
        pr.reviewers
            .retain(|r| *r != Reviewer::User("octocat".into()));
        assert_eq!(requested_via(&pr, "octocat"), "@acme/core, @acme/web");
        pr.reviewers.clear();
        assert_eq!(requested_via(&pr, "octocat"), "");
    }

    fn at(secs: u64) -> SystemTime {
        SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(secs)
    }

    #[test]
    fn parses_utc_timestamps() {
        assert_eq!(
            parse_timestamp("1970-01-01T00:00:00Z"),
            Some(SystemTime::UNIX_EPOCH)
        );
        assert_eq!(
            parse_timestamp("2026-11-26T12:00:00Z"),
            Some(at(1_795_694_400))
        );
        assert_eq!(
            parse_timestamp("2024-02-29T23:59:59Z"),
            Some(at(1_709_251_199)),
            "leap day"
        );
        assert_eq!(
            parse_timestamp("2000-03-01T00:00:00Z"),
            Some(at(951_868_800))
        );
        assert_eq!(
            parse_timestamp("2026-11-26T12:00:00.250Z"),
            Some(at(1_795_694_400) + std::time::Duration::from_millis(250)),
            "fractional seconds"
        );
    }

    #[test]
    fn rejects_malformed_timestamps() {
        for bad in [
            "",
            "garbage",
            "2026-11-26",
            "2026-11-26T12:00:00",
            "2026-11-26T12:00:00+09:00",
            "2026-11-26 12:00:00Z",
            "2026-13-01T00:00:00Z",
            "2026-00-01T00:00:00Z",
            "2023-02-29T00:00:00Z",
            "2026-04-31T00:00:00Z",
            "2026-11-26T24:00:00Z",
            "2026-11-26T12:60:00Z",
            "2026-11-26T12:00:61Z",
            "2026-11-26T12:00:00.Z",
            "1969-12-31T23:59:59Z",
            "+026-11-26T12:00:00Z",
        ] {
            assert_eq!(parse_timestamp(bad), None, "{bad:?}");
        }
    }

    #[test]
    fn age_of_compares_two_instants() {
        assert_eq!(age_of(at(0), at(90)), "1m");
        assert_eq!(age_of(at(100), at(0)), "now", "clock skew");
    }

    #[test]
    fn age_is_compact() {
        let now = parse_timestamp("2026-11-26T12:00:00Z").unwrap();
        assert_eq!(age("2026-11-26T11:59:30Z", now), "now");
        assert_eq!(age("2026-11-26T11:55:00Z", now), "5m");
        assert_eq!(age("2026-11-26T09:00:00Z", now), "3h");
        assert_eq!(age("2026-11-24T12:00:00Z", now), "2d");
        assert_eq!(age("2026-11-05T12:00:00Z", now), "3w");
        assert_eq!(age("2026-07-01T12:00:00Z", now), "4mo");
        assert_eq!(age("2024-11-01T12:00:00Z", now), "2y");
        assert_eq!(age("2026-11-26T13:00:00Z", now), "now", "clock skew");
        assert_eq!(age("garbage", now), "");
    }
}

#[cfg(test)]
pub(crate) mod properties {
    use super::*;
    use proptest::prelude::*;

    /// Any key press, with any modifier: letters, the dashboard's keys and
    /// navigation keys.
    pub fn key_event() -> impl Strategy<Value = KeyEvent> {
        let code = prop_oneof![
            prop::char::range('a', 'z').prop_map(KeyCode::Char),
            prop::sample::select(vec!['/', 'R', 'G', ' ', '#', '1', '-']).prop_map(KeyCode::Char),
            prop::sample::select(vec![
                KeyCode::Up,
                KeyCode::Down,
                KeyCode::Tab,
                KeyCode::BackTab,
                KeyCode::Enter,
                KeyCode::Esc,
                KeyCode::Backspace,
                KeyCode::Home,
                KeyCode::End,
                KeyCode::F(5),
            ]),
        ];
        let modifiers = prop::sample::select(vec![
            KeyModifiers::NONE,
            KeyModifiers::CONTROL,
            KeyModifiers::ALT,
            KeyModifiers::SHIFT,
        ]);
        (code, modifiers).prop_map(|(code, modifiers)| KeyEvent::new(code, modifiers))
    }

    fn is_leap(year: u64) -> bool {
        (year.is_multiple_of(4) && !year.is_multiple_of(100)) || year.is_multiple_of(400)
    }

    fn month_lengths(year: u64) -> [u64; 12] {
        [
            31,
            if is_leap(year) { 29 } else { 28 },
            31,
            30,
            31,
            30,
            31,
            31,
            30,
            31,
            30,
            31,
        ]
    }

    /// Seconds since the epoch, counted day by day.
    fn naive_seconds(year: u64, month: u64, day: u64, hms: (u64, u64, u64)) -> u64 {
        let year_days: u64 = (1970..year)
            .map(|y| if is_leap(y) { 366 } else { 365 })
            .sum();
        let month_days: u64 = month_lengths(year)[..usize::try_from(month - 1).unwrap()]
            .iter()
            .sum();
        (year_days + month_days + day - 1) * 86_400 + hms.0 * 3600 + hms.1 * 60 + hms.2
    }

    proptest! {
        #[test]
        fn timestamps_never_panic(text in any::<String>()) {
            let _ = parse_timestamp(&text);
        }

        #[test]
        fn timestamp_like_text_never_panics(
            text in "[0-9]{4}-[0-9]{2}-[0-9]{2}T[0-9]{2}:[0-9]{2}:[0-9]{2}(\\.[0-9]{0,12})?Z",
        ) {
            if let Some(time) = parse_timestamp(&text) {
                prop_assert!(time >= SystemTime::UNIX_EPOCH);
            }
        }

        #[test]
        fn valid_timestamps_match_a_day_count(
            year in 1970u64..2500,
            month in 1u64..=12,
            day_seed in 0u64..31,
            hms in (0u64..24, 0u64..60, 0u64..60),
        ) {
            let day = 1 + day_seed % month_lengths(year)[usize::try_from(month - 1).unwrap()];
            let text = format!("{year:04}-{month:02}-{day:02}T{:02}:{:02}:{:02}Z", hms.0, hms.1, hms.2);
            let expected = SystemTime::UNIX_EPOCH
                + std::time::Duration::from_secs(naive_seconds(year, month, day, hms));
            prop_assert_eq!(parse_timestamp(&text), Some(expected), "{}", text);
        }

        #[test]
        fn keys_keep_the_cursor_in_bounds(
            keys in prop::collection::vec(key_event(), 0..60),
            empty in any::<bool>(),
        ) {
            let mut app = App::new();
            let dashboard = if empty { Dashboard::default() } else { tests::sample() };
            app.set_dashboard(dashboard, SystemTime::UNIX_EPOCH);
            for key in keys {
                let _ = app.handle_key(key);
                for section in [Section::Reviews, Section::Issues] {
                    let len = app.visible(section).len();
                    prop_assert!(len == 0 || app.selected_index(section) < len);
                }
                prop_assert_eq!(app.selected_item().is_some(), !app.visible(app.section).is_empty());
                if let Mode::RepoPicker { cursor } = app.mode {
                    prop_assert!(cursor <= app.repos().len());
                }
                if let Some(repo) = &app.repo {
                    prop_assert!(app.repos().iter().any(|(r, _)| r == repo));
                }
            }
        }
    }
}
