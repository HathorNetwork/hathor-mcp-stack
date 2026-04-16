use std::{
    collections::HashMap,
    net::{IpAddr, SocketAddr},
    sync::Arc,
    time::{Duration, Instant},
};

use axum::{
    extract::{ConnectInfo, State},
    http::{HeaderMap, StatusCode},
    response::{Html, IntoResponse, Json},
    routing::{get, post},
    Router,
};
use clap::Parser;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::Mutex;
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use tracing::{error, info, warn};

const COOLDOWN_SECS: u64 = 86_400; // 24 hours
const HTML: &str = include_str!("../static/index.html");

#[derive(Parser, Debug, Clone)]
#[command(name = "hathor-faucet")]
struct Args {
    #[arg(long, default_value = "0.0.0.0")]
    host: String,
    #[arg(long, default_value_t = 8200)]
    port: u16,
    /// Base URL of the dedicated wallet-headless container
    #[arg(long, env = "FAUCET_WALLET_URL", default_value = "http://faucet-wallet:8000")]
    wallet_url: String,
    /// Wallet id used with /start + x-wallet-id
    #[arg(long, env = "FAUCET_WALLET_ID", default_value = "faucet")]
    wallet_id: String,
    /// x-api-key for wallet-headless (must match HEADLESS_API_KEY in wallet container)
    #[arg(long, env = "FAUCET_API_KEY", default_value = "")]
    api_key: String,
    /// Seed the faucet wallet with. If provided and wallet isn't started yet, the faucet will start it.
    #[arg(long, env = "FAUCET_SEED", default_value = "")]
    seed: String,
    /// If set, bypass IP cooldown (for local testing)
    #[arg(long, env = "FAUCET_DISABLE_RATE_LIMIT")]
    disable_rate_limit: bool,
}

#[derive(Clone)]
struct AppState {
    args: Arc<Args>,
    http: reqwest::Client,
    last_drip: Arc<Mutex<HashMap<IpAddr, Instant>>>,
}

#[derive(Serialize)]
struct StatusResp {
    address: Option<String>,
    balance_htr: f64,
    drip_htr: f64,
    cooldown_secs: u64,
    tier: &'static str,
    available: bool,
    network: &'static str,
}

#[derive(Deserialize)]
struct DripReq {
    address: String,
}

#[derive(Serialize)]
struct DripResp {
    success: bool,
    tx_hash: Option<String>,
    amount_htr: f64,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    retry_after_secs: Option<u64>,
}

fn drip_tier_for(balance_htr: f64) -> (&'static str, f64) {
    if balance_htr >= 10_000.0 {
        ("generous", 10.0)
    } else if balance_htr >= 1_000.0 {
        ("healthy", 5.0)
    } else if balance_htr >= 100.0 {
        ("modest", 1.0)
    } else if balance_htr >= 10.0 {
        ("trickle", 0.1)
    } else {
        ("empty", 0.0)
    }
}

