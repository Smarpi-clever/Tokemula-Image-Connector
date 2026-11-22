use super::*;
use std::path::PathBuf;

const TASK_ID: &str = "imgtask_synthetic_b3a";
const ORIGINAL_REF: &str = "fixture-request";
const QUERY_REF: &str = "separate-task-query-reference";

#[derive(Clone)]
enum TaskReply {
    Json(StatusCode, Value),
    Disconnect,
    Delay,
}

struct TaskState {
    reply: Mutex<TaskReply>,
    gets: AtomicUsize,
    paths: Mutex<Vec<String>>,
    download_auth: Mutex<Vec<bool>>,
    slow_download: AtomicBool,
    download_seen: tokio::sync::Notify,
    png: Vec<u8>,
}

struct TaskFixture {
    common: Arc<FixtureState>,
    task_state: Arc<TaskState>,
    base: String,
    server: JoinHandle<()>,
}

impl Drop for TaskFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn task_json(status: &str) -> Value {
    json!({
        "id":TASK_ID, "task_id":TASK_ID, "request_ref":ORIGINAL_REF,
        "object":"image.generation.task", "status":status,
        "created_at":1795109250u64, "expires_at":1795195650u64,
        "result_expires_at":null
    })
}

fn accepted_json() -> Value {
    let mut value = task_json("processing");
    value["poll_url"] = json!(format!("/v1/images/tasks/{TASK_ID}"));
    value
}

fn settled_fee(reference: &str) -> Value {
    json!({"schema_version":"1","request_ref":reference,"status":"settled",
        "visibility":"visible","settlement_source":"balance_debit",
        "amount":{"ledger_amount":"0.00007813","ledger_unit":"CNY"}})
}

impl TaskFixture {
    async fn new() -> Self {
        let mut catalog: Value =
            serde_json::from_str(include_str!("../tests/fixtures/http-catalog.json")).unwrap();
        let mut generation = catalog["models"][1]["capabilities"][0].clone();
        generation["mode"] = json!("single_async");
        catalog["models"][1]["capabilities"]
            .as_array_mut()
            .unwrap()
            .push(generation);
        let common = Arc::new(FixtureState {
            catalog: Mutex::new(catalog),
            mode: Mutex::new(Mode::Error(StatusCode::ACCEPTED, accepted_json())),
            fee: Mutex::new((StatusCode::OK, settled_fee(ORIGINAL_REF))),
            request_ref: Mutex::new(ORIGINAL_REF.into()),
            posts: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            fees: AtomicUsize::new(0),
            records: Mutex::new(Vec::new()),
            png: png(),
            post_seen: tokio::sync::Notify::new(),
        });
        let task_state = Arc::new(TaskState {
            reply: Mutex::new(TaskReply::Json(StatusCode::OK, task_json("processing"))),
            gets: AtomicUsize::new(0),
            paths: Mutex::new(Vec::new()),
            download_auth: Mutex::new(Vec::new()),
            slow_download: AtomicBool::new(false),
            download_seen: tokio::sync::Notify::new(),
            png: png(),
        });
        let router = Router::new()
            .route("/v1/images/models", get(catalog_handler))
            .route("/v1/images/generations", post(generation_handler))
            .route("/v1/images/edits", post(edit_handler))
            .route("/v1/images/generations/async", post(generation_handler))
            .route("/v1/images/edits/async", post(edit_handler))
            .route(
                "/v1/images/requests/{request_ref}/billing",
                get(fee_handler),
            )
            .with_state(common.clone())
            .merge(
                Router::new()
                    .route("/v1/images/tasks/{task_id}", get(task_handler))
                    .route("/synthetic-image", get(task_image_handler))
                    .with_state(task_state.clone()),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            common,
            task_state,
            base,
            server,
        }
    }

    fn client(&self, timeout: Duration) -> ImageClient {
        ImageClient::build(&self.base, Some(KEY.into()), timeout).unwrap()
    }

    fn completed(&self) -> Value {
        let mut value = task_json("completed");
        value["completed_at"] = json!(1795109251u64);
        value["result"] = json!({"data":[{"b64_json":STANDARD.encode(&self.common.png)}]});
        value
    }

    fn set_task(&self, value: Value) {
        *self.task_state.reply.lock().unwrap() = TaskReply::Json(StatusCode::OK, value);
    }

    fn counts(&self) -> (usize, usize, usize, usize) {
        (
            self.common.posts.load(Ordering::SeqCst),
            self.common.gets.load(Ordering::SeqCst),
            self.task_state.gets.load(Ordering::SeqCst),
            self.common.fees.load(Ordering::SeqCst),
        )
    }

    async fn wait_for_post(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notified = self.common.post_seen.notified();
                if self.common.posts.load(Ordering::SeqCst) > 0 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }

    fn download_url(&self) -> String {
        Url::parse(&self.base)
            .unwrap()
            .join("/synthetic-image")
            .unwrap()
            .to_string()
    }
}

