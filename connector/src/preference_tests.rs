//! User model preference acceptance: catalog preflight only, one image submission per operation.
use super::*;
use crate::settings::ModelPreferences;
use std::{collections::HashMap, path::Path};

#[derive(Clone)]
struct PreferencePost {
    path: String,
    fields: Value,
    files: Vec<(String, Vec<u8>)>,
}
struct PreferenceState {
    common: Arc<FixtureState>,
    catalog_status: Mutex<StatusCode>,
    posts: Mutex<Vec<PreferencePost>>,
    batches: Mutex<HashMap<String, Value>>,
    creates: AtomicUsize,
    tasks: AtomicUsize,
}
struct PreferenceFixture {
    state: Arc<PreferenceState>,
    base: String,
    server: JoinHandle<()>,
}
impl Drop for PreferenceFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn preferences(default: &str, fallbacks: &[&str], auto: bool) -> ModelPreferences {
    ModelPreferences {
        default_model: default.into(),
        fallback_models: fallbacks.iter().map(|id| (*id).into()).collect(),
        auto_fallback: auto,
    }
}
fn preference_catalog() -> Value {
    let model = |id: &str, rank: u32| {
        let mut capabilities = Vec::new();
        for mode in ["sync", "single_async", "batch"] {
            for operation in ["generation", "edit"] {
                let edit = operation == "edit";
                let mut capability = json!({"operation":operation,"mode":mode,
                    "parameters":{"max_images":4,"sizes":if mode=="batch"{vec!["1K"]}else{vec!["1024x1024"]},"qualities":["high"]},
                    "input_limits":{"max_request_bytes":268435456,"max_images":if edit{3}else{0},
                        "max_image_bytes":if edit{20971520}else{0},"mime_types":if edit{vec!["image/png"]}else{vec![]}},
                    "result_formats":if mode=="batch"{vec!["authenticated_download"]}else{vec!["b64_json"]},"cancel_supported":mode=="batch"});
                if mode == "batch" {
                    capability["input_limits"]["max_prompt_bytes"] = json!(1000);
                    capability["input_limits"]["max_batch_items"] = json!(10);
                    capability["input_limits"]["max_batch_output_images"] = json!(20);
                } else if edit {
                    capability["edit_options"] = json!({"backgrounds":["transparent","opaque"],
                        "output_formats":["png","webp"],"input_fidelities":["high"],
                        "transparent_output_formats":["png","webp"],
                        "mask":{"max_images":1,"max_image_bytes":4194303,"mime_types":["image/png"],
                            "requires_alpha":true,"same_dimensions_as_first_image":true,"applies_to_image":0}});
                }
                capabilities.push(capability);
            }
        }
        json!({"id":id,"display_name":format!("Synthetic {id}"),"preference_rank":rank,
            "availability":"configured","capabilities":capabilities,"pricing":{"status":"unknown"}})
    };
    json!({"schema_version":"1","observed_at":"2026-11-20T00:00:00Z","can_submit":true,
        "models":[model("ranked",0),model("preferred",50),model("backup-a",1),model("backup-b",2)]})
}
fn batch_response(model: &Value) -> Value {
    json!({"id":"batch_preference","object":"image.batch","task_name":"Synthetic preference",
        "status":"queued","model":model,"provider":"gemini","item_count":2,"success_count":0,"fail_count":0,
        "ledger_unit":"CNY","estimated_cost":0.25,"hold_amount":0.25,"actual_cost":null,
        "created_at":1795109250,"submitted_at":1795109251,"settled_at":null,
        "output_expires_at":null,"output_deleted_at":null})
}
impl PreferenceFixture {
    async fn new() -> Self {
        let common = Arc::new(FixtureState {
            catalog: Mutex::new(preference_catalog()),
            mode: Mutex::new(Mode::Image),
            fee: Mutex::new((
                StatusCode::OK,
                serde_json::from_str(include_str!("../tests/fixtures/http-billing.json")).unwrap(),
            )),
            request_ref: Mutex::new("fixture-request".into()),
            posts: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            fees: AtomicUsize::new(0),
            records: Mutex::new(vec![]),
            png: png(),
            post_seen: tokio::sync::Notify::new(),
        });
        let state = Arc::new(PreferenceState {
            common: common.clone(),
            catalog_status: Mutex::new(StatusCode::OK),
            posts: Mutex::new(vec![]),
            batches: Mutex::new(HashMap::new()),
            creates: AtomicUsize::new(0),
            tasks: AtomicUsize::new(0),
        });
        let router = Router::new()
            .route(
                "/v1/images/requests/{request_ref}/billing",
                get(fee_handler),
            )
            .with_state(common)
            .merge(
                Router::new()
                    .fallback(preference_handler)
                    .with_state(state.clone()),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            state,
            base,
            server,
        }
    }
    fn client(&self, pref: ModelPreferences, timeout: Duration) -> ImageClient {
        let mut client = ImageClient::build(&self.base, Some(KEY.into()), timeout).unwrap();
        client.preferences = Some(pref);
        client
    }
    fn count(&self) -> usize {
        self.state.common.posts.load(Ordering::SeqCst)
    }
    fn last(&self) -> PreferencePost {
        self.state.posts.lock().unwrap().last().unwrap().clone()
    }
    fn change_model(&self, id: &str, update: impl FnOnce(&mut Value)) {
        let mut catalog = self.state.common.catalog.lock().unwrap();
        update(
            catalog["models"]
                .as_array_mut()
                .unwrap()
                .iter_mut()
                .find(|m| m["id"] == id)
                .unwrap(),
        );
    }
}
async fn preference_handler(
    State(state): State<Arc<PreferenceState>>,
    request: Request,
) -> AxumResponse {
    use axum::extract::FromRequest;
    assert!(auth_ok(&request));
    let path = request.uri().path().to_owned();
    if request.method() == "GET" {
        if path == "/v1/images/models" {
            state.common.gets.fetch_add(1, Ordering::SeqCst);
            return (
                *state.catalog_status.lock().unwrap(),
                axum::Json(state.common.catalog.lock().unwrap().clone()),
            )
                .into_response();
        }
        state.tasks.fetch_add(1, Ordering::SeqCst);
        if path == "/v1/images/tasks/imgtask_preference" {
            return axum::Json(json!({"id":"imgtask_preference","task_id":"imgtask_preference","status":"processing",
                "request_ref":"fixture-request","created_at":1795156000,"expires_at":1795242400,"result_expires_at":null})).into_response();
        }
        if path.ends_with("/items") {
            return axum::Json(json!({"object":"list","data":[],"has_more":false})).into_response();
        }
        return axum::Json(batch_response(&json!("preferred"))).into_response();
    }
    let key = request
        .headers()
        .get("Idempotency-Key")
        .and_then(|v| v.to_str().ok())
        .map(str::to_owned);
    let multipart = request.headers()["content-type"]
        .to_str()
        .unwrap()
        .starts_with("multipart/");
    let mut record = PreferencePost {
        path: path.clone(),
        fields: json!({}),
        files: vec![],
    };
    if multipart {
        let mut upload = Multipart::from_request(request, &()).await.unwrap();
        while let Some(field) = upload.next_field().await.unwrap() {
            let name = field.name().unwrap().to_owned();
            if field.file_name().is_some() {
                record
                    .files
                    .push((name, field.bytes().await.unwrap().to_vec()));
            } else {
                record.fields[&name] = json!(field.text().await.unwrap());
            }
        }
    } else {
        let body = to_bytes(request.into_body(), MAX_METADATA_RESPONSE_BYTES)
            .await
            .unwrap();
        record.fields = serde_json::from_slice(&body).unwrap();
    }
    state.posts.lock().unwrap().push(record.clone());
    if path == "/v1/images/batches" {
        state.common.posts.fetch_add(1, Ordering::SeqCst);
        let key = key.unwrap();
        let mut batches = state.batches.lock().unwrap();
        if let Some(previous) = batches.get(&key) {
            if previous != &record.fields {
                return (
                    StatusCode::CONFLICT,
                    axum::Json(json!({"error":{"code":"BATCH_IMAGE_IDEMPOTENCY_CONFLICT"}})),
                )
                    .into_response();
            }
        } else {
            batches.insert(key, record.fields.clone());
            state.creates.fetch_add(1, Ordering::SeqCst);
        }
        let mut response = batch_response(&record.fields["model"]);
        response["task_name"] = record.fields["task_name"].clone();
        return axum::Json(response).into_response();
    }
    if path.ends_with("/async") {
        state.common.posts.fetch_add(1, Ordering::SeqCst);
        return (
            StatusCode::ACCEPTED,
            [("x-client-request-id", "fixture-request")],
            axum::Json(json!({"id":"imgtask_preference",
            "task_id":"imgtask_preference","request_ref":"fixture-request","status":"processing",
            "created_at":1795156000,"expires_at":1795242400,"result_expires_at":null})),
        )
            .into_response();
    }
    respond(&state.common).await
}
fn assert_selected(result: &CallToolResult, model: &str, fallback: bool) {
    assert_ne!(result.is_error, Some(true), "{:?}", metadata(result));
    assert_eq!(metadata(result)["selected_model"], model);
    assert_eq!(metadata(result)["model_selection"]["selected_model"], model);
    assert_eq!(
        metadata(result)["model_selection"]["used_fallback"],
        fallback
    );
}
fn local_image(root: &Path, name: &str, alpha: bool) -> std::path::PathBuf {
    let image = if alpha {
        image::DynamicImage::new_rgba8(2, 2)
    } else {
        image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            2,
            2,
            image::Rgb(if name.starts_with("second") {
                [80, 40, 10]
            } else {
                [10, 40, 80]
            }),
        ))
    };
    let path = root.join(name);
    image.save_with_format(&path, ImageFormat::Png).unwrap();
    path
}