async fn wallet_get(state: &AppState, path: &str) -> anyhow::Result<Value> {
    let url = format!("{}{}", state.args.wallet_url, path);
    let resp = state
        .http
        .get(&url)
        .header("x-api-key", state.args.api_key.clone())
        .header("x-wallet-id", state.args.wallet_id.clone())
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("{} {}: {}", status, path, text);
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

async fn wallet_post(state: &AppState, path: &str, body: Value) -> anyhow::Result<Value> {
    let url = format!("{}{}", state.args.wallet_url, path);
    let resp = state
        .http
        .post(&url)
        .header("x-api-key", state.args.api_key.clone())
        .header("x-wallet-id", state.args.wallet_id.clone())
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    if !status.is_success() {
        anyhow::bail!("{} {}: {}", status, path, text);
    }
    Ok(serde_json::from_str(&text).unwrap_or(Value::String(text)))
}

/// Boot the dedicated wallet (idempotent — ignores "already started").
async fn ensure_wallet_started(state: &AppState) -> anyhow::Result<()> {
    if state.args.seed.is_empty() {
        info!("FAUCET_SEED not set — assuming wallet is pre-started");
        return Ok(());
    }
    // Check status first
    match wallet_get(state, "/wallet/status").await {
        Ok(v) => {
            if v.get("success").and_then(|b| b.as_bool()).unwrap_or(false) {
                info!("faucet wallet already started");
                return Ok(());
            }
        }
        Err(e) => info!("wallet status pre-check: {e}"),
    }
    // Start (top-level /start)
    let body = json!({
        "wallet-id": state.args.wallet_id,
        "seed": state.args.seed,
    });
    let url = format!("{}/start", state.args.wallet_url);
    let resp = state
        .http
        .post(&url)
        .header("x-api-key", state.args.api_key.clone())
        .json(&body)
        .send()
        .await?;
    let status = resp.status();
    let text = resp.text().await?;
    info!(status = %status, "wallet /start response: {}", text);
    Ok(())
}

async fn faucet_address(state: &AppState) -> anyhow::Result<String> {
    let v = wallet_get(state, "/wallet/address").await?;
    v.get("address")
        .and_then(|s| s.as_str())
        .map(|s| s.to_string())
        .ok_or_else(|| anyhow::anyhow!("no address in response: {v}"))
}

async fn faucet_balance_htr(state: &AppState) -> anyhow::Result<f64> {
    // wallet-headless balance endpoint: /wallet/balance returns { available, locked } in cents for HTR token (00)
    let v = wallet_get(state, "/wallet/balance").await?;
    let available_cents = v
        .get("available")
        .and_then(|n| n.as_f64())
        .ok_or_else(|| anyhow::anyhow!("no available field: {v}"))?;
    Ok(available_cents / 100.0)
}

async fn status_handler(State(state): State<AppState>) -> impl IntoResponse {
    let address = faucet_address(&state).await.ok();
    let balance = faucet_balance_htr(&state).await.unwrap_or(0.0);
    let (tier, drip) = drip_tier_for(balance);
    let resp = StatusResp {
        address,
        balance_htr: balance,
        drip_htr: drip,
        cooldown_secs: COOLDOWN_SECS,
        tier,
        available: drip > 0.0,
        network: "playground testnet",
    };
    Json(resp)
}

fn client_ip(headers: &HeaderMap, conn: SocketAddr) -> IpAddr {
    // Trust reverse proxy headers (Traefik sets x-forwarded-for)
    if let Some(v) = headers.get("x-forwarded-for").and_then(|h| h.to_str().ok()) {
        if let Some(first) = v.split(',').next() {
            if let Ok(ip) = first.trim().parse::<IpAddr>() {
                return ip;
            }
        }
    }
    if let Some(v) = headers.get("x-real-ip").and_then(|h| h.to_str().ok()) {
        if let Ok(ip) = v.trim().parse::<IpAddr>() {
            return ip;
        }
    }
    conn.ip()
}

fn validate_address(addr: &str) -> Result<(), &'static str> {
    let trimmed = addr.trim();
    if trimmed.is_empty() {
        return Err("address is empty");
    }
    if trimmed.len() < 30 || trimmed.len() > 50 {
        return Err("address length looks wrong");
    }
    // Testnet addresses start with 'W'. Mainnet starts with 'H'.
    if !trimmed.starts_with('W') {
        return Err("only testnet addresses (starting with 'W') are accepted");
    }
    if !trimmed.chars().all(|c| c.is_ascii_alphanumeric()) {
        return Err("address contains invalid characters");
    }
    Ok(())
}

