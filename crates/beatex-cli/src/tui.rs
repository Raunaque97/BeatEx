//! Terminal interface for the native BeatEx client.
//!
//! The first iteration intentionally exposes execution work as unavailable rather
//! than pretending an operation succeeded.  The screen is still useful while the
//! transaction client is connected: it confirms the saved browser-wallet identity
//! and provides the final navigation structure.

use std::{
    io::{self, stdout},
    path::{Path, PathBuf},
    time::Duration,
};

use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph, Wrap},
    Frame, Terminal,
};

use crate::{load_profile, WalletProfile};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Operation {
    Deposit,
    PlaceOrReplaceOrder,
    SettleToBalance,
    Rollover,
    Withdraw,
}

impl Operation {
    const ALL: [Self; 5] = [
        Self::Deposit,
        Self::PlaceOrReplaceOrder,
        Self::SettleToBalance,
        Self::Rollover,
        Self::Withdraw,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Deposit => "Deposit",
            Self::PlaceOrReplaceOrder => "Place / replace order",
            Self::SettleToBalance => "Settle to balance",
            Self::Rollover => "Rollover",
            Self::Withdraw => "Withdraw",
        }
    }

    const fn hint(self) -> &'static str {
        match self {
            Self::Deposit => "Move tokens into a private trading balance",
            Self::PlaceOrReplaceOrder => "Create or replace the order for the active auction",
            Self::SettleToBalance => "Apply an auction result to the private balance",
            Self::Rollover => "Settle the previous auction and place the next order",
            Self::Withdraw => "Move funds from the private balance to the wallet",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum Activity {
    Idle,
    Unavailable(Operation),
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum WalletStatus {
    Connected(WalletProfile),
    Missing,
    Unreadable(String),
}

#[derive(Debug, Clone)]
struct App {
    profile_path: PathBuf,
    wallet: WalletStatus,
    selected: usize,
    activity: Activity,
    should_quit: bool,
}

impl App {
    fn load(profile_path: PathBuf) -> Self {
        let wallet = load_wallet(&profile_path);
        Self {
            profile_path,
            wallet,
            selected: 0,
            activity: Activity::Idle,
            should_quit: false,
        }
    }

    fn selected_operation(&self) -> Operation {
        Operation::ALL[self.selected]
    }

    fn select_next(&mut self) {
        self.selected = (self.selected + 1) % Operation::ALL.len();
    }

    fn select_previous(&mut self) {
        self.selected = self
            .selected
            .checked_sub(1)
            .unwrap_or(Operation::ALL.len() - 1);
    }

    fn activate_selected(&mut self) {
        self.activity = Activity::Unavailable(self.selected_operation());
    }

    fn reload_wallet(&mut self) {
        self.wallet = load_wallet(&self.profile_path);
    }

    fn handle_key(&mut self, key: KeyEvent) {
        if key.kind != KeyEventKind::Press {
            return;
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => self.should_quit = true,
            KeyCode::Down | KeyCode::Char('j') => self.select_next(),
            KeyCode::Up | KeyCode::Char('k') => self.select_previous(),
            KeyCode::Enter => self.activate_selected(),
            KeyCode::Char('r') => self.reload_wallet(),
            _ => {}
        }
    }
}

fn load_wallet(path: &Path) -> WalletStatus {
    match load_profile(path) {
        Ok(profile) => WalletStatus::Connected(profile),
        Err(crate::BridgeError::Io(error)) if error.kind() == io::ErrorKind::NotFound => {
            WalletStatus::Missing
        }
        Err(error) => WalletStatus::Unreadable(error.to_string()),
    }
}

/// Opens the terminal interface and restores the caller's terminal on exit.
pub async fn run(profile_path: PathBuf) -> io::Result<()> {
    tokio::task::spawn_blocking(move || run_blocking(profile_path))
        .await
        .map_err(io::Error::other)?
}

fn run_blocking(profile_path: PathBuf) -> io::Result<()> {
    enable_raw_mode()?;
    execute!(stdout(), EnterAlternateScreen)?;
    let _restore_terminal = TerminalRestoreGuard;

    let backend = CrosstermBackend::new(stdout());
    let mut terminal = Terminal::new(backend)?;
    terminal.clear()?;
    let mut app = App::load(profile_path);

    while !app.should_quit {
        terminal.draw(|frame| draw(frame, &app))?;
        if event::poll(EVENT_POLL_INTERVAL)? {
            if let Event::Key(key) = event::read()? {
                app.handle_key(key);
            }
        }
    }

    terminal.show_cursor()?;
    Ok(())
}

struct TerminalRestoreGuard;

impl Drop for TerminalRestoreGuard {
    fn drop(&mut self) {
        let _ = disable_raw_mode();
        let _ = execute!(stdout(), LeaveAlternateScreen);
    }
}

fn draw(frame: &mut Frame<'_>, app: &App) {
    let background = Block::default().style(Style::default().bg(Color::Rgb(8, 12, 20)));
    frame.render_widget(background, frame.area());

    let page = centered(frame.area(), 92, 30);
    frame.render_widget(Clear, page);
    let shell = Block::default()
        .title(Line::from(vec![
            Span::styled(
                " BeatEx ",
                Style::default()
                    .fg(Color::Rgb(109, 224, 190))
                    .add_modifier(Modifier::BOLD),
            ),
            Span::styled("native client ", Style::default().fg(Color::DarkGray)),
        ]))
        .title_alignment(Alignment::Left)
        .borders(Borders::ALL)
        .border_style(Style::default().fg(Color::Rgb(44, 62, 82)))
        .style(Style::default().bg(Color::Rgb(12, 18, 29)));
    frame.render_widget(shell, page);

    let interior = page.inner(ratatui::layout::Margin {
        horizontal: 2,
        vertical: 1,
    });
    let rows = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),
            Constraint::Min(11),
            Constraint::Length(6),
            Constraint::Length(1),
        ])
        .split(interior);

    draw_status(frame, rows[0], app);
    draw_operations(frame, rows[1], app);
    draw_activity(frame, rows[2], app);
    draw_footer(frame, rows[3]);
}

