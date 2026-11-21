//! Loopback/MCP acceptance for batch requests. All keys, files and responses are synthetic.
use super::*;
use std::{collections::HashMap, path::PathBuf};

const BATCH: &str = "batch_synthetic_b3b";
const SERVER_ITEM: &str = "server-expanded-07";
const MIXED_MODEL: &str = "synthetic-batch-mixed";

#[derive(Clone)]
enum BatchReply {
    Normal,
    Json(StatusCode, Value),
    Delay,
    Disconnect,
    Redirect(String),
}

#[derive(Clone)]
struct BatchRequest {
    method: String,
    path: String,
    query: String,
    auth: bool,
    idempotency: Option<String>,
    body: Value,
    body_len: usize,
}

struct BatchState {
    catalog: Mutex<Value>,
    submit_reply: Mutex<BatchReply>,
    cancel_reply: Mutex<BatchReply>,
    content_reply: Mutex<BatchReply>,
    batch: Mutex<Value>,
    jobs: Mutex<Vec<Value>>,
    items: Mutex<Vec<Value>>,
    requests: Mutex<Vec<BatchRequest>>,
    submissions: Mutex<HashMap<String, Value>>,
    creates: AtomicUsize,
    holds: AtomicUsize,
    submit_seen: tokio::sync::Notify,
    cancel_seen: tokio::sync::Notify,
    image: Mutex<Vec<u8>>,
    content_type: Mutex<String>,
    list_override: Mutex<Option<Value>>,
    items_override: Mutex<Option<Value>>,
    break_record_after_acceptance: Mutex<Option<PathBuf>>,
}

struct BatchFixture {
    state: Arc<BatchState>,
    base: String,
    server: JoinHandle<()>,
}
impl Drop for BatchFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

fn capability(operation: &str) -> Value {
    let edit = operation == "edit";
    let mut limits = json!({
        "max_images":if edit { 3 } else { 0 },
        "max_image_bytes":if edit { 20971520 } else { 0 },
        "mime_types":if edit { vec!["image/png", "image/jpeg", "image/webp"] } else { vec![] },
        "max_request_bytes":268435456,
        "max_prompt_bytes":1000,"max_batch_items":10,"max_batch_output_images":20
    });
    if edit {
        limits["max_batch_input_images"] = json!(12);
        limits["max_batch_input_bytes"] = json!(1048576);
    }
    json!({"operation":operation,"mode":"batch",
        "parameters":{"max_images":4,"sizes":["1K","2K"],"qualities":[]},
        "input_limits":limits,"result_formats":["authenticated_download"],"cancel_supported":true})
}

fn batch_catalog() -> Value {
    let model = |id: &str, rank: u32, operations: &[&str]| {
        json!({
            "id":id,"display_name":id,"availability":"configured","preference_rank":rank,
            "capabilities":operations.iter().map(|op| capability(op)).collect::<Vec<_>>(),
            "pricing":{"status":"unknown"}
        })
    };
    json!({"schema_version":"1","observed_at":"2026-11-20T00:00:00Z","can_submit":true,
        "models":[model("synthetic-generation-only",0,&["generation"]),
            model(MIXED_MODEL,1,&["generation","edit"]),
            model("synthetic-edit-only",2,&["edit"])]})
}

fn batch_json(status: &str) -> Value {
    json!({"id":BATCH,"object":"image.batch","task_name":"Synthetic batch","status":status,
        "model":MIXED_MODEL,"provider":"gemini","item_count":3,"success_count":2,"fail_count":1,
        "ledger_unit":"CNY","estimated_cost":0.0001562500,"hold_amount":0.0001562500,
        "actual_cost":0.0000781250,"created_at":1795109250u64,"submitted_at":1795109251u64,
        "settled_at":1795109252u64,"output_expires_at":1795195650u64,"output_deleted_at":null})
}

fn batch_item(id: &str, status: &str) -> Value {
    json!({"custom_id":id,"status":status,"mime_type":"image/png","file_extension":"png",
        "image_count":if status == "succeeded" { 2 } else { 0 },
        "error":if status == "failed" { json!({"code":"EMPTY_IMAGE_OUTPUT","message":"no image output","source":"provider"}) } else { Value::Null }})
}

impl BatchFixture {
    async fn new() -> Self {
        let mut pending = batch_json("queued");
        pending["actual_cost"] = Value::Null;
        pending["settled_at"] = Value::Null;
        let state = Arc::new(BatchState {
            catalog: Mutex::new(batch_catalog()),
            submit_reply: Mutex::new(BatchReply::Normal),
            cancel_reply: Mutex::new(BatchReply::Normal),
            content_reply: Mutex::new(BatchReply::Normal),
            batch: Mutex::new(pending),
            jobs: Mutex::new(vec![batch_json("completed")]),
            items: Mutex::new(vec![
                batch_item(SERVER_ITEM, "succeeded"),
                batch_item("failed-result", "failed"),
            ]),
            requests: Mutex::new(Vec::new()),
            submissions: Mutex::new(HashMap::new()),
            creates: AtomicUsize::new(0),
            holds: AtomicUsize::new(0),
            submit_seen: tokio::sync::Notify::new(),
            cancel_seen: tokio::sync::Notify::new(),
            image: Mutex::new(png()),
            content_type: Mutex::new("image/png".into()),
            list_override: Mutex::new(None),
            items_override: Mutex::new(None),
            break_record_after_acceptance: Mutex::new(None),
        });
        let router = Router::new()
            .fallback(batch_handler)
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            state,
            base,
            server,
        }
    }
    fn client(&self, timeout: Duration) -> ImageClient {
        ImageClient::build(&self.base, Some(KEY.into()), timeout).unwrap()
    }
    fn count(&self, method: &str, suffix: &str) -> usize {
        self.state
            .requests
            .lock()
            .unwrap()
            .iter()
            .filter(|r| r.method == method && r.path == format!("/v1/images/{suffix}"))
            .count()
    }
    fn posts(&self) -> usize {
        self.count("POST", "batches")
    }
    fn catalog_gets(&self) -> usize {
        self.count("GET", "models")
    }
    fn creates(&self) -> usize {
        self.state.creates.load(Ordering::SeqCst)
    }
    fn holds(&self) -> usize {
        self.state.holds.load(Ordering::SeqCst)
    }
    fn requests(&self) -> Vec<BatchRequest> {
        self.state.requests.lock().unwrap().clone()
    }
    async fn wait_for_request(&self, cancel: bool) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notify = if cancel {
                    &self.state.cancel_seen
                } else {
                    &self.state.submit_seen
                };
                let pending = notify.notified();
                if if cancel {
                    self.count("POST", &format!("batches/{BATCH}/cancel")) > 0
                } else {
                    self.posts() > 0
                } {
                    break;
                }
                pending.await;
            }
        })
        .await
        .unwrap();
    }
}

