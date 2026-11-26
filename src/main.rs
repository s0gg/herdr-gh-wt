use std::io::{BufRead, Write};
use std::process::ExitCode;
use std::sync::{Arc, mpsc};
use std::time::{Duration, SystemTime};

use anyhow::Result;
use ratatui::DefaultTerminal;
use ratatui::crossterm::event::{self, Event, KeyEventKind};

use herdr_gh_wt::app::{Action, App, Load};
use herdr_gh_wt::base_picker::{BaseCandidate, BasePicker, PickerAction};
use herdr_gh_wt::config::Config;
use herdr_gh_wt::github::{self, Dashboard, Item};
use herdr_gh_wt::herdr::{Herdr, PaneOpen};
use herdr_gh_wt::open::{Opener, Outcome};
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
            Ok(Some(action @ (Action::Open(_) | Action::OpenWithBase(_)))) => {
                let (item, choose_base) = match action {
                    Action::OpenWithBase(item) => (item, true),
                    Action::Open(item) => (item, false),
                    _ => unreachable!(),
                };
                ratatui::restore();
                match open(&opener, &item, choose_base) {
                    AfterOpen::Exit => return Ok(()),
                    AfterOpen::Back(notice) => app.notice = notice,
                }
                terminal = ratatui::init();
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

/// What the dashboard does once an item was opened (or not).
enum AfterOpen {
    /// Close the dashboard.
    Exit,
    /// Go back to the dashboard, optionally with a notice.
    Back(Option<String>),
}

/// Open `item` outside the TUI so worktrunk's hooks and prompts are visible.
/// With `choose_base`, a new issue branch starts from a branch the user picks.
/// When the focus moves to the item, the dashboard closes; otherwise it stays
/// so more items can be opened.
fn open(opener: &Opener, item: &Item, choose_base: bool) -> AfterOpen {
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
    let mut pick = |candidates: &[BaseCandidate]| pick_base(item, candidates);
    let result = if choose_base {
        opener.open_with(item, Some(&mut pick), &mut log)
    } else {
        opener.open(item, &mut log)
    };
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

/// Let the user pick the base branch in a full-screen picker; `None` cancels.
fn pick_base(item: &Item, candidates: &[BaseCandidate]) -> Result<Option<String>> {
    let mut picker = BasePicker::new(candidates.to_vec());
    let mut terminal = ratatui::init();
    let result = (|| loop {
        terminal.draw(|frame| ui::render_base_picker(frame, &picker, item))?;
        if let Event::Key(key) = event::read()?
            && key.kind == KeyEventKind::Press
        {
            match picker.handle_key(key) {
                PickerAction::Pick(base) => return Ok(Some(base)),
                PickerAction::Cancel => return Ok(None),
                PickerAction::None => {}
            }
        }
    })();
    ratatui::restore();
    if let Ok(Some(base)) = &result {
        println!("\x1b[36m›\x1b[0m Base branch: {base}");
    }
    result
}

/// Print `prompt` and wait for a line of input; returns what was typed.
fn wait_for_enter(prompt: &str) -> String {
    println!("{prompt}");
    let mut line = String::new();
    let _ = std::io::stdin().lock().read_line(&mut line);
    line.trim().to_string()
}
