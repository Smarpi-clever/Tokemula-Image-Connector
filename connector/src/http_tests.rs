use super::*;
use crate::mcp::ImageMcp;
use axum::{
    Router,
    body::{Body, Bytes, to_bytes},
    extract::{Multipart, Request, State},
    http::StatusCode,
    response::{IntoResponse, Response as AxumResponse},
    routing::{get, post},
};
use rmcp::{
    ClientLifecycleMode, ClientServiceExt, ServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, CallToolResult, ClientConfig, ProtocolVersion,
    },
    service::{RoleClient, RunningService},
};
use std::sync::{Arc, Mutex, atomic::AtomicUsize};
use tokio::{net::TcpListener, task::JoinHandle};

type DownloadState = (Arc<Mutex<Vec<bool>>>, Vec<u8>);

const KEY: &str = "SYNTHETIC_IMAGE_SECRET_DO_NOT_ECHO";
const SENSITIVE: &str = "SYNTHETIC_RAW_PRIVATE_PAYLOAD";

#[derive(Clone)]
enum Mode {
    Image,
    Url(String),
    Delay,
    Disconnect,
    Error(StatusCode, Value),
    Redirect(String),
}

#[derive(Default)]
struct Recorded {
    path: String,
    auth_ok: bool,
    content_type: String,
    fields: Value,
    file: Vec<u8>,
    body_bytes: usize,
}

struct FixtureState {
    catalog: Mutex<Value>,
    mode: Mutex<Mode>,
    fee: Mutex<(StatusCode, Value)>,
    request_ref: Mutex<String>,
    posts: AtomicUsize,
    gets: AtomicUsize,
    fees: AtomicUsize,
    records: Mutex<Vec<Recorded>>,
    png: Vec<u8>,
    post_seen: tokio::sync::Notify,
}

struct Fixture {
    state: Arc<FixtureState>,
    base: String,
    task: JoinHandle<()>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        self.task.abort();
    }
}

fn png() -> Vec<u8> {
    let mut bytes = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(2, 2)
        .write_to(&mut bytes, ImageFormat::Png)
        .unwrap();
    bytes.into_inner()
}

impl Fixture {
    async fn new() -> Self {
        let state = Arc::new(FixtureState {
            catalog: Mutex::new(
                serde_json::from_str(include_str!("../tests/fixtures/http-catalog.json")).unwrap(),
            ),
            mode: Mutex::new(Mode::Image),
            fee: Mutex::new((
                StatusCode::OK,
                serde_json::from_str(include_str!("../tests/fixtures/http-billing.json")).unwrap(),
            )),
            request_ref: Mutex::new("fixture-request".into()),
            posts: AtomicUsize::new(0),
            gets: AtomicUsize::new(0),
            fees: AtomicUsize::new(0),
            records: Mutex::new(Vec::new()),
            png: png(),
            post_seen: tokio::sync::Notify::new(),
        });
        let router = Router::new()
            .route("/v1/images/models", get(catalog_handler))
            .route("/v1/images/generations", post(generation_handler))
            .route("/v1/images/edits", post(edit_handler))
            .route(
                "/v1/images/requests/{request_ref}/billing",
                get(fee_handler),
            )
            .with_state(state.clone());
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let task = tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });
        Self { state, base, task }
    }

    fn client(&self, timeout: Duration) -> ImageClient {
        ImageClient::build(&self.base, Some(KEY.into()), timeout).unwrap()
    }
    fn count(&self) -> usize {
        self.state.posts.load(Ordering::SeqCst)
    }
    async fn wait_for_post(&self) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let notified = self.state.post_seen.notified();
                if self.count() > 0 {
                    break;
                }
                notified.await;
            }
        })
        .await
        .unwrap();
    }
}

fn auth_ok(request: &Request) -> bool {
    request
        .headers()
        .get("authorization")
        .is_some_and(|v| v == format!("Bearer {KEY}").as_str())
}

async fn catalog_handler(State(state): State<Arc<FixtureState>>, request: Request) -> AxumResponse {
    assert!(auth_ok(&request));
    state.gets.fetch_add(1, Ordering::SeqCst);
    axum::Json(state.catalog.lock().unwrap().clone()).into_response()
}

async fn generation_handler(
    State(state): State<Arc<FixtureState>>,
    request: Request,
) -> AxumResponse {
    let record = Recorded {
        path: request.uri().path().into(),
        auth_ok: auth_ok(&request),
        content_type: request
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        ..Default::default()
    };
    let body = to_bytes(request.into_body(), MAX_METADATA_RESPONSE_BYTES)
        .await
        .unwrap();
    let record = Recorded {
        fields: serde_json::from_slice(&body).unwrap(),
        body_bytes: body.len(),
        ..record
    };
    state.records.lock().unwrap().push(record);
    respond(&state).await
}

async fn edit_handler(State(state): State<Arc<FixtureState>>, request: Request) -> AxumResponse {
    use axum::extract::FromRequest;
    let mut record = Recorded {
        path: request.uri().path().into(),
        auth_ok: auth_ok(&request),
        content_type: request
            .headers()
            .get("content-type")
            .unwrap()
            .to_str()
            .unwrap()
            .into(),
        fields: json!({}),
        ..Default::default()
    };
    let (parts, body) = request.into_parts();
    let body = to_bytes(body, MAX_METADATA_RESPONSE_BYTES).await.unwrap();
    record.body_bytes = body.len();
    let request = Request::from_parts(parts, Body::from(body));
    let mut multipart = Multipart::from_request(request, &()).await.unwrap();
    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap().to_string();
        if name == "image" {
            assert_eq!(field.content_type(), Some("image/png"));
            record.file = field.bytes().await.unwrap().to_vec();
        } else {
            record.fields[&name] = json!(field.text().await.unwrap());
        }
    }
    state.records.lock().unwrap().push(record);
    respond(&state).await
}

