//! Rendering of the dashboard.

use std::time::SystemTime;

use ratatui::Frame;
use ratatui::layout::{Alignment, Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style, Stylize};
use ratatui::text::{Line, Span, Text};
use ratatui::widgets::{
    Block, BorderType, Cell, Clear, List, ListItem, ListState, Paragraph, Row, Table, TableState,
    Wrap,
};

use crate::app::{App, Load, Mode, Section, age, age_of, requested_via};
use crate::github::{Item, Kind};
use crate::picker::{BaseCandidate, Picker};

const ACCENT: Color = Color::Cyan;

/// A count as terminal cells, saturating at the largest size a terminal has.
fn cells(n: usize) -> u16 {
    u16::try_from(n).unwrap_or(u16::MAX)
}

/// The full-screen picker for the base branch of a new issue branch.
pub fn render_base_picker(frame: &mut Frame, picker: &Picker<BaseCandidate>, item: &Item) {
    let [header, prompt, list, footer] = Layout::vertical([
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());

    let title = Text::from(vec![
        Line::from(vec![
            Span::from(" New branch ")
                .bold()
                .fg(Color::Black)
                .bg(ACCENT),
            Span::from(format!(" {} {} ", item.repo, item.reference())).fg(Color::Blue),
            Span::from(item.title.clone()).bold(),
        ]),
        Line::from(" Choose the base (parent) branch for the new branch.").dim(),
    ]);
    frame.render_widget(Paragraph::new(title), header);

    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::from(" base ❯ ").fg(ACCENT).bold(),
            Span::from(picker.query.as_str()),
            Span::from("█").fg(ACCENT),
        ])),
        prompt,
    );

    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(ACCENT))
        .title(Line::from(format!(" Branches ({}) ", picker.filtered().len())).bold());
    let filtered = picker.filtered();
    if filtered.is_empty() {
        let typed = picker.query.trim();
        let message = if typed.is_empty() {
            "  No branches found. Type a base such as pr:123 or a commit.".to_string()
        } else {
            format!("  No branch matches; enter uses \"{typed}\" as the base.")
        };
        frame.render_widget(Paragraph::new(message).dim().block(block), list);
    } else {
        let entries: Vec<ListItem> = filtered
            .iter()
            .map(|c| {
                let mut spans = vec![Span::from(c.name.clone())];
                if c.is_default {
                    spans.push(Span::from("  default").fg(Color::Green));
                }
                if c.remote {
                    spans.push(Span::from("  remote").fg(Color::Yellow));
                }
                ListItem::new(Line::from(spans))
            })
            .collect();
        let widget = List::new(entries)
            .block(block)
            .highlight_symbol("▶ ")
            .highlight_style(Style::new().bg(Color::DarkGray).bold());
        let mut state = ListState::default().with_selected(Some(picker.cursor()));
        frame.render_stateful_widget(widget, list, &mut state);
    }

    frame.render_widget(
        Paragraph::new(hints(&[
            ("↑↓", "choose"),
            ("enter", "create from it"),
            ("alt-enter", "use the typed text"),
            ("esc", "cancel"),
        ])),
        footer,
    );
}

pub fn render(frame: &mut Frame, app: &App, now: SystemTime) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Fill(1),
        Constraint::Length(1),
    ])
    .areas(frame.area());
    render_header(frame, header, app, now);
    render_footer(frame, footer, app);

    match &app.load {
        Load::Loading if app.fetched_at.is_none() => {
            render_centered(
                frame,
                body,
                Text::from("Loading review requests and issues...").fg(ACCENT),
            );
        }
        Load::Failed(message) if app.fetched_at.is_none() => {
            let text = Text::from(vec![
                Line::from("Could not load data from GitHub").red().bold(),
                Line::from(""),
                Line::from(message.as_str()),
                Line::from(""),
                Line::from("Press R to retry or q to quit.").dim(),
            ]);
            render_centered(frame, body, text);
        }
        _ => render_body(frame, body, app, now),
    }

    if let Mode::RepoPicker { cursor } = app.mode {
        render_repo_picker(frame, body, app, cursor);
    }
}