async fn task_handler(State(state): State<Arc<TaskState>>, request: Request) -> AxumResponse {
    assert!(auth_ok(&request));
    state.gets.fetch_add(1, Ordering::SeqCst);
    state
        .paths
        .lock()
        .unwrap()
        .push(request.uri().path().into());
    let reply = state.reply.lock().unwrap().clone();
    match reply {
        TaskReply::Json(status, value) => (
            status,
            [("x-client-request-id", QUERY_REF)],
            axum::Json(value),
        )
            .into_response(),
        TaskReply::Delay => {
            tokio::time::sleep(Duration::from_millis(500)).await;
            (
                [("x-client-request-id", QUERY_REF)],
                axum::Json(task_json("processing")),
            )
                .into_response()
        }
        TaskReply::Disconnect => AxumResponse::builder()
            .header("x-client-request-id", QUERY_REF)
            .header("content-length", "1000")
            .body(Body::from_stream(futures_util::stream::once(async {
                Err::<Bytes, _>(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "synthetic query reset",
                ))
            })))
            .unwrap(),
    }
}

async fn task_image_handler(State(state): State<Arc<TaskState>>, request: Request) -> AxumResponse {
    state
        .download_auth
        .lock()
        .unwrap()
        .push(request.headers().contains_key("authorization"));
    state.download_seen.notify_one();
    if state.slow_download.load(Ordering::SeqCst) {
        tokio::time::sleep(Duration::from_secs(5)).await;
    }
    ([("content-type", "image/png")], state.png.clone()).into_response()
}

async fn connect_dirs(
    client: ImageClient,
    output_dir: PathBuf,
    task_dir: PathBuf,
    modern: bool,
) -> (RunningService<RoleClient, ClientConfig>, JoinHandle<()>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        ImageMcp::with_directories(client, output_dir, task_dir)
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let version = if modern {
        ProtocolVersion::V_2026_07_28
    } else {
        ProtocolVersion::V_2025_11_25
    };
    let lifecycle = if modern {
        ClientLifecycleMode::Discover {
            preferred_versions: vec![version.clone()],
        }
    } else {
        ClientLifecycleMode::Initialize
    };
    let client = ClientConfig::default()
        .with_protocol_version(version.clone())
        .serve_with_lifecycle(client_io, lifecycle)
        .await
        .unwrap();
    assert_eq!(client.peer_info().unwrap().protocol_version, version);
    (client, server)
}

async fn connect_task(
    fixture: &TaskFixture,
    directory: &tempfile::TempDir,
    modern: bool,
) -> (RunningService<RoleClient, ClientConfig>, JoinHandle<()>) {
    connect_dirs(
        fixture.client(Duration::from_secs(30)),
        directory.path().join("images"),
        directory.path().join("tasks"),
        modern,
    )
    .await
}

fn assert_safe(value: &impl Serialize) {
    let text = serde_json::to_string(value).unwrap();
    for secret in [KEY, SENSITIVE, "Authorization"] {
        assert!(
            !text.contains(secret),
            "diagnostic or recovery record included private data"
        );
    }
}

fn stored_record(directory: &tempfile::TempDir) -> Value {
    let root = directory.path().join("tasks");
    let files: Vec<_> = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1, "one minimal record per task");
    let record: Value = serde_json::from_slice(&std::fs::read(&files[0]).unwrap()).unwrap();
    assert_safe(&record);
    let allowed = [
        "task_id",
        "request_ref",
        "selected_model",
        "status",
        "created_at",
        "completed_at",
        "expires_at",
        "result_expires_at",
        "observed_at",
    ];
    assert!(
        record
            .as_object()
            .unwrap()
            .keys()
            .all(|key| allowed.contains(&key.as_str()))
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(&root).unwrap().permissions().mode() & 0o777,
            0o700
        );
        assert_eq!(
            std::fs::metadata(&files[0]).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    record
}

#[tokio::test]
async fn async_generation_and_edit_submit_once_save_private_reference_and_return_without_polling() {
    for edit in [false, true] {
        let fixture = TaskFixture::new().await;
        let directory = tempfile::tempdir().unwrap();
        let input = directory.path().join("input.png");
        std::fs::write(&input, &fixture.common.png).unwrap();
        let (client, server) = connect_task(&fixture, &directory, edit).await;
        let mut args = json!({"prompt":SENSITIVE,"model":"sample-26","mode":"single_async","n":1});
        if edit {
            args["image_path"] = json!(input);
        }
        let result = call(
            &client,
            if edit { "edit_image" } else { "generate_image" },
            args,
        )
        .await;
        assert_ne!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["submission"], "accepted");
        assert_eq!(metadata(&result)["task_id"], TASK_ID);
        assert_eq!(metadata(&result)["request_ref"], ORIGINAL_REF);
        assert_eq!(metadata(&result)["status"], "processing");
        assert_eq!(metadata(&result)["expires_at"], 1795195650u64);
        assert!(metadata(&result)["result_expires_at"].is_null());
        assert_eq!(metadata(&result)["recovery"]["status"], "saved");
        assert_eq!(fixture.counts(), (1, 1, 0, 0));
        assert_safe(&result);
        let record = stored_record(&directory);
        assert_eq!(record["task_id"], TASK_ID);
        assert_eq!(record["request_ref"], ORIGINAL_REF);
        assert_eq!(record["selected_model"], "sample-26");
        assert!(record["observed_at"].as_u64().is_some());
        {
            let records = fixture.common.records.lock().unwrap();
            let wire = &records[0];
            assert_eq!(
                wire.path,
                if edit {
                    "/v1/images/edits/async"
                } else {
                    "/v1/images/generations/async"
                }
            );
            assert!(wire.auth_ok);
            assert!(wire.fields.get("mode").is_none());
            assert_eq!(wire.fields["prompt"], SENSITIVE);
            if edit {
                assert_eq!(wire.file, fixture.common.png);
            }
        }
        close(client, server).await;
    }
}