async fn respond(state: &FixtureState) -> AxumResponse {
    state.posts.fetch_add(1, Ordering::SeqCst);
    state.post_seen.notify_waiters();
    let mode = state.mode.lock().unwrap().clone();
    match mode {
        Mode::Delay => {
            tokio::time::sleep(Duration::from_millis(500)).await;
            image_response(state)
        }
        Mode::Disconnect => AxumResponse::builder()
            .header("content-length", "1000")
            .body(Body::from_stream(futures_util::stream::once(async {
                tokio::time::sleep(Duration::from_millis(10)).await;
                Err::<Bytes, _>(std::io::Error::new(
                    std::io::ErrorKind::ConnectionReset,
                    "synthetic dropped response",
                ))
            })))
            .unwrap(),
        Mode::Error(status, value) => (
            status,
            [(
                "x-client-request-id",
                state.request_ref.lock().unwrap().clone(),
            )],
            axum::Json(value),
        )
            .into_response(),
        Mode::Redirect(url) => AxumResponse::builder()
            .status(StatusCode::TEMPORARY_REDIRECT)
            .header("location", url)
            .body(Body::empty())
            .unwrap(),
        Mode::Url(url) => (
            [(
                "x-client-request-id",
                state.request_ref.lock().unwrap().clone(),
            )],
            axum::Json(json!({"model":"fixture-effective","data":[{"url":url}]})),
        )
            .into_response(),
        Mode::Image => image_response(state),
    }
}

fn image_response(state: &FixtureState) -> AxumResponse {
    (
        [(
            "x-client-request-id",
            state.request_ref.lock().unwrap().clone(),
        )],
        axum::Json(
            json!({"model":"fixture-effective","data":[{"b64_json":STANDARD.encode(&state.png)}]}),
        ),
    )
        .into_response()
}

async fn fee_handler(State(state): State<Arc<FixtureState>>, request: Request) -> AxumResponse {
    assert!(auth_ok(&request));
    assert_eq!(
        request.uri().path(),
        format!(
            "/v1/images/requests/{}/billing",
            state.request_ref.lock().unwrap()
        )
    );
    state.fees.fetch_add(1, Ordering::SeqCst);
    let (status, value) = state.fee.lock().unwrap().clone();
    (status, axum::Json(value)).into_response()
}

