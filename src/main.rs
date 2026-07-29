//! An `SQLite`-backed shell-history searcher

use ::std::{
    cmp::Reverse,
    env, fs,
    io::{self, BufRead as _, Write as _},
    path, time,
};

use ::clap::Parser as _;
use ::crossterm::{event, terminal};
use ::fuzzy_matcher::{FuzzyMatcher as _, skim};
use ::ratatui::{backend, layout, style, text, widgets};

/// Bash init snippet printed by `stinkpot init`
const BASH_INIT_SCRIPT: &str = include_str!("../assets/shells/bash/init.sh");

fn main() -> std::process::ExitCode {
    let arguments = Arguments::parse();
    if let Err(error) = arguments.dispatch() {
        eprintln!("{error}");
        return std::process::ExitCode::FAILURE;
    }
    std::process::ExitCode::SUCCESS
}

/// Parsed CLI arguments for `stinkpot`.
#[derive(Debug, ::clap::Parser)]
#[clap(
    author = ::clap::crate_authors!(),
    version = ::clap::crate_version!(),
    propagate_version = true,
    long_about = ::clap::crate_description!()
)]
struct Arguments {
    /// Subcommand to run.
    #[clap(subcommand)]
    command: Command,
}

impl Arguments {
    /// Runs the selected subcommand.
    fn dispatch(self) -> Result<()> {
        match self.command {
            Command::Add { exit, arguments } => Command::add(exit, &arguments),
            Command::Import { history_file } => Command::import(history_file.as_ref()),
            Command::Init { shell } => {
                match shell {
                    SupportedShell::Bash => println!("{BASH_INIT_SCRIPT}"),
                }
                Ok(())
            }
            Command::List { count } => Command::list(count),
            Command::Search { arguments } => Command::search(&arguments),
        }
    }
}

/// All supported shells
#[derive(Debug, Clone, ::clap::ValueEnum)]
enum SupportedShell {
    /// Bash
    Bash,
}