#[tokio::test]
async fn async_selection_permissions_and_encoded_limits_reject_before_post() {
    let fixture = TaskFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let image_path = directory.path().join("input.png");
    std::fs::write(&image_path, &fixture.common.png).unwrap();
    let (client, server) = connect_task(&fixture, &directory, false).await;
    let catalog = fixture.common.catalog.lock().unwrap().clone();
    for case in 0..9 {
        *fixture.common.catalog.lock().unwrap() = catalog.clone();
        let mut args = json!({"prompt":"synthetic","model":"sample-26","mode":"single_async"});
        let mut tool = "generate_image";
        match case {
            0 => args["model"] = json!("sample-fast"),
            1 => {
                tool = "edit_image";
                args["model"] = json!("sample-fast");
                args["image_path"] = json!(image_path);
            }
            2 => args["size"] = json!("unsupported-size"),
            3 => args["n"] = json!(0),
            4 => fixture.common.catalog.lock().unwrap()["can_submit"] = json!(false),
            5 => {
                fixture.common.catalog.lock().unwrap()["models"][1]["capabilities"][2]["input_limits"]
                    ["max_request_bytes"] = json!(1)
            }
            6 => {
                tool = "edit_image";
                args["image_path"] = json!(image_path);
                fixture.common.catalog.lock().unwrap()["models"][1]["capabilities"][1]["input_limits"]
                    ["max_request_bytes"] = json!(fixture.common.png.len());
            }
            7 => args["mode"] = json!("batch"),
            _ => {
                tool = "edit_image";
                args["image_path"] = json!("relative.png");
            }
        }
        let result = call(&client, tool, args).await;
        assert_eq!(result.is_error, Some(true), "case {case}");
        assert_eq!(
            metadata(&result)["submission"],
            "not_submitted",
            "case {case}"
        );
        assert_eq!(fixture.common.posts.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.task_state.gets.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.common.fees.load(Ordering::SeqCst), 0);
    }
    assert!(!directory.path().join("tasks").exists());
    close(client, server).await;
}

#[tokio::test]
async fn unwritable_task_store_preserves_accepted_references_and_sync_defaults() {
    for edit in [false, true] {
        let fixture = TaskFixture::new().await;
        let directory = tempfile::tempdir().unwrap();
        let task_dir = directory.path().join("blocked-task-directory");
        std::fs::write(&task_dir, b"synthetic ordinary file").unwrap();
        let image_path = directory.path().join("input.png");
        std::fs::write(&image_path, &fixture.common.png).unwrap();
        let (client, server) = connect_dirs(
            fixture.client(Duration::from_secs(2)),
            directory.path().join("images"),
            task_dir.clone(),
            edit,
        )
        .await;
        let mut args = json!({"prompt":"synthetic","model":"sample-26","mode":"single_async"});
        if edit {
            args["image_path"] = json!(image_path);
        }
        let tool = if edit { "edit_image" } else { "generate_image" };
        let accepted = call(&client, tool, args.clone()).await;
        assert_ne!(accepted.is_error, Some(true));
        assert_eq!(metadata(&accepted)["submission"], "accepted");
        assert_eq!(metadata(&accepted)["task_id"], TASK_ID);
        assert_eq!(metadata(&accepted)["request_ref"], ORIGINAL_REF);
        assert_eq!(metadata(&accepted)["recovery"]["status"], "failed");
        assert!(
            metadata(&accepted)["recovery"]["error"]["code"]
                .as_str()
                .is_some()
        );
        assert_eq!(fixture.counts(), (1, 1, 0, 0));
        assert_eq!(
            std::fs::read(&task_dir).unwrap(),
            b"synthetic ordinary file"
        );
        let processing = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
        assert_ne!(processing.is_error, Some(true));
        assert_eq!(metadata(&processing)["status"], "processing");
        assert_eq!(metadata(&processing)["request_ref"], ORIGINAL_REF);
        assert_eq!(metadata(&processing)["recovery"]["status"], "failed");
        assert_eq!(fixture.counts(), (1, 1, 1, 0));

        *fixture.common.mode.lock().unwrap() = Mode::Image;
        args.as_object_mut().unwrap().remove("mode");
        if edit {
            args["model"] = json!("sample-edit-sync");
        }
        let sync = call(&client, tool, args).await;
        assert_image(&sync);
        assert_eq!(fixture.counts(), (2, 2, 1, 0));
        assert_eq!(
            fixture.common.records.lock().unwrap()[1].path,
            if edit {
                "/v1/images/edits"
            } else {
                "/v1/images/generations"
            }
        );
        close(client, server).await;
    }
}