async fn connect(
    client: ImageClient,
    modern: bool,
) -> (RunningService<RoleClient, ClientConfig>, JoinHandle<()>) {
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let output_directory = tempfile::tempdir().unwrap();
    let task = tokio::spawn(async move {
        let _directory = output_directory;
        ImageMcp::with_directories(
            client,
            _directory.path().join("images"),
            _directory.path().join("tasks"),
        )
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

async fn call(
    client: &RunningService<RoleClient, ClientConfig>,
    name: &str,
    arguments: Value,
) -> CallToolResult {
    let response = client
        .call_tool_once(
            CallToolRequestParams::new(name.to_owned())
                .with_arguments(arguments.as_object().unwrap().clone()),
        )
        .await
        .unwrap();
    let CallToolResponse::Complete(result) = response else {
        panic!("expected complete B2 tool result")
    };
    result
}

fn metadata(result: &CallToolResult) -> &Value {
    result.structured_content.as_ref().unwrap()
}
fn assert_no_fees(value: &Value) {
    match value {
        Value::Object(fields) => {
            for field in [
                "pricing",
                "estimated_total",
                "actual_fee",
                "fee_labels",
                "ledger_unit",
                "ledger_amount",
                "estimated_cost",
                "hold_amount",
                "actual_cost",
                "unit_price",
            ] {
                assert!(!fields.contains_key(field), "unexpected fee field: {field}");
            }
            for child in fields.values() {
                assert_no_fees(child);
            }
        }
        Value::Array(items) => {
            for item in items {
                assert_no_fees(item);
            }
        }
        _ => {}
    }
}

fn assert_image(result: &CallToolResult) {
    assert_no_fees(metadata(result));
    assert_ne!(result.is_error, Some(true));
    let value = serde_json::to_value(result).unwrap();
    let block = value["content"]
        .as_array()
        .unwrap()
        .iter()
        .find(|v| v["type"] == "image")
        .unwrap();
    assert_eq!(block["mimeType"], "image/png");
    let bytes = STANDARD.decode(block["data"].as_str().unwrap()).unwrap();
    assert_eq!(
        image_mime(&bytes, Submission::Unknown).unwrap(),
        "image/png"
    );
}

async fn close(client: RunningService<RoleClient, ClientConfig>, task: JoinHandle<()>) {
    client.cancel().await.unwrap();
    task.await.unwrap();
}

#[tokio::test]
#[ignore = "requires a synthetic catalog response generated by the server handler"]
async fn server_generated_catalog_crosses_http_boundary() {
    let path = std::env::var_os("TOKEMULA_IMAGE_CATALOG_FIXTURE")
        .expect("set TOKEMULA_IMAGE_CATALOG_FIXTURE to the generated synthetic response");
    let wire: Value = serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
    assert_eq!(wire["schema_version"], "1");
    assert!(!wire["models"].as_array().unwrap().is_empty());
    let fixture = Fixture::new().await;
    *fixture.state.catalog.lock().unwrap() = wire.clone();

    let (catalog, visible) = fixture
        .client(Duration::from_secs(2))
        .catalog()
        .await
        .expect("the server-generated response must pass the real HTTP boundary");

    assert_eq!(visible["schema_version"], "1");
    assert_eq!(catalog.can_submit(), wire["can_submit"].as_bool().unwrap());
    assert_eq!(
        catalog.models().len(),
        wire["models"].as_array().unwrap().len()
    );
    for (source, output) in wire["models"]
        .as_array()
        .unwrap()
        .iter()
        .zip(visible["models"].as_array().unwrap())
    {
        assert_eq!(output["id"], source["id"]);
        assert_no_fees(output);
        assert_eq!(output["capabilities"], source["capabilities"]);
    }
    assert_eq!(fixture.state.gets.load(Ordering::SeqCst), 1);
    assert_eq!(fixture.count(), 0);
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn formal_catalog_preserves_capabilities_without_exposing_prices() {
    let fixture = Fixture::new().await;
    fixture.state.catalog.lock().unwrap()["models"][0]["capabilities"]
        .as_array_mut()
        .unwrap()
        .push(json!({
            "operation":"edit", "mode":"batch",
            "parameters":{"max_images":4,"sizes":[],"qualities":[]},
            "input_limits":{"max_images":14,"max_image_bytes":10485760,"mime_types":["image/png","image/jpeg","image/webp"],
                "max_request_bytes":268435456,
                "max_prompt_bytes":4096,"max_batch_items":10,"max_batch_output_images":40,
                "max_batch_input_images":1000,"max_batch_input_bytes":134217728},
            "result_formats":["authenticated_download"],"cancel_supported":true
        }));
    let source = fixture.state.catalog.lock().unwrap().clone();
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    let list = call(&client, "list_image_models", json!({})).await;
    assert_eq!(metadata(&list)["schema_version"], "1");
    assert_eq!(metadata(&list)["observed_at"], source["observed_at"]);
    assert_eq!(
        metadata(&list)["connector_version"],
        env!("CARGO_PKG_VERSION")
    );
    let wire = serde_json::to_value(&list).unwrap();
    let text = wire["content"][0]["text"].as_str().unwrap();
    let visible: Value = serde_json::from_str(text).unwrap();
    assert_eq!(visible, *metadata(&list));
    assert_no_fees(&visible);
    assert!(!text.contains(KEY));
    assert_eq!(fixture.count(), 0);
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
    for (source, visible) in source["models"]
        .as_array()
        .unwrap()
        .iter()
        .zip(metadata(&list)["models"].as_array().unwrap())
    {
        assert_no_fees(visible);
        assert_eq!(visible["capabilities"], source["capabilities"]);
    }
    for field in ["max_batch_input_images", "max_batch_input_bytes"] {
        fixture.state.catalog.lock().unwrap()["models"][0]["capabilities"][1]["input_limits"]
            [field] = json!(0);
        let rejected = call(&client, "list_image_models", json!({})).await;
        assert_eq!(metadata(&rejected)["code"], "invalid_catalog");
        *fixture.state.catalog.lock().unwrap() = source.clone();
    }
    fixture.state.catalog.lock().unwrap()["models"][0]["capabilities"][1]["operation"] =
        json!("generation");
    let rejected = call(&client, "list_image_models", json!({})).await;
    assert_eq!(metadata(&rejected)["code"], "invalid_catalog");
    *fixture.state.catalog.lock().unwrap() = source.clone();
    assert_eq!(fixture.count(), 0);
    let result = call(
        &client,
        "generate_image",
        json!({"prompt":"synthetic", "model":"sample-fast"}),
    )
    .await;
    assert_image(&result);
    assert_eq!(metadata(&result)["selected_model"], "sample-fast");
    assert_no_fees(metadata(&result));
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.count(), 1);
    close(client, task).await;
}

#[tokio::test]
async fn formal_catalog_ignores_unused_prices_without_blocking_selection() {
    let fixture = Fixture::new().await;
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    for pricing in [
        json!({"status":"unknown"}),
        json!({"status":"hidden"}),
        json!({"status":"available","rates":"invalid and unused"}),
        Value::Null,
    ] {
        fixture.state.catalog.lock().unwrap()["models"][1]["pricing"] = pricing.clone();
        let result = call(&client, "generate_image", json!({"prompt":"synthetic"})).await;
        assert_image(&result);
        assert_no_fees(metadata(&result));
        assert_eq!(metadata(&result)["selected_model"], "sample-26");
    }
    for model in fixture.state.catalog.lock().unwrap()["models"]
        .as_array_mut()
        .unwrap()
    {
        model.as_object_mut().unwrap().remove("pricing");
    }
    let list = call(&client, "list_image_models", json!({})).await;
    assert_no_fees(metadata(&list));
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.count(), 4);
    close(client, task).await;
}

#[tokio::test]
async fn formal_catalog_rejects_invalid_capabilities_without_posting() {
    let fixture = Fixture::new().await;
    let original = fixture.state.catalog.lock().unwrap().clone();
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    for (pointer, value) in [
        ("/schema_version", json!("b2-dev-1")),
        ("/models/0", json!("malformed")),
        ("/models/0/capabilities/0/result_formats", json!([])),
        (
            "/models/0/capabilities/0/input_limits/max_request_bytes",
            json!(0),
        ),
        ("/models/0/capabilities/0/cancel_supported", json!(true)),
        (
            "/models/3/capabilities/0/input_limits/max_image_bytes",
            json!(0),
        ),
    ] {
        let mut invalid = original.clone();
        *invalid.pointer_mut(pointer).unwrap() = value;
        *fixture.state.catalog.lock().unwrap() = invalid;
        let result = call(&client, "generate_image", json!({"prompt":"synthetic"})).await;
        assert_eq!(metadata(&result)["code"], "invalid_catalog", "{pointer}");
        assert_eq!(metadata(&result)["submission"], "not_submitted");
    }
    assert_eq!(fixture.count(), 0);
    close(client, task).await;
}

#[tokio::test]
async fn server_edit_limits_and_empty_parameter_lists_are_enforced_before_posting() {
    let fixture = Fixture::new().await;
    let original = fixture.state.catalog.lock().unwrap().clone();
    let directory = tempfile::tempdir().unwrap();
    let path = directory.path().join("synthetic.png");
    std::fs::write(&path, &fixture.state.png).unwrap();
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    for (pointer, value) in [
        (
            "/input_limits/max_image_bytes",
            json!(fixture.state.png.len() - 1),
        ),
        ("/input_limits/mime_types", json!(["image/jpeg"])),
        ("/parameters/sizes", json!([])),
        ("/parameters/qualities", json!([])),
    ] {
        let mut catalog = original.clone();
        *catalog["models"][3]["capabilities"][0]
            .pointer_mut(pointer)
            .unwrap() = value;
        *fixture.state.catalog.lock().unwrap() = catalog;
        let result = call(&client, "edit_image", json!({"prompt":"synthetic", "image_path":path,"size":"1024x1024","quality":"standard"})).await;
        assert_eq!(result.is_error, Some(true), "{pointer}");
        assert_eq!(metadata(&result)["submission"], "not_submitted");
    }
    assert_eq!(fixture.count(), 0);
    close(client, task).await;
}

#[tokio::test]
async fn encoded_json_and_multipart_request_limits_reject_before_post_and_allow_exact_size() {
    for edit in [false, true] {
        let fixture = Fixture::new().await;
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("synthetic.png");
        std::fs::write(&path, &fixture.state.png).unwrap();
        let prompt = "synthetic \"quoted\"\\line\n中文";
        let model_index = if edit { 3 } else { 1 };
        let tool = if edit { "edit_image" } else { "generate_image" };
        let mut args = json!({"prompt":prompt,"model":if edit {"sample-edit-sync"} else {"sample-26"},
            "size":"1024x1024","quality":"standard","n":1});
        if edit {
            args["image_path"] = json!(path);
        }
        let raw_input_bytes = if edit {
            fixture.state.png.len()
        } else {
            prompt.len()
        };
        {
            let mut catalog = fixture.state.catalog.lock().unwrap();
            let limits = &mut catalog["models"][model_index]["capabilities"][0]["input_limits"];
            limits["max_request_bytes"] = json!(raw_input_bytes);
            if edit {
                limits["max_image_bytes"] = json!(raw_input_bytes);
            }
        }
        let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
        let rejected = call(&client, tool, args.clone()).await;
        assert_eq!(metadata(&rejected)["code"], "invalid_parameters");
        assert_eq!(metadata(&rejected)["submission"], "not_submitted");
        assert_eq!(fixture.count(), 0);
        assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);

        fixture.state.catalog.lock().unwrap()["models"][model_index]["capabilities"][0]["input_limits"]
            ["max_request_bytes"] = json!(16384);
        let result = call(&client, tool, args.clone()).await;
        assert_image(&result);
        let encoded_bytes = fixture.state.records.lock().unwrap()[0].body_bytes;
        assert!(encoded_bytes > raw_input_bytes);
        assert_eq!(fixture.count(), 1);

        fixture.state.catalog.lock().unwrap()["models"][model_index]["capabilities"][0]["input_limits"]
            ["max_request_bytes"] = json!(encoded_bytes - 1);
        let rejected = call(&client, tool, args.clone()).await;
        assert_eq!(metadata(&rejected)["code"], "invalid_parameters");
        assert_eq!(metadata(&rejected)["submission"], "not_submitted");
        assert_eq!(fixture.count(), 1);

        fixture.state.catalog.lock().unwrap()["models"][model_index]["capabilities"][0]["input_limits"]
            ["max_request_bytes"] = json!(encoded_bytes);
        let result = call(&client, tool, args).await;
        assert_image(&result);
        assert_eq!(fixture.count(), 2);
        assert_eq!(
            fixture.state.records.lock().unwrap()[1].body_bytes,
            encoded_bytes
        );
        assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
        close(client, task).await;
    }
}

#[tokio::test]
async fn both_protocols_discover_and_generate_through_http_with_correct_metadata() {
    for modern in [false, true] {
        let fixture = Fixture::new().await;
        let (client, task) = connect(fixture.client(Duration::from_secs(2)), modern).await;
        let tools = client.list_tools(None).await.unwrap().tools;
        assert_eq!(tools.len(), 8);
        assert!(!tools.iter().any(|tool| tool.name == "get_image_billing"));
        let generate = tools
            .iter()
            .find(|tool| tool.name == "generate_image")
            .unwrap();
        let schema = serde_json::to_value(&generate.input_schema).unwrap();
        let properties = schema["properties"].as_object().unwrap();
        assert_eq!(
            properties.keys().map(String::as_str).collect::<Vec<_>>(),
            [
                "complexity",
                "mode",
                "model",
                "n",
                "prompt",
                "quality",
                "result_mode",
                "size"
            ]
        );
        assert_eq!(schema["additionalProperties"], false);
        assert_eq!(fixture.count(), 0);
        let list = call(&client, "list_image_models", json!({})).await;
        assert_eq!(metadata(&list)["models"].as_array().unwrap().len(), 4);
        assert_eq!(fixture.count(), 0);
        let result = call(
            &client,
            "generate_image",
            json!({"prompt":"synthetic blue square","size":"1024x1024","quality":"standard","n":1}),
        )
        .await;
        assert_image(&result);
        assert_eq!(metadata(&result)["selected_model"], "sample-26");
        assert_eq!(metadata(&result)["effective_model"], "fixture-effective");
        assert_eq!(
            metadata(&result)["effective_model_source"],
            "server_response"
        );
        assert_no_fees(metadata(&result));
        {
            let records = fixture.state.records.lock().unwrap();
            assert_eq!(records.len(), 1);
            assert!(records[0].auth_ok);
            assert_eq!(records[0].path, "/v1/images/generations");
            assert!(records[0].content_type.starts_with("application/json"));
            assert_eq!(
                records[0].fields,
                json!({"model":"sample-26","prompt":"synthetic blue square","size":"1024x1024","quality":"standard","n":1})
            );
        }
        assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
        close(client, task).await;
    }
}

#[tokio::test]
async fn synchronous_edit_uses_its_own_model_and_only_the_named_file() {
    let fixture = Fixture::new().await;
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    let directory = tempfile::tempdir().unwrap();
    let selected = directory.path().join("selected.png");
    tokio::fs::write(&selected, &fixture.state.png)
        .await
        .unwrap();
    tokio::fs::write(directory.path().join("unrelated.png"), b"must not be read")
        .await
        .unwrap();
    let result = call(&client, "edit_image", json!({"prompt":"synthetic edit","image_path":selected,"size":"1024x1024","quality":"standard","n":1})).await;
    assert_image(&result);
    assert_eq!(metadata(&result)["selected_model"], "sample-edit-sync");
    assert_no_fees(metadata(&result));
    {
        let records = fixture.state.records.lock().unwrap();
        assert_eq!(records.len(), 1);
        assert!(records[0].auth_ok);
        assert_eq!(records[0].path, "/v1/images/edits");
        assert!(
            records[0]
                .content_type
                .starts_with("multipart/form-data; boundary=")
        );
        assert_eq!(
            records[0].fields,
            json!({"model":"sample-edit-sync","prompt":"synthetic edit","n":"1","size":"1024x1024","quality":"standard"})
        );
        assert_eq!(records[0].file, fixture.state.png);
    }
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
    assert_eq!(metadata(&result)["request_ref"], "fixture-request");
    assert_eq!(metadata(&result)["local_files"], json!([]));
    assert_eq!(std::fs::read_dir(directory.path()).unwrap().count(), 2);
    close(client, task).await;
}

#[tokio::test]
async fn rejection_and_invalid_input_never_reach_a_billable_post() {
    let fixture = Fixture::new().await;
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    for (tool, args, code) in [
        (
            "generate_image",
            json!({"prompt":"test","model":"missing"}),
            "model_not_found",
        ),
        (
            "generate_image",
            json!({"prompt":"test","model":"sample-paused"}),
            "model_unavailable",
        ),
        (
            "edit_image",
            json!({"prompt":"test","model":"sample-26","image_path":"/synthetic/does-not-need-to-exist.png"}),
            "unsupported_mode",
        ),
    ] {
        let result = call(&client, tool, args).await;
        assert_eq!(metadata(&result)["selection"]["code"], code);
        assert_eq!(metadata(&result)["submission"], "not_submitted");
    }
    let directory = tempfile::tempdir().unwrap();
    let invalid = directory.path().join("invalid.png");
    tokio::fs::write(&invalid, b"not an image").await.unwrap();
    for path in [
        "relative.png".to_owned(),
        invalid.to_string_lossy().into_owned(),
    ] {
        let result = call(
            &client,
            "edit_image",
            json!({"prompt":"test","image_path":path}),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["submission"], "not_submitted");
    }
    fixture.state.catalog.lock().unwrap()["can_submit"] = json!(false);
    fixture.state.catalog.lock().unwrap()["reason_code"] = json!("insufficient_balance");
    let list = call(&client, "list_image_models", json!({})).await;
    assert_ne!(list.is_error, Some(true));
    assert_eq!(metadata(&list)["can_submit"], false);
    let rejected = call(&client, "generate_image", json!({"prompt":"test"})).await;
    assert_eq!(
        metadata(&rejected)["selection"]["code"],
        "submission_not_allowed"
    );
    assert_eq!(fixture.count(), 0);
    close(client, task).await;
}

#[tokio::test]
async fn invalid_server_reference_is_not_invented_or_used_for_billing() {
    let fixture = Fixture::new().await;
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    for reference in ["", "private/value", "client:internal"] {
        *fixture.state.request_ref.lock().unwrap() = reference.into();
        let result = call(&client, "generate_image", json!({"prompt":"synthetic"})).await;
        assert_image(&result);
        assert!(metadata(&result)["request_ref"].is_null());
        assert_no_fees(metadata(&result));
    }
    assert_eq!(fixture.count(), 3);
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
    close(client, task).await;
}

#[tokio::test]
async fn failed_submissions_are_not_retried_or_replayed_on_redirect() {
    for mode in [
        Mode::Delay,
        Mode::Disconnect,
        Mode::Redirect("http://127.0.0.1:1/never".into()),
    ] {
        let fixture = Fixture::new().await;
        *fixture.state.mode.lock().unwrap() = mode;
        let (client, task) = connect(fixture.client(Duration::from_millis(100)), false).await;
        let result = call(&client, "generate_image", json!({"prompt":"test"})).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["submission"], "unknown");
        assert!(metadata(&result)["request_ref"].is_null());
        assert_eq!(fixture.count(), 1);
        assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
        close(client, task).await;
    }
}