/// Application error type.
#[derive(Debug, ::thiserror::Error)]
enum Error {
    /// A required environment variable was not set.
    #[error("missing environment variable `{0}`")]
    MissingEnv(&'static str),
    /// A `SQLite` operation failed.
    #[error("SQLite error: {0}")]
    Sqlite(#[from] rusqlite::Error),
    /// An I/O error occurred
    #[error("An I/O error occurred: {0}")]
    IO(#[from] ::std::io::Error),
}

/// Result alias using [`Error`].
type Result<T> = std::result::Result<T, Error>;

/// Top-level CLI subcommands.
#[derive(Debug, Clone, ::clap::Subcommand)]
enum Command {
    /// Record a shell command into the history database.
    Add {
        /// Exit status of the command being recorded.
        #[clap(short, long, default_value_t = 0)]
        exit: i32,
        /// Command words to store (everything after `--`).
        #[arg(required = true, last = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
    /// Import commands from a shell history file.
    Import {
        /// File to your bash history
        history_file: Option<std::path::PathBuf>,
    },
    /// Initialize stinkpot in your shell
    Init {
        /// The shell you want to initialize stinkpot for
        shell: SupportedShell,
    },
    /// Print recent history entries.
    List {
        /// The amount of entries to display
        #[clap(default_value_t = 50)]
        count: u32,
    },
    /// Interactively fuzzy-search history and print the selection.
    Search {
        /// Initial query words passed to the search TUI.
        #[arg(required = true, last = true, allow_hyphen_values = true)]
        arguments: Vec<String>,
    },
}

impl Command {
    /// Records a command into the history database
    fn add(exit: i32, arguments: &[String]) -> Result<()> {
        let command = arguments.join(" ");
        if command.trim().is_empty() {
            return Ok(());
        }

        let cwd = env::current_dir()?;
        let cwd = cwd.to_string_lossy();
        let session = env::var("STINKPOT_SESSION").unwrap_or_default();

        // re-run of the same command bumps its timestamp
        let database_path = Database::path()?;
        Database::open_connection(database_path)?
            .execute(
                "insert into history(cmd, cwd, exit, ts, session) values(?1, ?2, ?3, ?4, ?5)
on conflict(cmd) do update set
cwd     = excluded.cwd,
exit    = excluded.exit,
ts      = excluded.ts,
session = excluded.session",
                rusqlite::params![command, cwd, exit, Time::now_unix(), session],
            )
            .map(|_| ())
            .map_err(Into::into)
    }

    /// Prints the `count` most recent history rows
    fn list(count: u32) -> Result<()> {
        let database_connection = Database::open_connection(Database::path()?)?;
        let mut statement = database_connection
            .prepare("select ts, exit, cmd from history order by id desc limit ?1")?;
        let rows = statement.query_map([count], |row| {
            Ok((
                row.get::<_, Option<i64>>(0)?.unwrap_or(0),
                row.get::<_, Option<i32>>(1)?.unwrap_or(0),
                row.get::<_, String>(2)?,
            ))
        })?;

        for row in rows {
            let (ts, exit, cmd) = row?;
            let formatted = Time::format_utc(ts);
            println!("{formatted:>16}  ({exit})  {cmd}");
        }

        Ok(())
    }

    /// Imports commands from a history file into the database
    fn import(history_file: Option<&path::PathBuf>) -> Result<()> {
        let history_file_path = history_file.map_or_else(
            || {
                env::var_os("HISTFILE")
                    .map(path::PathBuf::from)
                    .or_else(|| {
                        env::var_os("HOME")
                            .map(|home| path::PathBuf::from(home).join(".bash_history"))
                    })
                    .ok_or(Error::MissingEnv("HOME"))
            },
            |p| Ok(p.to_owned()),
        )?;

        let history_file = fs::File::open(&history_file_path)?;
        let connection = Database::open_connection(Database::path()?)?;
        let transaction = connection.unchecked_transaction()?;

        let mut statement = transaction.prepare("insert into history(cmd, cwd, exit, ts, session) values(?1, ?2, ?3, ?4, ?5) on conflict(cmd) do update set ts = max(ts, excluded.ts)")?;

        let history_file_reader = io::BufReader::new(history_file);
        let mut timestamp = Time::now_unix();
        let mut last = String::new();
        let mut shell_commands_count: u32 = 0;

        for line in history_file_reader.lines() {
            let line = line?;
            // with HISTTIMEFORMAT set, bash writes a "#<epoch>" line before each cmd.
            if let Some(after) = line.strip_prefix('#')
                && let Ok(history_file_timestamp) = after.parse::<i64>()
            {
                timestamp = history_file_timestamp;
                continue;
            }

            let shell_command = line.trim_end_matches([' ', '\t']);
            if shell_command.trim().is_empty() || shell_command == last {
                continue;
            }

            statement.execute(::rusqlite::params![shell_command, "", 0i32, timestamp, ""])?;
            last = shell_command.to_string();
            shell_commands_count = shell_commands_count.saturating_add(1);
        }

        drop(statement);
        transaction.commit()?;
        println!(
            "Imported {shell_commands_count} commands from '{}'",
            history_file_path.display()
        );

        Ok(())
    }

    /// Runs the interactive fuzzy search and prints the chosen command
    fn search(arguments: &[String]) -> Result<()> {
        let initial = arguments.join(" ");

        let candidates = {
            let connection = Database::open_connection(Database::path()?)?;
            Database::load_candidates(&connection)?
        };

        let selected_command = Tui::run(candidates, initial)?;
        if let Some(selected_command) = selected_command {
            println!("{selected_command}");
        }

        Ok(())
    }
}

/// One history row shown in search results
struct Candidate {
    /// Command text
    cmd: String,
    /// Unix timestamp of last use
    ts: i64,
}

/// History database helpers (path resolution, open, load)
struct Database;

impl Database {
    /// Resolves the default history database path from the environment
    fn path() -> Result<path::PathBuf> {
        env::var_os("STINKPOT_DB_FILE")
            .map(path::PathBuf::from)
            .or_else(|| {
                env::var_os("XDG_DATA_HOME").map(|xdg| {
                    path::PathBuf::from(xdg)
                        .join("stinkpot")
                        .join("history.sqlite")
                })
            })
            .or_else(|| {
                env::var_os("HOME").map(|home| {
                    path::PathBuf::from(home)
                        .join(".local")
                        .join("share")
                        .join("stinkpot")
                        .join("history.sqlite")
                })
            })
            .ok_or(Error::MissingEnv("HOME"))
    }

    /// Opens (and creates) the history database with schema and WAL mode
    fn open_connection(database_path: path::PathBuf) -> Result<rusqlite::Connection> {
        if let Some(parent) = database_path.parent()
            && !parent.exists()
        {
            fs::create_dir_all(parent)?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                let _ = fs::set_permissions(parent, fs::Permissions::from_mode(0o700));
            }
        }

        let connection = rusqlite::Connection::open(database_path)?;
        connection.busy_timeout(time::Duration::from_secs(5))?;

        // Walk rows newest-first and stop at the limit instead of scanning
        // the whole table and sorting it on every invocation
        connection.execute_batch(
            "pragma journal_mode=WAL;
            create table if not exists history (
            id      integer primary key autoincrement,
            cmd     text not null unique,
            cwd     text,
            exit    integer,
            ts      integer,
            session text
            );
            create index if not exists history_ts_cmd on history(ts desc, cmd);",
        )?;

        Ok(connection)
    }

    /// Loads recent history rows for fuzzy search.
    fn load_candidates(connection: &rusqlite::Connection) -> Result<Vec<Candidate>> {
        let mut statement = connection.prepare(
            "select cmd, ts from history
            order by ts desc
            limit 10000",
        )?;

        let rows = statement.query_map([], |row| {
            Ok(Candidate {
                cmd: row.get(0)?,
                ts: row.get::<_, Option<i64>>(1)?.unwrap_or(0),
            })
        })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(Into::into)
    }
}

/// Unix-time helpers for history timestamps.
struct Time;

impl Time {
    /// Current unix time in seconds, or 0 if the system clock is before the epoch
    fn now_unix() -> i64 {
        time::SystemTime::now()
            .duration_since(time::UNIX_EPOCH)
            .map_or(0, |d| d.as_secs().cast_signed())
    }

    /// Formats a unix timestamp as UTC `YYYY-MM-DD HH:MM`
    #[expect(
        clippy::arithmetic_side_effects,
        reason = "civil calendar conversion from non-negative unix time uses bounded integer arithmetic"
    )]
    fn format_utc(ts: i64) -> String {
        if ts < 0 {
            return format!("{ts}");
        }
        let days = ts / 86_400;
        let tod = ts % 86_400;
        let hour = tod / 3600;
        let min = (tod % 3600) / 60;

        // Civil date from days since Unix epoch
        let z = days + 719_468;
        let era = z / 146_097;
        let doe = z - era * 146_097;
        let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
        let y = yoe + era * 400;
        let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
        let mp = (5 * doy + 2) / 153;
        let day = doy - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = if month <= 2 { y + 1 } else { y };

        format!("{year:04}-{month:02}-{day:02} {hour:02}:{min:02}")
    }

    /// Renders `ts` as a compact relative time string (e.g. `"3m"`, `"2d"`)
    fn relative_short(ts: i64) -> String {
        if ts == 0 {
            return "never".into();
        }
        let now = Self::now_unix();
        let secs = now.saturating_sub(ts).max(0);
        match secs {
            0..=1 => "now".into(),
            2..=59 => format!("{secs}s"),
            60..=119 => "1m".into(),
            s if s < 3600 => format!("{}m", s / 60),
            s if s < 7200 => "1hr".into(),
            s if s < 86_400 => format!("{}hrs", s / 3600),
            s if s < 172_800 => "1d".into(),
            s if s < 20 * 86_400 => format!("{}d", s / 86_400),
            s if s < 8 * 7 * 86_400 => format!("{}w", s / (7 * 86_400)),
            s if s < 365 * 86_400 => format!("{}mo", s / (30 * 86_400)),
            s if s < 18 * 30 * 86_400 => "1y".into(),
            s if s < 2 * 365 * 86_400 => "2y".into(),
            s if s < i64::MAX / 2 => format!("{}y", s / (365 * 86_400)),
            _ => "a long while ago".into(),
        }
    }
}

/// Interactive fuzzy-search UI state
struct SearchApp {
    /// Current query text
    query: String,
    /// Cursor column within `query` (char index)
    cursor_col: usize,
    /// Full candidate list from the database
    all: Vec<Candidate>,
    /// Indices into `all` matching the current query
    filtered: Vec<usize>,
    /// List selection state
    list_state: widgets::ListState,
    /// Accepted command, if any
    selected: Option<String>,
    /// Fuzzy matcher instance.
    matcher: skim::SkimMatcherV2,
}

impl SearchApp {
    /// Builds a search app and applies the initial query filter
    fn new(all: Vec<Candidate>, initial: String) -> Self {
        let cursor_col = initial.chars().count();
        let mut app = Self {
            query: initial,
            cursor_col,
            all,
            filtered: Vec::new(),
            list_state: widgets::ListState::default(),
            selected: None,
            matcher: skim::SkimMatcherV2::default().ignore_case(),
        };
        app.filter();
        app
    }

