use super::*;
use axum::{Json, routing::get};
use std::sync::atomic::AtomicUsize;
use tokio::task::JoinHandle;

const SYNTHETIC_KEY: &str = "D1_CONFIGURATION_SYNTHETIC_KEY";

struct Fixture {
    base: String,
    task: JoinHandle<()>,
    gets: Arc<AtomicUsize>,
    posts: Arc<AtomicUsize>,
    delay: Arc<AtomicBool>,
}
impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Fixture {
    async fn new() -> Self {
        let gets = Arc::new(AtomicUsize::new(0));
        let posts = Arc::new(AtomicUsize::new(0));
        let delay = Arc::new(AtomicBool::new(false));
        let g = gets.clone();
        let p = posts.clone();
        let d = delay.clone();
        let app = Router::new()
            .route(
                "/v1/images/models",
                get(move |headers: HeaderMap| {
                    let gets = g.clone();
                    let delay = d.clone();
                    async move {
                        gets.fetch_add(1, Ordering::SeqCst);
                        if delay.load(Ordering::SeqCst) {
                            tokio::time::sleep(Duration::from_millis(500)).await;
                        }
                        if headers
                            .get(header::AUTHORIZATION)
                            .and_then(|h| h.to_str().ok())
                            != Some(concat!("Bearer ", "D1_CONFIGURATION_SYNTHETIC_KEY"))
                        {
                            return (
                                StatusCode::UNAUTHORIZED,
                                Json(json!({"error":{"type":"authentication_error"}})),
                            )
                                .into_response();
                        }
                        Json(
                            serde_json::from_str::<Value>(include_str!(
                                "../tests/fixtures/http-catalog.json"
                            ))
                            .unwrap(),
                        )
                        .into_response()
                    }
                }),
            )
            .fallback(move || {
                let posts = p.clone();
                async move {
                    posts.fetch_add(1, Ordering::SeqCst);
                    StatusCode::NOT_FOUND
                }
            });
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        Self {
            base,
            task,
            gets,
            posts,
            delay,
        }
    }
}

fn client() -> reqwest::Client {
    reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .unwrap()
}
fn origin(info: &SessionInfo) -> String {
    reqwest::Url::parse(&info.url)
        .unwrap()
        .origin()
        .ascii_serialization()
}
fn post(
    c: &reqwest::Client,
    info: &SessionInfo,
    action: &str,
    body: Value,
) -> reqwest::RequestBuilder {
    c.post(format!("{}api/{action}", info.url))
        .header("X-Tokemula-Session", &info.session_id)
        .header(header::ORIGIN, origin(info))
        .json(&body)
}
async fn state(c: &reqwest::Client, info: &SessionInfo) -> Value {
    c.get(format!("{}api/state", info.url))
        .header("X-Tokemula-Session", &info.session_id)
        .send()
        .await
        .unwrap()
        .json()
        .await
        .unwrap()
}
fn preferences() -> Value {
    json!({"default_model":"sample-fast","fallback_models":[],"auto_fallback":false})
}