#[tokio::test]
async fn client_cancellation_does_not_repeat_a_submitted_post() {
    let fixture = Fixture::new().await;
    *fixture.state.mode.lock().unwrap() = Mode::Delay;
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), true).await;
    {
        let request = client.call_tool_once(
            CallToolRequestParams::new("generate_image")
                .with_arguments(json!({"prompt":"test"}).as_object().unwrap().clone()),
        );
        tokio::pin!(request);
        tokio::select! { _ = &mut request => panic!("fixture should still be waiting"), _ = fixture.wait_for_post() => {} }
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(fixture.count(), 1);
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
    close(client, task).await;
}

#[tokio::test]
async fn http_errors_and_parser_failures_have_safe_stable_diagnostics() {
    let fixture = Fixture::new().await;
    let client = fixture.client(Duration::from_secs(2));
    assert!(!format!("{client:?}").contains(KEY));
    let (mcp, task) = connect(client.clone(), false).await;
    for (status, code, expected) in [
        (StatusCode::UNAUTHORIZED, "other", "authentication_failed"),
        (StatusCode::FORBIDDEN, "other", "permission_denied"),
        (
            StatusCode::FORBIDDEN,
            "insufficient_balance",
            "insufficient_balance",
        ),
        (
            StatusCode::TOO_MANY_REQUESTS,
            "quota_exhausted",
            "quota_exhausted",
        ),
        (StatusCode::TOO_MANY_REQUESTS, "other", "rate_limited"),
        (StatusCode::BAD_REQUEST, "other", "invalid_parameters"),
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            "other",
            "upstream_failed",
        ),
    ] {
        *fixture.state.mode.lock().unwrap() = Mode::Error(
            status,
            json!({"error":{"code":code,"message":format!("Authorization: Bearer {KEY}; {SENSITIVE}")}}),
        );
        let result = call(&mcp, "generate_image", json!({"prompt":"test"})).await;
        assert_eq!(metadata(&result)["code"], expected);
        assert_eq!(metadata(&result)["request_ref"], "fixture-request");
        for output in [
            serde_json::to_string(&result).unwrap(),
            format!("{result:?}"),
        ] {
            assert!(!output.contains(KEY));
            assert!(!output.contains(SENSITIVE));
            assert!(!output.contains("Authorization"));
        }
        let error = client
            .execute(
                &crate::mcp::GenerateInput {
                    prompt: "test".into(),
                    model: None,
                    size: None,
                    quality: None,
                    complexity: None,
                    n: None,
                    mode: None,
                    result_mode: None,
                },
                None,
                &RequestProgress::default(),
            )
            .await
            .unwrap_err();
        for output in [
            format!("{error}"),
            format!("{error:?}"),
            serde_json::to_string(&error).unwrap(),
        ] {
            assert!(!output.contains(KEY));
            assert!(!output.contains(SENSITIVE));
        }
    }
    fixture.state.catalog.lock().unwrap()["models"][0]["availability"] = json!(KEY);
    let before = fixture.count();
    let result = call(&mcp, "list_image_models", json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert!(!serde_json::to_string(&result).unwrap().contains(KEY));
    assert_eq!(fixture.count(), before);
    close(mcp, task).await;
}