fn render_header(frame: &mut Frame, area: Rect, app: &App, now: SystemTime) {
    let title = Line::from(vec![
        Span::from(" GitHub ").bold().fg(Color::Black).bg(ACCENT),
        Span::from(" Review requests & assigned issues").bold(),
    ]);
    frame.render_widget(Paragraph::new(title), area);

    let mut status = vec![];
    if let Some(repo) = &app.repo {
        status.push(Span::from(format!("repo: {repo}")).fg(Color::Yellow));
    }
    if !app.query.is_empty() && app.mode != Mode::Search {
        status.push(Span::from(format!("filter: {}", app.query)).fg(Color::Yellow));
    }
    if !app.dashboard.viewer.is_empty() {
        status.push(Span::from(format!("@{}", app.dashboard.viewer)));
    }
    match &app.load {
        Load::Loading => status.push(Span::from("refreshing...").fg(ACCENT)),
        Load::Failed(message) => {
            status.push(Span::from(format!("refresh failed: {message}")).red());
        }
        Load::Loaded => {
            if let Some(at) = app.fetched_at {
                let ago = age_of(at, now);
                status.push(
                    Span::from(if ago == "now" {
                        "updated just now".to_string()
                    } else {
                        format!("updated {ago} ago")
                    })
                    .dim(),
                );
            }
        }
    }
    let mut spans = Vec::new();
    for (i, span) in status.into_iter().enumerate() {
        if i > 0 {
            spans.push(Span::from("  ·  ").dim());
        }
        spans.push(span);
    }
    spans.push(Span::from(" "));
    frame.render_widget(
        Paragraph::new(Line::from(spans)).alignment(Alignment::Right),
        area,
    );
}

fn render_footer(frame: &mut Frame, area: Rect, app: &App) {
    let line = match app.mode {
        Mode::Normal if app.notice.is_some() => {
            Line::from(format!(" {}", app.notice.as_deref().unwrap_or_default())).fg(Color::Yellow)
        }
        Mode::Search => Line::from(vec![
            Span::from(" /").fg(ACCENT).bold(),
            Span::from(app.query.as_str()),
            Span::from("█").fg(ACCENT),
            Span::from("   enter apply · esc clear · ctrl-u erase").dim(),
        ]),
        Mode::RepoPicker { .. } => {
            hints(&[("↑↓", "choose"), ("enter", "filter"), ("esc", "cancel")])
        }
        Mode::Normal => hints(&[
            ("↑↓", "move"),
            ("tab", "section"),
            ("enter", "open worktree"),
            ("b", "issue from base"),
            ("o", "browser"),
            ("r", "repository"),
            ("/", "search"),
            ("R", "refresh"),
            ("q", "quit"),
        ]),
    };
    frame.render_widget(Paragraph::new(line), area);
}

fn hints(pairs: &[(&str, &str)]) -> Line<'static> {
    let mut spans = vec![Span::from(" ")];
    for (key, what) in pairs {
        spans.push(Span::from(key.to_string()).fg(ACCENT).bold());
        spans.push(Span::from(format!(" {what}   ")).dim());
    }
    Line::from(spans)
}

fn render_centered(frame: &mut Frame, area: Rect, text: Text) {
    let height = cells(text.lines.len()).min(area.height);
    let [_, middle, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(height),
        Constraint::Fill(1),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(text)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: true }),
        middle,
    );
}

fn render_body(frame: &mut Frame, area: Rect, app: &App, now: SystemTime) {
    let (lists, details) = if area.width >= 150 {
        let [lists, details] =
            Layout::horizontal([Constraint::Percentage(62), Constraint::Percentage(38)])
                .areas(area);
        (lists, details)
    } else {
        let detail_height = if area.height >= 24 { 9 } else { 0 };
        let [lists, details] =
            Layout::vertical([Constraint::Fill(1), Constraint::Length(detail_height)]).areas(area);
        (lists, details)
    };

    let reviews = app.visible(Section::Reviews);
    let issues = app.visible(Section::Issues);
    let (reviews_height, issues_height) = split_heights(lists.height, reviews.len(), issues.len());
    let [reviews_area, issues_area] = Layout::vertical([
        Constraint::Length(reviews_height),
        Constraint::Length(issues_height),
    ])
    .areas(lists);

    render_section(frame, reviews_area, app, Section::Reviews, &reviews, now);
    render_section(frame, issues_area, app, Section::Issues, &issues, now);
    if details.height > 0 {
        render_details(frame, details, app, now);
    }
}

