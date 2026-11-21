use super::*;
use axum::{
    Router,
    body::Body,
    extract::{Request, State},
    http::{Method, StatusCode},
    response::{IntoResponse, Response},
    routing::any,
};
use serde_json::{Value, json};
use std::{
    path::{Path, PathBuf},
    process::Command,
    sync::atomic::{AtomicUsize, Ordering},
    time::{SystemTime, UNIX_EPOCH},
};

const KEY_ONE: &str = "D1_SYNTHETIC_KEY_ONE";
const KEY_TWO: &str = "D1_SYNTHETIC_KEY_TWO";

fn prefs(default: &str) -> ModelPreferences {
    ModelPreferences {
        default_model: default.into(),
        fallback_models: if default == "sample-fast" {
            vec!["sample-26".into()]
        } else {
            vec!["sample-fast".into()]
        },
        auto_fallback: true,
    }
}
fn validated(key: &str) -> ValidatedConfiguration {
    ValidatedConfiguration {
        key: Zeroizing::new(key.into()),
        summary: ValidationSummary {
            status: ValidationState::Ready,
            can_submit: true,
            reason_code: None,
            models: vec![
                ModelOption {
                    id: "sample-fast".into(),
                    display_name: "Synthetic Fast".into(),
                    availability: "configured".into(),
                    capabilities: vec![],
                },
                ModelOption {
                    id: "sample-26".into(),
                    display_name: "Synthetic Family".into(),
                    availability: "configured".into(),
                    capabilities: vec![],
                },
            ],
        },
    }
}
fn code<T>(result: Result<T>) -> &'static str {
    match result {
        Ok(_) => panic!("expected safe failure"),
        Err(e) => e.code,
    }
}
fn service(root: &Path, store: Arc<dyn SecretStore>) -> SettingsService {
    SettingsService::new(root.to_owned(), store, "http://127.0.0.1:9/v1/".into())
}

#[derive(Default)]
struct FaultStore {
    value: Mutex<Option<Vec<u8>>>,
    get_failure: Mutex<Option<&'static str>>,
    set_failure: Mutex<Option<&'static str>>,
    sets: AtomicUsize,
    deny_after_set: Mutex<Option<PathBuf>>,
    fail_set_number: AtomicUsize,
}
impl SecretStore for FaultStore {
    fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
        if let Some(code) = *self.get_failure.lock().unwrap() {
            return Err(error(code, "合成凭据库故障。"));
        }
        Ok(self
            .value
            .lock()
            .unwrap()
            .as_ref()
            .map(|v| Zeroizing::new(v.clone())))
    }
    fn set(&self, value: &[u8]) -> Result<()> {
        let call = self.sets.fetch_add(1, Ordering::SeqCst) + 1;
        if self.fail_set_number.load(Ordering::SeqCst) == call {
            return Err(error("credential_store_locked", "合成凭据库故障。"));
        }
        if let Some(code) = *self.set_failure.lock().unwrap() {
            return Err(error(code, "合成凭据库故障。"));
        }
        *self.value.lock().unwrap() = Some(value.to_vec());
        #[cfg(unix)]
        if let Some(root) = self.deny_after_set.lock().unwrap().take() {
            use std::os::unix::fs::PermissionsExt;
            fs::set_permissions(root, fs::Permissions::from_mode(0o500)).unwrap();
        }
        Ok(())
    }
    fn delete(&self) -> Result<()> {
        *self.value.lock().unwrap() = None;
        Ok(())
    }
}