async fn batch_handler(State(state): State<Arc<BatchState>>, request: Request) -> AxumResponse {
    let path = request.uri().path().to_owned();
    let query = request.uri().query().unwrap_or("").to_owned();
    let method = request.method().to_string();
    let auth = auth_ok(&request);
    let idempotency = request
        .headers()
        .get("idempotency-key")
        .map(|v| v.to_str().unwrap().to_owned());
    let bytes = to_bytes(request.into_body(), MAX_METADATA_RESPONSE_BYTES)
        .await
        .unwrap();
    let body = serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null);
    state.requests.lock().unwrap().push(BatchRequest {
        method: method.clone(),
        path: path.clone(),
        query: query.clone(),
        auth,
        idempotency: idempotency.clone(),
        body: body.clone(),
        body_len: bytes.len(),
    });
    if path.starts_with("/v1/") {
        assert!(auth, "current Key required on fixed API");
    }
    let batch_id = state.batch.lock().unwrap()["id"]
        .as_str()
        .unwrap()
        .to_owned();
    match (method.as_str(), path.as_str()) {
        ("GET", "/v1/images/models") => {
            axum::Json(state.catalog.lock().unwrap().clone()).into_response()
        }
        ("POST", "/v1/images/batches") => {
            state.submit_seen.notify_waiters();
            let mode = state.submit_reply.lock().unwrap().clone();
            if !matches!(mode, BatchReply::Normal) {
                return batch_reply(mode, Value::Null).await;
            }
            let key = idempotency.expect("every batch submission needs Idempotency-Key");
            assert!(
                !body["task_name"].as_str().unwrap_or("").trim().is_empty(),
                "stable task name must be explicit"
            );
            let conflict = {
                let mut submissions = state.submissions.lock().unwrap();
                if let Some(previous) = submissions.get(&key) {
                    previous != &body
                } else {
                    submissions.insert(key, body.clone());
                    state.creates.fetch_add(1, Ordering::SeqCst);
                    state.holds.fetch_add(1, Ordering::SeqCst);
                    false
                }
            };
            if conflict {
                return (StatusCode::CONFLICT,axum::Json(json!({"error":{"code":"BATCH_IMAGE_IDEMPOTENCY_CONFLICT","message":SENSITIVE}}))).into_response();
            }
            if let Some(root) = state.break_record_after_acceptance.lock().unwrap().take() {
                std::fs::rename(&root, root.with_extension("before-acceptance")).unwrap();
                std::fs::write(&root, b"synthetic storage failure").unwrap();
            }
            let mut batch = state.batch.lock().unwrap().clone();
            batch["model"] = body["model"].clone();
            batch["task_name"] = body["task_name"].clone();
            axum::Json(batch).into_response()
        }
        ("GET", "/v1/images/batches") => {
            if let Some(value) = state.list_override.lock().unwrap().clone() {
                return axum::Json(value).into_response();
            }
            let (offset, limit) = pagination(&query, 20, 100);
            let jobs = state.jobs.lock().unwrap();
            let data: Vec<_> = jobs.iter().skip(offset).take(limit).cloned().collect();
            axum::Json(json!({"object":"list","has_more":data.len()==limit,"data":data}))
                .into_response()
        }
        _ if method == "GET" && path == format!("/v1/images/batches/{batch_id}") => {
            axum::Json(state.batch.lock().unwrap().clone()).into_response()
        }
        _ if method == "GET" && path == format!("/v1/images/batches/{batch_id}/items") => {
            if let Some(value) = state.items_override.lock().unwrap().clone() {
                return axum::Json(value).into_response();
            }
            let (offset, limit) = pagination(&query, 100, 500);
            let items = state.items.lock().unwrap();
            let data: Vec<_> = items.iter().skip(offset).take(limit).cloned().collect();
            axum::Json(json!({"object":"list","has_more":data.len()==limit,"data":data}))
                .into_response()
        }
        _ if method == "GET" && path.ends_with("/content") => {
            let reply = state.content_reply.lock().unwrap().clone();
            if !matches!(reply, BatchReply::Normal) {
                return batch_reply(reply, Value::Null).await;
            }
            let image = state.image.lock().unwrap().clone();
            (
                [
                    ("content-type", state.content_type.lock().unwrap().clone()),
                    (
                        "content-disposition",
                        "attachment; filename=../../untrusted.png".to_owned(),
                    ),
                ],
                image,
            )
                .into_response()
        }
        _ if method == "POST" && path == format!("/v1/images/batches/{batch_id}/cancel") => {
            state.cancel_seen.notify_waiters();
            let reply = state.cancel_reply.lock().unwrap().clone();
            let value = state.batch.lock().unwrap().clone();
            batch_reply(reply, value).await
        }
        _ => (
            StatusCode::NOT_FOUND,
            axum::Json(json!({"error":{"code":"BATCH_IMAGE_NOT_FOUND","message":SENSITIVE}})),
        )
            .into_response(),
    }
}

fn pagination(query: &str, default: usize, max: usize) -> (usize, usize) {
    let url = Url::parse(&format!("http://localhost/?{query}")).unwrap();
    let params: HashMap<_, _> = url.query_pairs().into_owned().collect();
    let offset = params
        .get("cursor")
        .and_then(|s| s.parse::<usize>().ok())
        .unwrap_or(0);
    let limit = params
        .get("limit")
        .and_then(|s| s.parse::<usize>().ok())
        .filter(|n| *n > 0 && *n <= max)
        .unwrap_or(default);
    (offset, limit)
}
async fn batch_reply(reply: BatchReply, normal: Value) -> AxumResponse {
    match reply {
        BatchReply::Normal => axum::Json(normal).into_response(),
        BatchReply::Json(status, value) => (status, axum::Json(value)).into_response(),
        BatchReply::Delay => {
            tokio::time::sleep(Duration::from_secs(5)).await;
            axum::Json(normal).into_response()
        }
        BatchReply::Redirect(url) => AxumResponse::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header("location", url)
            .body(Body::empty())
            .unwrap(),
        BatchReply::Disconnect => AxumResponse::builder()
            .header("content-length", "1000")
            .body(Body::from_stream(futures_util::stream::once(async {
                Err::<Bytes, _>(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "synthetic dropped batch response",
                ))
            })))
            .unwrap(),
    }
}

async fn connect_batch(
    fixture: &BatchFixture,
    directory: &tempfile::TempDir,
    timeout: Duration,
    modern: bool,
) -> (RunningService<RoleClient, ClientConfig>, JoinHandle<()>) {
    connect_batch_client(fixture.client(timeout), directory, modern).await
}