/// Share `height` between the two lists: each gets what it needs (rows plus
/// borders and header) when possible, otherwise at least half.
fn split_heights(height: u16, reviews: usize, issues: usize) -> (u16, u16) {
    let need = |rows: usize| cells(rows.max(1)).saturating_add(3);
    let (need_reviews, need_issues) = (need(reviews), need(issues));
    let reviews_height = if need_reviews + need_issues <= height {
        need_reviews
    } else {
        need_reviews.min((height / 2).max(height.saturating_sub(need_issues)))
    };
    (reviews_height, height - reviews_height)
}

fn render_section(
    frame: &mut Frame,
    area: Rect,
    app: &App,
    section: Section,
    items: &[&Item],
    now: SystemTime,
) {
    let focused = app.section == section;
    let title = match section {
        Section::Reviews => format!(" Review requests ({}) ", items.len()),
        Section::Issues => format!(" Assigned issues ({}) ", items.len()),
    };
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(if focused {
            Style::new().fg(ACCENT)
        } else {
            Style::new().dark_gray()
        })
        .title(Line::from(title).bold());

    if items.is_empty() {
        frame.render_widget(Paragraph::new("  Nothing here").dim().block(block), area);
        return;
    }

    let repo_width = cells(
        items
            .iter()
            .map(|i| i.repo.chars().count())
            .max()
            .unwrap_or(0)
            .min(28),
    );
    let (header, widths, rows): (Vec<&str>, Vec<Constraint>, Vec<Row>) = match section {
        Section::Reviews => (
            vec![
                "Repository",
                "#",
                "Title",
                "Author",
                "Requested from",
                "Age",
            ],
            vec![
                Constraint::Length(repo_width),
                Constraint::Length(6),
                Constraint::Fill(1),
                Constraint::Max(16),
                Constraint::Max(30),
                Constraint::Length(4),
            ],
            items
                .iter()
                .map(|i| {
                    let mut title = vec![];
                    if i.is_draft {
                        title.push(Span::from("[draft] ").dim());
                    }
                    title.push(Span::from(i.title.clone()));
                    Row::new(vec![
                        Cell::from(i.repo.clone()).fg(Color::Blue),
                        Cell::from(format!("#{}", i.number)).fg(Color::Magenta),
                        Cell::from(Line::from(title)),
                        Cell::from(
                            i.author
                                .as_deref()
                                .map(|a| format!("@{a}"))
                                .unwrap_or_default(),
                        )
                        .dim(),
                        Cell::from(requested_via(i, &app.dashboard.viewer)).fg(Color::Yellow),
                        Cell::from(age(&i.updated_at, now)).dim(),
                    ])
                })
                .collect(),
        ),
        Section::Issues => (
            vec!["Repository", "#", "Title", "Labels", "Age"],
            vec![
                Constraint::Length(repo_width),
                Constraint::Length(6),
                Constraint::Fill(1),
                Constraint::Max(30),
                Constraint::Length(4),
            ],
            items
                .iter()
                .map(|i| {
                    Row::new(vec![
                        Cell::from(i.repo.clone()).fg(Color::Blue),
                        Cell::from(format!("#{}", i.number)).fg(Color::Magenta),
                        Cell::from(i.title.clone()),
                        Cell::from(i.labels.join(", ")).fg(Color::Green),
                        Cell::from(age(&i.updated_at, now)).dim(),
                    ])
                })
                .collect(),
        ),
    };

    let table = Table::new(rows, widths)
        .header(Row::new(header).dim().underlined())
        .block(block)
        .column_spacing(2)
        .highlight_symbol(if focused { "▶ " } else { "  " })
        .row_highlight_style(if focused {
            Style::new()
                .bg(Color::DarkGray)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::new()
        });
    let mut state = TableState::default().with_selected(Some(app.selected_index(section)));
    frame.render_stateful_widget(table, area, &mut state);
}

