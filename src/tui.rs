use std::io;
use std::sync::Arc;
use std::time::Duration;

use ratatui::crossterm::event::{self, Event as TermEvent, KeyCode, KeyEventKind, KeyModifiers};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Cell, List, ListItem, Paragraph, Row, Table, TableState};
use ratatui::Frame;
use solana_sdk::pubkey::Pubkey;
use std::str::FromStr;

use crate::copy::Side;
use crate::state::{AppState, Event, PositionSource, PositionStatus, Snapshot, Stamped};
use crate::trader::Trader;

/// Redraw (and key poll) period.
const FRAME: Duration = Duration::from_millis(100);

fn short(s: &str) -> String {
    if s.len() > 12 {
        format!("{}…{}", &s[..5], &s[s.len() - 5..])
    } else {
        s.to_string()
    }
}

fn sol(lamports: u64) -> f64 {
    lamports as f64 / 1_000_000_000.0
}

fn clock(ts_ms: u64) -> String {
    let secs = ts_ms / 1000;
    format!(
        "{:02}:{:02}:{:02}",
        (secs / 3600) % 24,
        (secs / 60) % 60,
        secs % 60
    )
}

fn describe(e: &Stamped) -> (Color, String) {
    match &e.event {
        Event::CreateDetected { mint, slot, .. } => (
            Color::Gray,
            format!("create    {} @ slot {slot}", short(mint)),
        ),
        Event::BuySent {
            mint,
            sol,
            sell_at_slot,
            ..
        } => (
            Color::Green,
            format!(
                "buy       {} {sol} SOL, sell at {sell_at_slot}",
                short(mint)
            ),
        ),
        Event::CopySignal {
            leader,
            mint,
            side,
            sol,
            ..
        } => (
            Color::Cyan,
            format!(
                "leader    {} {} {} {sol:.3} SOL",
                short(leader),
                if *side == Side::Buy { "bought" } else { "sold" },
                short(mint)
            ),
        ),
        Event::BuyFailed { mint, error } => {
            (Color::Red, format!("buy FAIL  {} {error}", short(mint)))
        }
        Event::Skipped { mint, reason } => (
            Color::DarkGray,
            format!("skip      {} {reason}", short(mint)),
        ),
        Event::SellStarted { mint } => (Color::Yellow, format!("selling   {}", short(mint))),
        Event::SellSent { mint, amount, .. } => (
            Color::Green,
            format!("sold      {} ({amount} tokens)", short(mint)),
        ),
        Event::SellFailed { mint, error } => {
            (Color::Red, format!("sell FAIL {} {error}", short(mint)))
        }
        Event::Balance { lamports } => (
            Color::DarkGray,
            format!("balance   {:.4} SOL", sol(*lamports)),
        ),
        Event::Stream { connected } => (
            if *connected { Color::Green } else { Color::Red },
            format!(
                "stream    {}",
                if *connected {
                    "connected"
                } else {
                    "disconnected"
                }
            ),
        ),
        Event::Paused { paused } => (
            Color::Yellow,
            format!("bot       {}", if *paused { "PAUSED" } else { "running" }),
        ),
        Event::CopyChanged { copy } => (
            Color::Cyan,
            format!(
                "copy      {} ({} wallet(s))",
                if copy.enabled { "on" } else { "off" },
                copy.wallets.len()
            ),
        ),
        Event::SettingsChanged { settings } => (
            Color::Cyan,
            format!(
                "settings  buy {} SOL, tip {} SOL, delay {} slots",
                settings.buy_amount_sol, settings.bribe_fee_sol, settings.sell_delay_slots
            ),
        ),
    }
}

