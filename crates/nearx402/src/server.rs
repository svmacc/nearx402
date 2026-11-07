//! One HTTP server for the demo: the x402 vendors, an x402 facilitator under
//! `/facilitator`, the finance dashboard, and an RPC proxy so the dashboard can
//! read a local sandbox without CORS issues.
//!
//! The vendors call the facilitator over HTTP, exactly as they would call a
//! facilitator on another host (see the standalone `facilitator` binary).

use std::net::SocketAddr;
use std::sync::{Arc, Mutex};

use anyhow::Result;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::{header, StatusCode};
use axum::response::{Html, IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde_json::json;
use tokio::net::TcpListener;

use crate::facilitator::{self, ApiKey, Facilitator, FacilitatorClient, FacilitatorConfig};
use crate::vendor::{self, VendorConfig};

const DASHBOARD: &str = include_str!("../../../dashboard/index.html");

#[derive(Clone)]
struct DashState {
    rpc_url: String,
    policy: String,
    network: String,
    http: reqwest::Client,
    transactions: Transactions,
}

/// Transactions a demo has made, for the dashboard to link from its log. The
/// contract's log does not record hashes, so this list only exists per run.
pub type Transactions = Arc<Mutex<Vec<serde_json::Value>>>;

/// Display name of the token. The demo token has no real value.
const TOKEN_SYMBOL: &str = "mUSDC";

pub struct Served {
    pub transactions: Transactions,
    pub addr: SocketAddr,
    /// Base URL of the facilitator the vendors use.
    pub facilitator_url: String,
    /// The vendors' credential for `/verify` and `/settle`, made for this run.
    pub facilitator_api_key: String,
    pub handle: tokio::task::JoinHandle<()>,
}

/// JSON-RPC methods the dashboard needs. Everything else is refused, so the
/// proxy cannot be used to send transactions or patch sandbox state.
const RPC_ALLOWED: &[&str] = &["query"];

pub async fn serve(
    port: u16,
    vendors: VendorConfig,
    mut facilitator: FacilitatorConfig,
    rpc_url: String,
    policy: String,
) -> Result<Served> {
    let listener = TcpListener::bind(("127.0.0.1", port)).await?;
    let addr = listener.local_addr()?;
    let facilitator_url = format!("http://{addr}/facilitator");
    let facilitator_api_key = facilitator::generate_api_key();
    facilitator.api_keys.push(ApiKey {
        key: facilitator_api_key.clone(),
        vendors: vendors
            .vendors
            .iter()
            .map(|v| v.account_id.clone())
            .collect(),
    });

    let transactions = Transactions::default();
    let dash = DashState {
        rpc_url,
        policy,
        network: vendors.network_name.clone(),
        http: reqwest::Client::new(),
        transactions: transactions.clone(),
    };
    let app = Router::new()
        .route("/", get(|| async { Html(DASHBOARD) }))
        .route("/config.json", get(config))
        .route("/transactions.json", get(list_transactions))
        .route("/rpc", post(rpc_proxy))
        .with_state(dash)
        .nest(
            "/facilitator",
            facilitator::router(Arc::new(Facilitator::new(facilitator)?)),
        )
        .merge(vendor::router(
            vendors,
            FacilitatorClient::new(facilitator_url.clone())
                .with_api_key(facilitator_api_key.clone()),
        ));

    let handle = tokio::spawn(async move {
        if let Err(e) = axum::serve(listener, app).await {
            eprintln!("server stopped: {e}");
        }
    });
    Ok(Served {
        transactions,
        addr,
        facilitator_url,
        facilitator_api_key,
        handle,
    })
}

async fn config(State(s): State<DashState>) -> impl IntoResponse {
    Json(json!({
        "rpc": "rpc",
        "policy": s.policy,
        "network": s.network,
        "symbol": TOKEN_SYMBOL,
    }))
}

async fn list_transactions(State(s): State<DashState>) -> impl IntoResponse {
    let list = s.transactions.lock().map(|t| t.clone()).unwrap_or_default();
    Json(list)
}

async fn rpc_proxy(State(s): State<DashState>, body: Bytes) -> Response {
    let method = serde_json::from_slice::<serde_json::Value>(&body)
        .ok()
        .and_then(|v| v.get("method")?.as_str().map(str::to_string));
    match method {
        Some(m) if RPC_ALLOWED.contains(&m.as_str()) => {}
        _ => {
            return (
                StatusCode::FORBIDDEN,
                "only single JSON-RPC `query` calls are proxied",
            )
                .into_response()
        }
    }
    let upstream = s
        .http
        .post(&s.rpc_url)
        .header(header::CONTENT_TYPE, "application/json")
        .body(body)
        .send()
        .await;
    match upstream {
        Ok(r) => {
            let status =
                StatusCode::from_u16(r.status().as_u16()).unwrap_or(StatusCode::BAD_GATEWAY);
            let bytes = r.bytes().await.unwrap_or_default();
            (status, [(header::CONTENT_TYPE, "application/json")], bytes).into_response()
        }
        Err(e) => (StatusCode::BAD_GATEWAY, e.to_string()).into_response(),
    }
}