#[tokio::test]
async fn url_download_and_its_redirect_target_never_receive_api_authentication() {
    let fixture = Fixture::new().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let state = (seen.clone(), fixture.state.png.clone());
    let router = Router::new()
        .route(
            "/redirect",
            get(
                |State((seen, _)): State<DownloadState>, request: Request| async move {
                    seen.lock()
                        .unwrap()
                        .push(request.headers().contains_key("authorization"));
                    AxumResponse::builder()
                        .status(302)
                        .header("location", "/image")
                        .body(Body::empty())
                        .unwrap()
                },
            ),
        )
        .route(
            "/image",
            get(
                |State((seen, bytes)): State<DownloadState>, request: Request| async move {
                    seen.lock()
                        .unwrap()
                        .push(request.headers().contains_key("authorization"));
                    // Deliberately wrong advertised type: the actual PNG controls MCP MIME.
                    ([("content-type", "image/jpeg")], bytes)
                },
            ),
        )
        .with_state(state);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/redirect", listener.local_addr().unwrap());
    let download_task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    *fixture.state.mode.lock().unwrap() = Mode::Url(url);
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    let result = call(&client, "generate_image", json!({"prompt":"test"})).await;
    assert_image(&result);
    assert_eq!(seen.lock().unwrap().as_slice(), &[false, false]);
    assert_eq!(metadata(&result)["remote_expires_at"], json!([null]));
    assert_eq!(fixture.count(), 1);
    close(client, task).await;
    download_task.abort();
}