    /// Recomputes `filtered` from `query` and clamps selection
    fn filter(&mut self) {
        let q = self.query.trim();
        if q.is_empty() {
            self.filtered = (0..self.all.len()).collect();
        } else {
            let mut scored: Vec<(i64, usize)> = self
                .all
                .iter()
                .enumerate()
                .filter_map(|(i, c)| self.matcher.fuzzy_match(&c.cmd, q).map(|s| (s, i)))
                .collect();
            scored.sort_by_key(|b| Reverse(b.0));
            self.filtered = scored.into_iter().map(|(_, i)| i).collect();
        }

        let len = self.filtered.len();
        if len == 0 {
            self.list_state.select(None);
        } else {
            let sel = self
                .list_state
                .selected()
                .unwrap_or(0)
                .min(len.saturating_sub(1));
            self.list_state.select(Some(sel));
        }
    }

    /// Moves the selection up one row
    fn move_up(&mut self) {
        if let Some(i) = self.list_state.selected()
            && i > 0
        {
            self.list_state.select(Some(i.saturating_sub(1)));
        }
    }

    /// Moves the selection down one row
    fn move_down(&mut self) {
        if let Some(i) = self.list_state.selected()
            && i.saturating_add(1) < self.filtered.len()
        {
            self.list_state.select(Some(i.saturating_add(1)));
        }
    }