fn draw(f: &mut Frame, snap: &Snapshot, table: &mut TableState) {
    let [header, body, footer] = Layout::vertical([
        Constraint::Length(6),
        Constraint::Min(6),
        Constraint::Length(1),
    ])
    .areas(f.area());
    let [positions_area, events_area] =
        Layout::vertical([Constraint::Percentage(45), Constraint::Percentage(55)]).areas(body);

    let status = if snap.settings.paused {
        Span::styled(
            "PAUSED",
            Style::new().fg(Color::Yellow).add_modifier(Modifier::BOLD),
        )
    } else {
        Span::styled(
            "RUNNING",
            Style::new().fg(Color::Green).add_modifier(Modifier::BOLD),
        )
    };
    let stream = if snap.stream_connected {
        Span::styled("stream up", Style::new().fg(Color::Green))
    } else {
        Span::styled("stream down", Style::new().fg(Color::Red))
    };
    let balance = snap
        .balance_lamports
        .map_or("…".to_string(), |l| format!("{:.4} SOL", sol(l)));
    f.render_widget(
        Paragraph::new(vec![
            Line::from(vec![
                status,
                Span::raw("  "),
                stream,
                Span::raw(format!("  slot {}", snap.slot)),
            ]),
            Line::from(format!("wallet {}  balance {balance}", short(&snap.wallet))),
            Line::from(format!(
                "buy {} SOL   tip {} SOL   sell after {} slots   mode {}   copy-trade {}",
                snap.settings.buy_amount_sol,
                snap.settings.bribe_fee_sol,
                snap.settings.sell_delay_slots,
                match snap.settings.mode {
                    crate::state::TradingMode::BuyOnly => "buy only",
                    crate::state::TradingMode::BuyAndSell => "buy & sell",
                    crate::state::TradingMode::SellOnly => "sell only",
                },
                match (snap.copy.enabled, snap.copy.wallets.len()) {
                    (false, _) => "off".to_string(),
                    (true, 0) => "on (no wallets)".to_string(),
                    (true, n) => format!("on ({n} wallet{})", if n == 1 { "" } else { "s" }),
                }
            )),
            Line::from(format!(
                "spent {:.4} SOL (cap {})   drawdown {}   slippage {}%",
                snap.session_spent_sol,
                snap.risk.max_session_spend_sol,
                snap.drawdown_sol.map_or("…".to_string(), |d| format!(
                    "{d:.4} SOL (cap {})",
                    snap.risk.max_session_drawdown_sol
                )),
                snap.risk.buy_slippage_percent
            )),
        ])
        .block(Block::bordered().title(" pumpfun sniper ")),
        header,
    );

    let rows = snap.positions.iter().rev().map(|p| {
        let (label, color) = match p.status {
            PositionStatus::Held => ("held", Color::Cyan),
            PositionStatus::Selling => ("selling", Color::Yellow),
            PositionStatus::Sold => ("sold", Color::Green),
            PositionStatus::Failed => ("FAILED", Color::Red),
        };
        Row::new(vec![
            Cell::from(short(&p.mint)),
            Cell::from(match p.source {
                PositionSource::Snipe => "snipe",
                PositionSource::Copy { .. } => "copy",
            }),
            Cell::from(label).style(Style::new().fg(color)),
            Cell::from(format!("{}", p.sol)),
            Cell::from(if p.status == PositionStatus::Held {
                format!("{}", p.sell_at_slot.saturating_sub(snap.slot))
            } else {
                "-".into()
            }),
            Cell::from(p.error.clone().unwrap_or_default()),
        ])
    });
    f.render_stateful_widget(
        Table::new(
            rows,
            [
                Constraint::Length(13),
                Constraint::Length(6),
                Constraint::Length(9),
                Constraint::Length(8),
                Constraint::Length(10),
                Constraint::Min(10),
            ],
        )
        .header(
            Row::new(["mint", "src", "status", "SOL", "slots left", "note"])
                .style(Style::new().add_modifier(Modifier::BOLD)),
        )
        .row_highlight_style(Style::new().add_modifier(Modifier::REVERSED))
        .block(Block::bordered().title(" positions ")),
        positions_area,
        table,
    );

    let height = events_area.height.saturating_sub(2) as usize;
    let items: Vec<ListItem> = snap
        .recent
        .iter()
        .rev()
        .take(height)
        .map(|e| {
            let (color, text) = describe(e);
            ListItem::new(Line::from(vec![
                Span::styled(clock(e.ts_ms), Style::new().fg(Color::DarkGray)),
                Span::raw(" "),
                Span::styled(text, Style::new().fg(color)),
            ]))
        })
        .collect();
    f.render_widget(
        List::new(items).block(Block::bordered().title(" events ")),
        events_area,
    );

    f.render_widget(
        Paragraph::new("↑/↓ select   s sell selected   p pause   m mode   c copy-trade   q quit")
            .style(Style::new().fg(Color::DarkGray)),
        footer,
    );
}

