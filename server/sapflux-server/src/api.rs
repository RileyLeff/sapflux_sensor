//! Admin HTTP API (axum). No authentication: bind it to localhost or the
//! tailnet address only.
//!
//! | method | path | |
//! |---|---|---|
//! | GET | `/healthz` | liveness |
//! | GET | `/api/devices` | all devices: last check-in, decoded board status, settings |
//! | GET | `/api/devices/{dev}` | one device (name or id) |
//! | POST | `/api/devices/{dev}/enable` | schedule on |
//! | POST | `/api/devices/{dev}/disable` | schedule off (remote off switch) |
//! | PUT | `/api/devices/{dev}/schedule` | JSON patch of settings, validated |
//! | POST | `/api/devices/{dev}/bump` | bump config_version |
//! | POST | `/api/devices/{dev}/reboot` | ask the gateway to reboot |
//! | GET | `/api/devices/{dev}/events?limit=` | events |
//! | GET | `/api/devices/{dev}/config-log?limit=` | settings history |
//! | GET | `/api/windows?board=&limit=` | stored windows |
//! | GET | `/api/windows/partial` | windows being reassembled |
//! | GET | `/api/windows/{board}/{run}` | window metadata |
//! | GET | `/api/windows/{board}/{run}/csv` | decoded CSV |
//! | GET | `/api/windows/{board}/{run}/raw` | raw bytes (WindowInfo frame + data) |

use crate::store::{SettingsPatch, Store};
use axum::extract::{Path, Query, State};
use axum::http::{header, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post, put};
use axum::{Json, Router};
use serde::Deserialize;
use std::sync::{Arc, Mutex};

type Db = Arc<Mutex<Store>>;

struct ApiError(StatusCode, String);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        (self.0, Json(serde_json::json!({ "error": self.1 }))).into_response()
    }
}

impl From<anyhow::Error> for ApiError {
    fn from(e: anyhow::Error) -> Self {
        let msg = format!("{e:#}");
        let code = if msg.starts_with("unknown device") {
            StatusCode::NOT_FOUND
        } else if msg.starts_with("schedule rejected") || msg.contains("must be") {
            StatusCode::UNPROCESSABLE_ENTITY
        } else {
            StatusCode::INTERNAL_SERVER_ERROR
        };
        ApiError(code, msg)
    }
}

type R<T> = Result<T, ApiError>;

#[derive(Deserialize)]
struct Limit {
    limit: Option<u32>,
    board: Option<String>,
}

fn with<T>(db: &Db, f: impl FnOnce(&Store) -> anyhow::Result<T>) -> R<T> {
    let s = db
        .lock()
        .map_err(|_| ApiError(StatusCode::INTERNAL_SERVER_ERROR, "store poisoned".into()))?;
    Ok(f(&s)?)
}

/// Build the router.
pub fn router(db: Db) -> Router {
    Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/api/devices", get(devices))
        .route("/api/devices/{dev}", get(device))
        .route("/api/devices/{dev}/enable", post(enable))
        .route("/api/devices/{dev}/disable", post(disable))
        .route("/api/devices/{dev}/schedule", put(schedule))
        .route("/api/devices/{dev}/bump", post(bump))
        .route("/api/devices/{dev}/reboot", post(reboot))
        .route("/api/devices/{dev}/events", get(events))
        .route("/api/devices/{dev}/config-log", get(config_log))
        .route("/api/windows", get(windows))
        .route("/api/windows/partial", get(partials))
        .route("/api/windows/{board}/{run}", get(window))
        .route("/api/windows/{board}/{run}/csv", get(window_csv))
        .route("/api/windows/{board}/{run}/raw", get(window_raw))
        .with_state(db)
}

async fn devices(State(db): State<Db>) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| s.devices())?))
}

async fn device(State(db): State<Db>, Path(dev): Path<String>) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| s.device(s.resolve(&dev)?))?))
}

async fn enable(State(db): State<Db>, Path(dev): Path<String>) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| s.set_enabled(s.resolve(&dev)?, true, "api"))?))
}

async fn disable(State(db): State<Db>, Path(dev): Path<String>) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| s.set_enabled(s.resolve(&dev)?, false, "api"))?))
}

async fn schedule(
    State(db): State<Db>,
    Path(dev): Path<String>,
    Json(p): Json<SettingsPatch>,
) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| s.update_settings(s.resolve(&dev)?, &p, "api"))?))
}

async fn bump(State(db): State<Db>, Path(dev): Path<String>) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| s.bump_version(s.resolve(&dev)?, "api"))?))
}

async fn reboot(State(db): State<Db>, Path(dev): Path<String>) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| {
        let id = s.resolve(&dev)?;
        s.request_reboot(id, "api")?;
        s.settings(id)
    })?))
}

async fn events(State(db): State<Db>, Path(dev): Path<String>, Query(q): Query<Limit>) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| s.events(Some(s.resolve(&dev)?), q.limit.unwrap_or(100)))?))
}

async fn config_log(
    State(db): State<Db>,
    Path(dev): Path<String>,
    Query(q): Query<Limit>,
) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| s.config_log(s.resolve(&dev)?, q.limit.unwrap_or(50)))?))
}

async fn windows(State(db): State<Db>, Query(q): Query<Limit>) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| s.windows(q.board.as_deref(), q.limit.unwrap_or(100)))?))
}

async fn partials(State(db): State<Db>) -> R<impl IntoResponse> {
    Ok(Json(with(&db, |s| s.partials())?))
}

fn find(db: &Db, board: &str, run: u32) -> R<crate::store::WindowRow> {
    with(db, |s| s.window(board, run, None))?
        .ok_or_else(|| ApiError(StatusCode::NOT_FOUND, format!("no window {board}/{run}")))
}

async fn window(State(db): State<Db>, Path((board, run)): Path<(String, u32)>) -> R<impl IntoResponse> {
    Ok(Json(find(&db, &board, run)?))
}

fn read_file(path: std::path::PathBuf) -> R<Vec<u8>> {
    std::fs::read(&path).map_err(|e| ApiError(StatusCode::NOT_FOUND, format!("{}: {e}", path.display())))
}

async fn window_csv(State(db): State<Db>, Path((board, run)): Path<(String, u32)>) -> R<impl IntoResponse> {
    let w = find(&db, &board, run)?;
    let body = read_file(std::path::PathBuf::from(format!("{}.csv", w.stem)))?;
    Ok((
        [
            (header::CONTENT_TYPE, "text/csv".to_string()),
            (
                header::CONTENT_DISPOSITION,
                format!("attachment; filename=\"{}_run_{}.csv\"", w.board_id, w.run_id),
            ),
        ],
        body,
    ))
}

async fn window_raw(State(db): State<Db>, Path((board, run)): Path<(String, u32)>) -> R<impl IntoResponse> {
    let w = find(&db, &board, run)?;
    let body = read_file(std::path::PathBuf::from(format!("{}.bin", w.stem)))?;
    Ok(([(header::CONTENT_TYPE, "application/octet-stream")], body))
}
