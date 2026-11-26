use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use ratatui::crossterm::event::{self, Event, KeyEventKind};
use ratatui::{DefaultTerminal, Frame};

use herdr_gh_wt::app::{Action, App, Load};
use herdr_gh_wt::config::Config;
use herdr_gh_wt::github::{self, Dashboard, Item};
use herdr_gh_wt::herdr::{Herdr, PaneOpen};
use herdr_gh_wt::open::{BranchChoice, IssueBranch, Opener, Outcome};
use herdr_gh_wt::picker::{
    BaseCandidate, BranchCandidate, Entry, Picker, PickerAction, SuggestedBranch, Suggestion,
};
use herdr_gh_wt::runner::{Runner, SystemRunner};
use herdr_gh_wt::ui;

const USAGE: &str = "\
herdr-gh-wt — GitHub review requests and assigned issues for Herdr

Usage:
  herdr-gh-wt launch      Open the dashboard pane (used by the plugin action)
  herdr-gh-wt dashboard   Run the dashboard in the current terminal
";

fn env(key: &str) -> Option<String> {
    std::env::var(key).ok()
}

fn main() -> ExitCode {
    let command = std::env::args().nth(1);
    let result = match command.as_deref() {
        Some("launch") => launch(),
        Some("dashboard") => dashboard(),
        Some("-V" | "--version") => {
            println!("herdr-gh-wt {}", env!("CARGO_PKG_VERSION"));
            Ok(())
        }
        Some("-h" | "--help") => {
            print!("{USAGE}");
            Ok(())
        }
        _ => {
            eprint!("{USAGE}");
            return ExitCode::from(2);
        }
    };
    match result {
        Ok(()) => ExitCode::SUCCESS,
        Err(err) => {
            eprintln!("herdr-gh-wt: {err:#}");
            ExitCode::FAILURE
        }
    }
}

/// Plugin action: open the dashboard as a Herdr plugin pane.
fn launch() -> Result<()> {
    // With a broken config the pane still opens with the defaults, and the
    // dashboard shows what is wrong; a failing action would only be logged.
    let config = Config::load(&env).unwrap_or_default();
    let pane = PaneOpen::dashboard(&config.dashboard, env("HERDR_PLUGIN_ID"));
    Herdr::from_env(&env).plugin_pane_open(&SystemRunner, &pane)
}

fn dashboard() -> Result<()> {
    let config = match Config::load(&env) {
        Ok(config) => config,
        Err(err) => {
            eprintln!("\x1b[31mherdr-gh-wt: {err:#}\x1b[0m");
            wait_for_enter("Press Enter to close.");
            return Err(err);
        }
    };
    let runner: Arc<dyn Runner + Send + Sync> = Arc::new(SystemRunner);
    let opener = Opener {
        runner: Arc::clone(&runner),
        herdr: Herdr::from_env(&env),
        pi: config.pi.clone(),
        focus: config.open.focus,
    };
    let limit = config.dashboard.limit;

    let (tx, rx) = mpsc::channel();
    let fetch = |tx: &mpsc::Sender<Result<Dashboard>>| {
        let (runner, tx) = (Arc::clone(&runner), tx.clone());
        std::thread::spawn(move || {
            let _ = tx.send(github::fetch(&*runner, limit));
        });
    };
    fetch(&tx);

    let mut app = App::new();
    let mut terminal = ratatui::init();
    let result = loop {
        match step(&mut terminal, &mut app, &rx) {
            Ok(None | Some(Action::None)) => {}
            Ok(Some(Action::Quit)) => break Ok(()),
            Ok(Some(Action::Refresh)) => {
                app.load = Load::Loading;
                fetch(&tx);
            }
            Ok(Some(Action::Browse(item))) => browse(&runner, &item, &mut app),
            Ok(Some(Action::Open(item))) => {
                if open_outside_tui(&opener, &item, Ask::Suggested, &mut app, &mut terminal) {
                    return Ok(());
                }
            }
            Ok(Some(Action::OpenWithBase(item))) => {
                if open_outside_tui(&opener, &item, Ask::Base, &mut app, &mut terminal) {
                    return Ok(());
                }
            }
            Ok(Some(Action::Link(item))) => {
                if open_outside_tui(&opener, &item, Ask::Branch, &mut app, &mut terminal) {
                    return Ok(());
                }
            }
            Err(err) => break Err(err),
        }
    };
    ratatui::restore();
    result
}

/// Draw, take in fetched data, and handle at most one key press.
fn step(
    terminal: &mut DefaultTerminal,
    app: &mut App,
    rx: &mpsc::Receiver<Result<Dashboard>>,
) -> Result<Option<Action>> {
    while let Ok(fetched) = rx.try_recv() {
        match fetched {
            Ok(dashboard) => app.set_dashboard(dashboard, SystemTime::now()),
            Err(err) => app.set_error(format!("{err:#}")),
        }
    }
    terminal.draw(|frame| ui::render(frame, app, SystemTime::now()))?;
    if !event::poll(Duration::from_millis(100))? {
        return Ok(None);
    }
    match event::read()? {
        Event::Key(key) if key.kind == KeyEventKind::Press => Ok(Some(app.handle_key(key))),
        _ => Ok(None),
    }
}

fn browse(runner: &Arc<dyn Runner + Send + Sync>, item: &Item, app: &mut App) {
    let cmd = github::browse_command(item);
    app.notice = Some(match runner.stdout(&cmd) {
        Ok(_) => format!("Opened {} {} in the browser", item.repo, item.reference()),
        Err(err) => format!("Could not open the browser: {err:#}"),
    });
}