fn render_details(frame: &mut Frame, area: Rect, app: &App, now: SystemTime) {
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().dark_gray())
        .title(Line::from(" Details ").bold());
    let Some(item) = app.selected_item() else {
        frame.render_widget(
            Paragraph::new("  Nothing selected").dim().block(block),
            area,
        );
        return;
    };

    let mut meta = vec![Span::from(format!("{} {}", item.repo, item.reference())).fg(Color::Blue)];
    if let Some(author) = &item.author {
        meta.push(Span::from(format!(" · by @{author}")).dim());
    }
    let ago = age(&item.updated_at, now);
    if !ago.is_empty() {
        meta.push(
            Span::from(if ago == "now" {
                " · updated just now".to_string()
            } else {
                format!(" · updated {ago} ago")
            })
            .dim(),
        );
    }

    let mut lines = vec![Line::from(item.title.clone()).bold(), Line::from(meta)];
    match item.kind {
        Kind::PullRequest => {
            let mut spans = vec![];
            if let Some(branch) = &item.head_ref {
                spans.push(Span::from("branch ").dim());
                spans.push(Span::from(branch.clone()).fg(Color::Green));
            }
            let via = requested_via(item, &app.dashboard.viewer);
            if !via.is_empty() {
                spans.push(Span::from("  review requested from ").dim());
                spans.push(Span::from(via).fg(Color::Yellow));
            }
            lines.push(Line::from(spans));
        }
        Kind::Issue => {
            if !item.labels.is_empty() {
                lines.push(Line::from(vec![
                    Span::from("labels ").dim(),
                    Span::from(item.labels.join(", ")).fg(Color::Green),
                ]));
            }
        }
    }
    lines.push(Line::from(item.url.clone()).dim().underlined());
    let body = item.body.trim();
    if !body.is_empty() {
        lines.push(Line::from(""));
        lines.extend(body.lines().map(|l| Line::from(l.to_string())));
    }
    frame.render_widget(
        Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .block(block),
        area,
    );
}