#[tokio::test]
async fn default_preference_beats_catalog_rank_and_fallback_is_per_operation() {
    let fixture = PreferenceFixture::new().await;
    let pref = preferences("preferred", &["backup-b", "backup-a"], true);
    let (client, task) = connect(fixture.client(pref.clone(), Duration::from_secs(2)), true).await;
    let first = call(&client, "generate_image", json!({"prompt":"primary"})).await;
    assert_selected(&first, "preferred", false);
    assert_image(&first);
    assert_eq!(metadata(&first)["effective_model"], "fixture-effective");
    fixture.change_model("preferred", |m| {
        m["availability"] = json!("temporarily_unavailable")
    });
    let fallback = call(&client, "generate_image", json!({"prompt":"fallback"})).await;
    assert_selected(&fallback, "backup-b", true);
    assert!(metadata(&fallback)["model_selection"]["reason_code"].is_string());
    fixture.change_model("preferred", |m| m["availability"] = json!("configured"));
    let again = call(&client, "generate_image", json!({"prompt":"primary again"})).await;
    assert_selected(&again, "preferred", false);
    assert_eq!(fixture.count(), 3);
    assert_eq!(fixture.state.common.gets.load(Ordering::SeqCst), 3);
    assert_eq!(fixture.state.common.fees.load(Ordering::SeqCst), 0);
    close(client, task).await;
}