    /// Accepts the currently selected candidate into `selected`
    fn accept(&mut self) {
        if let Some(candidate) = self
            .list_state
            .selected()
            .and_then(|i| self.candidate_at_filtered(i))
        {
            self.selected = Some(candidate.cmd.clone());
        }
    }

    /// Resolves a filtered-list index to its [`Candidate`]
    fn candidate_at_filtered(&self, filtered_index: usize) -> Option<&Candidate> {
        self.filtered
            .get(filtered_index)
            .and_then(|&idx| self.all.get(idx))
    }
}

/// Outcome of handling a key in the search TUI
enum KeyOutcome {
    /// Keep the TUI running
    Continue,
    /// Exit with an optional accepted command
    Done(Option<String>),
}

/// Restores the terminal when dropped (raw mode + alternate screen)
struct AlternateScreenGuard;

impl Drop for AlternateScreenGuard {
    fn drop(&mut self) {
        let _ = terminal::disable_raw_mode();
        let _ = crossterm::execute!(io::stderr(), terminal::LeaveAlternateScreen);
        // flush so the shell prompt isn't tangled with leftover alt-screen state
        let _ = io::stderr().flush();
    }
}

/// Alternate-screen fuzzy-search TUI
struct Tui;

impl Tui {
    /// Draws one search frame (prompt, list window, footer)
    fn draw_search(f: &mut ratatui::Frame, app: &SearchApp) {
        let chunks =
            layout::Layout::vertical([layout::Constraint::Length(1), layout::Constraint::Min(1)])
                .split(f.area());

        let Some(&prompt_area) = chunks.first() else {
            return;
        };
        let Some(&body_area) = chunks.get(1) else {
            return;
        };

        let prompt = widgets::Paragraph::new(text::Line::from(vec![
            text::Span::raw("> "),
            text::Span::raw(&app.query),
        ]));
        f.render_widget(prompt, prompt_area);
        // place cursor after prompt + query
        let cursor_x = u16::try_from(app.cursor_col.saturating_add(2)).unwrap_or(u16::MAX);
        f.set_cursor_position((cursor_x, prompt_area.y));

        let sel = app.list_state.selected().unwrap_or(0);
        let max_rows = env::var("STINKPOT_SEARCH_MAX_ROWS").unwrap_or_else(|_| "12".to_string());
        let max_rows = max_rows.parse::<usize>().unwrap_or(12).max(1);

        let start = sel.saturating_sub(max_rows.saturating_sub(1));
        let end = start.saturating_add(max_rows).min(app.filtered.len());

        let mut ts_width = 0usize;
        let times: Vec<String> = (start..end)
            .map(|i| {
                let t = app
                    .candidate_at_filtered(i)
                    .map_or_else(|| "never".into(), |c| Time::relative_short(c.ts));
                ts_width = ts_width.max(t.chars().count());
                t
            })
            .collect();

        let items: Vec<widgets::ListItem> = (start..end)
            .enumerate()
            .map(|(offset, i)| {
                let line = app
                    .candidate_at_filtered(i)
                    .map_or_else(String::new, |c| c.cmd.replace('\n', "  "));
                let ts = times
                    .get(offset)
                    .map_or_else(String::new, |t| format!("{t:>ts_width$}"));
                let item_style = if Some(i) == app.list_state.selected() {
                    style::Style::default()
                        .fg(style::Color::Blue)
                        .add_modifier(style::Modifier::BOLD)
                } else {
                    style::Style::default()
                };
                widgets::ListItem::new(text::Line::from(vec![
                    text::Span::styled(
                        ts,
                        style::Style::default().add_modifier(style::Modifier::DIM),
                    ),
                    text::Span::raw(" "),
                    text::Span::styled(line, item_style),
                ]))
            })
            .collect();

        // render only the window; list_state index is absolute so remap
        let mut window_state = widgets::ListState::default();
        if let Some(s) = app.list_state.selected()
            && s >= start
            && s < end
        {
            window_state.select(Some(s.saturating_sub(start)));
        }

        let list = widgets::List::new(items);
        let list_area = {
            let sub = layout::Layout::vertical([
                layout::Constraint::Min(1),
                layout::Constraint::Length(1),
            ])
            .split(body_area);
            let Some(&list_area) = sub.first() else {
                return;
            };
            let Some(&footer_area) = sub.get(1) else {
                return;
            };
            let footer = widgets::Paragraph::new(text::Span::styled(
                format!(
                    "  {} matches · ↑/↓ move · enter accept · esc cancel",
                    app.filtered.len()
                ),
                style::Style::default().add_modifier(style::Modifier::DIM),
            ));
            f.render_widget(footer, footer_area);
            list_area
        };
        f.render_stateful_widget(list, list_area, &mut window_state);
    }

