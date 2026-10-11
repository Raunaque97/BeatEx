//! Terminal interface for the native BeatEx client.
//!
//! The first iteration intentionally exposes execution work as unavailable rather
//! than pretending an operation succeeded.  The screen is still useful while the
//! transaction client is connected: it confirms the saved browser-wallet identity
//! and provides the final navigation structure.

use std::{
    collections::VecDeque,
    env,
    io::{self, stdout},
    path::{Path, PathBuf},
    process::Command,
    sync::mpsc::{self, Receiver, Sender},
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anchor_lang::AccountDeserialize;
use base64::{engine::general_purpose::STANDARD as BASE64, Engine as _};
use crossterm::{
    event::{self, Event, KeyCode, KeyEvent, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use pyth_solana_receiver_sdk::price_update::PriceUpdateV2;
use ratatui::{
    backend::CrosstermBackend,
    layout::{Alignment, Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span, Text},
    widgets::{
        Axis, Block, Borders, Chart, Clear, Dataset, GraphType, List, ListItem, ListState,
        Paragraph, Wrap,
    },
    Frame, Terminal,
};

use crate::{load_profile, WalletProfile};

const EVENT_POLL_INTERVAL: Duration = Duration::from_millis(200);
const PRICE_REFRESH_INTERVAL: Duration = Duration::from_secs(2);
const PRICE_HISTORY_LIMIT: usize = 60;
const DEFAULT_RPC_URL: &str = "https://api.devnet.solana.com";
const DEFAULT_PYTH_PRICE_ACCOUNT: &str = "4cSM2e6rvbGQUFiJbqytoVMi5GgghSMr8LwVrT9VPSPo";
const DEFAULT_PYTH_FEED_ID: &str =
    "e62df6c8b4a85fe1a67db44dc12de5db330f7ac66b72dc658afedf0f4a415b43";

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

#[derive(Debug, Clone)]
struct PriceSnapshot {
    price: i64,
    exponent: i32,
    fetched_at: Instant,
}

#[derive(Debug)]
enum PriceUpdate {
    Success(PriceSnapshot),
    Failure(String),
}

/// Source boundary for the price shown in the TUI.
///
/// The first implementation reads Pyth. An auction-backed implementation can
/// replace it later without changing the app state or rendering code.
trait PriceProvider: Send {
    fn fetch(&self) -> Result<PriceSnapshot, String>;
}

#[derive(Debug, Clone)]
struct PythPriceProvider {
    rpc_urls: Vec<String>,
    price_account: String,
    feed_id: [u8; 32],
}

impl PythPriceProvider {
    fn from_env() -> Result<Self, String> {
        let primary = env::var("BEATEX_RPC_URL").unwrap_or_else(|_| DEFAULT_RPC_URL.to_owned());
        let mut rpc_urls = vec![primary];
        if let Ok(fallbacks) = env::var("BEATEX_RPC_FALLBACK_URLS") {
            for fallback in fallbacks.split(',') {
                let fallback = fallback.trim();
                if !fallback.is_empty() && !rpc_urls.iter().any(|url| url == fallback) {
                    rpc_urls.push(fallback.to_owned());
                }
            }
        }

        let price_account = env::var("BEATEX_PYTH_PRICE_ACCOUNT")
            .unwrap_or_else(|_| DEFAULT_PYTH_PRICE_ACCOUNT.to_owned());
        let feed_id =
            env::var("BEATEX_PYTH_FEED_ID").unwrap_or_else(|_| DEFAULT_PYTH_FEED_ID.to_owned());

        Ok(Self {
            rpc_urls,
            price_account,
            feed_id: parse_feed_id(&feed_id)?,
        })
    }

    fn fetch_account_bytes(&self) -> Result<Vec<u8>, String> {
        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "getAccountInfo",
            "params": [self.price_account.as_str(), {"encoding": "base64", "commitment": "confirmed"}],
        })
        .to_string();
        let mut failures = Vec::new();

        for rpc_url in &self.rpc_urls {
            let output = match Command::new("curl")
                .args([
                    "--fail",
                    "--silent",
                    "--show-error",
                    "--max-time",
                    "10",
                    "--header",
                    "Content-Type: application/json",
                    "--data",
                    &request,
                    rpc_url,
                ])
                .output()
            {
                Ok(output) => output,
                Err(error) => {
                    failures.push(format!("{rpc_url}: could not run curl: {error}"));
                    continue;
                }
            };

            if !output.status.success() {
                failures.push(format!(
                    "{rpc_url}: {}",
                    String::from_utf8_lossy(&output.stderr).trim()
                ));
                continue;
            }

            let response: serde_json::Value = match serde_json::from_slice(&output.stdout) {
                Ok(response) => response,
                Err(error) => {
                    failures.push(format!("{rpc_url}: invalid JSON-RPC response: {error}"));
                    continue;
                }
            };
            if let Some(error) = response.get("error") {
                failures.push(format!("{rpc_url}: JSON-RPC error {error}"));
                continue;
            }

            let Some(data) = response
                .pointer("/result/value/data")
                .and_then(serde_json::Value::as_array)
            else {
                failures.push(format!("{rpc_url}: Pyth price account was not found"));
                continue;
            };
            if data.len() != 2 || data[1].as_str() != Some("base64") {
                failures.push(format!("{rpc_url}: account was not returned as base64"));
                continue;
            }
            let encoded = data[0]
                .as_str()
                .ok_or_else(|| format!("{rpc_url}: account data was not a string"))?;
            return BASE64
                .decode(encoded)
                .map_err(|error| format!("{rpc_url}: invalid account base64: {error}"));
        }

        Err(format!(
            "all configured RPC endpoints failed: {}",
            failures.join("; ")
        ))
    }
}

impl PriceProvider for PythPriceProvider {
    fn fetch(&self) -> Result<PriceSnapshot, String> {
        let mut bytes = self.fetch_account_bytes()?;
        let update = PriceUpdateV2::try_deserialize(&mut bytes.as_slice())
            .map_err(|error| format!("could not decode Pyth account: {error}"))?;
        let price = update.price_message;
        if price.feed_id != self.feed_id {
            return Err("Pyth account contains an unexpected feed id".to_owned());
        }
        if price.price <= 0 {
            return Err("Pyth returned a non-positive price".to_owned());
        }

        Ok(PriceSnapshot {
            price: price.price,
            exponent: price.exponent,
            fetched_at: Instant::now(),
        })
    }
}

struct PricePoller {
    stop: Sender<()>,
    handle: Option<JoinHandle<()>>,
}

impl PricePoller {
    fn start(provider: Box<dyn PriceProvider>) -> (Self, Receiver<PriceUpdate>) {
        let (updates_tx, updates_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let handle = thread::spawn(move || loop {
            let update = match provider.fetch() {
                Ok(snapshot) => PriceUpdate::Success(snapshot),
                Err(error) => PriceUpdate::Failure(error),
            };
            if updates_tx.send(update).is_err() {
                break;
            }
            match stop_rx.recv_timeout(PRICE_REFRESH_INTERVAL) {
                Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        });

        (
            Self {
                stop: stop_tx,
                handle: Some(handle),
            },
            updates_rx,
        )
    }
}

impl Drop for PricePoller {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
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
    price: Option<PriceSnapshot>,
    price_error: Option<String>,
    price_history: VecDeque<f64>,
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
            price: None,
            price_error: None,
            price_history: VecDeque::with_capacity(PRICE_HISTORY_LIMIT),
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

    fn apply_price_updates(&mut self, updates: &Receiver<PriceUpdate>) {
        while let Ok(update) = updates.try_recv() {
            match update {
                PriceUpdate::Success(snapshot) => {
                    let value = numeric_price(snapshot.price, snapshot.exponent);
                    self.price = Some(snapshot);
                    self.price_error = None;
                    if self.price_history.len() == PRICE_HISTORY_LIMIT {
                        self.price_history.pop_front();
                    }
                    self.price_history.push_back(value);
                }
                PriceUpdate::Failure(error) => self.price_error = Some(error),
            }
        }
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
    let provider = PythPriceProvider::from_env().map_err(io::Error::other)?;
    let (_price_poller, price_updates) = PricePoller::start(Box::new(provider));
    let mut app = App::load(profile_path);

    while !app.should_quit {
        app.apply_price_updates(&price_updates);
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
            Constraint::Min(14),
            Constraint::Length(6),
            Constraint::Length(1),
        ])
        .split(interior);

    draw_status(frame, rows[0], app);
    let main_columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(38), Constraint::Percentage(62)])
        .split(rows[1]);
    draw_operations(frame, main_columns[0], app);
    draw_price_chart(frame, main_columns[1], app);
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

    let network_lines = match (&app.price, &app.price_error) {
        (Some(snapshot), None) => vec![
            Line::from(vec![
                Span::styled("DEVNET", Style::default().fg(Color::Rgb(109, 224, 190))),
                Span::raw("  Solana · Pyth BTC/USD"),
            ]),
            Line::from(vec![Span::styled(
                format!(
                    "{} tUSD / tBTC",
                    format_price(snapshot.price, snapshot.exponent)
                ),
                Style::default()
                    .fg(Color::Rgb(109, 224, 190))
                    .add_modifier(Modifier::BOLD),
            )]),
            Line::styled(
                format!("live · updated {}", format_age(snapshot.fetched_at)),
                Style::default().fg(Color::DarkGray),
            ),
        ],
        (Some(snapshot), Some(error)) => vec![
            Line::from(vec![
                Span::styled("DEVNET", Style::default().fg(Color::Yellow)),
                Span::raw("  Solana · Pyth BTC/USD · stale"),
            ]),
            Line::styled(
                format!(
                    "{} tUSD / tBTC",
                    format_price(snapshot.price, snapshot.exponent)
                ),
                Style::default().fg(Color::Yellow),
            ),
            Line::styled(error, Style::default().fg(Color::DarkGray)),
        ],
        (None, Some(error)) => vec![
            Line::from(vec![
                Span::styled("DEVNET", Style::default().fg(Color::Red)),
                Span::raw("  Solana · Pyth unavailable"),
            ]),
            Line::styled(
                "Waiting for a live price",
                Style::default().fg(Color::Yellow),
            ),
            Line::styled(error, Style::default().fg(Color::DarkGray)),
        ],
        (None, None) => vec![
            Line::from(vec![
                Span::styled("DEVNET", Style::default().fg(Color::Rgb(109, 224, 190))),
                Span::raw("  Solana · Pyth BTC/USD"),
            ]),
            Line::styled("Loading live price…", Style::default().fg(Color::Yellow)),
            Line::styled(
                "Polling the configured Pyth account",
                Style::default().fg(Color::DarkGray),
            ),
        ],
    };
    frame.render_widget(
        Paragraph::new(network_lines)
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

fn draw_price_chart(frame: &mut Frame<'_>, area: Rect, app: &App) {
    if app.price_history.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::styled(
                    "Waiting for price samples…",
                    Style::default().fg(Color::Yellow),
                ),
                Line::default(),
                Line::styled(
                    "The chart will fill as Pyth updates arrive.",
                    Style::default().fg(Color::DarkGray),
                ),
            ])
            .block(
                Block::default()
                    .title(" Price chart · Pyth BTC/USD ")
                    .borders(Borders::ALL)
                    .border_style(Style::default().fg(Color::Rgb(44, 62, 82))),
            )
            .wrap(Wrap { trim: true }),
            area,
        );
        return;
    }

    let data: Vec<(f64, f64)> = app
        .price_history
        .iter()
        .enumerate()
        .map(|(index, price)| (index as f64, *price))
        .collect();
    let min_price = app
        .price_history
        .iter()
        .copied()
        .fold(f64::INFINITY, f64::min);
    let max_price = app
        .price_history
        .iter()
        .copied()
        .fold(f64::NEG_INFINITY, f64::max);
    let padding = ((max_price - min_price) * 0.15).max(1.0);
    let y_min = min_price - padding;
    let y_max = max_price + padding;
    let x_max = (data.len().saturating_sub(1).max(1)) as f64;
    let current = app.price.as_ref().map(|snapshot| {
        format!(
            "{} tUSD/tBTC",
            format_price(snapshot.price, snapshot.exponent)
        )
    });
    let title = current
        .map(|current| format!(" Price chart · {current} "))
        .unwrap_or_else(|| " Price chart · Pyth BTC/USD ".to_owned());
    let y_labels = vec![format_chart_price(y_min), format_chart_price(y_max)];
    let datasets = vec![Dataset::default()
        .name("Pyth")
        .marker(ratatui::symbols::Marker::Braille)
        .graph_type(GraphType::Line)
        .style(Style::default().fg(Color::Rgb(109, 224, 190)))
        .data(&data)];
    let chart = Chart::new(datasets)
        .block(
            Block::default()
                .title(title)
                .borders(Borders::ALL)
                .border_style(Style::default().fg(Color::Rgb(44, 62, 82))),
        )
        .x_axis(
            Axis::default()
                .title("samples")
                .bounds([0.0, x_max])
                .labels(vec!["oldest", "now"]),
        )
        .y_axis(
            Axis::default()
                .title("tUSD")
                .bounds([y_min, y_max])
                .labels(y_labels),
        );
    frame.render_widget(chart, area);
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