#[tokio::test]
async fn ordered_fallback_never_escapes_the_confirmed_chain_and_explicit_model_wins() {
    for (pref, explicit, expected) in [
        (
            preferences("missing", &["backup-b", "backup-a"], true),
            None,
            Some("backup-b"),
        ),
        (preferences("missing", &["backup-b"], false), None, None),
        (preferences("missing", &["also-missing"], true), None, None),
        (
            preferences("missing", &["backup-b"], false),
            Some("ranked"),
            Some("ranked"),
        ),
        (
            preferences("preferred", &["backup-b"], true),
            Some("missing"),
            None,
        ),
    ] {
        let fixture = PreferenceFixture::new().await;
        let (client, task) = connect(fixture.client(pref, Duration::from_secs(2)), false).await;
        let mut input = json!({"prompt":"ordered"});
        if let Some(id) = explicit {
            input["model"] = json!(id);
        }
        let result = call(&client, "generate_image", input).await;
        match expected {
            Some(id) => {
                assert_ne!(result.is_error, Some(true), "{:?}", metadata(&result));
                assert_eq!(metadata(&result)["selected_model"], id);
                assert_eq!(fixture.last().fields["model"], id);
                assert_eq!(fixture.count(), 1);
            }
            None => {
                assert_eq!(result.is_error, Some(true));
                assert_eq!(fixture.count(), 0);
            }
        }
        close(client, task).await;
    }
}

