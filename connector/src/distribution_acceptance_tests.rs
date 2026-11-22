//! Supplemental controlled-address image proof, separate from the packaged MCP inode probe.
use crate::{
    http::{ImageClient, RequestProgress},
    mcp::GenerateInput,
};
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::Notify;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Input {
    owned_root: PathBuf,
    update_command: Vec<String>,
    environment: BTreeMap<String, String>,
    output: PathBuf,
}

struct Fixture {
    posts: AtomicUsize,
    catalogs: AtomicUsize,
    fees: AtomicUsize,
    entered: Notify,
    release: Notify,
}

async fn catalog(State(state): State<Arc<Fixture>>) -> Json<Value> {
    state.catalogs.fetch_add(1, Ordering::SeqCst);
    Json(serde_json::from_str(include_str!("../tests/fixtures/http-catalog.json")).unwrap())
}

async fn generation(State(state): State<Arc<Fixture>>, headers: HeaderMap) -> Response {
    assert_eq!(
        headers.get("authorization").unwrap().to_str().unwrap(),
        "Bearer D2_SYNTHETIC_INFLIGHT"
    );
    state.posts.fetch_add(1, Ordering::SeqCst);
    state.entered.notify_one();
    state.release.notified().await;
    let image = image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 40, 200, 255]));
    let mut bytes = std::io::Cursor::new(Vec::new());
    image.write_to(&mut bytes, image::ImageFormat::Png).unwrap();
    (
        [("x-client-request-id", "d2-inflight-original")],
        Json(json!({"data":[{"b64_json":STANDARD.encode(bytes.into_inner())}]})),
    )
        .into_response()
}

async fn fee(State(state): State<Arc<Fixture>>) -> StatusCode {
    state.fees.fetch_add(1, Ordering::SeqCst);
    StatusCode::NOT_FOUND
}