async fn connect_batch_client(
    client: ImageClient,
    directory: &tempfile::TempDir,
    modern: bool,
) -> (RunningService<RoleClient, ClientConfig>, JoinHandle<()>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = ImageMcp::with_directories(
        client,
        directory.path().join("images"),
        directory.path().join("tasks"),
    );
    let task = tokio::spawn(async move {
        server
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
    (client, task)
}
fn batch_args(key: &str) -> Value {
    json!({"model":MIXED_MODEL,"idempotency_key":key,"items":[{"custom_id":"cover","prompt":"synthetic prompt"}]})
}
fn assert_safe(value: &impl Serialize) {
    let text = serde_json::to_string(value).unwrap();
    for secret in [KEY, SENSITIVE, "Authorization"] {
        assert!(!text.contains(secret), "private text exposed")
    }
}
fn recovery_records(directory: &tempfile::TempDir) -> Vec<Value> {
    let root = directory.path().join("tasks/batches");
    let records = std::fs::read_dir(&root)
        .unwrap()
        .map(|entry| {
            let path = entry.unwrap().path();
            let record: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
            assert_safe(&record);
            for forbidden in [
                "prompt",
                "reference_image_paths",
                "reference_images",
                "body",
                "authorization",
                "api_key",
                "key_hash",
            ] {
                assert!(
                    record.get(forbidden).is_none(),
                    "stored forbidden field {forbidden}"
                );
            }
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
            }
            record
        })
        .collect();
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(root).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    records
}

#[tokio::test]
async fn mixed_capabilities_inline_references_and_explicit_replay_keep_one_business_task() {
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reference.png");
    std::fs::write(&path, png()).unwrap();
    let input = json!({"idempotency_key":"explicit-replay","size":"2K","items":[
        {"custom_id":"cover","prompt":"synthetic cover","output_count":3},
        {"custom_id":"edit","prompt":"synthetic edit","reference_image_paths":[path]}]});
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), true).await;
    let first = call(&client, "submit_image_batch", input.clone()).await;
    assert_ne!(first.is_error, Some(true), "{}", metadata(&first));
    assert_eq!(metadata(&first)["submission"], "accepted");
    assert_eq!(metadata(&first)["batch_id"], BATCH);
    assert_eq!(metadata(&first)["selected_model"], MIXED_MODEL);
    assert_no_fees(metadata(&first));
    let records = recovery_records(&directory);
    assert_eq!(records.len(), 1);
    assert_eq!(records[0]["idempotency_key"], "explicit-replay");
    assert_eq!(records[0]["batch_id"], BATCH);
    let first_request = fixture
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert_eq!(first_request.body["model"], MIXED_MODEL);
    assert_eq!(first_request.body["image_size"], "2K");
    assert!(first_request.body.get("size").is_none());
    assert_eq!(
        first_request.body["items"].as_array().unwrap().len(),
        2,
        "server expands output_count"
    );
    assert_eq!(first_request.body["items"][0]["output_count"], 3);
    let reference = &first_request.body["items"][1]["reference_images"][0];
    assert_eq!(reference["mime_type"], "image/png");
    assert_eq!(
        STANDARD
            .decode(reference["data"].as_str().unwrap())
            .unwrap(),
        png()
    );
    assert!(reference.get("file_uri").is_none());
    close(client, server).await;
    // A new preferred model must not change the saved model during explicit replay.
    let mut reordered = batch_catalog();
    let mut new_preferred = reordered["models"][1].clone();
    new_preferred["id"] = json!("synthetic-new-preferred");
    new_preferred["preference_rank"] = json!(0);
    reordered["models"]
        .as_array_mut()
        .unwrap()
        .insert(0, new_preferred);
    *fixture.state.catalog.lock().unwrap() = reordered;
    // Rebuild the service; an explicit replay must reconstruct identical defaults.
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), false).await;
    assert_eq!(fixture.posts(), 1, "construction does not replay");
    let replay = call(&client, "submit_image_batch", input.clone()).await;
    assert_ne!(replay.is_error, Some(true), "{}", metadata(&replay));
    assert_eq!(metadata(&replay)["batch_id"], BATCH);
    let requests: Vec<_> = fixture
        .requests()
        .into_iter()
        .filter(|r| r.method == "POST")
        .collect();
    assert_eq!(requests[0].body, requests[1].body);
    assert_eq!(requests[0].idempotency, requests[1].idempotency);
    assert_eq!(
        (fixture.posts(), fixture.creates(), fixture.holds()),
        (2, 1, 1)
    );
    let mut changed = input;
    changed["items"][0]["prompt"] = json!("different content");
    let conflict = call(&client, "submit_image_batch", changed).await;
    assert_eq!(conflict.is_error, Some(true));
    assert!(
        metadata(&conflict)["code"]
            .as_str()
            .unwrap()
            .contains("idempotency")
    );
    assert_eq!(metadata(&conflict)["idempotency_key"], "explicit-replay");
    assert_safe(&conflict);
    assert_eq!(
        (fixture.posts(), fixture.creates(), fixture.holds()),
        (3, 1, 1)
    );
    assert_eq!(
        fixture
            .requests()
            .iter()
            .filter(|r| r.path.contains("/billing"))
            .count(),
        0
    );
    close(client, server).await;
}

#[tokio::test]
async fn batch_selection_and_every_item_limit_reject_before_post() {
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reference.png");
    std::fs::write(&path, png()).unwrap();
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), false).await;
    let base = batch_args("validation");
    let mut cases = vec![];
    let mut no_permission = batch_catalog();
    no_permission["can_submit"] = json!(false);
    no_permission["reason_code"] = json!("insufficient_balance");
    cases.push((no_permission, base.clone()));
    let mut wrong_model = base.clone();
    wrong_model["model"] = json!("synthetic-generation-only");
    wrong_model["items"]
        .as_array_mut()
        .unwrap()
        .push(json!({"custom_id":"edit","prompt":"x","reference_image_paths":[path]}));
    cases.push((batch_catalog(), wrong_model));
    let mut size = base.clone();
    size["size"] = json!("1024x1024");
    cases.push((batch_catalog(), size));
    for count in [0, 5] {
        let mut input = base.clone();
        input["items"][0]["output_count"] = json!(count);
        cases.push((batch_catalog(), input));
    }
    let mut too_many = base.clone();
    too_many["items"] = json!(
        (0..11)
            .map(|n| json!({"custom_id":format!("i{n}"),"prompt":"x"}))
            .collect::<Vec<_>>()
    );
    cases.push((batch_catalog(), too_many));
    let mut outputs = base.clone();
    outputs["items"] = json!(
        (0..6)
            .map(|n| json!({"custom_id":format!("i{n}"),"prompt":"x","output_count":4}))
            .collect::<Vec<_>>()
    );
    cases.push((batch_catalog(), outputs));
    let mut unicode = base.clone();
    unicode["items"]
        .as_array_mut()
        .unwrap()
        .push(json!({"custom_id":"later","prompt":"图".repeat(334)}));
    cases.push((batch_catalog(), unicode));
    let mut refs = base.clone();
    refs["items"][0]["reference_image_paths"] = json!([path, path, path, path]);
    cases.push((batch_catalog(), refs));
    let mut weighted = base.clone();
    weighted["items"] = json!([
        {"custom_id":"a","prompt":"x","output_count":3,"reference_image_paths":[path,path,path]},
        {"custom_id":"b","prompt":"x","output_count":2,"reference_image_paths":[path,path]}]);
    cases.push((batch_catalog(), weighted));
    let mut byte_catalog = batch_catalog();
    byte_catalog["models"][1]["capabilities"][1]["input_limits"]["max_batch_input_bytes"] =
        json!(png().len() * 2 - 1);
    let mut bytes = base.clone();
    bytes["items"][0]["output_count"] = json!(2);
    bytes["items"][0]["reference_image_paths"] = json!([path]);
    cases.push((byte_catalog, bytes));
    let mut mime = batch_catalog();
    mime["models"][1]["capabilities"][1]["input_limits"]["mime_types"] = json!(["image/jpeg"]);
    let mut edit = base.clone();
    edit["items"][0]["reference_image_paths"] = json!([path]);
    cases.push((mime, edit.clone()));
    let mut size_limit = batch_catalog();
    size_limit["models"][1]["capabilities"][1]["input_limits"]["max_image_bytes"] =
        json!(png().len() - 1);
    cases.push((size_limit, edit));
    let mut relative = base;
    relative["items"][0]["reference_image_paths"] = json!(["relative.png"]);
    cases.push((batch_catalog(), relative));
    for (index, (catalog, input)) in cases.into_iter().enumerate() {
        *fixture.state.catalog.lock().unwrap() = catalog;
        let result = call(&client, "submit_image_batch", input).await;
        assert_eq!(
            result.is_error,
            Some(true),
            "case {index}: {}",
            metadata(&result)
        );
        assert_eq!(
            metadata(&result)["submission"],
            "not_submitted",
            "case {index}"
        );
        assert_eq!(fixture.posts(), 0, "case {index}");
        if index == 1 {
            assert_eq!(
                metadata(&result)["selection"]["alternatives"],
                json!([MIXED_MODEL]),
                "alternatives must satisfy every item"
            );
        }
        assert_safe(&result);
    }
    close(client, server).await;
}