#[test]
fn coherent_snapshots_update_logout_and_nonsecret_preferences() {
    assert!(
        std::env::var_os("TOKEMULA_IMAGE_KEY").is_none(),
        "run this owned fixture without inherited override"
    );
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let settings = service(root.path(), store.clone());
    assert_eq!(
        settings.status().unwrap().preferences_status,
        "not_configured"
    );
    settings
        .save(&validated(KEY_ONE), prefs("sample-fast"))
        .unwrap();
    assert_eq!(settings.status().unwrap().preferences_status, "active");
    let old = settings.snapshot().unwrap();
    settings
        .save(&validated(KEY_TWO), prefs("sample-26"))
        .unwrap();
    let new = settings.snapshot().unwrap();
    assert!(old.api_key.as_deref() == Some(KEY_ONE));
    assert_eq!(old.preferences, Some(prefs("sample-fast")));
    assert!(new.api_key.as_deref() == Some(KEY_TWO));
    assert_eq!(new.preferences, Some(prefs("sample-26")));
    assert_eq!(new.source, CredentialSource::SystemStore);
    let bytes = fs::read(settings.settings_path()).unwrap();
    assert!(
        !bytes
            .windows(KEY_ONE.len())
            .any(|v| v == KEY_ONE.as_bytes())
    );
    assert!(
        !bytes
            .windows(KEY_TWO.len())
            .any(|v| v == KEY_TWO.as_bytes())
    );
    let state = settings.logout().unwrap();
    assert_eq!(state.source, CredentialSource::None);
    assert_eq!(state.preferences_status, "saved_without_credential");
    assert_eq!(state.preferences, Some(prefs("sample-26")));
    assert!(settings.snapshot().unwrap().api_key.is_none());
    assert!(store.value.lock().unwrap().is_none());
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            fs::metadata(root.path()).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(settings.settings_path())
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o600
        );
    }
}

#[test]
fn storage_lock_initialization_and_save_failures_keep_previous_configuration() {
    assert!(std::env::var_os("TOKEMULA_IMAGE_KEY").is_none());
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let settings = service(root.path(), store.clone());
    settings
        .save(&validated(KEY_ONE), prefs("sample-fast"))
        .unwrap();
    let old_file = fs::read(settings.settings_path()).unwrap();
    *store.get_failure.lock().unwrap() = Some("credential_store_restart_required");
    assert_eq!(
        code(settings.snapshot()),
        "credential_store_restart_required"
    );
    assert_eq!(code(settings.status()), "credential_store_restart_required");
    *store.get_failure.lock().unwrap() = Some("credential_store_locked");
    assert_eq!(code(settings.snapshot()), "credential_store_locked");
    *store.get_failure.lock().unwrap() = None;
    assert!(settings.snapshot().unwrap().api_key.as_deref() == Some(KEY_ONE));
    *store.set_failure.lock().unwrap() = Some("credential_store_locked");
    assert_eq!(
        code(settings.save(&validated(KEY_TWO), prefs("sample-26"))),
        "credential_store_locked"
    );
    *store.set_failure.lock().unwrap() = None;
    assert!(settings.snapshot().unwrap().api_key.as_deref() == Some(KEY_ONE));
    assert_eq!(fs::read(settings.settings_path()).unwrap(), old_file);
    assert_eq!(
        native_error(keyring::v1::Error::NoDefaultStore).code,
        "credential_store_restart_required"
    );
    assert_eq!(
        native_error(keyring::v1::Error::NoStorageAccess(Box::new(
            std::io::Error::other("sensitive synthetic platform payload")
        )))
        .code,
        "credential_store_locked"
    );
    let serialized = serde_json::to_string(&native_error(keyring::v1::Error::PlatformFailure(
        Box::new(std::io::Error::other(KEY_ONE)),
    )))
    .unwrap();
    assert!(!serialized.contains(KEY_ONE));
    assert_eq!(
        code(settings.save(
            &validated(KEY_TWO),
            ModelPreferences {
                default_model: "foreign".into(),
                fallback_models: vec![],
                auto_fallback: false
            }
        )),
        "invalid_model_preferences"
    );
    assert_eq!(
        code(settings.save(
            &validated(KEY_TWO),
            ModelPreferences {
                default_model: "sample-fast".into(),
                fallback_models: vec!["sample-fast".into()],
                auto_fallback: true
            }
        )),
        "invalid_model_preferences"
    );
}