#[tokio::test]
async fn malformed_tool_arguments_do_not_echo_values_in_protocol_errors() {
    let fixture = Fixture::new().await;
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    let result = call(&client, "generate_image", json!({"prompt":"test","n":KEY})).await;
    assert_eq!(metadata(&result)["code"], "invalid_parameters");
    assert!(!format!("{result:?}").contains(KEY));
    assert!(!serde_json::to_string(&result).unwrap().contains(KEY));
    assert_eq!(fixture.count(), 0);
    close(client, task).await;
}

#[tokio::test]
async fn redirect_never_replays_a_post_to_a_second_api_server() {
    let fixture = Fixture::new().await;
    let target = Fixture::new().await;
    *fixture.state.mode.lock().unwrap() =
        Mode::Redirect(format!("{}images/generations", target.base));
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    let result = call(&client, "generate_image", json!({"prompt":"test"})).await;
    assert_eq!(metadata(&result)["code"], "redirect_refused");
    assert_eq!(fixture.count(), 1);
    assert_eq!(target.count(), 0);
    close(client, task).await;
}

#[tokio::test]
async fn failed_result_download_cannot_report_the_generation_as_not_submitted() {
    let fixture = Fixture::new().await;
    let unused = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/image", unused.local_addr().unwrap());
    drop(unused);
    *fixture.state.mode.lock().unwrap() = Mode::Url(url);
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    let result = call(&client, "generate_image", json!({"prompt":"test"})).await;
    assert_eq!(metadata(&result)["submission"], "unknown");
    assert_eq!(metadata(&result)["request_ref"], "fixture-request");
    assert_eq!(fixture.count(), 1);
    close(client, task).await;
}

#[tokio::test]
async fn invalid_image_payload_keeps_server_reference_without_echoing_payload() {
    let fixture = Fixture::new().await;
    let (client, task) = connect(fixture.client(Duration::from_secs(2)), false).await;
    for body in [
        json!({"data":[]}),
        json!({"data":[{"b64_json":KEY}]}),
        json!({"data":[{"b64_json":STANDARD.encode(SENSITIVE)}]}),
        json!({"data":[{"url":format!("invalid:{KEY}")}]}),
        json!({"data":[{}]}),
    ] {
        *fixture.state.mode.lock().unwrap() = Mode::Error(StatusCode::OK, body);
        let result = call(&client, "generate_image", json!({"prompt":"synthetic"})).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["request_ref"], "fixture-request");
        let encoded = serde_json::to_string(&result).unwrap();
        assert!(!encoded.contains(KEY));
        assert!(!encoded.contains(SENSITIVE));
    }
    assert_eq!(fixture.count(), 5);
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
    close(client, task).await;
}

#[tokio::test]
async fn cancellation_after_server_headers_retains_reference() {
    let fixture = Fixture::new().await;
    let download_seen = Arc::new(tokio::sync::Notify::new());
    let notify = download_seen.clone();
    let router = Router::new().route(
        "/image",
        get(move || {
            let notify = notify.clone();
            async move {
                notify.notify_one();
                tokio::time::sleep(Duration::from_secs(5)).await;
                StatusCode::SERVICE_UNAVAILABLE
            }
        }),
    );
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    *fixture.state.mode.lock().unwrap() =
        Mode::Url(format!("http://{}/image", listener.local_addr().unwrap()));
    let download_task = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let client = fixture.client(Duration::from_secs(2));
    let input = crate::mcp::GenerateInput {
        prompt: "synthetic".into(),
        model: None,
        size: None,
        quality: None,
        complexity: None,
        n: None,
        mode: None,
        result_mode: None,
    };
    let progress = RequestProgress::default();
    {
        let execution = client.execute(&input, None, &progress);
        tokio::pin!(execution);
        tokio::select! {
            _ = &mut execution => panic!("download should still be waiting"),
            seen = tokio::time::timeout(Duration::from_secs(2), download_seen.notified()) => seen.expect("download did not start"),
        }
    }
    let error = progress.cancelled();
    assert_eq!(error.submission, Submission::Unknown);
    assert_eq!(error.request_ref.as_deref(), Some("fixture-request"));
    assert_eq!(fixture.count(), 1);
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
    download_task.abort();
}

fn noise_png(width: u32, height: u32) -> Vec<u8> {
    let mut pixels = vec![0u8; (width * height * 3) as usize];
    let mut state = 0x12345678u32;
    for pixel in &mut pixels {
        state ^= state << 13;
        state ^= state >> 17;
        state ^= state << 5;
        *pixel = state as u8;
    }
    let mut buffer = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image::RgbImage::from_raw(width, height, pixels).unwrap())
        .write_to(&mut buffer, ImageFormat::Png)
        .unwrap();
    buffer.into_inner()
}