#[tokio::test]
async fn encoded_batch_json_limit_counts_base64_and_accepts_exact_body() {
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("reference.png");
    std::fs::write(&path, png()).unwrap();
    let mut input = batch_args("encoded-size");
    input["task_name"] = json!("Stable explicit name");
    input["items"][0]["reference_image_paths"] = json!([path]);
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), false).await;
    let first = call(&client, "submit_image_batch", input.clone()).await;
    assert_ne!(first.is_error, Some(true), "{}", metadata(&first));
    let bytes = fixture
        .requests()
        .iter()
        .find(|r| r.method == "POST")
        .unwrap()
        .body_len;
    assert!(bytes > png().len() + STANDARD.encode(png()).len());
    for (limit, expected_posts) in [(bytes - 1, 1), (bytes, 2)] {
        let mut catalog = batch_catalog();
        for cap in catalog["models"][1]["capabilities"].as_array_mut().unwrap() {
            cap["input_limits"]["max_request_bytes"] = json!(limit);
        }
        *fixture.state.catalog.lock().unwrap() = catalog;
        let result = call(&client, "submit_image_batch", input.clone()).await;
        assert_eq!(
            result.is_error == Some(true),
            limit < bytes,
            "{}",
            metadata(&result)
        );
        assert_eq!(fixture.posts(), expected_posts);
    }
    assert_eq!((fixture.creates(), fixture.holds()), (1, 1));
    close(client, server).await;
}

#[tokio::test]
async fn generated_key_is_saved_before_post_and_accepted_storage_failure_keeps_id() {
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("tasks"), b"not a directory").unwrap();
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), false).await;
    let result = call(
        &client,
        "submit_image_batch",
        json!({"items":[{"custom_id":"x","prompt":"x"}]}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(metadata(&result)["submission"], "not_submitted");
    assert_eq!(fixture.posts(), 0);
    close(client, server).await;
    let directory = tempfile::tempdir().unwrap();
    *fixture.state.break_record_after_acceptance.lock().unwrap() =
        Some(directory.path().join("tasks/batches"));
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), true).await;
    let result = call(
        &client,
        "submit_image_batch",
        json!({"items":[{"custom_id":"x","prompt":"x"}]}),
    )
    .await;
    assert_ne!(result.is_error, Some(true), "{}", metadata(&result));
    assert_eq!(metadata(&result)["submission"], "accepted");
    assert_eq!(metadata(&result)["batch_id"], BATCH);
    assert_eq!(metadata(&result)["recovery"]["status"], "failed");
    assert!(
        !metadata(&result)["idempotency_key"]
            .as_str()
            .unwrap()
            .is_empty()
    );
    assert_eq!(fixture.posts(), 1);
    let prior = directory.path().join("tasks/batches.before-acceptance");
    let file = std::fs::read_dir(prior)
        .unwrap()
        .next()
        .unwrap()
        .unwrap()
        .path();
    let record: Value = serde_json::from_slice(&std::fs::read(file).unwrap()).unwrap();
    assert_eq!(
        record["idempotency_key"],
        metadata(&result)["idempotency_key"]
    );
    assert!(record["batch_id"].is_null());
    assert_safe(&record);
    close(client, server).await;
}

#[tokio::test]
async fn submit_timeout_disconnect_redirect_and_cancelled_wait_never_retry() {
    for mode in [
        BatchReply::Delay,
        BatchReply::Disconnect,
        BatchReply::Redirect("http://127.0.0.1:1/no-replay".into()),
    ] {
        let fixture = BatchFixture::new().await;
        let directory = tempfile::tempdir().unwrap();
        *fixture.state.submit_reply.lock().unwrap() = mode;
        let (client, server) =
            connect_batch(&fixture, &directory, Duration::from_millis(100), true).await;
        let result = call(&client, "submit_image_batch", batch_args("unknown-submit")).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["submission"], "unknown");
        assert_eq!(metadata(&result)["idempotency_key"], "unknown-submit");
        assert_eq!(fixture.posts(), 1);
        assert_eq!(recovery_records(&directory).len(), 1);
        assert_safe(&result);
        close(client, server).await;
    }
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    *fixture.state.submit_reply.lock().unwrap() = BatchReply::Delay;
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(10), true).await;
    {
        let request = client.call_tool_once(
            CallToolRequestParams::new("submit_image_batch")
                .with_arguments(batch_args("cancel-wait").as_object().unwrap().clone()),
        );
        tokio::pin!(request);
        tokio::select! { _=&mut request=>panic!("request should wait"),_=fixture.wait_for_request(false)=>{} }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fixture.posts(), 1);
    assert_eq!(
        recovery_records(&directory)[0]["idempotency_key"],
        "cancel-wait"
    );
    close(client, server).await;
}

#[tokio::test]
async fn list_and_items_use_offset_pages_without_catalog_and_preserve_expanded_ids() {
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    fixture.state.catalog.lock().unwrap()["can_submit"] = json!(false);
    *fixture.state.jobs.lock().unwrap() = (0..22)
        .map(|i| {
            let mut v = batch_json("completed");
            v["id"] = json!(format!("batch-{i}"));
            v
        })
        .collect();
    *fixture.state.batch.lock().unwrap() = batch_json("completed");
    *fixture.state.items.lock().unwrap() = (0..22)
        .map(|i| {
            batch_item(
                &format!("server-expanded-{i:02}"),
                if i == 1 { "failed" } else { "succeeded" },
            )
        })
        .collect();
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), false).await;
    let list = call(&client, "get_image_batch", json!({})).await;
    assert_ne!(list.is_error, Some(true), "{}", metadata(&list));
    assert_eq!(metadata(&list)["data"].as_array().unwrap().len(), 20);
    assert_eq!(metadata(&list)["has_more"], true);
    assert_eq!(metadata(&list)["next_cursor"], "20");
    let tail = call(
        &client,
        "get_image_batch",
        json!({"cursor":metadata(&list)["next_cursor"]}),
    )
    .await;
    assert_eq!(metadata(&tail)["data"].as_array().unwrap().len(), 2);
    assert_eq!(metadata(&tail)["has_more"], false);
    assert!(metadata(&tail)["next_cursor"].is_null());
    let page = call(
        &client,
        "get_image_batch",
        json!({"batch_id":BATCH,"limit":2,"cursor":"7"}),
    )
    .await;
    assert_ne!(page.is_error, Some(true), "{}", metadata(&page));
    assert_eq!(
        metadata(&page)["items"]["data"][0]["custom_id"],
        SERVER_ITEM
    );
    assert_eq!(metadata(&page)["items"]["next_cursor"], "9");
    assert_eq!(metadata(&page)["items"]["has_more"], true);
    assert_no_fees(metadata(&page));
    assert_eq!(metadata(&page)["fail_count"], 1);
    assert_eq!(fixture.count("GET", "batches"), 2);
    assert_eq!(fixture.count("GET", &format!("batches/{BATCH}")), 1);
    assert_eq!(fixture.count("GET", &format!("batches/{BATCH}/items")), 1);
    assert_eq!((fixture.posts(), fixture.catalog_gets()), (0, 0));
    let requests = fixture.requests();
    assert_eq!(pagination(&requests[0].query, 20, 100), (0, 20));
    assert_eq!(pagination(&requests[1].query, 20, 100), (20, 20));
    close(client, server).await;
}