async fn drip_handler(
    State(state): State<AppState>,
    ConnectInfo(addr): ConnectInfo<SocketAddr>,
    headers: HeaderMap,
    Json(req): Json<DripReq>,
) -> impl IntoResponse {
    let ip = client_ip(&headers, addr);

    if let Err(e) = validate_address(&req.address) {
        return (
            StatusCode::BAD_REQUEST,
            Json(DripResp {
                success: false,
                tx_hash: None,
                amount_htr: 0.0,
                error: Some(e.into()),
                retry_after_secs: None,
            }),
        );
    }

    // Rate limit
    if !state.args.disable_rate_limit {
        let mut map = state.last_drip.lock().await;
        let now = Instant::now();
        if let Some(last) = map.get(&ip) {
            let elapsed = now.saturating_duration_since(*last);
            if elapsed < Duration::from_secs(COOLDOWN_SECS) {
                let remaining = Duration::from_secs(COOLDOWN_SECS) - elapsed;
                warn!(%ip, remaining = remaining.as_secs(), "rate limited");
                return (
                    StatusCode::TOO_MANY_REQUESTS,
                    Json(DripResp {
                        success: false,
                        tx_hash: None,
                        amount_htr: 0.0,
                        error: Some("you already received a drip recently — come back tomorrow".into()),
                        retry_after_secs: Some(remaining.as_secs()),
                    }),
                );
            }
        }
        // Reserve slot BEFORE sending so concurrent requests can't double-spend
        map.insert(ip, now);
        drop(map);
    }

    // Compute current drip from balance
    let balance = match faucet_balance_htr(&state).await {
        Ok(b) => b,
        Err(e) => {
            error!("balance lookup failed: {e}");
            return (
                StatusCode::SERVICE_UNAVAILABLE,
                Json(DripResp {
                    success: false,
                    tx_hash: None,
                    amount_htr: 0.0,
                    error: Some(format!("faucet wallet unreachable: {e}")),
                    retry_after_secs: None,
                }),
            );
        }
    };
    let (_tier, drip_htr) = drip_tier_for(balance);
    if drip_htr <= 0.0 {
        // refund the slot
        if !state.args.disable_rate_limit {
            state.last_drip.lock().await.remove(&ip);
        }
        return (
            StatusCode::SERVICE_UNAVAILABLE,
            Json(DripResp {
                success: false,
                tx_hash: None,
                amount_htr: 0.0,
                error: Some(format!(
                    "faucet is dry ({:.2} HTR) — please top it up",
                    balance
                )),
                retry_after_secs: None,
            }),
        );
    }

    // Send
    let amount_cents = (drip_htr * 100.0).round() as u64;
    let body = json!({
        "address": req.address,
        "value": amount_cents,
    });
    match wallet_post(&state, "/wallet/simple-send-tx", body).await {
        Ok(v) => {
            let success = v.get("success").and_then(|b| b.as_bool()).unwrap_or(false);
            if !success {
                if !state.args.disable_rate_limit {
                    state.last_drip.lock().await.remove(&ip);
                }
                let err = v
                    .get("error")
                    .or_else(|| v.get("message"))
                    .and_then(|s| s.as_str())
                    .unwrap_or("wallet refused the send")
                    .to_string();
                return (
                    StatusCode::BAD_GATEWAY,
                    Json(DripResp {
                        success: false,
                        tx_hash: None,
                        amount_htr: 0.0,
                        error: Some(err),
                        retry_after_secs: None,
                    }),
                );
            }
            let tx_hash = v
                .get("hash")
                .or_else(|| v.get("txid"))
                .or_else(|| v.get("tx_id"))
                .and_then(|s| s.as_str())
                .map(|s| s.to_string());
            info!(%ip, amount = drip_htr, ?tx_hash, "drip ok");
            (
                StatusCode::OK,
                Json(DripResp {
                    success: true,
                    tx_hash,
                    amount_htr: drip_htr,
                    error: None,
                    retry_after_secs: None,
                }),
            )
        }
        Err(e) => {
            if !state.args.disable_rate_limit {
                state.last_drip.lock().await.remove(&ip);
            }
            error!("send failed: {e}");
            (
                StatusCode::BAD_GATEWAY,
                Json(DripResp {
                    success: false,
                    tx_hash: None,
                    amount_htr: 0.0,
                    error: Some(format!("send failed: {e}")),
                    retry_after_secs: None,
                }),
            )
        }
    }
}

async fn index_handler() -> Html<&'static str> {
    Html(HTML)
}

async fn health_handler() -> &'static str {
    "OK"
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "info,tower_http=info".into()),
        )
        .init();

    let args = Args::parse();
    let http = reqwest::Client::builder()
        .timeout(Duration::from_secs(60))
        .build()?;
    let state = AppState {
        args: Arc::new(args.clone()),
        http,
        last_drip: Arc::new(Mutex::new(HashMap::new())),
    };

    // Fire-and-forget wallet bootstrap. Retries forever with exponential
    // backoff capped at 60s — the faucet is useless until the wallet is
    // loaded, so we never "give up".
    let bootstrap_state = state.clone();
    tokio::spawn(async move {
        let mut delay = Duration::from_secs(2);
        loop {
            match ensure_wallet_started(&bootstrap_state).await {
                Ok(_) => {
                    // Verify by reading an address — /start may succeed but
                    // take a few seconds to index the first address.
                    match faucet_address(&bootstrap_state).await {
                        Ok(addr) => {
                            info!(%addr, "faucet wallet ready");
                            return;
                        }
                        Err(e) => {
                            warn!("wallet started but address not ready yet: {e}");
                        }
                    }
                }
                Err(e) => warn!("wallet bootstrap attempt failed: {e}"),
            }
            tokio::time::sleep(delay).await;
            delay = (delay * 2).min(Duration::from_secs(60));
        }
    });

    let app = Router::new()
        .route("/", get(index_handler))
        .route("/health", get(health_handler))
        .route("/api/status", get(status_handler))
        .route("/api/drip", post(drip_handler))
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr: SocketAddr = format!("{}:{}", args.host, args.port).parse()?;
    info!(%addr, "faucet listening");
    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app.into_make_service_with_connect_info::<SocketAddr>())
        .await?;
    Ok(())
}
