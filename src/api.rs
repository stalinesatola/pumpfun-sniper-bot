use std::env;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::str::FromStr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result};
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Request, State};
use axum::http::{header, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use log::{info, warn};
use rand::Rng;
use serde::Deserialize;
use serde_json::json;
use solana_sdk::pubkey::Pubkey;
use tokio::sync::broadcast::error::RecvError;
use tower_http::services::ServeDir;

use crate::copy::CopyView;
use crate::state::{AppState, SettingsUpdate, SettingsView, Snapshot, Stamped, TradingMode};
use crate::trader::Trader;

const DEFAULT_BIND: &str = "127.0.0.1:8787";
const DEFAULT_UI_DIR: &str = "ui/dist";
/// Events are pushed to browsers in batches this often.
const BATCH_INTERVAL: Duration = Duration::from_millis(50);

pub struct ApiConfig {
    pub bind: SocketAddr,
    pub token: String,
    pub token_generated: bool,
    pub ui_dir: PathBuf,
}

impl ApiConfig {
    /// `API_BIND=off` disables the server. Without `API_TOKEN` a random one is generated
    /// for this run, so the API is never open by accident.
    pub fn from_env() -> Result<Option<Self>> {
        let bind = env::var("API_BIND").unwrap_or_else(|_| DEFAULT_BIND.to_string());
        if bind.trim().eq_ignore_ascii_case("off") {
            return Ok(None);
        }
        let bind = SocketAddr::from_str(bind.trim()).with_context(|| {
            format!("API_BIND must look like 127.0.0.1:8787 or `off`, got `{bind}`")
        })?;

        let (token, token_generated) = match env::var("API_TOKEN") {
            Ok(t) if t.trim().len() >= 16 => (t.trim().to_string(), false),
            Ok(t) if !t.trim().is_empty() => {
                anyhow::bail!("API_TOKEN must be at least 16 characters")
            }
            _ => (random_token(), true),
        };

        Ok(Some(Self {
            bind,
            token,
            token_generated,
            ui_dir: PathBuf::from(
                env::var("UI_DIR").unwrap_or_else(|_| DEFAULT_UI_DIR.to_string()),
            ),
        }))
    }
}

fn random_token() -> String {
    let mut rng = rand::rng();
    (0..32)
        .filter_map(|_| char::from_digit(rng.random_range(0..16u32), 16))
        .collect()
}

#[derive(Clone)]
pub struct ApiState {
    pub app: Arc<AppState>,
    pub trader: Trader,
    token: Arc<String>,
}

/// Compares without short-circuiting on the first differing byte.
fn token_matches(expected: &str, given: &str) -> bool {
    let (a, b) = (expected.as_bytes(), given.as_bytes());
    let mut diff = (a.len() ^ b.len()) as u8;
    for i in 0..a.len().min(b.len()) {
        diff |= a[i] ^ b[i];
    }
    diff == 0
}

fn extract_token(req: &Request) -> Option<String> {
    if let Some(v) = req
        .headers()
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
    {
        if let Some(t) = v.strip_prefix("Bearer ") {
            return Some(t.to_string());
        }
    }
    // Browsers cannot set headers on a WebSocket, so `/ws?token=` is accepted too.
    req.uri()
        .query()?
        .split('&')
        .find_map(|kv| kv.strip_prefix("token="))
        .map(str::to_string)
}

async fn require_token(State(st): State<ApiState>, req: Request, next: Next) -> Response {
    match extract_token(&req) {
        Some(t) if token_matches(&st.token, &t) => next.run(req).await,
        _ => (StatusCode::UNAUTHORIZED, "missing or invalid token").into_response(),
    }
}

pub fn router(app: Arc<AppState>, trader: Trader, token: String, ui_dir: PathBuf) -> Router {
    let state = ApiState {
        app,
        trader,
        token: Arc::new(token),
    };

    let protected = Router::new()
        .route("/ws", get(ws_handler))
        .route("/api/state", get(get_state))
        .route("/api/settings", post(post_settings))
        .route("/api/pause", post(post_pause))
        .route("/api/mode", post(post_mode))
        .route("/api/copy", post(post_copy))
        .route("/api/copy/wallets", post(post_copy_wallet))
        .route("/api/copy/wallets/{address}", delete(delete_copy_wallet))
        .route("/api/sell/{mint}", post(post_sell))
        .route_layer(middleware::from_fn_with_state(state.clone(), require_token));

    // The page itself is static and public; everything that reads or changes the bot needs the token.
    Router::new()
        .merge(protected)
        .fallback_service(ServeDir::new(ui_dir))
        .with_state(state)
}

pub async fn serve(cfg: ApiConfig, app: Arc<AppState>, trader: Trader) -> Result<()> {
    if !cfg.bind.ip().is_loopback() {
        warn!("API listens on {} which is not loopback: anyone who can reach it and has the token controls the bot", cfg.bind);
    }
    if !cfg.ui_dir.join("index.html").exists() {
        warn!(
            "No UI build at {} (run `npm run build` in ui/); the API still works",
            cfg.ui_dir.display()
        );
    }

    let listener = tokio::net::TcpListener::bind(cfg.bind)
        .await
        .with_context(|| format!("could not bind the API to {}", cfg.bind))?;
    info!("Dashboard on http://{}", cfg.bind);
    if cfg.token_generated {
        info!("API_TOKEN not set; token for this run: {}", cfg.token);
    }

    axum::serve(listener, router(app, trader, cfg.token, cfg.ui_dir))
        .await
        .context("API server stopped")
}

async fn get_state(State(st): State<ApiState>) -> Json<Snapshot> {
    Json(st.app.snapshot())
}

async fn post_settings(
    State(st): State<ApiState>,
    Json(update): Json<SettingsUpdate>,
) -> Result<Json<SettingsView>, (StatusCode, String)> {
    st.app
        .update_settings(&update)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

#[derive(Deserialize)]
struct PauseBody {
    paused: bool,
}

async fn post_pause(State(st): State<ApiState>, Json(body): Json<PauseBody>) -> Json<SettingsView> {
    st.app.set_paused(body.paused);
    Json(st.app.settings.view())
}

#[derive(Deserialize)]
struct ModeBody {
    mode: TradingMode,
}

async fn post_mode(State(st): State<ApiState>, Json(body): Json<ModeBody>) -> Json<SettingsView> {
    Json(st.app.set_mode(body.mode))
}

#[derive(Deserialize)]
struct CopyBody {
    enabled: bool,
}

async fn post_copy(State(st): State<ApiState>, Json(body): Json<CopyBody>) -> Json<CopyView> {
    Json(st.app.set_copy_enabled(body.enabled))
}

#[derive(Deserialize)]
struct WalletBody {
    address: String,
}

async fn post_copy_wallet(
    State(st): State<ApiState>,
    Json(body): Json<WalletBody>,
) -> Result<Json<CopyView>, (StatusCode, String)> {
    st.app
        .add_copy_wallet(&body.address)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

async fn delete_copy_wallet(
    State(st): State<ApiState>,
    Path(address): Path<String>,
) -> Result<Json<CopyView>, (StatusCode, String)> {
    st.app
        .remove_copy_wallet(&address)
        .map(Json)
        .map_err(|e| (StatusCode::BAD_REQUEST, e))
}

async fn post_sell(
    State(st): State<ApiState>,
    Path(mint): Path<String>,
) -> Result<StatusCode, (StatusCode, String)> {
    let mint = Pubkey::from_str(&mint)
        .map_err(|_| (StatusCode::BAD_REQUEST, "invalid mint".to_string()))?;
    st.trader
        .sell_now(&mint)
        .map_err(|e| (StatusCode::CONFLICT, e))?;
    Ok(StatusCode::ACCEPTED)
}

async fn ws_handler(ws: WebSocketUpgrade, State(st): State<ApiState>) -> Response {
    ws.on_upgrade(move |socket| ws_loop(socket, st.app))
}

async fn send_json(socket: &mut WebSocket, value: serde_json::Value) -> Result<(), axum::Error> {
    socket.send(Message::Text(value.to_string().into())).await
}

/// Sends a full snapshot, then one compact message every `BATCH_INTERVAL` when something
/// changed. A slow browser can lag the broadcast channel; it then gets a fresh snapshot
/// instead of holding anything up.
async fn ws_loop(mut socket: WebSocket, app: Arc<AppState>) {
    let mut rx = app.subscribe();
    if send_json(
        &mut socket,
        json!({ "type": "snapshot", "data": app.snapshot() }),
    )
    .await
    .is_err()
    {
        return;
    }

    let mut tick = tokio::time::interval(BATCH_INTERVAL);
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut batch: Vec<Stamped> = Vec::new();
    let mut last_slot = app.slot();

    loop {
        tokio::select! {
            received = rx.recv() => match received {
                Ok(event) => batch.push(event),
                Err(RecvError::Lagged(_)) => {
                    batch.clear();
                    if send_json(&mut socket, json!({ "type": "snapshot", "data": app.snapshot() })).await.is_err() {
                        return;
                    }
                }
                Err(RecvError::Closed) => return,
            },
            _ = tick.tick() => {
                let slot = app.slot();
                if batch.is_empty() && slot == last_slot {
                    continue;
                }
                last_slot = slot;
                let msg = json!({ "type": "tick", "slot": slot, "events": std::mem::take(&mut batch) });
                if send_json(&mut socket, msg).await.is_err() {
                    return;
                }
            },
            incoming = socket.recv() => match incoming {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => return,
                Some(Ok(_)) => {}
            },
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::pumpfun::PumpFunSDK;
    use crate::pumpswap::constants::PriorityFeePump;
    use crate::state::Settings;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use solana_sdk::signature::Keypair;
    use solana_sdk::signer::Signer;
    use tower::ServiceExt;

    async fn app() -> (Router, Arc<AppState>) {
        let payer = Keypair::new();
        let state = Arc::new(AppState::new(
            payer.pubkey(),
            Settings::new(0.001, 0.001, 7),
        ));
        let sdk = PumpFunSDK::new(
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
            PriorityFeePump::default(),
        )
        .await;
        let trader = Trader::new(sdk, payer, state.clone());
        (
            router(
                state.clone(),
                trader,
                "t".repeat(16),
                PathBuf::from("/nonexistent"),
            ),
            state,
        )
    }

    fn req(method: &str, uri: &str, token: Option<&str>, body: Body) -> HttpRequest<Body> {
        let mut b = HttpRequest::builder()
            .method(method)
            .uri(uri)
            .header("content-type", "application/json");
        if let Some(t) = token {
            b = b.header("authorization", format!("Bearer {t}"));
        }
        b.body(body).unwrap()
    }

    #[test]
    fn token_comparison() {
        assert!(token_matches("abcdef", "abcdef"));
        assert!(!token_matches("abcdef", "abcdeg"));
        assert!(!token_matches("abcdef", "abcde"));
        assert!(!token_matches("abcdef", ""));
    }

    #[tokio::test]
    async fn api_requires_the_token() {
        let (app, _) = app().await;
        let token = "t".repeat(16);

        let res = app
            .clone()
            .oneshot(req("GET", "/api/state", None, Body::empty()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = app
            .clone()
            .oneshot(req("GET", "/api/state", Some("wrong"), Body::empty()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = app
            .clone()
            .oneshot(req("GET", "/api/state", Some(&token), Body::empty()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        // WebSocket-style query token is accepted too (and still rejected when wrong).
        let res = app
            .clone()
            .oneshot(req(
                "GET",
                &format!("/api/state?token={token}"),
                None,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        let res = app
            .oneshot(req("GET", "/api/state?token=nope", None, Body::empty()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
    }

    #[tokio::test]
    async fn settings_and_pause_change_the_live_state() {
        let (app, state) = app().await;
        let token = "t".repeat(16);

        let ok = Body::from(r#"{"buy_amount_sol":0.25,"sell_delay_slots":12}"#);
        let res = app
            .clone()
            .oneshot(req("POST", "/api/settings", Some(&token), ok))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert_eq!(state.settings.buy_amount_sol(), 0.25);
        assert_eq!(state.settings.sell_delay_slots(), 12);

        let bad = Body::from(r#"{"buy_amount_sol":9999}"#);
        let res = app
            .clone()
            .oneshot(req("POST", "/api/settings", Some(&token), bad))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(state.settings.buy_amount_sol(), 0.25);

        let res = app
            .oneshot(req(
                "POST",
                "/api/pause",
                Some(&token),
                Body::from(r#"{"paused":true}"#),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(state.settings.paused());
    }

    #[tokio::test]
    async fn manual_sell_validates_the_mint_and_the_position() {
        let (app, _) = app().await;
        let token = "t".repeat(16);

        let res = app
            .clone()
            .oneshot(req(
                "POST",
                "/api/sell/not-a-key",
                Some(&token),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let unknown = Pubkey::new_unique();
        let res = app
            .oneshot(req(
                "POST",
                &format!("/api/sell/{unknown}"),
                Some(&token),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::CONFLICT);
    }

    #[tokio::test]
    async fn copy_trade_can_be_switched_and_its_wallets_edited() {
        let (app, state) = app().await;
        let token = "t".repeat(16);
        let wallet = Pubkey::new_unique().to_string();

        // Everything needs the token.
        let res = app
            .clone()
            .oneshot(req(
                "POST",
                "/api/copy",
                None,
                Body::from(r#"{"enabled":false}"#),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);
        let res = app
            .clone()
            .oneshot(req(
                "DELETE",
                &format!("/api/copy/wallets/{wallet}"),
                None,
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        // Add, with a bad address rejected and the bot's own wallet refused.
        let add = |body: String| req("POST", "/api/copy/wallets", Some(&token), Body::from(body));
        let res = app
            .clone()
            .oneshot(add(format!(r#"{{"address":"{wallet}"}}"#)))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(state.follow.active());
        assert_eq!(state.follow.list(), vec![wallet.clone()]);

        let res = app
            .clone()
            .oneshot(add(r#"{"address":"not-an-address"}"#.into()))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        let res = app
            .clone()
            .oneshot(add(format!(r#"{{"address":"{}"}}"#, state.wallet())))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert_eq!(state.follow.list().len(), 1);

        // Switch off and on.
        let res = app
            .clone()
            .oneshot(req(
                "POST",
                "/api/copy",
                Some(&token),
                Body::from(r#"{"enabled":false}"#),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(!state.follow.active() && !state.follow.enabled());
        let res = app
            .clone()
            .oneshot(req(
                "POST",
                "/api/copy",
                Some(&token),
                Body::from(r#"{"enabled":true}"#),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(state.follow.active());

        // Remove, and the snapshot reflects it.
        let res = app
            .clone()
            .oneshot(req(
                "DELETE",
                &format!("/api/copy/wallets/{wallet}"),
                Some(&token),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::OK);
        assert!(state.follow.list().is_empty());
        let res = app
            .oneshot(req(
                "DELETE",
                "/api/copy/wallets/garbage",
                Some(&token),
                Body::empty(),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::BAD_REQUEST);
        assert!(!state.snapshot().copy.active);
    }

    #[tokio::test]
    async fn the_trading_mode_can_be_changed_through_the_api() {
        let (app, state) = app().await;
        let token = "t".repeat(16);

        let res = app
            .clone()
            .oneshot(req(
                "POST",
                "/api/mode",
                None,
                Body::from(r#"{"mode":"sell_only"}"#),
            ))
            .await
            .unwrap();
        assert_eq!(res.status(), StatusCode::UNAUTHORIZED);

        for (body, expected) in [
            (
                r#"{"mode":"sell_only"}"#,
                crate::state::TradingMode::SellOnly,
            ),
            (r#"{"mode":"buy_only"}"#, crate::state::TradingMode::BuyOnly),
            (
                r#"{"mode":"buy_and_sell"}"#,
                crate::state::TradingMode::BuyAndSell,
            ),
        ] {
            let res = app
                .clone()
                .oneshot(req("POST", "/api/mode", Some(&token), Body::from(body)))
                .await
                .unwrap();
            assert_eq!(res.status(), StatusCode::OK);
            assert_eq!(state.settings.mode(), expected);
        }

        for bad in [r#"{"mode":"both"}"#, r#"{}"#, "nonsense"] {
            let res = app
                .clone()
                .oneshot(req("POST", "/api/mode", Some(&token), Body::from(bad)))
                .await
                .unwrap();
            assert!(res.status().is_client_error(), "{bad} must be rejected");
            assert_eq!(state.settings.mode(), crate::state::TradingMode::BuyAndSell);
        }
    }
}