#[tokio::test]
async fn large_single_and_aggregate_images_preserve_originals_and_fit_default_mcp_transport() {
    use rmcp::model::{RequestId, ServerJsonRpcMessage, ServerResult};
    use std::time::{SystemTime, UNIX_EPOCH};
    for (width, height, count, modern) in [
        (2304, 2048, 1, false),
        (1408, 1280, 3, true),
        (2304, 2048, 4, true),
    ] {
        let bytes = noise_png(width, height);
        assert!(bytes.len() < MAX_IMAGE_BYTES);
        let encoded = STANDARD.encode(&bytes);
        if width == 2304 {
            assert!(encoded.len() > 16 * 1024 * 1024);
            if count == 4 {
                assert!(encoded.len() * count > 64 * 1024 * 1024);
            }
        } else {
            assert!(encoded.len() < 8 * 1024 * 1024);
            assert!(encoded.len() * count > 16 * 1024 * 1024);
        }
        let fixture = Fixture::new().await;
        let data: Vec<_> = (0..count)
            .map(|_| json!({"b64_json":encoded,"expires_at":null}))
            .collect();
        *fixture.state.mode.lock().unwrap() = Mode::Error(
            StatusCode::OK,
            json!({"model":"fixture-effective","data":data}),
        );
        let (client, task) = connect(fixture.client(Duration::from_secs(30)), modern).await;
        let before = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_secs();
        let result = tokio::time::timeout(
            Duration::from_secs(30),
            call(
                &client,
                "generate_image",
                json!({"prompt":"synthetic noise","n":count}),
            ),
        )
        .await
        .expect("default MCP client must receive the result");
        assert_image(&result);
        let response = ServerJsonRpcMessage::response(
            ServerResult::CallToolResult(result.clone()),
            RequestId::Number(1),
        );
        let wire = serde_json::to_vec(&response).unwrap();
        assert!(wire.len() < 8 * 1024 * 1024);
        assert_eq!(fixture.count(), 1);
        assert_eq!(metadata(&result)["selected_model"], "sample-26");
        assert_eq!(metadata(&result)["effective_model"], "fixture-effective");
        assert_no_fees(metadata(&result));
        assert_eq!(
            metadata(&result)["remote_expires_at"],
            json!(vec![Value::Null; count])
        );
        let files = metadata(&result)["local_files"].as_array().unwrap();
        assert!(!files.is_empty());
        if count == 3 {
            assert!(files.len() > 1);
        }
        let result_json = serde_json::to_value(&result).unwrap();
        let content = result_json["content"].as_array().unwrap();
        assert_eq!(
            content
                .iter()
                .filter(|block| block["type"] == "image")
                .count(),
            count
        );
        for file in files {
            let path = Path::new(file["path"].as_str().unwrap());
            assert!(path.is_absolute());
            assert_eq!(std::fs::read(path).unwrap(), bytes);
            assert_eq!(file["inline_representation"], "preview");
            assert_eq!(file["mime_type"], "image/png");
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
            let decoded = image::load_from_memory(&preview).unwrap();
            assert!(decoded.width() <= 256 && decoded.height() <= 256);
            assert!(content.iter().any(|block| {
                block["type"] == "text"
                    && block["text"]
                        .as_str()
                        .unwrap()
                        .contains(file["path"].as_str().unwrap())
                    && block["text"].as_str().unwrap().contains("预览")
            }));
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                assert_eq!(
                    std::fs::metadata(path).unwrap().permissions().mode() & 0o777,
                    0o600
                );
                assert_eq!(
                    std::fs::metadata(path.parent().unwrap())
                        .unwrap()
                        .permissions()
                        .mode()
                        & 0o777,
                    0o700
                );
            }
        }
        close(client, task).await;
    }
}

#[path = "task_tests.rs"]
mod tasks;

#[path = "batch_tests.rs"]
mod batches;

#[path = "advanced_edit_tests.rs"]
mod advanced;

#[path = "preference_tests.rs"]
mod preferences;

#[path = "configuration_mcp_tests.rs"]
mod configuration_mcp;

#[tokio::test]
async fn persistent_output_preflight_blocks_generation_edit_and_batch_before_any_post() {
    let fixture = Fixture::new().await;
    let directory = tempfile::tempdir().unwrap();
    let blocked = directory.path().join("not-a-directory");
    std::fs::write(&blocked, b"keep").unwrap();
    let server = ImageMcp::with_directories(
        fixture.client(Duration::from_secs(2)),
        directory.path().join("cache"),
        directory.path().join("tasks"),
    )
    .with_persistent_output_for_tests(blocked);
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let task = tokio::spawn(async move {
        server
            .serve(server_io)
            .await
            .unwrap()
            .waiting()
            .await
            .unwrap();
    });
    let client = ClientConfig::default()
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .serve(client_io)
        .await
        .unwrap();
    for (tool, args) in [
        ("generate_image", json!({"prompt":"synthetic"})),
        (
            "generate_image",
            json!({"prompt":"synthetic", "result_mode":"files"}),
        ),
        (
            "edit_image",
            json!({"prompt":"synthetic","image_path":"/synthetic/input.png"}),
        ),
        (
            "submit_image_batch",
            json!({"items":[{"custom_id":"one","prompt":"synthetic"}]}),
        ),
    ] {
        let result = call(&client, tool, args).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["code"], "image_storage_unavailable");
        assert_eq!(metadata(&result)["submission"], "not_submitted");
    }
    assert_eq!(fixture.count(), 0);
    assert_eq!(fixture.state.gets.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
    client.cancel().await.unwrap();
    task.await.unwrap();
}

