//! Explicit synthetic browser acceptance fixture; never uses the normal product credential entry.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
};
use serde_json::{Value, json};
use std::{
    fs,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokemula_image_connector::{
    configuration::{SessionManager, SessionStatus},
    settings::{ModelPreferences, SettingsService},
};

const PREFIX: &str = "D1_SYNTHETIC_BROWSER_";
struct CatalogFixture {
    gets: AtomicUsize,
    posts: AtomicUsize,
    catalog: Value,
}
async fn catalog(State(fixture): State<Arc<CatalogFixture>>, headers: HeaderMap) -> Response {
    fixture.gets.fetch_add(1, Ordering::SeqCst);
    let bearer = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    let state = bearer
        .strip_prefix(&format!("Bearer {PREFIX}"))
        .unwrap_or("INVALID");
    if state == "INVALID" {
        return (
            StatusCode::UNAUTHORIZED,
            Json(json!({"error":{"code":"invalid_api_key"}})),
        )
            .into_response();
    }
    if state == "UNAVAILABLE" {
        return (
            StatusCode::NOT_FOUND,
            Json(json!({"error":{"code":"not_deployed"}})),
        )
            .into_response();
    }
    if state == "DENIED" {
        return (
            StatusCode::FORBIDDEN,
            Json(json!({"error":{"code":"image_access_denied"}})),
        )
            .into_response();
    }
    let mut catalog = fixture.catalog.clone();
    if state == "NO_MODELS" {
        catalog["models"] = json!([]);
    }
    if state == "NO_ACCESS" {
        catalog["models"] = json!([]);
        catalog["can_submit"] = json!(false);
        catalog["reason_code"] = json!("image_access_disabled");
    }
    if state == "RESTRICTED" {
        catalog["can_submit"] = json!(false);
        catalog["reason_code"] = json!("insufficient_balance");
    }
    Json(catalog).into_response()
}
async fn unexpected(State(fixture): State<Arc<CatalogFixture>>, method: Method) -> StatusCode {
    if method == Method::POST {
        fixture.posts.fetch_add(1, Ordering::SeqCst);
    }
    StatusCode::NOT_FOUND
}
struct Cleanup(SettingsService);
impl Drop for Cleanup {
    fn drop(&mut self) {
        let _ = self.0.logout();
    }
}
fn private_json(path: &std::path::Path, value: &Value) {
    fs::write(path, serde_json::to_vec_pretty(value).unwrap()).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).unwrap();
    }
}

#[tokio::main]
async fn main() {
    assert!(
        std::env::var_os("TOKEMULA_IMAGE_KEY").is_none(),
        "clear inherited authentication for this fixture"
    );
    let root = PathBuf::from(std::env::var_os("D1_PAGE_ROOT").expect("explicit D1_PAGE_ROOT"));
    assert!(root.is_absolute());
    fs::create_dir_all(&root).unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&root, fs::Permissions::from_mode(0o700)).unwrap();
    }
    let mode = std::env::var("D1_PAGE_SCENARIO").unwrap_or_else(|_| "save".into());
    assert!(matches!(
        mode.as_str(),
        "save" | "cancel" | "expired" | "save_failure" | "save_unknown"
    ));
    let mut catalog_data: Value =
        serde_json::from_str(include_str!("../tests/fixtures/http-catalog.json")).unwrap();
    let mut model = catalog_data["models"][0].clone();
    let models = [
        ("image-studio", "图像工作室"),
        ("image-fast", "快速图片"),
        ("image-detail", "细节图片"),
    ]
    .into_iter()
    .map(|(id, name)| {
        model["id"] = json!(id);
        model["display_name"] = json!(name);
        model["family"] = Value::Null;
        model.clone()
    })
    .collect::<Vec<_>>();
    // Keep the existing valid fixture metadata; only the synthetic public model identities differ.
    catalog_data["models"] = json!(models);
    for model in catalog_data["models"].as_array_mut().unwrap() {
        model.as_object_mut().unwrap().remove("family");
    }
    let fixture = Arc::new(CatalogFixture {
        gets: AtomicUsize::new(0),
        posts: AtomicUsize::new(0),
        catalog: catalog_data,
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1/", listener.local_addr().unwrap());
    let api = tokio::spawn(
        axum::serve(
            listener,
            Router::new()
                .route("/v1/images/models", get(catalog))
                .fallback(unexpected)
                .with_state(fixture.clone()),
        )
        .into_future(),
    );
    let unique = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let namespace = format!(
        "tokemula-image-test-d1-page-{}-{unique}",
        std::process::id()
    );
    let service =
        SettingsService::for_isolated_tests(root.join("settings"), &namespace, &base).unwrap();
    private_json(
        &root.join("credential-namespace.json"),
        &json!({"namespace":namespace,"account":"synthetic-test-key","synthetic_only":true}),
    );
    let _cleanup = Cleanup(service.clone());
    assert!(service.snapshot().unwrap().api_key.is_none());
    let old_preferences = ModelPreferences {
        default_model: "image-fast".into(),
        fallback_models: vec![],
        auto_fallback: false,
    };
    let seed = service.validate(format!("{PREFIX}SEED")).await.unwrap();
    service.save(&seed, old_preferences.clone()).unwrap();
    let obstruction = root.join("settings/settings.json");
    let previous_file = root.join("settings/settings-before-fixture.json");
    if mode == "save_failure" {
        fs::rename(&obstruction, &previous_file).unwrap();
        fs::create_dir(&obstruction).unwrap();
    }
    let ttl = if mode == "expired" {
        Duration::from_secs(8)
    } else {
        Duration::from_secs(180)
    };
    let manager = SessionManager::with_test_ttl(service.clone(), ttl);
    let info = manager.start().await.unwrap();
    private_json(
        &root.join("session.json"),
        &json!({"url":info.url,"expires_at":info.expires_at,"namespace":namespace,"scenario":mode,"obstruction":obstruction,"previous_file":previous_file}),
    );
    println!("Browser fixture ready; metadata stored in the explicit isolated root.");
    let result = manager.wait().await.unwrap();
    let snapshot = service.snapshot().unwrap();
    let saved = result.status == SessionStatus::Saved;
    let expected_key = if saved {
        format!("{PREFIX}RESTRICTED")
    } else {
        format!("{PREFIX}SEED")
    };
    let key_is_expected = snapshot.api_key.as_deref() == Some(expected_key.as_str());
    assert!(
        key_is_expected,
        "synthetic system credential differs from expected browser outcome"
    );
    if !saved {
        assert_eq!(snapshot.preferences.as_ref(), Some(&old_preferences));
    }
    let preferences = snapshot.preferences.clone();
    drop(snapshot);
    service.logout().unwrap();
    assert!(service.snapshot().unwrap().api_key.is_none());
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 0);
    private_json(
        &root.join("result.json"),
        &json!({"status":result.status,"scenario":mode,"synthetic_credential_is_expected":key_is_expected,"preferences":preferences,"credential_removed":true,"catalog_gets":fixture.gets.load(Ordering::SeqCst),"image_posts":0,"namespace":namespace}),
    );
    api.abort();
    println!(
        "Browser fixture finished; synthetic credential removed and image POST count is zero."
    );
}