/// What the user is asked before an item opens.
#[derive(Clone, Copy)]
enum Ask {
    /// Only whether to use a branch that looks like the issue's own, when
    /// there is one.
    Suggested,
    /// The base branch of a new issue branch.
    Base,
    /// The existing branch to link an issue to.
    Branch,
}

/// Leave the TUI to open `item`, then come back to it. Returns whether the
/// dashboard should close instead.
fn open_outside_tui(
    opener: &Opener,
    item: &Item,
    ask: Ask,
    app: &mut App,
    terminal: &mut DefaultTerminal,
) -> bool {
    ratatui::restore();
    match open(opener, item, ask) {
        AfterOpen::Exit => true,
        AfterOpen::Back(notice) => {
            app.notice = notice;
            *terminal = ratatui::init();
            false
        }
    }
}

/// What the dashboard does once an item was opened (or not).
enum AfterOpen {
    /// Close the dashboard.
    Exit,
    /// Go back to the dashboard, optionally with a notice.
    Back(Option<String>),
}

/// Open `item` outside the TUI so worktrunk's hooks and prompts are visible,
/// after asking the user what `ask` says. When the focus moves to the item,
/// the dashboard closes; otherwise it stays so more items can be opened.
fn open(opener: &Opener, item: &Item, ask: Ask) -> AfterOpen {
    print!("\x1b[2J\x1b[H");
    println!(
        "\x1b[1mOpening {} {}\x1b[0m: {}\n",
        item.repo,
        item.reference(),
        item.title
    );
    let mut log = |line: &str| {
        println!("\x1b[36m›\x1b[0m {line}");
        let _ = std::io::stdout().flush();
    };
    let mut base = |candidates: &[BaseCandidate]| pick_base(item, candidates);
    let mut branch = |candidates: &[BranchCandidate]| pick_branch(item, candidates);
    let mut suggested = |suggestions: &[SuggestedBranch]| pick_suggested(item, suggestions);
    let how = match ask {
        Ask::Suggested => IssueBranch::Suggest(&mut suggested),
        Ask::Base => IssueBranch::FromBase(&mut base),
        Ask::Branch => IssueBranch::Link(&mut branch),
    };
    let result = opener.open_with(item, how, &mut log);
    let name = format!("{} {}", item.repo, item.reference());
    match result {
        Ok(Outcome::AlreadyOpen { .. } | Outcome::Opened { .. }) if opener.focus => AfterOpen::Exit,
        Ok(Outcome::AlreadyOpen { .. }) => {
            AfterOpen::Back(Some(format!("{name} is already open in Herdr")))
        }
        Ok(Outcome::Opened { label, .. }) => AfterOpen::Back(Some(format!(
            "Opened {name} in the background as \"{label}\""
        ))),
        Ok(Outcome::Cancelled) => AfterOpen::Back(None),
        Err(err) => {
            println!("\n\x1b[31m✗ {err:#}\x1b[0m");
            let answer = wait_for_enter(
                "\nPress Enter to return to the dashboard, or q and Enter to close.",
            );
            if answer.eq_ignore_ascii_case("q") {
                AfterOpen::Exit
            } else {
                AfterOpen::Back(None)
            }
        }
    }
}

/// Run `picker` full screen until the user picks an entry or typed text,
/// or cancels.
fn run_picker<T: Entry + Clone>(
    mut picker: Picker<T>,
    render: impl Fn(&mut Frame, &Picker<T>),
) -> Result<PickerAction<T>> {
    let mut terminal = ratatui::init();
    let result = (|| loop {
        terminal.draw(|frame| render(frame, &picker))?;
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match picker.handle_key(key) {
                PickerAction::None => {}
                done => return Ok(done),
            }
        }
    })();
    ratatui::restore();
    result
}

/// Let the user pick the base branch of a new issue branch; `None` cancels.
fn pick_base(item: &Item, candidates: &[BaseCandidate]) -> Result<Option<String>> {
    let picker = Picker::new(candidates.to_vec()).accepting_typed();
    let base = match run_picker(picker, |frame, picker| {
        ui::render_base_picker(frame, picker, item);
    })? {
        PickerAction::Pick(candidate) => Some(candidate.name),
        PickerAction::Typed(base) => Some(base),
        PickerAction::Cancel | PickerAction::None => None,
    };
    if let Some(base) = &base {
        println!("\x1b[36m›\x1b[0m Base branch: {base}");
    }
    Ok(base)
}

/// Let the user pick the branch to link an issue to; `None` cancels.
fn pick_branch(item: &Item, candidates: &[BranchCandidate]) -> Result<Option<String>> {
    let picker = Picker::new(candidates.to_vec());
    Ok(
        match run_picker(picker, |frame, picker| {
            ui::render_branch_picker(frame, picker, item);
        })? {
            PickerAction::Pick(candidate) => Some(candidate.name),
            _ => None,
        },
    )
}

/// Let the user choose between the branches that look like the issue's own
/// and a new branch; `None` cancels.
fn pick_suggested(item: &Item, suggestions: &[SuggestedBranch]) -> Result<Option<BranchChoice>> {
    let entries = suggestions
        .iter()
        .cloned()
        .map(Suggestion::Existing)
        .chain([Suggestion::NewBranch])
        .collect();
    Ok(
        match run_picker(Picker::new(entries), |frame, picker| {
            ui::render_suggestion_picker(frame, picker, item);
        })? {
            PickerAction::Pick(Suggestion::Existing(s)) => {
                Some(BranchChoice::Existing(s.branch.name))
            }
            PickerAction::Pick(Suggestion::NewBranch) => Some(BranchChoice::New),
            _ => None,
        },
    )
}

/// Print `prompt` and wait for a line of input; returns what was typed.
fn wait_for_enter(prompt: &str) -> String {
    println!("{prompt}");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}