#[test]
fn cached_initialization_failure_requires_new_process_context_but_lock_can_retry() {
    use std::sync::{OnceLock, atomic::AtomicBool};
    struct InitializingStore {
        ready: AtomicBool,
        initialization: OnceLock<Result<()>>,
        attempts: AtomicUsize,
    }
    impl SecretStore for InitializingStore {
        fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
            self.initialization
                .get_or_init(|| {
                    self.attempts.fetch_add(1, Ordering::SeqCst);
                    if self.ready.load(Ordering::SeqCst) {
                        Ok(())
                    } else {
                        Err(native_error(keyring::v1::Error::NoDefaultStore))
                    }
                })
                .clone()?;
            Ok(None)
        }
        fn set(&self, _: &[u8]) -> Result<()> {
            unreachable!()
        }
        fn delete(&self) -> Result<()> {
            unreachable!()
        }
    }
    let root = tempfile::tempdir().unwrap();
    let first = Arc::new(InitializingStore {
        ready: AtomicBool::new(false),
        initialization: OnceLock::new(),
        attempts: AtomicUsize::new(0),
    });
    let settings = service(root.path(), first.clone());
    assert_eq!(
        code(settings.snapshot()),
        "credential_store_restart_required"
    );
    first.ready.store(true, Ordering::SeqCst);
    assert_eq!(
        code(settings.snapshot()),
        "credential_store_restart_required"
    );
    assert_eq!(first.attempts.load(Ordering::SeqCst), 1);
    let fresh = Arc::new(InitializingStore {
        ready: AtomicBool::new(true),
        initialization: OnceLock::new(),
        attempts: AtomicUsize::new(0),
    });
    assert!(
        service(root.path(), fresh)
            .snapshot()
            .unwrap()
            .api_key
            .is_none()
    );
    let lock = Arc::new(FaultStore::default());
    let unlocked = service(root.path(), lock.clone());
    *lock.get_failure.lock().unwrap() = Some("credential_store_locked");
    assert_eq!(code(unlocked.snapshot()), "credential_store_locked");
    *lock.get_failure.lock().unwrap() = None;
    assert!(unlocked.snapshot().unwrap().api_key.is_none());
}

