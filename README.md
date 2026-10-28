# Pump.fun Sniper Bot — Solana Memecoin Sniper (Rust + Jito Shredstream)

**Live Pump.fun sniper** for Solana. Detects new token creates from **Jito Shredstream**, buys in the same slot window, and exits on a delayed sell. Built in **Rust** with **Pump.fun** and **PumpSwap** SDKs — not a Node toy, not a log-subscribe bot.

[![Solana](https://img.shields.io/badge/Solana-3.x-14F195?logo=solana&logoColor=white)](https://solana.com)
[![Rust](https://img.shields.io/badge/Rust-1.89.0-DEA584?logo=rust&logoColor=white)](https://www.rust-lang.org)
[![Pump.fun](https://img.shields.io/badge/Pump.fun-Sniper-00FF88)](https://pump.fun)
[![Jito](https://img.shields.io/badge/Jito-Shredstream-FF6B00)](https://www.jito.wtf)
[![Telegram](https://img.shields.io/badge/Telegram-dexoryn-26A5E4?logo=telegram&logoColor=white)](https://t.me/bondsbtc)

---

## Screenshots

Captured from `cargo run --example demo`, which runs the real dashboard and terminal UI on **simulated** activity (no wallet, RPC or Shredstream needed). Wallet addresses and token mints are random test values.

**Web dashboard** — status, wallet balance chart, live settings, risk limits, positions with *Sell now* / *Retry sell*, and the event feed (including tokens skipped by the risk checks).

![Web dashboard](docs/screenshots/dashboard.png)

**Terminal UI** (`--tui`) — the same live state in the terminal.

![Terminal UI](docs/screenshots/tui.png)

**Login** — the API only answers to the token from your `.env`.

![Login](docs/screenshots/login.png)

## Why this sniper

Most public “Pump.fun snipers” wait on RPC logs or Helius webhooks. That is already late.

This bot ingests **Jito Shredstream entries** over gRPC, deserializes shreds, and matches Pump.fun `create` instruction discriminators before the market is crowded. Buys go out with compute-budget + tip accounts (Astralane / Jito-style bribes). A sell is cached and fired a few slots later.

If you need a **private sniper**, copy-trade, or a custom shredstream cluster (Cherry / Teraswitch / Latitude), message **[t.me/dexoryn](https://t.me/dexoryn)**.

## Features

- **Pump.fun create sniper** — parse mint, creator, amounts, and blockhash from the create tx
- **Jito Shredstream** — `ShredstreamProxy` subscribe, not Yellowstone-only logs
- **Risk limits** — position / spend / drawdown caps, slippage, creator and keyword filters
- **Dashboard** — web panel and terminal UI with live settings
- **Pump.fun SDK** — buy / sell instruction builder + versioned transactions
- **PumpSwap SDK** — AMM pool math, slippage, buy/sell execution
- **Pinned stack** — Rust 1.89.0, Solana SDK 3.x, thin LTO release builds

## Stack

| Layer | Tech |
| --- | --- |
| Language | Rust 1.89.0 (`rustfmt` + `clippy`) |
| Chain | Solana 3.x |
| Ingest | Jito Shredstream gRPC |
| Venues | Pump.fun · PumpSwap (SDK, not used by the sniper loop) |
| Landing | Astralane endpoint + tip |
| Runtime | Tokio, moka cache, rustls |

## How it works

```text
Shredstream proxy  →  deserialize entries  →  scan Pump.fun program
        ↓
   create discriminator match
        ↓
   parse mint / creator / buy amounts / blockhash
        ↓
   PumpFunSDK.buy  (+ compute budget, ATA, tip)
        ↓
   cache FutureSell  →  sell N slots later
```

1. Connect to `SHREDSTREAM_URL` (Cherry, Teraswitch, or Latitude).
2. Decode `solana_entry::Entry` batches from each slot.
3. If an instruction hits the Pump.fun program and the create discriminator, parse the tx.
4. Submit a buy with `SOL_BUY_AMOUNT` and `BRIBE_FEE`.
5. Queue an automatic sell after a short slot delay.

## Requirements

- Rust **1.89.0** (see `rust-toolchain.toml`)
- A funded Solana wallet
- Shredstream endpoint (not public RPC)
- An Astralane endpoint (or compatible) to send transactions through

## Configuration

Copy `.example.env` → `.env` (gitignored — never commit keys). Nothing secret or provider-specific is hardcoded; the bot refuses to start if a required variable is missing or invalid.

```env
PVT_KEY=BASE58_PVT_KEY_HERE
RPC_URL=https://YOUR_RPC_HOST/?api-key=YOUR_KEY
ASTRALANE_URL=http://YOUR_ASTRALANE_HOST/iris?api-key=YOUR_KEY
SHREDSTREAM_URL=http://YOUR_SHREDSTREAM_HOST:9999
SOL_BUY_AMOUNT=0.0001
BRIBE_FEE=0.0001
SELL_DELAY_SLOTS=7
```

| Variable | Required | Description |
| --- | --- | --- |
| `PVT_KEY` | yes | Wallet secret in base58 |
| `RPC_URL` | yes | RPC used for balances and blockhashes |
| `ASTRALANE_URL` | yes | Endpoint transactions are sent through |
| `SHREDSTREAM_URL` | yes | Jito shredstream proxy |
| `SOL_BUY_AMOUNT` | no (0.0001) | SOL size per Pump.fun buy |
| `BRIBE_FEE` | no (0.0001) | Landing tip in SOL |
| `SELL_DELAY_SLOTS` | no (7) | Slots to wait before selling |

### Risk limits

Every new token is checked before a buy; a refused one shows up as `skipped` in the dashboard with the reason. Set `0` to turn a limit off.

| Variable | Default | Meaning |
| --- | --- | --- |
| `MAX_OPEN_POSITIONS` | 3 | Positions held or being sold at once |
| `MAX_SESSION_SPEND_SOL` | 1.0 | Total SOL spent on buys per run |
| `MAX_SESSION_DRAWDOWN_SOL` | 0.5 | Stop buying when the wallet is this far below its starting balance (open positions count as spent until sold) |
| `MIN_BALANCE_RESERVE_SOL` | 0.01 | SOL that must remain after a buy |
| `BUY_SLIPPAGE_PERCENT` | 5 | Extra SOL (in %) the buy may cost if the price moves before it lands; max 50 |
| `MIN_DEV_BUY_SOL` / `MAX_DEV_BUY_SOL` | off | Only tokens whose creator buys within this range at launch |
| `BLOCKED_CREATORS` | none | Comma-separated creator addresses to ignore |
| `BLOCKED_KEYWORDS` | none | Comma-separated words; a token whose name or symbol contains one is ignored |

Sells still go out at any price (`min_sol_output = 0`): in a dumping market getting out matters more than the price, so there is no sell slippage limit. There is no stop-loss or take-profit; positions are sold after `SELL_DELAY_SLOTS`.

The sell reads the real token balance on-chain and uses a fresh blockhash, retrying a few times. If the buy never landed there is nothing to sell and the position is dropped with a log line. If the Shredstream connection drops, the bot reconnects with backoff and keeps its pending sells.

## Build and run

```bash
cargo build --release
cargo run --release
```

Use `--release`. Debug builds are too slow for slot-race sniping.

## Dashboard (web and terminal)

Two front ends share one live state, so they always agree: a **web panel** and a **terminal UI**. The trading loop only publishes events; it never waits for either, so a slow browser cannot delay a buy.

**Web panel**

```bash
cd ui && npm install && npm run build   # once, and after changing the UI
cargo run --release                     # then open http://127.0.0.1:8787
```

Log in with `API_TOKEN` (or the token printed in the log if you left it unset). It shows slot, stream status and wallet balance, a balance chart, positions with *Sell now* / *Retry sell*, and a live event feed. You can pause the bot and change buy amount, tip and sell delay without restarting; changes apply from the next buy and are bounded (buy at most 10 SOL, tip at most 0.1 SOL).

The API listens on `127.0.0.1` only and every call needs the token. Your wallet key never goes through it. To reach it from another machine use an SSH tunnel (`ssh -L 8787:127.0.0.1:8787 host`) instead of binding to a public address.

Customising the look: colours and fonts are tokens at the top of `ui/src/app.css`; layout is in `ui/src/App.svelte`. `npm run dev` serves it with hot reload and proxies to the running bot.

**Terminal UI**

```bash
cargo run --release -- --tui
```

Keys: `↑/↓` select, `s` sell the selected position, `p` pause/resume, `q` quit. Logs go to `sniper.log` while the TUI is open. Build without it using `--no-default-features`.

**Try it without a wallet:** `cargo run --example demo` (add `-- --tui`) runs both interfaces on simulated activity.

## Repository layout

```text
src/main.rs              Pump.fun create sniper entrypoint
src/risk.rs              Pre-buy risk checks and limits
src/state.rs             Shared live state, events and runtime settings
src/trader.rs            Sell execution with retries
src/api.rs               Dashboard API (REST + WebSocket) and static UI
src/tui.rs               Terminal dashboard (ratatui)
ui/                      Web dashboard (Svelte + Tailwind + uPlot)
src/types.rs             Shared buy/sell amounts and tx parsers
src/pumpfun/             Pump.fun constants, ix builder, SDK
src/pumpswap/            PumpSwap pool math, ix builder, SDK
jito_protos/             Jito shredstream protobuf definitions
```

## Contributions

This project started as [dexoryn](https://github.com/dexoryn)'s sniper; the credit for the original bot and its Pump.fun / PumpSwap SDKs is theirs. The changes below were added on top of it (PRs [#1](https://github.com/stalinesatola/pumpfun-sniper-bot/pull/1) and [#2](https://github.com/stalinesatola/pumpfun-sniper-bot/pull/2)), with the help of Claude Code:

- **Security** — removed every hardcoded API key and wallet secret from the code and rewrote the git history to drop them; all endpoints and keys now come from `.env`, validated at startup so a bad value fails with a clear message instead of a panic.
- **Robustness** — Shredstream reconnects with backoff and keeps pending sells; sells use the real on-chain token balance and a fresh blockhash, retry, and a position can never be sold twice; the transaction parser no longer panics on lookup-table or truncated data; the `unwrap()`s on the trading path became proper errors.
- **Dashboard** — a web panel (Svelte 5, Tailwind, uPlot) and a terminal UI (`ratatui`) sharing one live state, with pause, manual sell and runtime settings. The trading loop only publishes events and never waits for the UI. The API is localhost-only and token-protected.
- **Risk limits** — position, spend and drawdown caps, balance reserve, creator and keyword filters, creator launch-buy range, and configurable buy slippage; every refusal is shown with its reason.
- **Dependencies and tooling** — migrated to Solana SDK 3.x on Rust 1.89, with byte-for-byte golden tests of the transactions and the Shredstream entry format captured before the upgrade; known vulnerabilities in the dependency tree went from 41 to 3 (all `zstd`, upstream); unused dependencies removed; `dotenv` replaced by `dotenvy`.
- **Cleanup and CI** — about 6,300 lines of unused code removed (copy-trade listener, Yellowstone gRPC, Whirlpool, Jito searcher client); `cargo fmt`, GitHub Actions (format, build, test, clippy, UI build, informational `cargo audit`) and Dependabot.

**What is and is not verified:** the unit, API, terminal-UI and golden tests pass, and the dashboards were exercised in a real browser and terminal against the demo. The bot itself has **not** been run against live Shredstream, RPC or on-chain orders by these changes — try it with a tiny `SOL_BUY_AMOUNT` first.


## Disclaimer

Trading memecoins can result in total loss of funds. You own your keys, RPC/Shredstream costs, tips, and legal compliance. This repo is provided as-is. No profit is guaranteed.

Keep `.env` local. Rotate any key that has ever been pasted into chat, screenshots, or git history.