#[tokio::test]
async fn invalid_read_branch_arguments_make_no_requests() {
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), false).await;
    for input in [
        json!({"custom_id":"x"}),
        json!({"image_index":0}),
        json!({"batch_id":BATCH,"image_index":0}),
        json!({"batch_id":BATCH,"custom_id":SERVER_ITEM,"limit":2}),
        json!({"batch_id":BATCH,"custom_id":SERVER_ITEM,"cursor":"0"}),
        json!({"cursor":"last-id"}),
        json!({"cursor":"-1"}),
    ] {
        let result = call(&client, "get_image_batch", input).await;
        assert_eq!(result.is_error, Some(true), "{}", metadata(&result));
    }
    let result = call(
        &client,
        "cancel_image_task",
        json!({"task_id":"single-async"}),
    )
    .await;
    assert_eq!(result.is_error, Some(true));
    assert!(fixture.requests().is_empty());
    close(client, server).await;
}

#[tokio::test]
async fn selected_content_has_current_key_actual_index_and_no_external_auth_or_resubmission() {
    let fixture = BatchFixture::new().await;
    let target = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    *fixture.state.batch.lock().unwrap() = batch_json("completed");
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), true).await;
    let input = json!({"batch_id":BATCH,"custom_id":SERVER_ITEM,"image_index":1});
    let image = call(&client, "get_image_batch", input.clone()).await;
    assert_image(&image);
    let caption = serde_json::to_string(&image).unwrap();
    assert!(caption.contains("图片已返回；任务状态见结果信息。"));
    assert!(!caption.contains("金额"));
    assert_eq!(metadata(&image)["batch_id"], BATCH);
    assert_eq!(metadata(&image)["custom_id"], SERVER_ITEM);
    assert_eq!(metadata(&image)["image_index"], 1);
    let content = fixture
        .requests()
        .into_iter()
        .find(|r| r.path.ends_with("/content"))
        .unwrap();
    assert!(content.auth);
    assert!(
        content
            .path
            .ends_with(&format!("/items/{SERVER_ITEM}/content"))
    );
    assert_eq!(content.query, "image_index=1");
    assert_eq!(
        fixture.count("GET", &format!("batches/{BATCH}/items")),
        0,
        "content does not list or auto-download other items"
    );
    *fixture.state.content_reply.lock().unwrap() = BatchReply::Redirect(format!(
        "{}images/batches/{BATCH}/items/{SERVER_ITEM}/content",
        target.base
    ));
    let redirect = call(&client, "get_image_batch", input.clone()).await;
    assert_eq!(redirect.is_error, Some(true));
    assert_eq!(metadata(&redirect)["batch_id"], BATCH);
    assert_eq!(metadata(&redirect)["custom_id"], SERVER_ITEM);
    assert_eq!(metadata(&redirect)["status"], "completed");
    assert_eq!(metadata(&redirect)["query_state"], "retrieved");
    assert!(target.requests().is_empty());
    for (status, code) in [
        (StatusCode::GONE, "BATCH_IMAGE_OUTPUT_DELETED"),
        (StatusCode::NOT_FOUND, "BATCH_IMAGE_ITEM_NOT_FOUND"),
    ] {
        *fixture.state.content_reply.lock().unwrap() =
            BatchReply::Json(status, json!({"error":{"code":code,"message":SENSITIVE}}));
        let result = call(&client, "get_image_batch", input.clone()).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["batch_id"], BATCH);
        assert_eq!(metadata(&result)["custom_id"], SERVER_ITEM);
        assert_ne!(metadata(&result)["submission"], "not_submitted");
        assert!(
            !metadata(&result)["message"]
                .as_str()
                .unwrap()
                .contains("费用")
        );
        assert_safe(&result);
    }
    assert_eq!((fixture.posts(), fixture.catalog_gets()), (0, 0));
    assert_eq!(
        fixture.count(
            "GET",
            &format!("batches/{BATCH}/items/{SERVER_ITEM}/content")
        ),
        4
    );
    close(client, server).await;
}

#[tokio::test]
async fn large_content_preserves_original_preview_budget_and_metadata_on_cache_failure() {
    use rmcp::model::{RequestId, ServerJsonRpcMessage, ServerResult};
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let bytes = noise_png(2304, 2048);
    assert!(STANDARD.encode(&bytes).len() > 8 * 1024 * 1024);
    *fixture.state.image.lock().unwrap() = bytes.clone();
    *fixture.state.batch.lock().unwrap() = batch_json("completed");
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(10), true).await;
    let input = json!({"batch_id":BATCH,"custom_id":SERVER_ITEM});
    let result = call(&client, "get_image_batch", input.clone()).await;
    assert_image(&result);
    assert_eq!(metadata(&result)["image_index"], 0);
    let original = &metadata(&result)["local_files"][0];
    assert_eq!(original["inline_representation"], "preview");
    let path = Path::new(original["path"].as_str().unwrap());
    assert!(path.starts_with(directory.path().join("images")));
    assert!(!path.to_string_lossy().contains("untrusted"));
    assert_eq!(std::fs::read(path).unwrap(), bytes);
    let expires = original["expires_at"].as_u64().unwrap();
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs();
    assert!((now + 86390..=now + 86400).contains(&expires));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        assert_eq!(
            std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
    let wire =
        ServerJsonRpcMessage::response(ServerResult::CallToolResult(result), RequestId::Number(1));
    assert!(serde_json::to_vec(&wire).unwrap().len() < 8 * 1024 * 1024);
    close(client, server).await;
    let directory = tempfile::tempdir().unwrap();
    std::fs::write(directory.path().join("images"), b"not a directory").unwrap();
    let (client, server) =
        connect_batch(&fixture, &directory, Duration::from_secs(10), false).await;
    let failed = call(&client, "get_image_batch", input).await;
    assert_eq!(failed.is_error, Some(true));
    assert_eq!(metadata(&failed)["batch_id"], BATCH);
    assert_eq!(metadata(&failed)["custom_id"], SERVER_ITEM);
    assert_eq!(metadata(&failed)["status"], "completed");
    assert_eq!(metadata(&failed)["query_state"], "retrieved");
    assert_ne!(metadata(&failed)["submission"], "not_submitted");
    assert_eq!(fixture.posts(), 0);
    close(client, server).await;
}

