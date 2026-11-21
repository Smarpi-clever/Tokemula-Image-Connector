//! Short-lived, user-requested loopback configuration. No secret leaves the settings service.

use crate::image_parameters::ImagePreferences;
use crate::settings::{
    CredentialSource, ModelPreferences, SettingsError, SettingsService, ValidatedConfiguration,
    ValidationSummary,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::State,
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    response::{IntoResponse, Response},
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::{
    net::TcpListener,
    sync::{Mutex, Notify},
    time::Instant,
};

const SESSION_TTL: Duration = Duration::from_secs(5 * 60);
const TERMINAL_GRACE: Duration = Duration::from_secs(2);
const MAX_BODY: usize = 16 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionStatus {
    Active,
    Saved,
    Cancelled,
    Expired,
}

#[derive(Debug, Clone, Serialize)]
pub struct SessionInfo {
    pub url: String,
    pub session_id: String,
    pub expires_at: u64,
    pub status: SessionStatus,
}

#[derive(Clone)]
pub struct SessionManager {
    service: SettingsService,
    session: Arc<Mutex<Option<Arc<Session>>>>,
    ttl: Duration,
}

struct Session {
    service: SettingsService,
    token: String,
    origin: String,
    expires_at: u64,
    deadline: Instant,
    state: Mutex<SessionState>,
    terminal: Notify,
    stopped: Notify,
    is_stopped: AtomicBool,
}

struct SessionState {
    status: SessionStatus,
    generation: u64,
    pending: Option<ValidatedConfiguration>,
    validation: Option<ValidationSummary>,
    validation_error: Option<SettingsError>,
    preferences: Option<ModelPreferences>,
    image_preferences: Option<ImagePreferences>,
    active_source: Option<CredentialSource>,
}

fn error(code: &'static str, message: &'static str) -> SettingsError {
    SettingsError { code, message }
}
fn unavailable() -> SettingsError {
    error(
        "configuration_session_unavailable",
        "无法启动本机连接页，请使用终端配置。",
    )
}

impl SessionManager {
    pub fn new(service: SettingsService) -> Self {
        Self {
            service,
            session: Arc::new(Mutex::new(None)),
            ttl: SESSION_TTL,
        }
    }

    /// Controlled local acceptance hook; production sessions always use the normal five-minute TTL.
    #[doc(hidden)]
    pub fn with_test_ttl(service: SettingsService, ttl: Duration) -> Self {
        Self {
            ttl,
            ..Self::new(service)
        }
    }

    /// Creates a page only for an explicit configuration request; repeated requests reuse it.
    pub async fn start(&self) -> Result<SessionInfo, SettingsError> {
        let mut current = self.session.lock().await;
        if let Some(session) = current.as_ref() {
            let mut state = session.state.lock().await;
            if state.status == SessionStatus::Active && Instant::now() < session.deadline {
                return Ok(session.info(state.status));
            }
            if state.status == SessionStatus::Active {
                session.finish(&mut state, SessionStatus::Expired);
            }
        }
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(|_| unavailable())?;
        let token = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let listener = TcpListener::bind((std::net::Ipv4Addr::LOCALHOST, 0))
            .await
            .map_err(|_| unavailable())?;
        let origin = format!(
            "http://{}",
            listener.local_addr().map_err(|_| unavailable())?
        );
        let expires_at = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_err(|_| unavailable())?
            .as_secs()
            + self.ttl.as_secs().max(1);
        let status = self.service.status()?;
        let (pending, validation_error) = match self.service.validate_saved().await {
            Ok(value) => (value, None),
            Err(error) => (None, Some(error)),
        };
        let validation = pending.as_ref().map(|v| v.summary().clone());
        let session = Arc::new(Session {
            service: self.service.clone(),
            token,
            origin,
            expires_at,
            deadline: Instant::now() + self.ttl,
            state: Mutex::new(SessionState {
                status: SessionStatus::Active,
                generation: 0,
                pending,
                validation,
                validation_error,
                preferences: status.preferences,
                image_preferences: status.image_preferences,
                active_source: Some(status.source),
            }),
            terminal: Notify::new(),
            stopped: Notify::new(),
            is_stopped: AtomicBool::new(false),
        });
        let shutdown_session = session.clone();
        let stopped_session = session.clone();
        let router = Router::new().fallback(request).with_state(session.clone());
        tokio::spawn(async move {
            let shutdown = async move {
                loop {
                    let notified = shutdown_session.terminal.notified();
                    if shutdown_session.state.lock().await.status != SessionStatus::Active {
                        break;
                    }
                    tokio::select! {
                        _ = notified => {},
                        _ = tokio::time::sleep_until(shutdown_session.deadline) => {
                            let mut state = shutdown_session.state.lock().await;
                            if state.status == SessionStatus::Active { shutdown_session.finish(&mut state, SessionStatus::Expired); }
                        }
                    }
                }
                // Keep the terminal response readable, then close without a background service.
                tokio::time::sleep(TERMINAL_GRACE).await;
            };
            let _ = axum::serve(listener, router)
                .with_graceful_shutdown(shutdown)
                .await;
            stopped_session.is_stopped.store(true, Ordering::SeqCst);
            stopped_session.stopped.notify_waiters();
        });
        let info = session.info(SessionStatus::Active);
        *current = Some(session);
        Ok(info)
    }

    pub async fn status(&self) -> Option<SessionInfo> {
        let current = self.session.lock().await.clone()?;
        let status = current.state.lock().await.status;
        Some(current.info(status))
    }

    /// Waits for graceful server shutdown so a terminal CLI cannot truncate the save response.
    pub async fn wait(&self) -> Result<SessionInfo, SettingsError> {
        let session = self
            .session
            .lock()
            .await
            .clone()
            .ok_or_else(|| error("session_not_started", "请先创建本机连接会话。"))?;
        loop {
            let stopped = session.stopped.notified();
            if session.is_stopped.load(Ordering::SeqCst) {
                break;
            }
            stopped.await;
        }
        Ok(session.info(session.state.lock().await.status))
    }

    pub async fn cancel(&self) -> Result<SessionInfo, SettingsError> {
        let session = self
            .session
            .lock()
            .await
            .clone()
            .ok_or_else(|| error("session_not_started", "请先创建本机连接会话。"))?;
        let mut state = session.state.lock().await;
        if state.status == SessionStatus::Active {
            session.finish(&mut state, SessionStatus::Cancelled);
        }
        Ok(session.info(state.status))
    }
}

impl Session {
    fn info(&self, status: SessionStatus) -> SessionInfo {
        SessionInfo {
            url: format!("{}/connect/{}/", self.origin, self.token),
            session_id: self.token.clone(),
            expires_at: self.expires_at,
            status,
        }
    }
    fn finish(&self, state: &mut SessionState, status: SessionStatus) {
        state.status = status;
        state.pending = None;
        state.validation = None;
        state.generation += 1;
        self.terminal.notify_waiters();
    }
    fn expire_if_due(&self, state: &mut SessionState) {
        if state.status == SessionStatus::Active && Instant::now() >= self.deadline {
            self.finish(state, SessionStatus::Expired);
        }
    }
    fn state_value(&self, state: &SessionState) -> Value {
        json!({"status":state.status,"expires_at":self.expires_at,"preferences":state.preferences,"image_preferences":state.image_preferences,"validation":state.validation,"validation_error":state.validation_error,"active_source":state.active_source,"status_command":crate::distribution::management_command("status")})
    }
}

fn secure(mut response: Response) -> Response {
    let headers = response.headers_mut();
    headers.insert(
        header::CACHE_CONTROL,
        HeaderValue::from_static("no-store, no-cache, max-age=0"),
    );
    headers.insert(header::PRAGMA, HeaderValue::from_static("no-cache"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "cross-origin-resource-policy",
        HeaderValue::from_static("same-origin"),
    );
    headers.insert("content-security-policy", HeaderValue::from_static("default-src 'none'; script-src 'self'; style-src 'self'; connect-src 'self'; img-src 'self' data:; base-uri 'none'; frame-ancestors 'none'; form-action 'self'"));
    response
}
fn json_response(status: StatusCode, value: Value) -> Response {
    secure((status, axum::Json(value)).into_response())
}
fn failure(status: StatusCode, err: SettingsError, session_status: SessionStatus) -> Response {
    json_response(
        status,
        json!({"status":session_status,"error":{"code":err.code,"message":err.message}}),
    )
}
fn settings_error_status(error: &SettingsError) -> StatusCode {
    match error.code {
        "invalid_key" | "invalid_model_preferences" | "invalid_image_preferences" => {
            StatusCode::BAD_REQUEST
        }
        "image_access_denied" => StatusCode::FORBIDDEN,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}
fn denied() -> Response {
    failure(
        StatusCode::FORBIDDEN,
        error("session_access_denied", "请使用本次连接页完成操作。"),
        SessionStatus::Active,
    )
}
fn content(content_type: &'static str, body: &'static str) -> Response {
    secure(([(header::CONTENT_TYPE, content_type)], body).into_response())
}
fn associated(headers: &HeaderMap, session: &Session) -> bool {
    headers.get_all("x-tokemula-session").iter().count() == 1
        && headers
            .get("x-tokemula-session")
            .and_then(|h| h.to_str().ok())
            == Some(session.token.as_str())
}

async fn request(State(session): State<Arc<Session>>, req: Request<Body>) -> Response {
    let expected_host = session
        .origin
        .strip_prefix("http://")
        .expect("fixed local origin");
    if req.headers().get_all(header::HOST).iter().count() != 1
        || req.headers().get_all(header::ORIGIN).iter().count() > 1
        || req
            .headers()
            .get(header::HOST)
            .and_then(|h| h.to_str().ok())
            != Some(expected_host)
        || req
            .uri()
            .authority()
            .is_some_and(|a| a.as_str() != expected_host)
        || req
            .headers()
            .get(header::ORIGIN)
            .is_some_and(|h| h.to_str().ok() != Some(session.origin.as_str()))
        || req.uri().query().is_some()
    {
        return denied();
    }
    let prefix = format!("/connect/{}/", session.token);
    let Some(path) = req.uri().path().strip_prefix(&prefix).map(str::to_owned) else {
        return denied();
    };
    if req.method() == Method::GET {
        if path == "api/state" {
            if !associated(req.headers(), &session) {
                return denied();
            }
            let mut state = session.state.lock().await;
            session.expire_if_due(&mut state);
            return json_response(StatusCode::OK, session.state_value(&state));
        }
        return match path.as_str() {
            "" => content(
                "text/html; charset=utf-8",
                include_str!("../assets/configure/index.html"),
            ),
            "app.js" => content(
                "text/javascript; charset=utf-8",
                include_str!("../assets/configure/app.js"),
            ),
            "app.css" => content(
                "text/css; charset=utf-8",
                include_str!("../assets/configure/app.css"),
            ),
            _ => failure(
                StatusCode::NOT_FOUND,
                error("session_route_not_found", "连接页地址无效。"),
                SessionStatus::Active,
            ),
        };
    }
    if req.method() != Method::POST
        || !associated(req.headers(), &session)
        || req
            .headers()
            .get(header::ORIGIN)
            .and_then(|h| h.to_str().ok())
            != Some(session.origin.as_str())
        || req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|h| h.to_str().ok())
            .map(|h| h.split(';').next().unwrap_or("").trim())
            != Some("application/json")
    {
        return denied();
    }
    let bytes = match to_bytes(req.into_body(), MAX_BODY).await {
        Ok(bytes) => bytes,
        Err(_) => {
            return failure(
                StatusCode::PAYLOAD_TOO_LARGE,
                error("invalid_parameters", "配置内容过大，请重新输入。"),
                SessionStatus::Active,
            );
        }
    };
    match path.as_str() {
        "api/validate" => validate(session, &bytes).await,
        "api/save" => save(session, &bytes).await,
        "api/cancel" => {
            let mut state = session.state.lock().await;
            session.expire_if_due(&mut state);
            if state.status == SessionStatus::Active {
                session.finish(&mut state, SessionStatus::Cancelled);
            }
            json_response(StatusCode::OK, session.state_value(&state))
        }
        _ => failure(
            StatusCode::NOT_FOUND,
            error("session_route_not_found", "连接页地址无效。"),
            SessionStatus::Active,
        ),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ValidateInput {
    key: String,
}

async fn validate(session: Arc<Session>, bytes: &[u8]) -> Response {
    let terminal = session.terminal.notified();
    let mut state = session.state.lock().await;
    session.expire_if_due(&mut state);
    if state.status != SessionStatus::Active {
        return terminal_response(&session, &state);
    }
    state.generation += 1;
    let generation = state.generation;
    state.pending = None;
    state.validation = None;
    state.validation_error = None;
    drop(state);
    let Ok(input) = serde_json::from_slice::<ValidateInput>(bytes) else {
        return failure(
            StatusCode::BAD_REQUEST,
            error("invalid_parameters", "请输入有效的 API Key。"),
            SessionStatus::Active,
        );
    };
    let result = tokio::select! {
        result = session.service.validate(input.key) => result,
        _ = terminal => {
            let state = session.state.lock().await;
            return terminal_response(&session, &state);
        },
        _ = tokio::time::sleep_until(session.deadline) => {
            let mut state = session.state.lock().await;
            session.expire_if_due(&mut state);
            return terminal_response(&session, &state);
        }
    };
    let mut state = session.state.lock().await;
    session.expire_if_due(&mut state);
    if state.status != SessionStatus::Active {
        return terminal_response(&session, &state);
    }
    if generation != state.generation {
        return failure(
            StatusCode::CONFLICT,
            error("validation_replaced", "Key 已更新，请使用最新验证结果。"),
            state.status,
        );
    }
    match result {
        Ok(validated) => {
            state.validation = Some(validated.summary().clone());
            state.pending = Some(validated);
            json_response(StatusCode::OK, session.state_value(&state))
        }
        Err(err) => failure(settings_error_status(&err), err, state.status),
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SaveInput {
    default_model: String,
    fallback_models: Vec<String>,
    auto_fallback: bool,
    image_preferences: Option<ImagePreferences>,
}

async fn save(session: Arc<Session>, bytes: &[u8]) -> Response {
    let mut state = session.state.lock().await;
    session.expire_if_due(&mut state);
    if state.status != SessionStatus::Active {
        return terminal_response(&session, &state);
    }
    let Ok(input) = serde_json::from_slice::<SaveInput>(bytes) else {
        return failure(
            StatusCode::BAD_REQUEST,
            error("invalid_parameters", "请选择默认模型和备用顺序。"),
            state.status,
        );
    };
    let preferences = ModelPreferences {
        default_model: input.default_model,
        fallback_models: input.fallback_models,
        auto_fallback: input.auto_fallback,
    };
    let image_preferences = input.image_preferences;
    let Some(validated) = state.pending.take() else {
        return failure(
            StatusCode::CONFLICT,
            error("validation_required", "请先验证本次 API Key。"),
            state.status,
        );
    };
    // Once accepted, save is one serialized transaction; expiry/cancel cannot report it uncommitted.
    let service = session.service.clone();
    let saved_preferences = preferences.clone();
    let saved_image_preferences = image_preferences.clone();
    let result = tokio::task::spawn_blocking(move || {
        let result = service.save_configuration(&validated, preferences, image_preferences);
        (result, validated)
    })
    .await;
    match result {
        Ok((Ok(outcome), _)) => {
            state.preferences = Some(saved_preferences);
            if saved_image_preferences.is_some() {
                state.image_preferences = saved_image_preferences;
            }
            state.active_source = Some(outcome.active_source);
            session.finish(&mut state, SessionStatus::Saved);
            json_response(StatusCode::OK, session.state_value(&state))
        }
        Ok((Err(err), validated)) => {
            state.pending = Some(validated);
            failure(settings_error_status(&err), err, state.status)
        }
        Err(_) => {
            session.finish(&mut state, SessionStatus::Cancelled);
            failure(
                StatusCode::INTERNAL_SERVER_ERROR,
                error(
                    "save_result_unknown",
                    "保存结果尚未取得，请用 status 核对后再配置。",
                ),
                state.status,
            )
        }
    }
}

fn terminal_response(session: &Session, state: &SessionState) -> Response {
    if state.status == SessionStatus::Expired {
        failure(
            StatusCode::GONE,
            error("session_expired", "连接页已过期，请重新发起配置。"),
            state.status,
        )
    } else {
        json_response(StatusCode::OK, session.state_value(state))
    }
}

#[cfg(test)]
#[path = "configuration_tests.rs"]
mod tests;