#[cfg(unix)]
#[test]
fn preference_commit_failure_rolls_back_system_entry_and_partial_failure_is_explicit() {
    use std::os::unix::fs::PermissionsExt;
    assert!(std::env::var_os("TOKEMULA_IMAGE_KEY").is_none());
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let settings = service(root.path(), store.clone());
    settings
        .save(&validated(KEY_ONE), prefs("sample-fast"))
        .unwrap();
    let old_file = fs::read(settings.settings_path()).unwrap();
    *store.deny_after_set.lock().unwrap() = Some(root.path().to_owned());
    let result = settings.save(&validated(KEY_TWO), prefs("sample-26"));
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(code(result), "settings_storage_failed");
    assert!(settings.snapshot().unwrap().api_key.as_deref() == Some(KEY_ONE));
    assert_eq!(fs::read(settings.settings_path()).unwrap(), old_file);
    let rollback_call = store.sets.load(Ordering::SeqCst) + 2;
    store.fail_set_number.store(rollback_call, Ordering::SeqCst);
    *store.deny_after_set.lock().unwrap() = Some(root.path().to_owned());
    let result = settings.save(&validated(KEY_TWO), prefs("sample-26"));
    fs::set_permissions(root.path(), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(code(result), "configuration_save_partial");
    // The atomic envelope remains coherent even when rollback is itself unavailable.
    let context = settings.snapshot().unwrap();
    assert!(context.api_key.as_deref() == Some(KEY_TWO));
    assert_eq!(context.preferences, Some(prefs("sample-26")));
    assert_eq!(fs::read(settings.settings_path()).unwrap(), old_file);
}

#[derive(Default)]
struct ApiState {
    mode: Mutex<String>,
    gets: AtomicUsize,
    posts: AtomicUsize,
}
struct Api {
    state: Arc<ApiState>,
    base: String,
    task: tokio::task::JoinHandle<()>,
}
impl Drop for Api {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Api {
    async fn start() -> Self {
        let state = Arc::new(ApiState::default());
        let router = Router::new().fallback(any(api)).with_state(state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { state, base, task }
    }
}
async fn api(State(state): State<Arc<ApiState>>, request: Request) -> Response {
    if request.method() != Method::GET || request.uri().path() != "/v1/images/models" {
        state.posts.fetch_add(1, Ordering::SeqCst);
        return StatusCode::METHOD_NOT_ALLOWED.into_response();
    }
    let header = request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    assert!(header == format!("Bearer {KEY_ONE}") || header == format!("Bearer {KEY_TWO}"));
    state.gets.fetch_add(1, Ordering::SeqCst);
    let mode = state.mode.lock().unwrap().clone();
    let failure = match mode.as_str() {
        "invalid" => Some(StatusCode::UNAUTHORIZED),
        "unavailable" => Some(StatusCode::NOT_FOUND),
        "denied" => Some(StatusCode::FORBIDDEN),
        "server" => Some(StatusCode::SERVICE_UNAVAILABLE),
        _ => None,
    };
    if let Some(status) = failure {
        return (
            status,
            axum::Json(json!({"error":{"message":"sensitive server detail"}})),
        )
            .into_response();
    }
    let mut body: Value =
        serde_json::from_str(include_str!("../tests/fixtures/http-catalog.json")).unwrap();
    if mode == "empty" || mode == "no-access" {
        body["models"] = json!([]);
    }
    if mode == "balance" {
        body["can_submit"] = json!(false);
        body["reason_code"] = json!("insufficient_balance");
    }
    if mode == "no-access" {
        body["can_submit"] = json!(false);
        body["reason_code"] = json!("image_access_disabled");
    }
    Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/json")
        .body(Body::from(body.to_string()))
        .unwrap()
}

#[tokio::test]
async fn validation_uses_only_catalog_get_and_distinguishes_real_states() {
    let api = Api::start().await;
    let root = tempfile::tempdir().unwrap();
    let settings = SettingsService::in_memory_for_tests(root.path().to_owned(), &api.base);
    for (mode, expected) in [
        ("invalid", "invalid_key"),
        ("unavailable", "catalog_unavailable"),
        ("server", "catalog_unavailable"),
        ("denied", "image_access_denied"),
    ] {
        *api.state.mode.lock().unwrap() = mode.into();
        assert_eq!(code(settings.validate(KEY_ONE.into()).await), expected);
    }
    for (mode, expected) in [
        ("", ValidationState::Ready),
        ("balance", ValidationState::SubmissionRestricted),
        ("empty", ValidationState::NoModels),
        ("no-access", ValidationState::NoImageAccess),
    ] {
        *api.state.mode.lock().unwrap() = mode.into();
        let value = settings.validate(KEY_ONE.into()).await.unwrap();
        assert_eq!(value.summary().status, expected);
        if mode == "balance" {
            assert_eq!(
                value.summary().reason_code.as_deref(),
                Some("insufficient_balance")
            );
            settings.save(&value, prefs("sample-fast")).unwrap();
            assert_eq!(settings.status().unwrap().availability, "available");
        }
        let safe = serde_json::to_string(value.summary()).unwrap();
        assert!(!safe.contains(KEY_ONE));
    }
    assert_eq!(api.state.gets.load(Ordering::SeqCst), 8);
    assert_eq!(api.state.posts.load(Ordering::SeqCst), 0);
}

#[test]
#[ignore = "child-only explicit owned fixture"]
fn environment_child_helper() {
    struct RefuseStore;
    impl SecretStore for RefuseStore {
        fn get(&self) -> Result<Option<Zeroizing<Vec<u8>>>> {
            panic!("environment override must not access store");
        }
        fn set(&self, _: &[u8]) -> Result<()> {
            panic!("no import")
        }
        fn delete(&self) -> Result<()> {
            panic!("no delete")
        }
    }
    let root = tempfile::tempdir().unwrap();
    let settings = service(root.path(), Arc::new(RefuseStore));
    fs::write(
        settings.image_preferences_path(),
        b"{deliberately invalid saved defaults",
    )
    .unwrap();
    let context = settings.snapshot().unwrap();
    assert!(context.preferences.is_none());
    assert!(context.image_preferences.is_none());
    assert_eq!(context.source, CredentialSource::Environment);
    if std::env::var("TOKEMULA_IMAGE_KEY").unwrap().is_empty() {
        assert!(context.api_key.is_none());
    } else {
        assert!(context.api_key.as_deref() == Some(KEY_TWO));
    }
    assert!(!settings.settings_path().exists());
}
#[test]
fn environment_override_is_explicit_including_empty_and_never_imports() {
    for value in ["", KEY_TWO] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "settings::tests::environment_child_helper",
            ])
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("TOKEMULA_IMAGE_KEY", value)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "owned environment child failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"));
        assert!(
            !output
                .stdout
                .windows(KEY_TWO.len())
                .any(|v| v == KEY_TWO.as_bytes())
        );
    }
}