#[tokio::test]
async fn cancel_states_limits_failures_and_unknown_results_never_poll_or_repeat() {
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let (client, server) =
        connect_batch(&fixture, &directory, Duration::from_millis(100), false).await;
    for (status, state) in [
        ("running", "requested"),
        ("completed", "already_finished"),
        ("cancelled", "completed"),
    ] {
        *fixture.state.batch.lock().unwrap() = batch_json(status);
        let result = call(&client, "cancel_image_task", json!({"batch_id":BATCH})).await;
        assert_ne!(result.is_error, Some(true), "{}", metadata(&result));
        assert_eq!(metadata(&result)["status"], status);
        assert_eq!(metadata(&result)["cancel_state"], state);
        assert_no_fees(metadata(&result));
    }
    for (reply, expected_state, expected_code) in [
        (
            BatchReply::Json(
                StatusCode::FORBIDDEN,
                json!({"error":{"code":"insufficient_balance","message":SENSITIVE}}),
            ),
            "rejected",
            "insufficient_balance",
        ),
        (
            BatchReply::Json(
                StatusCode::BAD_GATEWAY,
                json!({"error":{"code":"BATCH_IMAGE_CANCEL_FAILED","message":SENSITIVE}}),
            ),
            "failed",
            "batch_cancel_failed",
        ),
        (
            BatchReply::Json(
                StatusCode::BAD_GATEWAY,
                json!({"error":{"code":"UNCLASSIFIED_PROXY_ERROR","message":SENSITIVE}}),
            ),
            "unknown",
            "upstream_failed",
        ),
        (BatchReply::Delay, "unknown", "timeout"),
        (BatchReply::Disconnect, "unknown", "transport_failed"),
        (
            BatchReply::Redirect("http://127.0.0.1:1/no-cancel-replay".into()),
            "unknown",
            "redirect_refused",
        ),
    ] {
        *fixture.state.cancel_reply.lock().unwrap() = reply;
        let result = call(&client, "cancel_image_task", json!({"batch_id":BATCH})).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["batch_id"], BATCH);
        assert_ne!(metadata(&result)["submission"], "not_submitted");
        assert_eq!(metadata(&result)["cancel_state"], expected_state);
        assert_eq!(metadata(&result)["code"], expected_code);
        assert_safe(&result);
    }
    assert_eq!(fixture.count("POST", &format!("batches/{BATCH}/cancel")), 9);
    assert_eq!(fixture.catalog_gets(), 0);
    assert_eq!(fixture.posts(), 0);
    assert!(fixture.requests().iter().all(|r| r.method == "POST"));
    close(client, server).await;
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    *fixture.state.cancel_reply.lock().unwrap() = BatchReply::Delay;
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(10), true).await;
    {
        let request = client.call_tool_once(
            CallToolRequestParams::new("cancel_image_task")
                .with_arguments(json!({"batch_id":BATCH}).as_object().unwrap().clone()),
        );
        tokio::pin!(request);
        tokio::select! {_=&mut request=>panic!("cancel should wait"),_=fixture.wait_for_request(true)=>{}}
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fixture.count("POST", &format!("batches/{BATCH}/cancel")), 1);
    assert_eq!(fixture.posts(), 0);
    close(client, server).await;
}

#[tokio::test]
#[ignore = "requires B3b real Go batch handler/service artifact"]
async fn server_generated_batch_dtos_cross_http_mcp_with_original_states_pages_and_content() {
    let source =
        std::env::var("TOKEMULA_B3B_GO_ARTIFACT").expect("explicit synthetic Go artifact path");
    let artifact: Value = serde_json::from_slice(&std::fs::read(source).unwrap()).unwrap();
    let exchanges = artifact["exchanges"].as_array().unwrap();
    let exchange = |name: &str| {
        exchanges
            .iter()
            .find(|e| e["name"] == name)
            .unwrap()
            .clone()
    };
    // These are producer-side counters, separately reported from this consumer's HTTP calls.
    assert_eq!(artifact["replay_counts"]["business_create_calls"], 1);
    assert_eq!(artifact["replay_counts"]["reserve_calls"], 1);
    assert_eq!(artifact["replay_counts"]["provider_submits"], 1);
    assert_eq!(artifact["http_counts"]["submit_post"], 3);
    assert_eq!(
        exchange("submit")["body"]["id"],
        exchange("replay_same")["body"]["id"]
    );
    assert_eq!(exchange("replay_conflict")["http_status"], 409);
    assert_eq!(
        artifact["expanded_custom_ids"],
        json!(["cover_01", "cover_02", "detail"])
    );
    assert_eq!(artifact["request"]["items"].as_array().unwrap().len(), 2);
    assert_eq!(
        artifact["normalized_request"]["items"]
            .as_array()
            .unwrap()
            .len(),
        3
    );
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    *fixture.state.items_override.lock().unwrap() = Some(exchange("items_page_1")["body"].clone());
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), true).await;
    let check_batch = |actual: &Value, expected: &Value| {
        assert_eq!(actual["batch_id"], expected["id"]);
        for field in [
            "status",
            "task_name",
            "model",
            "item_count",
            "success_count",
            "fail_count",
            "created_at",
            "submitted_at",
            "settled_at",
            "output_expires_at",
            "output_deleted_at",
        ] {
            if let Some(value) = expected.get(field) {
                assert_eq!(&actual[field], value, "real Go field {field}");
            }
        }
        assert_no_fees(actual);
    };
    for name in [
        "submit",
        "detail_completed",
        "detail_submitted",
        "detail_running",
        "detail_indexing",
        "detail_settling",
        "detail_failed",
        "detail_cancelled",
        "detail_output_deleted",
        "detail_legacy_hold",
    ] {
        let expected = exchange(name)["body"].clone();
        *fixture.state.batch.lock().unwrap() = expected.clone();
        let result = call(
            &client,
            "get_image_batch",
            json!({"batch_id":expected["id"], "limit":2}),
        )
        .await;
        assert_ne!(result.is_error, Some(true), "{name}: {}", metadata(&result));
        check_batch(metadata(&result), &expected);
        assert_safe(&result);
    }
    let detail = exchange("detail_completed")["body"].clone();
    let batch_id = detail["id"].as_str().unwrap().to_owned();
    *fixture.state.batch.lock().unwrap() = detail.clone();
    for (name, cursor, next_cursor) in [
        ("items_page_1", "0", Some("2")),
        ("items_page_2", "2", None),
    ] {
        let expected = exchange(name)["body"].clone();
        *fixture.state.items_override.lock().unwrap() = Some(expected.clone());
        let result = call(
            &client,
            "get_image_batch",
            json!({"batch_id":batch_id,"limit":2,"cursor":cursor}),
        )
        .await;
        assert_ne!(result.is_error, Some(true), "{name}: {}", metadata(&result));
        let items = &metadata(&result)["items"];
        assert_eq!(
            items["data"].as_array().unwrap().len(),
            expected["data"].as_array().unwrap().len()
        );
        for (actual, source) in items["data"]
            .as_array()
            .unwrap()
            .iter()
            .zip(expected["data"].as_array().unwrap())
        {
            for field in [
                "custom_id",
                "status",
                "mime_type",
                "file_extension",
                "image_count",
            ] {
                assert_eq!(actual[field], source[field], "real Go item field {field}");
            }
            assert_eq!(actual["error"].is_null(), source["error"].is_null());
        }
        assert_eq!(items["has_more"], expected["has_more"]);
        assert_eq!(items["next_cursor"], json!(next_cursor));
    }
    for (name, cursor) in [("list_page_1", "0"), ("list_page_2", "2")] {
        let expected = exchange(name)["body"].clone();
        *fixture.state.list_override.lock().unwrap() = Some(expected.clone());
        let result = call(
            &client,
            "get_image_batch",
            json!({"cursor":cursor,"limit":2}),
        )
        .await;
        assert_ne!(result.is_error, Some(true), "{name}: {}", metadata(&result));
        assert_eq!(metadata(&result)["has_more"], expected["has_more"]);
        for (actual, source) in metadata(&result)["data"]
            .as_array()
            .unwrap()
            .iter()
            .zip(expected["data"].as_array().unwrap())
        {
            check_batch(actual, source);
        }
        assert_eq!(
            metadata(&result)["data"].as_array().unwrap().len(),
            expected["data"].as_array().unwrap().len()
        );
    }
    let content = exchange("content_image_1");
    let bytes = STANDARD
        .decode(content["content_base64"].as_str().unwrap())
        .unwrap();
    *fixture.state.image.lock().unwrap() = bytes.clone();
    *fixture.state.content_type.lock().unwrap() = content["content_type"].as_str().unwrap().into();
    let result = call(
        &client,
        "get_image_batch",
        json!({"batch_id":batch_id,"custom_id":"cover_01","image_index":1}),
    )
    .await;
    assert_ne!(result.is_error, Some(true), "{}", metadata(&result));
    check_batch(metadata(&result), &detail);
    let encoded = serde_json::to_value(&result).unwrap();
    let image = encoded["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|c| c["type"] == "image")
        .unwrap();
    assert_eq!(image["mimeType"], "image/jpeg");
    assert_eq!(
        STANDARD.decode(image["data"].as_str().unwrap()).unwrap(),
        bytes
    );
    assert_eq!(metadata(&result)["custom_id"], "cover_01");
    assert_eq!(metadata(&result)["image_index"], 1);
    for (name, detail_name, custom_id) in [
        ("content_failed_item", "detail_completed", "cover_02"),
        (
            "content_output_deleted",
            "detail_output_deleted",
            "cover_01",
        ),
    ] {
        let error = exchange(name);
        let detail = exchange(detail_name)["body"].clone();
        *fixture.state.batch.lock().unwrap() = detail.clone();
        *fixture.state.content_reply.lock().unwrap() = BatchReply::Json(
            StatusCode::from_u16(error["http_status"].as_u64().unwrap() as u16).unwrap(),
            error["body"].clone(),
        );
        let result = call(
            &client,
            "get_image_batch",
            json!({"batch_id":detail["id"],"custom_id":custom_id}),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["batch_id"], detail["id"]);
        assert_eq!(metadata(&result)["custom_id"], custom_id);
        assert_eq!(metadata(&result)["status"], detail["status"]);
        assert_eq!(metadata(&result)["query_state"], "retrieved");
    }
    *fixture.state.batch.lock().unwrap() = detail;
    for (name, expected_state) in [
        ("cancel_requested_running", Some("requested")),
        ("cancel_completed", Some("already_finished")),
        ("cancel_failed", None),
    ] {
        let response = exchange(name);
        *fixture.state.cancel_reply.lock().unwrap() = BatchReply::Json(
            StatusCode::from_u16(response["http_status"].as_u64().unwrap() as u16).unwrap(),
            response["body"].clone(),
        );
        let result = call(&client, "cancel_image_task", json!({"batch_id":batch_id})).await;
        assert_eq!(metadata(&result)["batch_id"], batch_id);
        if let Some(state) = expected_state {
            assert_ne!(result.is_error, Some(true));
            check_batch(metadata(&result), &response["body"]);
            assert_eq!(metadata(&result)["cancel_state"], state);
        } else {
            assert_eq!(result.is_error, Some(true));
            assert_eq!(metadata(&result)["code"], "batch_cancel_failed");
            assert_eq!(metadata(&result)["cancel_state"], "failed");
        }
    }
    let requests = fixture.requests();
    assert!(requests.iter().all(|request| request.auth));
    assert_eq!((fixture.posts(), fixture.catalog_gets()), (0, 0));
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.path.contains("/billing"))
            .count(),
        0
    );
    assert_eq!(requests.iter().filter(|r| r.method == "POST").count(), 3);
    assert_eq!(
        requests
            .iter()
            .filter(|r| r.path.ends_with("/content"))
            .count(),
        3
    );
    assert!(
        requests
            .iter()
            .any(|r| r.path.ends_with("/items/cover_01/content") && r.query == "image_index=1")
    );
    println!(
        "B3b real Go consumer: {} GET, 3 cancel POST, 0 submit POST, 0 catalog, 0 fee calls; producer explicit submit/replay/conflict HTTP POST=3, business create=1/reserve=1/provider submit=1",
        requests.iter().filter(|r| r.method == "GET").count()
    );
    close(client, server).await;
}