#[tokio::test]
async fn dual_sources_prefer_nonempty_b64_and_never_fallback_on_damaged_data() {
    let fixture = Fixture::new().await;
    let seen = Arc::new(Mutex::new(Vec::new()));
    let router = Router::new()
        .route(
            "/image",
            get(
                |State((seen, bytes)): State<DownloadState>, request: Request| async move {
                    seen.lock()
                        .unwrap()
                        .push(request.headers().contains_key("authorization"));
                    bytes
                },
            ),
        )
        .with_state((seen.clone(), fixture.state.png.clone()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}/image", listener.local_addr().unwrap());
    let download = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let (client, server) = connect(fixture.client(Duration::from_secs(2)), false).await;
    let encoded = STANDARD.encode(&fixture.state.png);
    let cases = [
        (Some(encoded.as_str()), true, false),
        (None, true, true),
        (Some(""), true, true),
        (Some(" \t\n"), true, true),
        (Some("not-valid-base64"), false, false),
        (Some("bm90IGFuIGltYWdl"), false, false),
    ];
    let mut downloads = 0;
    for (index, (b64, success, needs_download)) in cases.into_iter().enumerate() {
        let mut item = json!({"url":url});
        if let Some(encoded) = b64 {
            item["b64_json"] = json!(encoded);
        }
        *fixture.state.mode.lock().unwrap() = Mode::Error(StatusCode::OK, json!({"data":[item]}));
        let result = call(&client, "generate_image", json!({"prompt":SENSITIVE})).await;
        if success {
            assert_image(&result);
        } else {
            assert_eq!(metadata(&result)["code"], "invalid_image");
            assert_eq!(metadata(&result)["submission"], "unknown");
            assert_eq!(metadata(&result)["request_ref"], "fixture-request");
            let visible = serde_json::to_string(&result).unwrap();
            assert!(!visible.contains("not-valid-base64"));
            assert!(!visible.contains("bm90IGFuIGltYWdl"));
        }
        downloads += usize::from(needs_download);
        assert_eq!(seen.lock().unwrap().len(), downloads);
        assert!(seen.lock().unwrap().iter().all(|has_auth| !has_auth));
        assert_eq!(fixture.count(), index + 1);
    }
    close(client, server).await;
    download.abort();
}

#[tokio::test]
async fn traced_http_stages_use_only_safe_fields_and_count_actual_constructions() {
    let fixture = Fixture::new().await;
    let trace = crate::performance::OperationTrace::new("synthetic");
    let input = serde_json::from_value(json!({"prompt":SENSITIVE})).unwrap();
    fixture
        .client(Duration::from_secs(2))
        .with_trace(trace.clone())
        .execute(&input, None, &RequestProgress::default())
        .await
        .unwrap();
    trace.finish("success", Submission::Accepted, "ok");
    let events = trace.events();
    let stages: Vec<_> = events
        .iter()
        .map(|event| event["stage"].as_str().unwrap())
        .collect();
    for required in [
        "catalog_get_start",
        "catalog_headers",
        "catalog_body_complete",
        "catalog_json_complete",
        "catalog_validation_complete",
        "candidate_selection_start",
        "candidate_counts",
        "candidate_selection_complete",
        "image_post_start",
        "image_post_headers",
        "image_post_body_complete",
        "image_post_json_complete",
        "image_source_b64",
        "image_decode_complete",
        "image_validation_complete",
        "operation_end",
    ] {
        assert!(stages.contains(&required), "missing {required}");
    }
    let counts = events
        .iter()
        .find(|event| event["stage"] == "candidate_counts")
        .unwrap();
    assert!(
        counts["candidate_count"].as_u64().unwrap()
            >= counts["construction_count"].as_u64().unwrap()
    );
    assert!(counts["construction_count"].as_u64().unwrap() > 0);
    let visible = serde_json::to_string(&events).unwrap();
    for sensitive in [
        KEY,
        SENSITIVE,
        &fixture.base,
        "fixture-request",
        "Authorization",
        "b64_json",
    ] {
        assert!(!visible.contains(sensitive));
    }
    assert_eq!(fixture.count(), 1);
}

#[tokio::test]
async fn files_delivery_is_explicit_sync_only_and_never_sent_upstream() {
    let fixture = Fixture::new().await;
    let (client, server) = connect(fixture.client(Duration::from_secs(2)), false).await;
    for args in [
        json!({"prompt":"synthetic", "result_mode":"files"}),
        json!({"prompt":"synthetic", "result_mode":"files", "mode":"single_async"}),
        json!({"prompt":"synthetic", "result_mode":"invalid"}),
    ] {
        let result = call(&client, "generate_image", args).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["submission"], "not_submitted");
        assert_eq!(fixture.count(), 0);
    }
    close(client, server).await;
    let directory = tempfile::tempdir().unwrap();
    let input_path = directory.path().join("参考 图.png");
    std::fs::write(&input_path, &fixture.state.png).unwrap();
    let mcp = ImageMcp::with_directories(
        fixture.client(Duration::from_secs(2)),
        directory.path().join("cache"),
        directory.path().join("tasks"),
    )
    .with_persistent_output_for_tests(directory.path().join("桌面 空格/Tokemula-Images"));
    std::fs::create_dir_all(directory.path().join("桌面 空格")).unwrap();
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
    let server = tokio::spawn(async move {
        mcp.serve(server_io).await.unwrap().waiting().await.unwrap();
    });
    let client = ClientConfig::default()
        .with_protocol_version(ProtocolVersion::V_2025_11_25)
        .serve(client_io)
        .await
        .unwrap();
    for (tool, args) in [
        (
            "generate_image",
            json!({"prompt":"synthetic", "result_mode":"files"}),
        ),
        (
            "edit_image",
            json!({"prompt":"synthetic", "result_mode":"files", "image_path":input_path}),
        ),
    ] {
        let result = call(&client, tool, args).await;
        assert_ne!(result.is_error, Some(true));
        assert!(
            result
                .content
                .iter()
                .all(|content| !matches!(content, rmcp::model::ContentBlock::Image(_)))
        );
        let file = &metadata(&result)["local_files"][0];
        assert_eq!(
            std::fs::read(file["path"].as_str().unwrap()).unwrap(),
            fixture.state.png
        );
        assert_eq!(file["inline_representation"], "none");
    }
    let records = fixture.state.records.lock().unwrap();
    assert_eq!(records.len(), 2);
    assert!(
        records
            .iter()
            .all(|record| record.fields.get("result_mode").is_none())
    );
    drop(records);
    close(client, server).await;
}