#[tokio::test]
async fn reloaded_reference_survives_query_progress_completion_expiry_and_image_cleanup() {
    let fixture = TaskFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let (client, server) = connect_task(&fixture, &directory, true).await;
    let result = call(
        &client,
        "generate_image",
        json!({"prompt":"synthetic","mode":"single_async"}),
    )
    .await;
    assert_eq!(metadata(&result)["submission"], "accepted");
    close(client, server).await;
    assert_eq!(fixture.counts(), (1, 1, 0, 0));
    fixture.common.catalog.lock().unwrap()["can_submit"] = json!(false);
    let (client, server) = connect_task(&fixture, &directory, false).await;
    assert_eq!(
        fixture.counts(),
        (1, 1, 0, 0),
        "startup performs no network recovery"
    );
    let processing = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    assert_eq!(metadata(&processing)["status"], "processing");
    assert_eq!(metadata(&processing)["request_ref"], ORIGINAL_REF);
    assert_eq!(fixture.counts(), (1, 1, 1, 0));
    fixture.set_task(fixture.completed());
    let complete = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    assert_image(&complete);
    assert_eq!(metadata(&complete)["request_ref"], ORIGINAL_REF);
    assert_no_fees(metadata(&complete));
    assert_eq!(fixture.counts(), (1, 1, 2, 0));
    assert_eq!(stored_record(&directory)["status"], "completed");

    let images = directory.path().join("images");
    std::fs::create_dir_all(&images).unwrap();
    let expired_image = images.join("tic-image-1-abcdefghijkl.png");
    std::fs::write(&expired_image, &fixture.common.png).unwrap();
    *fixture.task_state.reply.lock().unwrap() = TaskReply::Json(
        StatusCode::NOT_FOUND,
        json!({"error":{"message":SENSITIVE}}),
    );
    let expired = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    assert_eq!(expired.is_error, Some(true));
    assert_eq!(metadata(&expired)["code"], "task_not_found");
    assert!(
        !metadata(&expired)["message"]
            .as_str()
            .unwrap()
            .contains("费用")
    );
    assert_eq!(metadata(&expired)["query_state"], "not_obtained");
    assert_eq!(metadata(&expired)["submission"], "unknown");
    assert_eq!(metadata(&expired)["task_id"], TASK_ID);
    assert_eq!(metadata(&expired)["request_ref"], ORIGINAL_REF);
    assert!(
        metadata(&expired)["status"].is_null(),
        "cached completed is not a new server state"
    );
    assert_safe(&expired);
    assert!(!expired_image.exists());
    assert_eq!(stored_record(&directory)["request_ref"], ORIGINAL_REF);
    assert_eq!(fixture.counts(), (1, 1, 3, 0));
    close(client, server).await;
}

#[tokio::test]
async fn task_query_needs_only_safe_id_and_current_key_not_catalog_or_local_registration() {
    let fixture = TaskFixture::new().await;
    fixture.common.catalog.lock().unwrap()["can_submit"] = json!(false);
    let directory = tempfile::tempdir().unwrap();
    let (client, server) = connect_task(&fixture, &directory, true).await;
    let tools = client.list_tools(None).await.unwrap().tools;
    assert_eq!(tools.len(), 8);
    let tool = tools
        .iter()
        .find(|tool| tool.name == "get_image_task")
        .unwrap();
    let schema = serde_json::to_value(&tool.input_schema).unwrap();
    assert_eq!(
        schema["properties"]
            .as_object()
            .unwrap()
            .keys()
            .map(String::as_str)
            .collect::<Vec<_>>(),
        ["task_id"]
    );
    assert_eq!(schema["required"], json!(["task_id"]));
    assert_eq!(schema["additionalProperties"], false);
    for args in [
        json!({"task_id":"../escape"}),
        json!({"task_id":"a/b"}),
        json!({"task_id":"a?x"}),
        json!({"task_id":"a#x"}),
        json!({"task_id":""}),
        json!({"task_id":"https://synthetic.invalid/task"}),
        json!({"task_id":TASK_ID,"poll_url":SENSITIVE}),
        json!({"task_id":TASK_ID,"api_key":KEY}),
        json!({"task_id":TASK_ID,"request_ref":QUERY_REF}),
    ] {
        let result = call(&client, "get_image_task", args).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["code"], "invalid_parameters");
        assert_safe(&result);
    }
    assert_eq!(fixture.counts(), (0, 0, 0, 0));
    assert!(!directory.path().join("tasks").exists());
    let processing = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    assert_ne!(processing.is_error, Some(true));
    assert_eq!(metadata(&processing)["status"], "processing");
    assert_eq!(metadata(&processing)["request_ref"], ORIGINAL_REF);
    assert_eq!(fixture.counts(), (0, 0, 1, 0));
    assert_eq!(
        fixture.task_state.paths.lock().unwrap().as_slice(),
        &[format!("/v1/images/tasks/{TASK_ID}")]
    );
    std::fs::write(
        directory
            .path()
            .join("tasks")
            .join(format!("{TASK_ID}.json")),
        b"{broken",
    )
    .unwrap();
    let recovered = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    assert_ne!(recovered.is_error, Some(true));
    assert_eq!(metadata(&recovered)["status"], "processing");
    assert_eq!(metadata(&recovered)["recovery"]["status"], "saved");
    assert_eq!(
        metadata(&recovered)["recovery"]["read_error"]["code"],
        "task_storage_failed"
    );
    assert_eq!(stored_record(&directory)["request_ref"], ORIGINAL_REF);
    assert_eq!(fixture.counts(), (0, 0, 2, 0));
    let mut mismatched = task_json("processing");
    mismatched["task_id"] = json!("imgtask_other");
    fixture.set_task(mismatched);
    let error = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    assert_eq!(metadata(&error)["code"], "invalid_task_response");
    assert_eq!(metadata(&error)["query_state"], "not_obtained");
    assert_eq!(metadata(&error)["request_ref"], ORIGINAL_REF);
    assert_eq!(fixture.counts(), (0, 0, 3, 0));
    close(client, server).await;
}