fn render_repo_picker(frame: &mut Frame, area: Rect, app: &App, cursor: usize) {
    let repos = app.repos();
    let total: usize = repos.iter().map(|(_, n)| n).sum();
    let mut entries = vec![ListItem::new(format!("All repositories ({total})"))];
    entries.extend(
        repos
            .iter()
            .map(|(repo, n)| ListItem::new(format!("{repo} ({n})"))),
    );

    let rows = cells(entries.len());
    let longest = cells(
        repos
            .iter()
            .map(|(r, _)| r.chars().count() + 8)
            .max()
            .unwrap_or(0)
            .max(28),
    );
    let popup_width = longest.saturating_add(6).min(area.width);
    let popup_height = rows.saturating_add(2).min(area.height);
    let [_, row, _] = Layout::vertical([
        Constraint::Fill(1),
        Constraint::Length(popup_height),
        Constraint::Fill(1),
    ])
    .areas(area);
    let [_, popup, _] = Layout::horizontal([
        Constraint::Fill(1),
        Constraint::Length(popup_width),
        Constraint::Fill(1),
    ])
    .areas(row);

    let list = List::new(entries)
        .block(
            Block::bordered()
                .border_type(BorderType::Rounded)
                .border_style(Style::new().fg(ACCENT))
                .title(Line::from(" Filter by repository ").bold()),
        )
        .highlight_symbol("▶ ")
        .highlight_style(Style::new().bg(Color::DarkGray).bold());
    let mut state = ListState::default().with_selected(Some(cursor));
    frame.render_widget(Clear, popup);
    frame.render_stateful_widget(list, popup, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::tests::sample;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    fn draw(app: &App, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let now = crate::app::parse_timestamp("2026-11-26T03:00:00Z").unwrap();
        terminal.draw(|frame| render(frame, app, now)).unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    fn loaded() -> App {
        let mut app = App::new();
        app.set_dashboard(sample(), SystemTime::UNIX_EPOCH);
        app
    }

    fn press(app: &mut App, code: KeyCode) {
        app.handle_key(KeyEvent::new(code, KeyModifiers::NONE));
    }

    #[test]
    fn shows_both_sections_with_counts_and_rows() {
        let screen = draw(&loaded(), 120, 30);
        assert!(screen.contains("Review requests (3)"), "{screen}");
        assert!(screen.contains("Assigned issues (2)"), "{screen}");
        assert!(screen.contains("Add login"), "{screen}");
        assert!(screen.contains("Parser crashes"), "{screen}");
        assert!(screen.contains("#11"), "{screen}");
        assert!(screen.contains("acme/lib"), "{screen}");
        assert!(screen.contains("@octocat"), "{screen}");
        assert!(screen.contains("3h"), "age column: {screen}");
        assert!(
            screen.contains("b issue from base"),
            "footer hints: {screen}"
        );
    }

    #[test]
    fn shows_details_of_the_selection() {
        let screen = draw(&loaded(), 120, 30);
        assert!(
            screen.contains("https://github.com/acme/app/pull/1"),
            "{screen}"
        );
        assert!(screen.contains("branch-1"), "{screen}");
    }

    #[test]
    fn shows_loading_and_errors() {
        let screen = draw(&App::new(), 80, 20);
        assert!(screen.contains("Loading"), "{screen}");
        let mut app = App::new();
        app.set_error("gh: not logged in".into());
        let screen = draw(&app, 80, 20);
        assert!(screen.contains("gh: not logged in"), "{screen}");
        assert!(screen.contains("retry"), "{screen}");
    }

    #[test]
    fn shows_empty_sections() {
        let mut app = App::new();
        app.set_dashboard(crate::github::Dashboard::default(), SystemTime::UNIX_EPOCH);
        let screen = draw(&app, 80, 20);
        assert!(screen.contains("Review requests (0)"), "{screen}");
        assert!(screen.contains("Nothing here"), "{screen}");
    }

    #[test]
    fn shows_the_repo_picker() {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('r'));
        let screen = draw(&app, 100, 30);
        assert!(screen.contains("All repositories (5)"), "{screen}");
        assert!(screen.contains("acme/lib (2)"), "{screen}");
    }

    #[test]
    fn shows_active_filters_and_the_search_prompt() {
        let mut app = loaded();
        press(&mut app, KeyCode::Char('/'));
        for c in "login".chars() {
            press(&mut app, KeyCode::Char(c));
        }
        let screen = draw(&app, 120, 30);
        assert!(screen.contains("/login"), "{screen}");
        assert!(screen.contains("Review requests (1)"), "{screen}");
        press(&mut app, KeyCode::Enter);
        app.repo = Some("acme/app".into());
        let screen = draw(&app, 120, 30);
        assert!(screen.contains("repo: acme/app"), "{screen}");
        assert!(screen.contains("filter: login"), "{screen}");
    }

    #[test]
    fn shows_notices_in_the_footer() {
        let mut app = loaded();
        app.notice = Some("could not open the browser".into());
        let screen = draw(&app, 120, 30);
        assert!(screen.contains("could not open the browser"), "{screen}");
    }

    fn draw_picker(picker: &Picker<BaseCandidate>, width: u16, height: u16) -> String {
        let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
        let item = sample().assigned_issues[0].clone();
        terminal
            .draw(|frame| render_base_picker(frame, picker, &item))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..buffer.area.height {
            for x in 0..buffer.area.width {
                text.push_str(buffer[(x, y)].symbol());
            }
            text.push('\n');
        }
        text
    }

    #[test]
    fn base_picker_lists_branches_with_their_kind() {
        let picker = Picker::new(crate::picker::tests::candidates()).accepting_typed();
        let screen = draw_picker(&picker, 100, 20);
        assert!(screen.contains("Issue #10"), "{screen}");
        assert!(screen.contains("Parser crashes"), "{screen}");
        assert!(screen.contains("main"), "{screen}");
        assert!(screen.contains("default"), "{screen}");
        assert!(screen.contains("origin/epic/search-v2"), "{screen}");
        assert!(screen.contains("remote"), "{screen}");
        assert!(screen.contains("esc"), "hints: {screen}");
    }

    #[test]
    fn base_picker_shows_the_query_and_the_typed_fallback() {
        let mut picker = Picker::new(crate::picker::tests::candidates()).accepting_typed();
        for c in "pr:123".chars() {
            picker.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }
        let screen = draw_picker(&picker, 100, 20);
        assert!(screen.contains("pr:123"), "{screen}");
        assert!(screen.contains("No branch matches"), "{screen}");
        draw_picker(&picker, 10, 3);
    }

    #[test]
    fn renders_in_a_tiny_terminal_without_panicking() {
        draw(&loaded(), 20, 5);
        draw(&loaded(), 1, 1);
    }

    #[test]
    fn labels_issues_and_drafts() {
        let mut app = loaded();
        app.dashboard.review_requests[0].is_draft = true;
        let screen = draw(&app, 120, 30);
        assert!(screen.contains("draft"), "{screen}");
        assert!(screen.contains("bug"), "issue labels: {screen}");
    }
}