    /// Handles one key event
    fn handle_key(app: &mut SearchApp, key: event::KeyEvent) -> KeyOutcome {
        if key.kind != event::KeyEventKind::Press {
            return KeyOutcome::Continue;
        }

        match (key.modifiers, key.code) {
            (event::KeyModifiers::CONTROL, event::KeyCode::Char('c'))
            | (_, event::KeyCode::Esc) => {
                app.selected = None;
                KeyOutcome::Done(None)
            }
            (_, event::KeyCode::Enter | event::KeyCode::Tab) => {
                app.accept();
                KeyOutcome::Done(app.selected.clone())
            }
            (_, event::KeyCode::Up) | (event::KeyModifiers::CONTROL, event::KeyCode::Char('p')) => {
                app.move_up();
                KeyOutcome::Continue
            }
            (_, event::KeyCode::Down)
            | (event::KeyModifiers::CONTROL, event::KeyCode::Char('n')) => {
                app.move_down();
                KeyOutcome::Continue
            }
            (_, event::KeyCode::Backspace) => {
                if app.cursor_col > 0 {
                    let mut chars: Vec<char> = app.query.chars().collect();
                    let remove_at = app.cursor_col.saturating_sub(1);
                    if remove_at < chars.len() {
                        chars.remove(remove_at);
                        app.query = chars.into_iter().collect();
                        app.cursor_col = remove_at;
                        app.filter();
                    }
                }
                KeyOutcome::Continue
            }
            (_, event::KeyCode::Delete) => {
                let mut chars: Vec<char> = app.query.chars().collect();
                if app.cursor_col < chars.len() {
                    chars.remove(app.cursor_col);
                    app.query = chars.into_iter().collect();
                    app.filter();
                }
                KeyOutcome::Continue
            }
            (_, event::KeyCode::Left) => {
                app.cursor_col = app.cursor_col.saturating_sub(1);
                KeyOutcome::Continue
            }
            (_, event::KeyCode::Right) => {
                let len = app.query.chars().count();
                if app.cursor_col < len {
                    app.cursor_col = app.cursor_col.saturating_add(1);
                }
                KeyOutcome::Continue
            }
            (_, event::KeyCode::Home) => {
                app.cursor_col = 0;
                KeyOutcome::Continue
            }
            (_, event::KeyCode::End) => {
                app.cursor_col = app.query.chars().count();
                KeyOutcome::Continue
            }
            (event::KeyModifiers::CONTROL, event::KeyCode::Char('u')) => {
                app.query.clear();
                app.cursor_col = 0;
                app.filter();
                KeyOutcome::Continue
            }
            (m, event::KeyCode::Char(c))
                if m == event::KeyModifiers::NONE || m == event::KeyModifiers::SHIFT =>
            {
                let mut chars: Vec<char> = app.query.chars().collect();
                if app.cursor_col <= chars.len() {
                    chars.insert(app.cursor_col, c);
                    app.query = chars.into_iter().collect();
                    app.cursor_col = app.cursor_col.saturating_add(1);
                    app.filter();
                }
                KeyOutcome::Continue
            }
            _ => KeyOutcome::Continue,
        }
    }