#[tokio::test]
async fn terminal_tasks_keep_original_references_and_images_without_fee_requests() {
    let fixture = TaskFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let (client, server) = connect_task(&fixture, &directory, false).await;
    fixture.set_task(fixture.completed());
    let complete = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    assert_image(&complete);
    assert_eq!(metadata(&complete)["request_ref"], ORIGINAL_REF);
    let mut failed = task_json("failed");
    failed["error"] =
        json!({"message":format!("Authorization: {KEY}; {SENSITIVE}"),"type":"api_error"});
    fixture.set_task(failed);
    let failed = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    assert_eq!(metadata(&failed)["status"], "failed");
    assert_eq!(metadata(&failed)["request_ref"], ORIGINAL_REF);
    assert_no_fees(metadata(&failed));
    assert_safe(&failed);
    let mut legacy = fixture.completed();
    legacy["id"] = json!("imgtask_legacy");
    legacy["task_id"] = json!("imgtask_legacy");
    legacy.as_object_mut().unwrap().remove("request_ref");
    fixture.set_task(legacy);
    let legacy = call(
        &client,
        "get_image_task",
        json!({"task_id":"imgtask_legacy"}),
    )
    .await;
    assert_image(&legacy);
    assert!(metadata(&legacy)["request_ref"].is_null());
    assert_eq!(fixture.counts(), (0, 0, 3, 0));
    close(client, server).await;
}

#[tokio::test]
async fn inconsistent_or_missing_202_identifiers_never_claim_valid_acceptance_or_resubmit() {
    let fixture = TaskFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let (client, server) = connect_task(&fixture, &directory, true).await;
    for case in 0..5 {
        let mut body = accepted_json();
        match case {
            0 => {
                body.as_object_mut().unwrap().remove("request_ref");
            }
            1 => body["request_ref"] = json!("different-original-reference"),
            2 => body["id"] = json!("imgtask_other"),
            3 => body["task_id"] = json!("../escape"),
            _ => body["status"] = json!(SENSITIVE),
        }
        *fixture.common.mode.lock().unwrap() = Mode::Error(StatusCode::ACCEPTED, body);
        let result = call(
            &client,
            "generate_image",
            json!({"prompt":"synthetic","mode":"single_async"}),
        )
        .await;
        assert_eq!(result.is_error, Some(true), "case {case}");
        assert_eq!(metadata(&result)["submission"], "unknown", "case {case}");
        let (reference, source) = match case {
            1 => ("different-original-reference", "task_json"),
            4 => (ORIGINAL_REF, "task_json"),
            _ => (ORIGINAL_REF, "submission_header"),
        };
        assert_eq!(metadata(&result)["request_ref"], reference, "case {case}");
        assert_eq!(
            metadata(&result)["request_ref_source"],
            source,
            "case {case}"
        );
        assert_safe(&result);
        assert_eq!(fixture.counts(), (case + 1, case + 1, 0, 0));
    }
    assert!(!directory.path().join("tasks").exists());
    *fixture.common.mode.lock().unwrap() = Mode::Error(
        StatusCode::FORBIDDEN,
        json!({"error":{"code":"permission_denied","message":SENSITIVE}}),
    );
    let rejected = call(
        &client,
        "generate_image",
        json!({"prompt":"synthetic","mode":"single_async"}),
    )
    .await;
    assert_eq!(metadata(&rejected)["submission"], "rejected");
    assert_eq!(metadata(&rejected)["request_ref"], ORIGINAL_REF);
    assert_safe(&rejected);
    assert_eq!(fixture.counts(), (6, 6, 0, 0));
    close(client, server).await;
}