#[test]
#[ignore = "child-only native fixture; synthetic keys are fixed in test code"]
fn native_process_helper() {
    let phase = std::env::var("D1_NATIVE_PHASE").expect("explicit phase required");
    let root = PathBuf::from(std::env::var("D1_NATIVE_ROOT").unwrap());
    let namespace = std::env::var("D1_NATIVE_NAMESPACE").unwrap();
    let base = std::env::var("D1_NATIVE_BASE").unwrap();
    assert!(namespace.starts_with("tokemula-image-test-d1-"));
    let settings = SettingsService::for_isolated_tests(root, &namespace, &base).unwrap();
    match phase.as_str() {
        "write-one" | "write-two" => {
            let runtime = tokio::runtime::Runtime::new().unwrap();
            let key = if phase == "write-one" {
                KEY_ONE
            } else {
                KEY_TWO
            };
            let preferred = if phase == "write-one" {
                "sample-fast"
            } else {
                "sample-26"
            };
            let value = runtime.block_on(settings.validate(key.into())).unwrap();
            settings.save(&value, prefs(preferred)).unwrap();
        }
        "read-one" | "read-two" => {
            let context = settings.snapshot().unwrap();
            let key = if phase == "read-one" {
                KEY_ONE
            } else {
                KEY_TWO
            };
            let preferred = if phase == "read-one" {
                "sample-fast"
            } else {
                "sample-26"
            };
            assert!(context.api_key.as_deref() == Some(key));
            assert_eq!(context.preferences, Some(prefs(preferred)));
            assert_eq!(context.source, CredentialSource::SystemStore);
        }
        "logout" => {
            settings.logout().unwrap();
        }
        "read-deleted" => {
            let context = settings.snapshot().unwrap();
            assert!(context.api_key.is_none());
            assert_eq!(context.preferences, Some(prefs("sample-26")));
            assert_eq!(context.source, CredentialSource::None);
        }
        _ => panic!("unknown fixture phase"),
    }
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
#[ignore = "requires explicit D1_NATIVE_STORE_TEST=1; uses only a fresh synthetic system entry"]
async fn native_system_store_cross_process_write_read_update_delete() {
    assert_eq!(std::env::var("D1_NATIVE_STORE_TEST").as_deref(), Ok("1"));
    assert!(std::env::var_os("TOKEMULA_IMAGE_KEY").is_none());
    let root = tempfile::tempdir().unwrap();
    let login_home = PathBuf::from(
        std::env::var_os("D1_NATIVE_LOGIN_HOME")
            .expect("explicit native keychain login HOME required"),
    );
    assert!(login_home.is_absolute());
    let namespace = format!(
        "tokemula-image-test-d1-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    struct Cleanup {
        service: String,
    }
    impl Drop for Cleanup {
        fn drop(&mut self) {
            let store = NativeStore {
                service: self.service.clone(),
                account: "synthetic-test-key".into(),
            };
            let _ = store.delete();
        }
    }
    let cleanup = Cleanup {
        service: namespace.clone(),
    };
    let store = NativeStore {
        service: namespace.clone(),
        account: "synthetic-test-key".into(),
    };
    assert!(store.get().unwrap().is_none(), "new namespace only");
    let api = Api::start().await;
    for phase in [
        "write-one",
        "read-one",
        "write-two",
        "read-two",
        "logout",
        "read-deleted",
    ] {
        let output = Command::new(std::env::current_exe().unwrap())
            .args([
                "--ignored",
                "--exact",
                "settings::tests::native_process_helper",
            ])
            .env_clear()
            .env("HOME", &login_home)
            .env("XDG_CONFIG_HOME", root.path().join("xdg-config"))
            .env("XDG_DATA_HOME", root.path().join("xdg-data"))
            .env("XDG_STATE_HOME", root.path().join("xdg-state"))
            .env("XDG_CACHE_HOME", root.path().join("xdg-cache"))
            .env("PATH", "/usr/bin:/bin")
            .env("D1_NATIVE_PHASE", phase)
            .env("D1_NATIVE_ROOT", root.path())
            .env("D1_NATIVE_NAMESPACE", &namespace)
            .env("D1_NATIVE_BASE", &api.base)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "native fixture phase {phase} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(String::from_utf8_lossy(&output.stdout).contains("test result: ok. 1 passed"));
        assert!(
            !output
                .stdout
                .windows(KEY_ONE.len())
                .any(|v| v == KEY_ONE.as_bytes())
        );
        assert!(
            !output
                .stdout
                .windows(KEY_TWO.len())
                .any(|v| v == KEY_TWO.as_bytes())
        );
    }
    assert!(store.get().unwrap().is_none());
    assert_eq!(api.state.gets.load(Ordering::SeqCst), 2);
    assert_eq!(api.state.posts.load(Ordering::SeqCst), 0);
    if let Ok(path) = std::env::var("D1_NATIVE_EVIDENCE_OUTPUT") {
        fs::write(path, serde_json::to_vec_pretty(&json!({"service":namespace,"account":"synthetic-test-key","phases":6,"catalog_gets":2,"image_posts":0,"entry_deleted":true,"native_login_home_explicit":true,"configuration_root_isolated":true})).unwrap()).unwrap();
    }
    drop(cleanup);
}

fn image_prefs(size: &str) -> crate::image_parameters::ImagePreferences {
    crate::image_parameters::ImagePreferences {
        version: 1,
        profiles: vec![crate::image_parameters::ImagePreference {
            operation: crate::Operation::Generation,
            mode: crate::ExecutionMode::Sync,
            size: Some(size.into()),
            quality: Some("high".into()),
        }],
    }
}
fn image_validated(key: &str) -> ValidatedConfiguration {
    let mut value = validated(key);
    for model in &mut value.summary.models {
        model.capabilities = vec![
            json!({"operation":"generation","mode":"sync","parameters":{"max_images":1,"sizes":["1024x1024","1536x1024"],"qualities":["low","high"],"complexities":["low","medium","high"]}}),
        ];
    }
    value
}
#[test]
fn d4_image_preferences_are_independent_versioned_and_use_managed_paths() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let data = crate::distribution::DataPaths {
        tasks: root.path().join("state/tasks"),
        cache: root.path().join("cache"),
    };
    let settings = service(&root.path().join("settings"), store.clone()).with_data_paths(&data);
    assert!(settings.read_image_preferences().unwrap().is_none());
    let defaults = image_prefs("1536x1024");
    settings
        .save_configuration(
            &image_validated(KEY_ONE),
            prefs("sample-fast"),
            Some(defaults.clone()),
        )
        .unwrap();
    assert_eq!(
        settings.image_preferences_path(),
        root.path().join("state/image-preferences.json")
    );
    assert_eq!(
        settings.snapshot().unwrap().image_preferences,
        Some(defaults.clone())
    );
    let envelope = store.value.lock().unwrap().clone().unwrap();
    let envelope_json: Value = serde_json::from_slice(&envelope).unwrap();
    assert_eq!(
        envelope_json
            .as_object()
            .unwrap()
            .keys()
            .cloned()
            .collect::<Vec<_>>(),
        ["api_key", "preferences", "version"]
    );
    let model_json: Value =
        serde_json::from_slice(&fs::read(settings.settings_path()).unwrap()).unwrap();
    assert_eq!(model_json.as_object().unwrap().len(), 3);
    assert!(
        !fs::read_to_string(settings.image_preferences_path())
            .unwrap()
            .contains(KEY_ONE)
    );
    settings.logout().unwrap();
    assert_eq!(settings.read_image_preferences().unwrap(), Some(defaults));
    assert_eq!(
        settings.status().unwrap().image_preferences_status,
        "saved_without_credential"
    );
    fs::write(root.path().join("state/user-note.json"), b"user").unwrap();
    settings.purge().unwrap();
    assert!(!settings.image_preferences_path().exists());
    assert!(root.path().join("state/user-note.json").exists());
}
#[test]
fn d4_invalid_preferences_and_invalid_save_preserve_all_originals() {
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let settings = service(root.path(), store.clone());
    settings
        .save_configuration(
            &image_validated(KEY_ONE),
            prefs("sample-fast"),
            Some(image_prefs("1024x1024")),
        )
        .unwrap();
    let old_key = store.value.lock().unwrap().clone();
    let old_settings = fs::read(settings.settings_path()).unwrap();
    let old_image = fs::read(settings.image_preferences_path()).unwrap();
    assert_eq!(
        code(settings.save_configuration(
            &image_validated(KEY_TWO),
            prefs("sample-26"),
            Some(image_prefs("999x999"))
        )),
        "invalid_image_preferences"
    );
    assert_eq!(store.value.lock().unwrap().clone(), old_key);
    assert_eq!(fs::read(settings.settings_path()).unwrap(), old_settings);
    assert_eq!(
        fs::read(settings.image_preferences_path()).unwrap(),
        old_image
    );
    fs::write(settings.image_preferences_path(), b"{broken").unwrap();
    assert_eq!(code(settings.snapshot()), "image_preferences_invalid");
    assert_eq!(
        code(settings.save_configuration(
            &image_validated(KEY_TWO),
            prefs("sample-26"),
            Some(image_prefs("1536x1024"))
        )),
        "image_preferences_invalid"
    );
    assert_eq!(
        fs::read(settings.image_preferences_path()).unwrap(),
        b"{broken"
    );
    assert_eq!(store.value.lock().unwrap().clone(), old_key);
    assert_eq!(fs::read(settings.settings_path()).unwrap(), old_settings);
}
#[cfg(unix)]
#[test]
fn d4_image_commit_failure_rolls_back_old_envelope_and_model_file() {
    use std::os::unix::fs::PermissionsExt;
    let root = tempfile::tempdir().unwrap();
    let store = Arc::new(FaultStore::default());
    let data = crate::distribution::DataPaths {
        tasks: root.path().join("state/tasks"),
        cache: root.path().join("cache"),
    };
    let settings = service(&root.path().join("settings"), store.clone()).with_data_paths(&data);
    settings
        .save_configuration(
            &image_validated(KEY_ONE),
            prefs("sample-fast"),
            Some(image_prefs("1024x1024")),
        )
        .unwrap();
    let old_key = store.value.lock().unwrap().clone();
    let old_settings = fs::read(settings.settings_path()).unwrap();
    let old_image = fs::read(settings.image_preferences_path()).unwrap();
    *store.deny_after_set.lock().unwrap() = Some(root.path().join("state"));
    let result = settings.save_configuration(
        &image_validated(KEY_TWO),
        prefs("sample-26"),
        Some(image_prefs("1536x1024")),
    );
    fs::set_permissions(root.path().join("state"), fs::Permissions::from_mode(0o700)).unwrap();
    assert_eq!(code(result), "image_preferences_storage_failed");
    assert_eq!(store.value.lock().unwrap().clone(), old_key);
    assert_eq!(fs::read(settings.settings_path()).unwrap(), old_settings);
    assert_eq!(
        fs::read(settings.image_preferences_path()).unwrap(),
        old_image
    );
}

#[test]
#[ignore = "explicit synthetic compatibility export to an owned root"]
fn d4_export_old_format_compatibility_inputs() {
    let root = PathBuf::from(
        std::env::var_os("D4_SETTINGS_COMPAT_ROOT").expect("owned compatibility root"),
    );
    assert!(
        root.is_absolute()
            && root
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("tokemula-d4-compat-")
    );
    fs::create_dir_all(&root).unwrap();
    let store = Arc::new(FaultStore::default());
    let paths = crate::distribution::DataPaths {
        tasks: root.join("state/tasks"),
        cache: root.join("cache"),
    };
    let settings = service(&root.join("settings"), store.clone()).with_data_paths(&paths);
    settings
        .save_configuration(
            &image_validated(KEY_ONE),
            prefs("sample-fast"),
            Some(image_prefs("1536x1024")),
        )
        .unwrap();
    fs::write(
        root.join("synthetic-envelope.json"),
        store.value.lock().unwrap().as_ref().unwrap(),
    )
    .unwrap();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(
            root.join("synthetic-envelope.json"),
            fs::Permissions::from_mode(0o600),
        )
        .unwrap();
    }
}