#[tokio::test]
#[ignore = "requires B3b real Go batch submit artifact"]
async fn server_generated_batch_request_crosses_client_submit_and_exports_actual_wire() {
    let source =
        std::env::var("TOKEMULA_B3B_GO_ARTIFACT").expect("explicit synthetic Go artifact path");
    let artifact: Value = serde_json::from_slice(&std::fs::read(source).unwrap()).unwrap();
    let request = &artifact["request"];
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let model = request["model"].as_str().unwrap();
    fixture.state.catalog.lock().unwrap()["models"][1]["id"] = json!(model);
    let submit = artifact["exchanges"]
        .as_array()
        .unwrap()
        .iter()
        .find(|e| e["name"] == "submit")
        .unwrap();
    *fixture.state.batch.lock().unwrap() = submit["body"].clone();
    let items: Vec<Value> = request["items"].as_array().unwrap().iter().enumerate().map(|(index,item)| {
        let mut input = json!({"custom_id":item["custom_id"],"prompt":item["prompt"],"output_count":item["output_count"]});
        if let Some(references) = item["reference_images"].as_array() {
            let paths: Vec<_> = references.iter().enumerate().map(|(reference_index,reference)| {
                let path = directory.path().join(format!("reference-{index}-{reference_index}.png"));
                let bytes = STANDARD.decode(reference["data"].as_str().unwrap()).unwrap();
                std::fs::write(&path,bytes).unwrap();
                path
            }).collect();
            input["reference_image_paths"] = json!(paths);
        }
        input
    }).collect();
    let arguments = json!({"model":model,"task_name":request["task_name"],"size":request["image_size"],"idempotency_key":artifact["idempotency_key"],"items":items});
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), true).await;
    let result = call(&client, "submit_image_batch", arguments).await;
    assert_ne!(result.is_error, Some(true), "{}", metadata(&result));
    assert_eq!(metadata(&result)["submission"], "accepted");
    assert_eq!(metadata(&result)["batch_id"], submit["body"]["id"]);
    assert_no_fees(metadata(&result));
    let sent = fixture
        .requests()
        .into_iter()
        .find(|r| r.method == "POST")
        .unwrap();
    assert!(sent.auth);
    assert_eq!(
        sent.idempotency.as_deref(),
        artifact["idempotency_key"].as_str()
    );
    for field in ["model", "task_name", "image_size", "response_mime_type"] {
        assert_eq!(
            sent.body[field], request[field],
            "real Go request field {field}"
        );
    }
    assert_eq!(
        sent.body["items"].as_array().unwrap().len(),
        request["items"].as_array().unwrap().len(),
        "client leaves expansion to server"
    );
    for (actual, source) in sent.body["items"]
        .as_array()
        .unwrap()
        .iter()
        .zip(request["items"].as_array().unwrap())
    {
        for field in ["custom_id", "prompt", "output_count"] {
            assert_eq!(actual[field], source[field]);
        }
        let empty = Vec::new();
        let actual_references = actual["reference_images"].as_array().unwrap_or(&empty);
        let source_references = source["reference_images"].as_array().unwrap_or(&empty);
        assert_eq!(actual_references.len(), source_references.len());
        for (actual, source) in actual_references.iter().zip(source_references) {
            assert_eq!(actual["mime_type"], source["mime_type"]);
            assert_eq!(actual["data"], source["data"]);
            assert!(actual.get("file_uri").is_none());
        }
    }
    for unsupported in [
        "provider",
        "parent_batch_id",
        "aspect_ratio",
        "metadata",
        "session_id",
        "idempotency_key",
    ] {
        assert!(sent.body.get(unsupported).is_none());
    }
    if let Ok(path) = std::env::var("TOKEMULA_B3B_REQUEST_EXPORT") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(
                &json!({"request": sent.body, "idempotency_key": sent.idempotency}),
            )
            .unwrap(),
        )
        .unwrap();
    }
    assert_eq!(
        (
            fixture.catalog_gets(),
            fixture.posts(),
            fixture.creates(),
            fixture.holds()
        ),
        (1, 1, 1, 1)
    );
    assert_eq!(
        fixture.requests().len(),
        2,
        "submit does not poll or query fees"
    );
    assert_eq!(recovery_records(&directory).len(), 1);
    println!(
        "B3b real Go request -> Rust submit: 1 catalog GET, 1 submit POST; captured actual JSON wire, 2 original items, original inline PNG bytes; 0 query/cancel/fee calls"
    );
    close(client, server).await;
}