/// Blocking terminal UI. Run it with `spawn_blocking`; it returns when the user quits.
/// It reads the shared state directly, so it adds no load to the trading loop.
pub fn run(state: Arc<AppState>, trader: Trader) -> io::Result<()> {
    let mut terminal = ratatui::init();
    let result = event_loop(&mut terminal, &state, &trader);
    ratatui::restore();
    result
}

fn event_loop(
    terminal: &mut ratatui::DefaultTerminal,
    state: &AppState,
    trader: &Trader,
) -> io::Result<()> {
    let mut table = TableState::default();
    table.select(Some(0));
    let mut notice = String::new();

    loop {
        let snap = state.snapshot();
        terminal.draw(|f| {
            draw(f, &snap, &mut table);
            if !notice.is_empty() {
                let area = f.area();
                let row = ratatui::layout::Rect {
                    x: area.x,
                    y: area.y + area.height.saturating_sub(1),
                    width: area.width,
                    height: 1,
                };
                f.render_widget(
                    Paragraph::new(notice.clone()).style(Style::new().fg(Color::Yellow)),
                    row,
                );
            }
        })?;

        if !event::poll(FRAME)? {
            continue;
        }
        let TermEvent::Key(key) = event::read()? else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
        notice.clear();

        let count = snap.positions.len();
        match key.code {
            KeyCode::Char('q') | KeyCode::Esc => return Ok(()),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => return Ok(()),
            KeyCode::Up | KeyCode::Char('k') => {
                table.select(Some(table.selected().unwrap_or(0).saturating_sub(1)));
            }
            KeyCode::Down | KeyCode::Char('j') => {
                table.select(Some(
                    (table.selected().unwrap_or(0) + 1).min(count.saturating_sub(1)),
                ));
            }
            KeyCode::Char('p') => state.set_paused(!snap.settings.paused),
            KeyCode::Char('m') => {
                use crate::state::TradingMode::*;
                state.set_mode(match snap.settings.mode {
                    BuyAndSell => BuyOnly,
                    BuyOnly => SellOnly,
                    SellOnly => BuyAndSell,
                });
            }
            KeyCode::Char('c') => {
                state.set_copy_enabled(!snap.copy.enabled);
            }
            KeyCode::Char('s') => {
                // The table shows newest first.
                let selected = table
                    .selected()
                    .and_then(|i| snap.positions.iter().rev().nth(i));
                match selected.and_then(|p| Pubkey::from_str(&p.mint).ok()) {
                    Some(mint) => {
                        if let Err(e) = trader.sell_now(&mint) {
                            notice = e;
                        }
                    }
                    None => notice = "no position selected".into(),
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::state::Settings;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn dashboard_renders_positions_and_events() {
        let state = AppState::new(Pubkey::new_unique(), Settings::new(0.001, 0.002, 7));
        let mint = Pubkey::new_unique();
        state.set_slot(100);
        state.record_balance(1_500_000_000);
        state.record_buy_sent(&mint, &Pubkey::new_unique(), "sig", 0.001, 107);

        let mut terminal = Terminal::new(TestBackend::new(100, 24)).unwrap();
        let mut table = TableState::default();
        table.select(Some(0));
        terminal
            .draw(|f| draw(f, &state.snapshot(), &mut table))
            .unwrap();

        let screen: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|c| c.symbol())
            .collect();
        assert!(screen.contains("RUNNING"));
        assert!(screen.contains("1.5000 SOL"));
        assert!(screen.contains(&short(&mint.to_string())));
        assert!(screen.contains("held"));
        assert!(screen.contains("buy"));
        // The risk line sits at the bottom of the header and used to be clipped.
        assert!(screen.contains("spent 0.0010 SOL"));
        assert!(screen.contains("drawdown"));
    }
}