fn draw_status(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(58), Constraint::Percentage(42)])
        .split(area);

    let wallet_lines = match &app.wallet {
        WalletStatus::Connected(profile) => vec![
            Line::from(vec![
                Span::styled("● ", Style::default().fg(Color::Green)),
                Span::styled(
                    format!("{} connected", profile.wallet_name),
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::styled(
                short_address(&profile.address),
                Style::default().fg(Color::Gray),
            ),
        ],
        WalletStatus::Missing => vec![
            Line::from(vec![
                Span::styled("○ ", Style::default().fg(Color::Yellow)),
                Span::styled(
                    "No wallet connected",
                    Style::default().add_modifier(Modifier::BOLD),
                ),
            ]),
            Line::styled(
                "Run `beatex wallet connect`, then press r",
                Style::default().fg(Color::DarkGray),
            ),
        ],
        WalletStatus::Unreadable(error) => vec![
            Line::from(vec![
                Span::styled("! ", Style::default().fg(Color::Red)),
                Span::styled("Wallet profile unreadable", Modifier::BOLD),
            ]),
            Line::styled(error, Style::default().fg(Color::DarkGray)),
        ],
    };
    frame.render_widget(
        Paragraph::new(wallet_lines)
            .block(Block::default().title(" Wallet ").borders(Borders::ALL))
            .wrap(Wrap { trim: true }),
        columns[0],
    );

    frame.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                Span::styled("DEVNET", Style::default().fg(Color::Rgb(109, 224, 190))),
                Span::raw("  Solana"),
            ]),
            Line::styled(
                "RPC execution is not connected yet",
                Style::default().fg(Color::DarkGray),
            ),
        ])
        .block(Block::default().title(" Network ").borders(Borders::ALL))
        .wrap(Wrap { trim: true }),
        columns[1],
    );
}