    /// Runs the alternate-screen search TUI; returns the accepted command if any
    fn run(all: Vec<Candidate>, initial: String) -> io::Result<Option<String>> {
        terminal::enable_raw_mode()?;
        let mut stdout = io::stderr();
        crossterm::execute!(stdout, terminal::EnterAlternateScreen)?;
        let _guard = AlternateScreenGuard;

        let backend = backend::CrosstermBackend::new(stdout);
        let mut term = ratatui::Terminal::new(backend)?;

        let mut app = SearchApp::new(all, initial);
        let result = loop {
            term.draw(|f| Self::draw_search(f, &app))?;

            if !event::poll(time::Duration::from_millis(250))? {
                continue;
            }
            let event::Event::Key(key) = event::read()? else {
                continue;
            };
            if let KeyOutcome::Done(selected) = Self::handle_key(&mut app, key) {
                break selected;
            }
        };

        Ok(result)
    }
}

#[cfg(test)]
#[expect(
    clippy::expect_used,
    reason = "tests panic on setup failure via expect"
)]
mod tests {
    use super::*;

    use std::sync::atomic::{AtomicU64, Ordering};

    /// Unique temp directory for one DB-backed test
    fn scratch_dir() -> path::PathBuf {
        static N: AtomicU64 = AtomicU64::new(0);
        let dir = env::temp_dir().join(format!(
            "stinkpot-test-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        let _ = fs::remove_dir_all(&dir);
        fs::create_dir_all(&dir).expect("create scratch dir");
        dir
    }

    #[test]
    fn short_rel_time_buckets() {
        let now = Time::now_unix();
        assert_eq!(Time::relative_short(0), "never");
        assert_eq!(Time::relative_short(now), "now");
        assert_eq!(Time::relative_short(now - 5), "5s");
        assert_eq!(Time::relative_short(now - 90), "1m");
        assert_eq!(Time::relative_short(now - 10 * 60), "10m");
        assert_eq!(Time::relative_short(now - 3 * 3600), "3hrs");
        assert_eq!(Time::relative_short(now - 3 * 86_400), "3d");
        assert_eq!(Time::relative_short(now - 3 * 7 * 86_400), "3w");
        assert_eq!(Time::relative_short(now - 90 * 86_400), "3mo");
        assert_eq!(Time::relative_short(now - 400 * 86_400), "1y");
        assert_eq!(Time::relative_short(now + 60), "now"); // future timestamps clamp to 0s
    }

    #[test]
    fn format_utc_known_epoch() {
        assert_eq!(Time::format_utc(1_700_000_000), "2023-11-14 22:13");
        assert_eq!(Time::format_utc(0), "1970-01-01 00:00");
        assert_eq!(Time::format_utc(-1), "-1");
    }

    #[test]
    fn open_creates_parents_and_is_idempotent() {
        let dir = scratch_dir();
        let path = dir.join("nested/a/history.db");
        Database::open_connection(path.clone()).expect("open nested db");
        Database::open_connection(path.clone()).expect("reopen nested db");
        assert!(path.is_file());
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn open_and_upsert() {
        let dir = scratch_dir();
        let path = dir.join("history.db");
        let conn = Database::open_connection(path).expect("open db");
        conn.execute(
            "insert into history(cmd, cwd, exit, ts, session) values(?1, ?2, ?3, ?4, ?5)
            on conflict(cmd) do update set ts = excluded.ts",
            rusqlite::params!["echo hi", "/tmp", 0i32, 100i64, ""],
        )
        .expect("insert first row");
        conn.execute(
            "insert into history(cmd, cwd, exit, ts, session) values(?1, ?2, ?3, ?4, ?5)
            on conflict(cmd) do update set ts = excluded.ts",
            rusqlite::params!["echo hi", "/tmp", 0i32, 200i64, ""],
        )
        .expect("upsert same cmd");
        let ts: i64 = conn
            .query_row("select ts from history where cmd = ?1", ["echo hi"], |r| {
                r.get(0)
            })
            .expect("read upserted ts");
        assert_eq!(ts, 200);
        let n: i64 = conn
            .query_row("select count(*) from history", [], |r| r.get(0))
            .expect("count history rows");
        assert_eq!(n, 1);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn load_candidates_newest_first() {
        let dir = scratch_dir();
        let path = dir.join("history.db");
        let conn = Database::open_connection(path).expect("open db");
        for (cmd, ts) in [("old", 10i64), ("mid", 20), ("new", 30)] {
            conn.execute(
                "insert into history(cmd, cwd, exit, ts, session) values(?1, '', 0, ?2, '')",
                rusqlite::params![cmd, ts],
            )
            .expect("insert candidate");
        }
        let candidates = Database::load_candidates(&conn).expect("load candidates");
        assert_eq!(
            candidates
                .iter()
                .map(|c| c.cmd.as_str())
                .collect::<Vec<_>>(),
            ["new", "mid", "old"]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    fn sample_candidates() -> Vec<Candidate> {
        vec![
            Candidate {
                cmd: "git status".into(),
                ts: 3,
            },
            Candidate {
                cmd: "cargo test".into(),
                ts: 2,
            },
            Candidate {
                cmd: "git commit".into(),
                ts: 1,
            },
        ]
    }

    #[test]
    fn search_app_empty_query_lists_all() {
        let app = SearchApp::new(sample_candidates(), String::new());
        assert_eq!(app.filtered, vec![0, 1, 2]);
        assert_eq!(app.list_state.selected(), Some(0));
    }

    #[test]
    fn search_app_filters_and_accepts() {
        let mut app = SearchApp::new(sample_candidates(), "git".into());
        assert!(app.filtered.len() >= 2);
        assert!(
            app.filtered
                .iter()
                .all(|&i| app.all.get(i).is_some_and(|c| c.cmd.contains("git")))
        );
        app.accept();
        assert!(app.selected.as_deref().is_some_and(|s| s.contains("git")));
    }

    #[test]
    fn search_app_selection_moves_and_clamps() {
        let mut app = SearchApp::new(sample_candidates(), String::new());
        app.move_up();
        assert_eq!(app.list_state.selected(), Some(0));
        app.move_down();
        app.move_down();
        assert_eq!(app.list_state.selected(), Some(2));
        app.move_down();
        assert_eq!(app.list_state.selected(), Some(2));
        app.query = "zzz-no-match".into();
        app.filter();
        assert!(app.filtered.is_empty());
        assert_eq!(app.list_state.selected(), None);
    }

    #[test]
    fn handle_key_accept_cancel_and_edit() {
        let mut app = SearchApp::new(sample_candidates(), String::new());

        let esc = event::KeyEvent::new(event::KeyCode::Esc, event::KeyModifiers::NONE);
        assert!(matches!(
            Tui::handle_key(&mut app, esc),
            KeyOutcome::Done(None)
        ));

        let mut app = SearchApp::new(sample_candidates(), String::new());
        let enter = event::KeyEvent::new(event::KeyCode::Enter, event::KeyModifiers::NONE);
        assert!(matches!(
            Tui::handle_key(&mut app, enter),
            KeyOutcome::Done(Some(_))
        ));

        let mut app = SearchApp::new(sample_candidates(), String::new());
        let ch = event::KeyEvent::new(event::KeyCode::Char('g'), event::KeyModifiers::NONE);
        assert!(matches!(
            Tui::handle_key(&mut app, ch),
            KeyOutcome::Continue
        ));
        assert_eq!(app.query, "g");
        assert_eq!(app.cursor_col, 1);

        let clear = event::KeyEvent::new(event::KeyCode::Char('u'), event::KeyModifiers::CONTROL);
        assert!(matches!(
            Tui::handle_key(&mut app, clear),
            KeyOutcome::Continue
        ));
        assert!(app.query.is_empty());
        assert_eq!(app.cursor_col, 0);
    }
}