#[tokio::test]
async fn async_submit_timeout_disconnect_redirect_and_cancellation_never_replay_post() {
    for mode in [
        Mode::Delay,
        Mode::Disconnect,
        Mode::Redirect("http://127.0.0.1:1/no-replay".into()),
    ] {
        let fixture = TaskFixture::new().await;
        let directory = tempfile::tempdir().unwrap();
        *fixture.common.mode.lock().unwrap() = mode;
        let (client, server) = connect_dirs(
            fixture.client(Duration::from_millis(100)),
            directory.path().join("images"),
            directory.path().join("tasks"),
            true,
        )
        .await;
        let result = call(
            &client,
            "generate_image",
            json!({"prompt":"synthetic","mode":"single_async"}),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["submission"], "unknown");
        assert_eq!(fixture.counts(), (1, 1, 0, 0));
        close(client, server).await;
    }
    let fixture = TaskFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    *fixture.common.mode.lock().unwrap() = Mode::Delay;
    let (client, server) = connect_task(&fixture, &directory, true).await;
    {
        let request = client.call_tool_once(
            CallToolRequestParams::new("generate_image").with_arguments(
                json!({"prompt":"synthetic","mode":"single_async"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        );
        tokio::pin!(request);
        tokio::select! { _ = &mut request => panic!("response must still be waiting"), _ = fixture.wait_for_post() => {} }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fixture.counts(), (1, 1, 0, 0));
    close(client, server).await;
}

#[tokio::test]
async fn query_failures_and_cancelled_download_keep_task_identity_without_resubmission() {
    let fixture = TaskFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let (client, server) = connect_dirs(
        fixture.client(Duration::from_millis(100)),
        directory.path().join("images"),
        directory.path().join("tasks"),
        false,
    )
    .await;
    let accepted = call(
        &client,
        "generate_image",
        json!({"prompt":"synthetic","mode":"single_async"}),
    )
    .await;
    assert_eq!(metadata(&accepted)["submission"], "accepted");
    for (index, reply) in [
        TaskReply::Json(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":{"message":SENSITIVE}}),
        ),
        TaskReply::Json(StatusCode::UNAUTHORIZED, json!({"error":{"message":KEY}})),
        TaskReply::Disconnect,
        TaskReply::Delay,
    ]
    .into_iter()
    .enumerate()
    {
        *fixture.task_state.reply.lock().unwrap() = reply;
        let result = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["submission"], "unknown");
        assert_eq!(metadata(&result)["query_state"], "not_obtained");
        assert_eq!(metadata(&result)["task_id"], TASK_ID);
        assert_eq!(metadata(&result)["request_ref"], ORIGINAL_REF);
        assert_eq!(fixture.counts(), (1, 1, index + 1, 0));
        assert_safe(&result);
    }
    close(client, server).await;

    let mut complete = fixture.completed();
    complete["result"] = json!({"data":[{"url":fixture.download_url()}]});
    fixture.set_task(complete);
    fixture
        .task_state
        .slow_download
        .store(true, Ordering::SeqCst);
    let mut direct = fixture.client(Duration::from_secs(10));
    direct.task_store = Some(crate::tasks::TaskStore {
        root: directory.path().join("tasks"),
    });
    let progress = RequestProgress::for_task(TASK_ID);
    {
        let pending = direct.task(TASK_ID, &progress);
        tokio::pin!(pending);
        tokio::select! {
            _ = &mut pending => panic!("download must still be waiting"),
            seen = tokio::time::timeout(Duration::from_secs(2), fixture.task_state.download_seen.notified()) => seen.unwrap(),
        }
    }
    let cancelled = serde_json::to_value(progress.cancelled()).unwrap();
    assert_eq!(cancelled["submission"], "unknown");
    assert_eq!(cancelled["query_state"], "retrieved");
    assert_eq!(cancelled["status"], "completed");
    assert_eq!(cancelled["task_id"], TASK_ID);
    assert_eq!(cancelled["request_ref"], ORIGINAL_REF);
    assert_eq!(fixture.counts(), (1, 1, 5, 0));
    assert_eq!(
        fixture.task_state.download_auth.lock().unwrap().as_slice(),
        &[false]
    );
}

#[tokio::test]
async fn completed_large_single_and_multiple_results_preserve_originals_and_message_budget_without_fee_queries()
 {
    use rmcp::model::{RequestId, ServerJsonRpcMessage, ServerResult};
    use std::time::{SystemTime, UNIX_EPOCH};
    for (width, height, count) in [
        (2304, 2048, 1usize),
        (1408, 1280, 3usize),
        (2304, 2048, 4usize),
    ] {
        let fixture = TaskFixture::new().await;
        let directory = tempfile::tempdir().unwrap();
        let bytes = noise_png(width, height);
        let encoded = STANDARD.encode(&bytes);
        assert!(encoded.len() * count > 16 * 1024 * 1024);
        let mut value = fixture.completed();
        value["result"]["data"] = json!(
            (0..count)
                .map(|_| json!({"b64_json":encoded}))
                .collect::<Vec<_>>()
        );
        fixture.set_task(value);
        *fixture.common.fee.lock().unwrap() = (StatusCode::SERVICE_UNAVAILABLE, json!({}));
        let (client, server) = connect_task(&fixture, &directory, count > 1).await;
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
        assert_ne!(
            result.is_error,
            Some(true),
            "{count} task images: {}",
            metadata(&result)
        );
        assert_image(&result);
        assert_no_fees(metadata(&result));
        assert_eq!(metadata(&result)["expires_at"], 1795195650u64);
        assert!(metadata(&result)["result_expires_at"].is_null());
        let wire = ServerJsonRpcMessage::response(
            ServerResult::CallToolResult(result.clone()),
            RequestId::Number(1),
        );
        assert!(serde_json::to_vec(&wire).unwrap().len() < 8 * 1024 * 1024);
        let json = serde_json::to_value(&result).unwrap();
        let content = json["content"].as_array().unwrap();
        assert_eq!(
            content
                .iter()
                .filter(|block| block["type"] == "image")
                .count(),
            count
        );
        let files = metadata(&result)["local_files"].as_array().unwrap();
        assert!(!files.is_empty());
        for file in files {
            let path = Path::new(file["path"].as_str().unwrap());
            assert!(path.starts_with(directory.path().join("images")));
            assert_eq!(std::fs::read(path).unwrap(), bytes);
            assert_eq!(file["inline_representation"], "preview");
            let expires = file["expires_at"].as_u64().unwrap();
            let after = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_secs();
            assert!(expires >= before + 86400 && expires <= after + 86400);
            let index = file["image_index"].as_u64().unwrap() as usize;
            let preview = STANDARD
                .decode(content[index]["data"].as_str().unwrap())
                .unwrap();
            let image = image::load_from_memory(&preview).unwrap();
            assert!(image.width() <= 256 && image.height() <= 256);
        }
        assert_eq!(stored_record(&directory)["status"], "completed");
        assert_eq!(fixture.counts(), (0, 0, 1, 0));
        close(client, server).await;
    }
}

#[tokio::test]
#[ignore = "requires S2b real Go/PG/Redis task artifacts"]
async fn server_generated_tasks_cross_http_and_mcp_with_original_references() {
    let path = std::env::var_os("TOKEMULA_IMAGE_TASK_FIXTURE")
        .expect("set TOKEMULA_IMAGE_TASK_FIXTURE to S2b-go-async-billing.json");
    let source: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    let cases = source["cases"].as_array().unwrap();
    assert_eq!(cases.len(), 3);
    let fixture = TaskFixture::new().await;
    fixture.common.catalog.lock().unwrap()["can_submit"] = json!(false);
    let directory = tempfile::tempdir().unwrap();
    let (client, server) = connect_task(&fixture, &directory, true).await;
    for (index, case) in cases.iter().enumerate() {
        let reference = case["request_ref"].as_str().unwrap();
        let id = case["task_id"].as_str().unwrap();
        assert_eq!(case["task_request_ref"], reference);
        assert_eq!(case["header_request_ref"], reference);
        assert_ne!(reference, QUERY_REF);
        let mut task = case["task"].clone();
        // Only image transport is rebound; task state and original references stay intact.
        if let Some(data) = task["result"]["data"].as_array_mut() {
            for item in data {
                item["url"] = json!(fixture.download_url());
            }
        }
        fixture.set_task(task);
        *fixture.common.request_ref.lock().unwrap() = reference.into();
        *fixture.common.fee.lock().unwrap() = (
            StatusCode::from_u16(case["http_status"].as_u64().unwrap() as u16).unwrap(),
            case["body"].clone(),
        );
        let result = call(&client, "get_image_task", json!({"task_id":id})).await;
        assert_ne!(result.is_error, Some(true), "{}", case["name"]);
        assert_eq!(metadata(&result)["task_id"], id);
        assert_eq!(metadata(&result)["status"], case["task_status"]);
        assert_eq!(metadata(&result)["request_ref"], reference);
        assert_eq!(metadata(&result)["expires_at"], case["task"]["expires_at"]);
        assert_eq!(
            metadata(&result)["result_expires_at"],
            case["task"]["result_expires_at"]
        );
        assert_no_fees(metadata(&result));
        if case["task_status"] == "completed" {
            assert_image(&result);
        } else {
            let failure = &metadata(&result)["failure"];
            if case["name"] == "async_offload_failed" {
                assert_eq!(failure["code"], "image_result_storage_failed");
                assert!(failure["message"].as_str().unwrap().contains("转存失败"));
            } else {
                assert_eq!(failure["code"], "image_task_failed");
                assert!(failure["message"].as_str().is_some());
            }
        }
        assert_eq!(fixture.counts(), (0, 0, index + 1, 0));
        eprintln!(
            "consumed {}: original task fields; result image URL only rebound to loopback; query header {} differs from original {}",
            case["name"], QUERY_REF, reference
        );
    }
    assert_eq!(
        fixture.task_state.download_auth.lock().unwrap().as_slice(),
        &[false]
    );
    close(client, server).await;
}

#[tokio::test]
async fn regression_202_missing_or_conflicting_header_preserves_json_reference() {
    for header in ["", "different-submission-header"] {
        let fixture = TaskFixture::new().await;
        *fixture.common.request_ref.lock().unwrap() = header.into();
        let directory = tempfile::tempdir().unwrap();
        let (client, server) = connect_task(&fixture, &directory, false).await;
        let result = call(
            &client,
            "generate_image",
            json!({"prompt":"synthetic", "mode":"single_async"}),
        )
        .await;
        let observed = metadata(&result).clone();
        assert_eq!(fixture.counts(), (1, 1, 0, 0));
        assert_eq!(observed["submission"], "unknown");
        assert_eq!(observed["task_id"], TASK_ID);
        assert!(!directory.path().join("tasks").exists());
        close(client, server).await;
        assert_eq!(
            observed["request_ref"], ORIGINAL_REF,
            "safe JSON request_ref survives an invalid 202"
        );
        assert_eq!(observed["request_ref_source"], "task_json");
    }
}

#[tokio::test]
async fn regression_query_invalid_status_keeps_json_reference_only_for_matching_identity() {
    let fixture = TaskFixture::new().await;
    fixture.set_task(task_json("unexpected_state"));
    let directory = tempfile::tempdir().unwrap();
    let (client, server) = connect_task(&fixture, &directory, false).await;
    let result = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    let observed = metadata(&result).clone();
    assert_eq!(fixture.counts(), (0, 0, 1, 0));
    assert_eq!(observed["query_state"], "not_obtained");
    assert_eq!(observed["request_ref"], ORIGINAL_REF);
    assert_eq!(observed["request_ref_source"], "task_json");
    assert!(!directory.path().join("tasks").exists());

    let mut unrelated = task_json("processing");
    unrelated["id"] = json!("imgtask_other");
    unrelated["task_id"] = json!("imgtask_other");
    unrelated["request_ref"] = json!("another-task-reference");
    fixture.set_task(unrelated);
    let result = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    assert_eq!(metadata(&result)["code"], "invalid_task_response");
    assert_eq!(metadata(&result)["query_state"], "not_obtained");
    assert_eq!(metadata(&result)["task_id"], TASK_ID);
    assert!(metadata(&result)["request_ref"].is_null());
    assert!(metadata(&result)["request_ref_source"].is_null());
    assert_eq!(fixture.counts(), (0, 0, 2, 0));
    close(client, server).await;
}

#[tokio::test]
async fn regression_legacy_cache_failure_preserves_known_reference_without_fee_lookup() {
    let fixture = TaskFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let blocked = directory.path().join("images");
    std::fs::write(&blocked, b"ordinary file blocks image cache").unwrap();
    let (client, server) = connect_task(&fixture, &directory, false).await;
    let processing = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    assert_eq!(metadata(&processing)["request_ref"], ORIGINAL_REF);
    let mut legacy = fixture.completed();
    legacy.as_object_mut().unwrap().remove("request_ref");
    let encoded = STANDARD.encode(noise_png(1792, 1792));
    assert!(encoded.len() > 8 * 1024 * 1024);
    legacy["result"]["data"] = json!([{"b64_json":encoded}]);
    fixture.set_task(legacy);
    let result = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
    let observed = metadata(&result).clone();
    assert_eq!(result.is_error, Some(true));
    assert_eq!(observed["code"], "image_storage_failed");
    assert_eq!(fixture.counts(), (0, 0, 2, 0));
    assert_eq!(observed["task_id"], TASK_ID);
    close(client, server).await;
    assert_eq!(observed["request_ref"], ORIGINAL_REF);
    assert_eq!(observed["request_ref_source"], "local_record");
    assert_eq!(observed["query_state"], "retrieved");
    assert_eq!(observed["status"], "completed");
}

#[tokio::test]
async fn regression_failed_reasons_are_static_allowlisted_and_keep_original_reference() {
    let fixture = TaskFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let (client, server) = connect_task(&fixture, &directory, true).await;
    for (index, (error, expected)) in [
        (json!({"type":"timeout_error","message":SENSITIVE}), "image_task_timeout"),
        (json!({"code":"timeout_error","message":SENSITIVE}), "image_task_timeout"),
        (json!({"type":KEY,"code":SENSITIVE,"message":format!("Authorization: {KEY}")}), "image_task_failed"),
        (json!({"type":"api_error","message":format!("failed to store generated image to object storage {KEY}")}), "image_task_failed"),
    ]
    .into_iter()
    .enumerate()
    {
        let mut task = task_json("failed");
        task["error"] = error;
        fixture.set_task(task);
        let result = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
        assert_ne!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["status"], "failed");
        assert_eq!(metadata(&result)["failure"]["code"], expected);
        if expected == "image_task_timeout" {
            assert!(metadata(&result)["failure"]["message"].as_str().unwrap().contains("超时"));
        }
        assert_no_fees(metadata(&result));
        assert_eq!(fixture.counts(), (0, 0, index + 1, 0));
        assert_safe(&result);
    }
    close(client, server).await;
}

#[tokio::test]
async fn completed_async_dual_sources_share_b64_precedence_and_no_fallback() {
    let fixture = TaskFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let (client, server) = connect_task(&fixture, &directory, false).await;
    let mut downloads = 0;
    for (b64, success, needs_download) in [
        (STANDARD.encode(&fixture.common.png), true, false),
        (" \t\n".into(), true, true),
        ("damaged-encoding".into(), false, false),
        (STANDARD.encode(b"not an image"), false, false),
    ] {
        let mut completed = fixture.completed();
        completed["result"]["data"][0] = json!({"b64_json":b64,"url":fixture.download_url()});
        fixture.set_task(completed);
        let result = call(&client, "get_image_task", json!({"task_id":TASK_ID})).await;
        if success {
            assert_image(&result);
        } else {
            assert_eq!(metadata(&result)["code"], "invalid_image");
            assert_eq!(metadata(&result)["request_ref"], ORIGINAL_REF);
        }
        downloads += usize::from(needs_download);
        let auth = fixture.task_state.download_auth.lock().unwrap();
        assert_eq!(auth.len(), downloads);
        assert!(auth.iter().all(|has_auth| !has_auth));
        assert_eq!(fixture.common.posts.load(Ordering::SeqCst), 0);
    }
    close(client, server).await;
}