fn draw_operations(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let items = Operation::ALL.into_iter().map(|operation| {
        ListItem::new(Line::from(vec![
            Span::styled("  ", Style::default()),
            Span::styled(operation.label(), Modifier::BOLD),
            Span::styled("  ·  not wired", Style::default().fg(Color::DarkGray)),
        ]))
    });
    let list = List::new(items)
        .block(
            Block::default()
                .title(" Operations ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(44, 62, 82))),
        )
        .highlight_symbol("› ")
        .highlight_style(
            Style::default()
                .fg(Color::Rgb(109, 224, 190))
                .bg(Color::Rgb(23, 36, 48)),
        );
    let mut state = ListState::default();
    state.select(Some(app.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn draw_activity(frame: &mut Frame<'_>, area: Rect, app: &App) {
    let selected = app.selected_operation();
    let activity = match app.activity {
        Activity::Idle => Line::styled(
            "Idle — no proof generation or transaction in progress",
            Style::default().fg(Color::Gray),
        ),
        Activity::Unavailable(operation) => Line::from(vec![
            Span::styled(operation.label(), Style::default().fg(Color::Yellow)),
            Span::raw(" is not wired yet. No transaction was submitted."),
        ]),
    };

    frame.render_widget(
        Paragraph::new(Text::from(vec![
            activity,
            Line::default(),
            Line::from(vec![
                Span::styled("Selected: ", Style::default().fg(Color::DarkGray)),
                Span::raw(selected.hint()),
            ]),
        ]))
        .block(
            Block::default()
                .title(" Proof & transaction activity ")
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(44, 62, 82))),
        )
        .wrap(Wrap { trim: true }),
        area,
    );
}

fn draw_footer(frame: &mut Frame<'_>, area: Rect) {
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled("↑/↓", Modifier::BOLD),
            Span::raw(" navigate   "),
            Span::styled("enter", Modifier::BOLD),
            Span::raw(" inspect   "),
            Span::styled("r", Modifier::BOLD),
            Span::raw(" reload wallet   "),
            Span::styled("q/esc", Modifier::BOLD),
            Span::raw(" quit"),
        ]))
        .alignment(Alignment::Center)
        .style(Style::default().fg(Color::DarkGray)),
        area,
    );
}

fn centered(area: Rect, max_width: u16, max_height: u16) -> Rect {
    let width = area.width.min(max_width);
    let height = area.height.min(max_height);
    Rect::new(
        area.x + area.width.saturating_sub(width) / 2,
        area.y + area.height.saturating_sub(height) / 2,
        width,
        height,
    )
}

fn short_address(address: &str) -> String {
    if address.len() <= 20 {
        return address.to_owned();
    }
    format!("{}…{}", &address[..8], &address[address.len() - 8..])
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> App {
        App {
            profile_path: PathBuf::from("wallet.json"),
            wallet: WalletStatus::Missing,
            selected: 0,
            activity: Activity::Idle,
            should_quit: false,
        }
    }

    #[test]
    fn navigation_wraps_in_both_directions() {
        let mut state = app();
        state.select_previous();
        assert_eq!(state.selected_operation(), Operation::Withdraw);
        state.select_next();
        assert_eq!(state.selected_operation(), Operation::Deposit);
    }

    #[test]
    fn activating_an_operation_reports_unavailable_without_success() {
        let mut state = app();
        state.select_next();
        state.activate_selected();
        assert_eq!(
            state.activity,
            Activity::Unavailable(Operation::PlaceOrReplaceOrder)
        );
    }

    #[test]
    fn quit_keys_end_the_loop() {
        for code in [KeyCode::Char('q'), KeyCode::Esc] {
            let mut state = app();
            state.handle_key(KeyEvent::new(code, crossterm::event::KeyModifiers::NONE));
            assert!(state.should_quit);
        }
    }

    #[test]
    fn address_abbreviation_keeps_both_ends() {
        assert_eq!(
            short_address("A2gma24FSeKwzBm7MNcPtW4gY28yGDHjc6oXTBkYzsSF"),
            "A2gma24F…TBkYzsSF"
        );
    }
}