#[tokio::test]
async fn asynchronous_generation_selects_fallback_once_and_returns_without_polling() {
    let fixture = PreferenceFixture::new().await;
    fixture.change_model("preferred", |m| {
        m["capabilities"]
            .as_array_mut()
            .unwrap()
            .retain(|c| c["mode"] != "single_async")
    });
    let (client, task) = connect(
        fixture.client(
            preferences("preferred", &["backup-a"], true),
            Duration::from_secs(2),
        ),
        true,
    )
    .await;
    let result = call(
        &client,
        "generate_image",
        json!({"prompt":"async","mode":"single_async","n":2}),
    )
    .await;
    assert_selected(&result, "backup-a", true);
    assert_eq!(metadata(&result)["submission"], "accepted");
    assert_eq!(metadata(&result)["request_ref"], "fixture-request");
    assert_eq!(fixture.last().path, "/v1/images/generations/async");
    assert_eq!(fixture.last().fields["n"], 2);
    assert_eq!(fixture.count(), 1);
    assert_eq!(fixture.state.tasks.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.common.fees.load(Ordering::SeqCst), 0);
    close(client, task).await;
}

#[tokio::test]
async fn complete_edit_requirements_determine_fallback_without_dropping_inputs_or_mask() {
    for asynchronous in [false, true] {
        let fixture = PreferenceFixture::new().await;
        fixture.change_model("preferred", |m| {
            for cap in m["capabilities"].as_array_mut().unwrap() {
                if cap["operation"] == "edit" {
                    cap.as_object_mut().unwrap().remove("edit_options");
                    cap["input_limits"]["max_images"] = json!(1);
                }
            }
        });
        let dir = tempfile::tempdir().unwrap();
        let first = local_image(dir.path(), "first.png", false);
        let second = local_image(dir.path(), "second.png", false);
        let mask = local_image(dir.path(), "mask.png", true);
        let expected_files = vec![
            ("image[]".to_owned(), std::fs::read(&first).unwrap()),
            ("image[]".to_owned(), std::fs::read(&second).unwrap()),
            ("mask".to_owned(), std::fs::read(&mask).unwrap()),
        ];
        let (client, task) = connect(
            fixture.client(
                preferences("preferred", &["backup-b"], true),
                Duration::from_secs(2),
            ),
            false,
        )
        .await;
        let mut input = json!({"prompt":"edit full","image_paths":[first,second],"mask_path":mask,"background":"transparent","output_format":"webp","input_fidelity":"high","n":2});
        if asynchronous {
            input["mode"] = json!("single_async");
        }
        let result = call(&client, "edit_image", input).await;
        assert_selected(&result, "backup-b", true);
        let post = fixture.last();
        assert_eq!(post.files, expected_files);
        assert_eq!(post.fields["n"], "2");
        assert_eq!(post.fields["background"], "transparent");
        assert_eq!(post.fields["output_format"], "webp");
        assert_eq!(post.fields["input_fidelity"], "high");
        assert_eq!(fixture.count(), 1);
        close(client, task).await;
    }
}