#[tokio::test]
async fn regression_rejected_replay_retains_saved_identity_before_auth_catalog_and_input_checks() {
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let input = batch_args("saved-id-replay");
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), false).await;
    let accepted = call(&client, "submit_image_batch", input.clone()).await;
    assert_ne!(accepted.is_error, Some(true));
    assert_eq!(metadata(&accepted)["batch_id"], BATCH);
    let saved = recovery_records(&directory).remove(0);
    close(client, server).await;
    let check = |result: &CallToolResult| {
        assert_eq!(result.is_error, Some(true));
        let result = metadata(result);
        assert_eq!(
            result["batch_id"], BATCH,
            "known identity survives preflight refusal"
        );
        assert_eq!(result["idempotency_key"], "saved-id-replay");
        assert_eq!(result["task_name"], saved["task_name"]);
        assert_eq!(result["selected_model"], saved["selected_model"]);
        assert!(
            result["status"].is_null(),
            "saved observation is not fresh server state"
        );
        assert_ne!(result["query_state"], "retrieved");
        assert_eq!(result["submission"], "not_submitted");
        assert_safe(result);
    };
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), true).await;
    let mut exhausted = batch_catalog();
    exhausted["can_submit"] = json!(false);
    exhausted["reason_code"] = json!("insufficient_balance");
    *fixture.state.catalog.lock().unwrap() = exhausted;
    let denied = call(&client, "submit_image_batch", input.clone()).await;
    check(&denied);
    assert_eq!(fixture.posts(), 1);
    *fixture.state.catalog.lock().unwrap() = batch_catalog();
    let mut bad_name = input.clone();
    bad_name["task_name"] = json!(" ");
    let invalid = call(&client, "submit_image_batch", bad_name).await;
    check(&invalid);
    assert_eq!(fixture.posts(), 1);
    let mut missing_file = input.clone();
    missing_file["items"][0]["reference_image_paths"] =
        json!([directory.path().join("missing-image.png")]);
    let missing = call(&client, "submit_image_batch", missing_file).await;
    check(&missing);
    assert_eq!(fixture.posts(), 1);
    close(client, server).await;
    let before = fixture.requests().len();
    let unauthenticated = ImageClient::build(&fixture.base, None, Duration::from_secs(3)).unwrap();
    let (client, server) = connect_batch_client(unauthenticated, &directory, false).await;
    let no_key = call(&client, "submit_image_batch", input).await;
    check(&no_key);
    assert_eq!(metadata(&no_key)["code"], "credential_unavailable");
    assert_eq!(
        fixture.requests().len(),
        before,
        "credential failure performs no network request"
    );
    assert_eq!(
        fixture.posts(),
        1,
        "all four explicit rejected replays add zero POST"
    );
    assert_eq!(
        recovery_records(&directory)[0],
        saved,
        "preflight refusal leaves the accepted record intact"
    );
    close(client, server).await;
}

#[tokio::test]
async fn regression_server_custom_ids_with_query_or_fragment_characters_are_preserved_and_encoded()
{
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    *fixture.state.batch.lock().unwrap() = batch_json("completed");
    *fixture.state.items.lock().unwrap() = ["sku#2", "photo?detail", "sku/2", "sku\\2", ".", ".."]
        .into_iter()
        .map(|id| batch_item(id, "succeeded"))
        .collect();
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), false).await;
    let page = call(&client, "get_image_batch", json!({"batch_id":BATCH})).await;
    assert_ne!(page.is_error, Some(true), "{}", metadata(&page));
    let listed: Vec<_> = metadata(&page)["items"]["data"]
        .as_array()
        .unwrap()
        .iter()
        .map(|item| item["custom_id"].as_str().unwrap())
        .collect();
    assert_eq!(
        listed,
        ["sku#2", "photo?detail", "sku/2", "sku\\2", ".", ".."]
    );
    for id in [".", ".."] {
        let before = fixture.requests().len();
        let result = call(
            &client,
            "get_image_batch",
            json!({"batch_id":BATCH,"custom_id":id}),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["code"], "invalid_parameters");
        assert_eq!(metadata(&result)["submission"], "unknown");
        assert_eq!(metadata(&result)["batch_id"], BATCH);
        assert_eq!(metadata(&result)["custom_id"], id);
        assert_eq!(
            fixture.requests().len(),
            before,
            "unroutable item does not access the API"
        );
    }
    for (custom_id, encoded) in [
        ("sku#2", "sku%232"),
        ("photo?detail", "photo%3Fdetail"),
        ("sku/2", "sku%2F2"),
        ("sku\\2", "sku%5C2"),
    ] {
        let result = call(
            &client,
            "get_image_batch",
            json!({"batch_id":BATCH,"custom_id":custom_id,"image_index":1}),
        )
        .await;
        assert_image(&result);
        assert_eq!(metadata(&result)["batch_id"], BATCH);
        assert_eq!(metadata(&result)["custom_id"], custom_id);
        let requests = fixture.requests();
        let request = requests.last().unwrap();
        assert_eq!(
            request.path,
            format!("/v1/images/batches/{BATCH}/items/{encoded}/content")
        );
        assert_eq!(request.query, "image_index=1");
        assert!(request.auth);
        assert_eq!(request.method, "GET");
    }
    assert_eq!((fixture.posts(), fixture.catalog_gets()), (0, 0));
    assert_eq!(
        fixture.requests().len(),
        10,
        "one detail+items, then four detail+content reads"
    );
    close(client, server).await;
}

#[tokio::test]
async fn regression_cancel_quota_429_is_definite_rejection_with_task_identity() {
    let fixture = BatchFixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    *fixture.state.cancel_reply.lock().unwrap() = BatchReply::Json(
        StatusCode::TOO_MANY_REQUESTS,
        json!({"error":{"type":"insufficient_quota","code":"insufficient_quota","message":"synthetic quota exhausted"}}),
    );
    let (client, server) = connect_batch(&fixture, &directory, Duration::from_secs(3), false).await;
    let result = call(&client, "cancel_image_task", json!({"batch_id":BATCH})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(metadata(&result)["code"], "rate_limited");
    assert_eq!(metadata(&result)["cancel_state"], "rejected");
    assert_eq!(metadata(&result)["submission"], "unknown");
    assert_eq!(metadata(&result)["batch_id"], BATCH);
    assert_eq!(fixture.count("POST", &format!("batches/{BATCH}/cancel")), 1);
    assert_eq!((fixture.posts(), fixture.catalog_gets()), (0, 0));
    assert_eq!(
        fixture.requests().len(),
        1,
        "no retry or polling after quota refusal"
    );
    close(client, server).await;
}