fn parse_feed_id(value: &str) -> Result<[u8; 32], String> {
    let value = value.strip_prefix("0x").unwrap_or(value);
    if value.len() != 64 {
        return Err("BEATEX_PYTH_FEED_ID must contain 32 bytes of hex".to_owned());
    }
    let mut feed_id = [0u8; 32];
    for (index, byte) in feed_id.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&value[index * 2..index * 2 + 2], 16)
            .map_err(|_| "BEATEX_PYTH_FEED_ID contains invalid hex".to_owned())?;
    }
    Ok(feed_id)
}

fn format_price(price: i64, exponent: i32) -> String {
    format_chart_price(numeric_price(price, exponent))
}

fn numeric_price(price: i64, exponent: i32) -> f64 {
    (price as f64) * 10f64.powi(exponent)
}

fn format_chart_price(price: f64) -> String {
    let rendered = format!("{price:.2}");
    let (sign, unsigned) = rendered
        .strip_prefix('-')
        .map_or(("", rendered.as_str()), |value| ("-", value));
    let (whole, fraction) = unsigned.split_once('.').unwrap_or((unsigned, "00"));
    let mut grouped = String::new();
    for (index, character) in whole.chars().enumerate() {
        if index > 0 && (whole.len() - index) % 3 == 0 {
            grouped.push(',');
        }
        grouped.push(character);
    }
    format!("{sign}{grouped}.{fraction}")
}

fn format_age(fetched_at: Instant) -> String {
    let seconds = fetched_at.elapsed().as_secs();
    match seconds {
        0 => "now".to_owned(),
        1 => "1s ago".to_owned(),
        seconds => format!("{seconds}s ago"),
    }
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
            price: None,
            price_error: None,
            price_history: VecDeque::new(),
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

    #[test]
    fn feed_id_parser_accepts_hex_with_or_without_prefix() {
        let plain = parse_feed_id(DEFAULT_PYTH_FEED_ID).unwrap();
        let prefixed = parse_feed_id(&format!("0x{DEFAULT_PYTH_FEED_ID}")).unwrap();
        assert_eq!(plain, prefixed);
        assert_eq!(plain[0], 0xe6);
    }

    #[test]
    fn price_formatter_applies_pyth_exponent_and_grouping() {
        assert_eq!(format_price(8_550_000_000_000, -8), "85,500.00");
    }
}