#[tokio::test]
async fn batch_replay_retains_first_model_after_user_preferences_change() {
    let fixture = PreferenceFixture::new().await;
    fixture.change_model("preferred", |m| {
        m["capabilities"]
            .as_array_mut()
            .unwrap()
            .retain(|c| !(c["operation"] == "edit" && c["mode"] == "batch"))
    });
    let dir = tempfile::tempdir().unwrap();
    let reference = local_image(dir.path(), "batch.png", false);
    let input = json!({"idempotency_key":"stable-preference","items":[{"custom_id":"generation","prompt":"gen","output_count":2},{"custom_id":"edit","prompt":"edit","reference_image_paths":[reference]}]});
    let mut client = fixture.client(
        preferences("preferred", &["backup-a"], true),
        Duration::from_secs(2),
    );
    client.task_store = Some(TaskStore {
        root: dir.path().join("tasks"),
    });
    let first = client
        .submit_batch(
            &serde_json::from_value(input.clone()).unwrap(),
            &BatchProgress::submission(),
        )
        .await
        .unwrap();
    assert_eq!(first.metadata["selected_model"], "backup-a");
    assert_eq!(first.metadata["model_selection"]["used_fallback"], true);
    let original = fixture.last().fields;
    client.preferences = Some(preferences("ranked", &["backup-b"], true));
    let replay = client
        .submit_batch(
            &serde_json::from_value(input).unwrap(),
            &BatchProgress::submission(),
        )
        .await
        .unwrap();
    assert_eq!(replay.metadata["selected_model"], "backup-a");
    assert_eq!(fixture.last().fields, original);
    assert_eq!(fixture.count(), 2);
    assert_eq!(fixture.state.creates.load(Ordering::SeqCst), 1);
    assert_eq!(original["items"].as_array().unwrap().len(), 2);
    assert_eq!(original["items"][0]["output_count"], 2);
    assert_eq!(
        original["items"][1]["reference_images"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[tokio::test]
async fn admission_and_catalog_failures_do_not_try_fallback_models_or_post() {
    for case in ["balance", "invalid", "unavailable"] {
        let fixture = PreferenceFixture::new().await;
        {
            let mut catalog = fixture.state.common.catalog.lock().unwrap();
            match case {
                "balance" => {
                    catalog["can_submit"] = json!(false);
                    catalog["reason_code"] = json!("insufficient_balance");
                }
                "invalid" => {
                    catalog["schema_version"] = json!("bad");
                }
                _ => {
                    *fixture.state.catalog_status.lock().unwrap() = StatusCode::SERVICE_UNAVAILABLE;
                }
            }
        }
        let (client, task) = connect(
            fixture.client(
                preferences("missing", &["backup-a", "backup-b"], true),
                Duration::from_secs(2),
            ),
            false,
        )
        .await;
        let result = call(&client, "generate_image", json!({"prompt":"must not post"})).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(fixture.count(), 0);
        assert_eq!(fixture.state.common.gets.load(Ordering::SeqCst), 1);
        close(client, task).await;
    }
}

#[tokio::test]
async fn post_timeout_never_changes_model_and_history_queries_ignore_preferences() {
    let fixture = PreferenceFixture::new().await;
    *fixture.state.common.mode.lock().unwrap() = Mode::Delay;
    let (client, task) = connect(
        fixture.client(
            preferences("missing", &["backup-a", "backup-b"], true),
            Duration::from_millis(50),
        ),
        true,
    )
    .await;
    let result = call(&client, "generate_image", json!({"prompt":"one post only"})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(fixture.count(), 1);
    assert_eq!(fixture.last().fields["model"], "backup-a");
    fixture.state.common.catalog.lock().unwrap()["can_submit"] = json!(false);
    let queries_before = fixture.state.common.gets.load(Ordering::SeqCst);
    let history = call(
        &client,
        "get_image_task",
        json!({"task_id":"imgtask_preference"}),
    )
    .await;
    assert_ne!(history.is_error, Some(true), "{:?}", metadata(&history));
    assert_eq!(metadata(&history)["status"], "processing");
    assert_eq!(fixture.state.common.fees.load(Ordering::SeqCst), 0);
    assert_eq!(
        fixture.state.common.gets.load(Ordering::SeqCst),
        queries_before
    );
    assert_eq!(fixture.count(), 1);
    assert_eq!(fixture.state.tasks.load(Ordering::SeqCst), 1);
    close(client, task).await;
}

#[tokio::test]
async fn fallback_uses_complete_encoded_request_and_skips_incompatible_backups() {
    for operation in ["generation", "edit", "batch"] {
        let fixture = PreferenceFixture::new().await;
        fixture.change_model("preferred", |model| {
            for cap in model["capabilities"].as_array_mut().unwrap() {
                cap["input_limits"]["max_request_bytes"] = json!(1);
            }
        });
        fixture.change_model("backup-a", |model| {
            for cap in model["capabilities"].as_array_mut().unwrap() {
                cap["parameters"]["max_images"] = json!(1);
            }
        });
        let directory = tempfile::tempdir().unwrap();
        let image = local_image(directory.path(), "bounded.png", false);
        let (tool, input) = match operation {
            "generation" => ("generate_image", json!({"prompt":"complete","n":2})),
            "edit" => (
                "edit_image",
                json!({"prompt":"complete","n":2,"image_path":image}),
            ),
            _ => (
                "submit_image_batch",
                json!({"items":[{"custom_id":"bounded","prompt":"complete","output_count":2}]}),
            ),
        };
        let (client, task) = connect(
            fixture.client(
                preferences("preferred", &["backup-a", "backup-b"], true),
                Duration::from_secs(2),
            ),
            true,
        )
        .await;
        let result = call(&client, tool, input).await;
        assert_selected(&result, "backup-b", true);
        assert_eq!(fixture.count(), 1);
        let fields = fixture.last().fields;
        if operation == "batch" {
            assert_eq!(fields["items"][0]["output_count"], 2);
        } else if operation == "edit" {
            assert_eq!(fields["n"], "2");
        } else {
            assert_eq!(fields["n"], 2);
        }
        close(client, task).await;
    }
}

#[path = "image_parameter_tests.rs"]
mod parameters;