#[tokio::test]
#[ignore = "requires explicit owned installation and signed loopback update fixture"]
async fn synthetic_image_completes_once_while_installed_pair_switches() {
    let path = std::env::var_os("TOKEMULA_D2_INFLIGHT_INPUT").expect("explicit D2 input required");
    let input: Input = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert!(input.owned_root.is_absolute() && input.owned_root.is_dir());
    assert!(input.output.is_absolute());
    assert!(input.update_command.len() >= 2);
    assert!(PathBuf::from(&input.update_command[0]).starts_with(&input.owned_root));
    for name in [
        "HOME",
        "XDG_CONFIG_HOME",
        "XDG_DATA_HOME",
        "XDG_STATE_HOME",
        "TMPDIR",
    ] {
        assert!(PathBuf::from(input.environment.get(name).unwrap()).starts_with(&input.owned_root));
    }
    let fixture = Arc::new(Fixture {
        posts: AtomicUsize::new(0),
        catalogs: AtomicUsize::new(0),
        fees: AtomicUsize::new(0),
        entered: Notify::new(),
        release: Notify::new(),
    });
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = Router::new()
        .route("/v1/images/models", get(catalog))
        .route("/v1/images/generations", post(generation))
        .route("/v1/images/requests/{reference}/billing", get(fee))
        .with_state(fixture.clone());
    let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    let client = ImageClient::build(
        &format!("http://{address}/v1/"),
        Some("D2_SYNTHETIC_INFLIGHT".into()),
        Duration::from_secs(120),
    )
    .unwrap();
    let request = GenerateInput {
        prompt: "Synthetic image held across an installed version switch".into(),
        model: Some("sample-fast".into()),
        size: None,
        quality: None,
        complexity: None,
        n: Some(1),
        mode: None,
        result_mode: None,
    };
    let progress = RequestProgress::default();
    let image_request =
        tokio::spawn(async move { client.execute(&request, None, &progress).await });
    tokio::time::timeout(Duration::from_secs(10), fixture.entered.notified())
        .await
        .expect("synthetic image request did not reach its one POST");
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.fees.load(Ordering::SeqCst), 0);
    let command = input.update_command;
    let env = input.environment;
    let update = tokio::task::spawn_blocking(move || {
        std::process::Command::new(&command[0])
            .args(&command[1..])
            .env_clear()
            .envs(env)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    fixture.release.notify_one();
    let image = tokio::time::timeout(Duration::from_secs(20), image_request)
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    assert!(
        update.status.success(),
        "installed upgrade failed: {}",
        String::from_utf8_lossy(&update.stderr)
    );
    assert_eq!(image.images.len(), 1);
    assert_eq!(image.metadata["request_ref"], "d2-inflight-original");
    assert_eq!(fixture.posts.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.fees.load(Ordering::SeqCst), 0);
    std::fs::write(
        input.output,
        serde_json::to_vec_pretty(&json!({
            "evidence_scope":"controlled-library-address image request during actual installed CLI upgrade",
            "catalog_gets":fixture.catalogs.load(Ordering::SeqCst),
            "image_posts":fixture.posts.load(Ordering::SeqCst),
            "fee_gets":fixture.fees.load(Ordering::SeqCst),
            "image_completed":true,
            "request_ref_preserved":true,
            "actual_update_exit":update.status.code(),
            "update_stdout":String::from_utf8_lossy(&update.stdout),
        }))
        .unwrap(),
    )
    .unwrap();
    server.abort();
}

#[cfg(unix)]
#[tokio::test]
#[ignore = "requires explicit signed release fixture; uses isolated paths and an in-memory credential store"]
async fn desktop_originals_survive_keep_and_purge_uninstall() {
    use crate::{
        distribution::{DataPaths, Installer},
        output::PersistentOutput,
        settings::SettingsService,
    };
    use std::fs;

    let release =
        PathBuf::from(std::env::var_os("D2_AUDIT_RELEASE").expect("explicit signed fixture"));
    let trust = PathBuf::from(std::env::var_os("D2_AUDIT_TRUST").expect("explicit public trust"));
    assert!(release.is_absolute() && trust.is_absolute());
    let mut encoded = std::io::Cursor::new(Vec::new());
    image::RgbaImage::from_pixel(2, 2, image::Rgba([20, 40, 200, 255]))
        .write_to(&mut encoded, image::ImageFormat::Png)
        .unwrap();
    let original = encoded.into_inner();

    for purge in [false, true] {
        let temporary = tempfile::tempdir().unwrap();
        let home = temporary.path().join("用户 home space");
        let desktop = home.join("Desktop");
        fs::create_dir_all(&desktop).unwrap();
        let paths = DataPaths {
            tasks: home.join("state/tasks"),
            cache: home.join("cache/images"),
        };
        let installer = Installer::new(home.join("installation"), home.join("adapters"))
            .unwrap()
            .with_data_paths(paths.clone())
            .unwrap();
        assert_eq!(
            installer.setup(&release, &trust, None).await.unwrap()["status"],
            "installed"
        );
        let output = PersistentOutput {
            root: desktop.join("Tokemula-Images"),
        };
        let saved = output.save(&original, "image/png").unwrap();
        fs::create_dir_all(&paths.cache).unwrap();
        let expired_cache = paths.cache.join("tic-image-1-AbCdEf123456.png");
        fs::write(&expired_cache, b"expired temporary cache").unwrap();
        let settings_directory = home.join("settings");
        fs::create_dir(&settings_directory).unwrap();
        let preferences = settings_directory.join("settings.json");
        fs::write(
            &preferences,
            br#"{"default_model":"sample-fast","fallback_models":[],"auto_fallback":false}"#,
        )
        .unwrap();
        let service =
            SettingsService::in_memory_for_tests(settings_directory, "http://127.0.0.1:9/v1/");
        let result = installer.uninstall(&service, purge, purge).unwrap();
        assert_eq!(result["status"], "uninstalled");
        assert_eq!(
            result["settings"],
            if purge { "purged" } else { "retained" }
        );
        assert_eq!(result["user_files"], "preserved");
        assert_eq!(fs::read(&saved).unwrap(), original);
        assert!(saved.starts_with(fs::canonicalize(desktop.join("Tokemula-Images")).unwrap()));
        assert!(!expired_cache.exists());
        assert_eq!(preferences.exists(), !purge);
    }
}