#[tokio::test]
async fn sessions_are_lazy_reused_and_reject_unassociated_or_cross_origin_writes() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new().await;
    let service = SettingsService::in_memory_for_tests(root.path().join("settings"), &f.base);
    let manager = SessionManager::new(service.clone());
    assert!(manager.status().await.is_none());
    assert_eq!(f.gets.load(Ordering::SeqCst), 0);
    let info = manager.start().await.unwrap();
    let same = manager.start().await.unwrap();
    assert_eq!(info.session_id, same.session_id);
    assert_eq!(info.session_id.len(), 64);
    let c = client();
    let page = c.get(&info.url).send().await.unwrap();
    assert_eq!(page.status(), StatusCode::OK);
    assert!(
        page.headers()[header::CACHE_CONTROL]
            .to_str()
            .unwrap()
            .contains("no-store")
    );
    assert!(
        page.headers()["content-security-policy"]
            .to_str()
            .unwrap()
            .contains("frame-ancestors 'none'")
    );
    assert!(!page.text().await.unwrap().contains(SYNTHETIC_KEY));
    assert_eq!(
        c.get(format!("{}api/state", info.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&c, &info, "validate", json!({"key":SYNTHETIC_KEY}))
            .header(header::ORIGIN, "https://foreign.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&c, &info, "validate", json!({"key":SYNTHETIC_KEY}))
            .header(header::HOST, "foreign.invalid")
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        c.post(format!("{}api/validate", info.url))
            .header(header::ORIGIN, origin(&info))
            .json(&json!({"key":SYNTHETIC_KEY}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        c.get(format!("{}?key=not-accepted", info.url))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        post(&c, &info, "validate", json!({"key":"x".repeat(MAX_BODY)}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::PAYLOAD_TOO_LARGE
    );
    assert_eq!(f.gets.load(Ordering::SeqCst), 0);
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    let cancelled = post(&c, &info, "cancel", json!({}))
        .send()
        .await
        .unwrap()
        .json::<Value>()
        .await
        .unwrap();
    assert_eq!(cancelled["status"], "cancelled");
    assert!(service.snapshot().unwrap().api_key.is_none());
    assert_eq!(
        manager.wait().await.unwrap().status,
        SessionStatus::Cancelled
    );
    assert!(c.get(&info.url).send().await.is_err());
}

#[tokio::test]
async fn validation_replacement_save_repetition_and_cancel_preserve_configuration() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new().await;
    let service = SettingsService::in_memory_for_tests(root.path().join("settings"), &f.base);
    let manager = SessionManager::new(service.clone());
    let info = manager.start().await.unwrap();
    let c = client();
    let validated = post(&c, &info, "validate", json!({"key":SYNTHETIC_KEY}))
        .send()
        .await
        .unwrap();
    assert_eq!(validated.status(), StatusCode::OK);
    let visible = validated.text().await.unwrap();
    assert!(!visible.contains(SYNTHETIC_KEY));
    assert_eq!(
        post(&c, &info, "validate", json!({"key":"invalid-synthetic"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    assert_eq!(
        post(&c, &info, "save", preferences())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::CONFLICT
    );
    assert!(service.snapshot().unwrap().api_key.is_none());
    assert_eq!(
        post(&c, &info, "validate", json!({"key":SYNTHETIC_KEY}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    let (a, b) = tokio::join!(
        post(&c, &info, "save", preferences()).send(),
        post(&c, &info, "save", preferences()).send()
    );
    for response in [a.unwrap(), b.unwrap()] {
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(response.json::<Value>().await.unwrap()["status"], "saved");
    }
    assert!(service.snapshot().unwrap().api_key.as_deref() == Some(SYNTHETIC_KEY));
    assert_eq!(f.gets.load(Ordering::SeqCst), 3);
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    assert_eq!(manager.wait().await.unwrap().status, SessionStatus::Saved);
    let next = manager.start().await.unwrap();
    assert_ne!(next.session_id, info.session_id);
    assert_eq!(
        post(&c, &next, "validate", json!({"key":"invalid-synthetic"}))
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::BAD_REQUEST
    );
    manager.cancel().await.unwrap();
    manager.wait().await.unwrap();
    assert!(service.snapshot().unwrap().api_key.as_deref() == Some(SYNTHETIC_KEY));
}

#[tokio::test]
async fn expiry_cancels_pending_validation_without_saving_and_closes_the_server() {
    let root = tempfile::tempdir().unwrap();
    let f = Fixture::new().await;
    f.delay.store(true, Ordering::SeqCst);
    let service = SettingsService::in_memory_for_tests(root.path().join("settings"), &f.base);
    let manager = SessionManager::with_test_ttl(service.clone(), Duration::from_millis(80));
    let info = manager.start().await.unwrap();
    let c = client();
    let validation = post(&c, &info, "validate", json!({"key":SYNTHETIC_KEY}))
        .send()
        .await
        .unwrap();
    assert_eq!(validation.status(), StatusCode::GONE);
    assert_eq!(state(&c, &info).await["status"], "expired");
    assert_eq!(
        post(&c, &info, "save", preferences())
            .send()
            .await
            .unwrap()
            .status(),
        StatusCode::GONE
    );
    assert!(service.snapshot().unwrap().api_key.is_none());
    assert_eq!(manager.wait().await.unwrap().status, SessionStatus::Expired);
    assert!(c.get(&info.url).send().await.is_err());
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn cancellation_discards_inflight_validation_and_preserves_the_prior_configuration() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let service = SettingsService::in_memory_for_tests(root.path().join("settings"), &fixture.base);
    let manager = SessionManager::new(service.clone());
    let info = manager.start().await.unwrap();
    fixture.delay.store(true, Ordering::SeqCst);
    let c = client();
    let response = post(&c, &info, "validate", json!({"key":SYNTHETIC_KEY})).send();
    let cancel = async {
        while fixture.gets.load(Ordering::SeqCst) == 0 {
            tokio::task::yield_now().await;
        }
        manager.cancel().await.unwrap();
    };
    let (result, ()) = tokio::join!(response, cancel);
    assert_eq!(
        result.unwrap().json::<Value>().await.unwrap()["status"],
        "cancelled"
    );
    assert!(service.snapshot().unwrap().api_key.is_none());
    assert_eq!(
        manager.wait().await.unwrap().status,
        SessionStatus::Cancelled
    );
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn d4_saved_key_page_updates_image_defaults_without_key_reentry_and_cancel_is_readonly() {
    let root = tempfile::tempdir().unwrap();
    let fixture = Fixture::new().await;
    let service = SettingsService::in_memory_for_tests(root.path().join("settings"), &fixture.base);
    let validated = service.validate(SYNTHETIC_KEY.into()).await.unwrap();
    let models = ModelPreferences {
        default_model: "sample-fast".into(),
        fallback_models: vec![],
        auto_fallback: false,
    };
    service.save(&validated, models).unwrap();
    let manager = SessionManager::new(service.clone());
    let info = manager.start().await.unwrap();
    let c = client();
    let before = state(&c, &info).await;
    assert!(before["validation"]["models"][0]["capabilities"].is_array());
    assert_eq!(before["active_source"], "system_store");
    assert!(
        !serde_json::to_string(&before)
            .unwrap()
            .contains(SYNTHETIC_KEY)
    );
    let mut body = preferences();
    body["image_preferences"] = json!({"version":1,"profiles":[{"operation":"generation","mode":"sync","size":"1024x1024","quality":"standard"}]});
    let response = post(&c, &info, "save", body).send().await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.json::<Value>().await.unwrap()["status"], "saved");
    let saved = std::fs::read(service.image_preferences_path()).unwrap();
    assert_eq!(
        service.snapshot().unwrap().api_key.as_deref(),
        Some(SYNTHETIC_KEY)
    );
    let next = manager.start().await.unwrap();
    assert_eq!(
        state(&c, &next).await["image_preferences"]["profiles"][0]["quality"],
        "standard"
    );
    post(&c, &next, "cancel", json!({})).send().await.unwrap();
    assert_eq!(
        std::fs::read(service.image_preferences_path()).unwrap(),
        saved
    );
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 0);
}
