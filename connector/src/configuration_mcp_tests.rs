//! Actual MCP transport checks; settings and HTTP are controlled local fixtures.
use super::*;
use crate::{
    configuration::SessionManager,
    settings::{ModelPreferences, SettingsService},
};
use rmcp::{
    ClientHandler, ErrorData,
    model::{
        ClientCapabilities, ElicitRequestParams, ElicitResult, ElicitationAction, Implementation,
        InputResponses,
    },
    service::RequestContext,
};
use tokio::sync::Notify;

const OLD_KEY: &str = "D1_SYNTHETIC_OLD_IDENTITY";
const NEW_KEY: &str = "D1_SYNTHETIC_NEW_IDENTITY";

#[derive(Clone)]
struct UrlClient {
    info: ClientConfig,
    action: ElicitationAction,
    requests: Arc<Mutex<Vec<Value>>>,
}
impl UrlClient {
    fn new(modern: bool, url: bool, action: ElicitationAction) -> Self {
        let capabilities: ClientCapabilities = serde_json::from_value(if url {
            json!({"elicitation":{"url":{}}})
        } else {
            json!({})
        })
        .unwrap();
        Self {
            info: ClientConfig::new(
                capabilities,
                Implementation::new("synthetic-url-client", "1"),
            )
            .with_protocol_version(if modern {
                ProtocolVersion::V_2026_07_28
            } else {
                ProtocolVersion::V_2025_11_25
            }),
            action,
            requests: Arc::default(),
        }
    }
}
impl ClientHandler for UrlClient {
    fn get_info(&self) -> ClientConfig {
        self.info.clone()
    }
    async fn create_elicitation(
        &self,
        request: ElicitRequestParams,
        _: RequestContext<RoleClient>,
    ) -> Result<ElicitResult, ErrorData> {
        assert!(
            matches!(request, ElicitRequestParams::UrlElicitationParams { .. }),
            "configuration must never use a secret-collecting form"
        );
        self.requests
            .lock()
            .unwrap()
            .push(serde_json::to_value(request).unwrap());
        Ok(ElicitResult::new(self.action.clone()))
    }
}
async fn open_mcp(
    server: ImageMcp,
    handler: UrlClient,
    modern: bool,
) -> (RunningService<RoleClient, UrlClient>, JoinHandle<()>) {
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
    let lifecycle = if modern {
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        }
    } else {
        ClientLifecycleMode::Initialize
    };
    let client = handler
        .serve_with_lifecycle(client_io, lifecycle)
        .await
        .unwrap();
    (client, task)
}
fn params(name: &str, arguments: Value) -> CallToolRequestParams {
    CallToolRequestParams::new(name.to_owned())
        .with_arguments(arguments.as_object().unwrap().clone())
}
fn complete(response: CallToolResponse) -> CallToolResult {
    let CallToolResponse::Complete(result) = response else {
        panic!("expected complete response")
    };
    result
}
async fn invoke(
    client: &RunningService<RoleClient, UrlClient>,
    name: &str,
    arguments: Value,
) -> CallToolResult {
    complete(
        client
            .call_tool_once(params(name, arguments))
            .await
            .unwrap(),
    )
}
async fn stop(
    client: RunningService<RoleClient, UrlClient>,
    task: JoinHandle<()>,
    manager: SessionManager,
) {
    if manager.status().await.is_some() {
        manager.cancel().await.unwrap();
        manager.wait().await.unwrap();
    }
    client.cancel().await.unwrap();
    task.await.unwrap();
}
fn managed(
    fixture: &Fixture,
    root: &std::path::Path,
) -> (ImageMcp, SettingsService, SessionManager) {
    assert!(
        std::env::var_os("TOKEMULA_IMAGE_KEY").is_none(),
        "run with explicit cleared test authentication"
    );
    let service = SettingsService::in_memory_for_tests(root.join("settings"), &fixture.base);
    let server = ImageMcp::with_managed_directories(
        fixture.client(Duration::from_secs(2)),
        service.clone(),
        root.join("images"),
        root.join("tasks"),
    );
    let sessions = server.configuration_sessions_for_tests().unwrap();
    (server, service, sessions)
}

#[tokio::test]
async fn configuration_without_url_capability_is_explicit_secret_free_and_reuses_one_session() {
    for modern in [false, true] {
        let fixture = Fixture::new().await;
        let root = tempfile::tempdir().unwrap();
        let (server, service, sessions) = managed(&fixture, root.path());
        let handler = UrlClient::new(modern, false, ElicitationAction::Accept);
        let recorded = handler.requests.clone();
        let (client, task) = open_mcp(server, handler, modern).await;
        assert!(sessions.status().await.is_none());
        let tools = client.list_tools(None).await.unwrap().tools;
        assert_eq!(tools.len(), 8);
        let schema = serde_json::to_value(
            &tools
                .iter()
                .find(|t| t.name == "configure_tokemula")
                .unwrap()
                .input_schema,
        )
        .unwrap();
        assert_eq!(
            schema["properties"]
                .as_object()
                .unwrap()
                .keys()
                .map(String::as_str)
                .collect::<Vec<_>>(),
            vec!["interface"]
        );
        assert!(
            sessions.status().await.is_none(),
            "enumeration cannot create a page"
        );
        let terminal = invoke(
            &client,
            "configure_tokemula",
            json!({"interface":"terminal"}),
        )
        .await;
        assert_eq!(metadata(&terminal)["status"], "terminal_required");
        assert!(
            metadata(&terminal)["terminal_command"]
                .as_str()
                .unwrap()
                .ends_with("configure --terminal")
        );
        assert!(sessions.status().await.is_none());
        let invalid = invoke(&client, "configure_tokemula", json!({"api_key":OLD_KEY})).await;
        assert_eq!(invalid.is_error, Some(true));
        assert!(!serde_json::to_string(&invalid).unwrap().contains(OLD_KEY));
        assert!(sessions.status().await.is_none());
        let first = invoke(&client, "configure_tokemula", json!({})).await;
        let second = invoke(
            &client,
            "configure_tokemula",
            json!({"interface":"browser"}),
        )
        .await;
        assert_eq!(
            metadata(&first)["configuration"]["session_id"],
            metadata(&second)["configuration"]["session_id"]
        );
        assert_eq!(metadata(&first)["configuration"]["status"], "active");
        let link = metadata(&first)["configuration"]["url"].as_str().unwrap();
        assert_eq!(Url::parse(link).unwrap().host_str(), Some("127.0.0.1"));
        assert_eq!(
            reqwest::Client::builder()
                .no_proxy()
                .build()
                .unwrap()
                .get(link)
                .send()
                .await
                .unwrap()
                .status(),
            StatusCode::OK
        );
        assert!(recorded.lock().unwrap().is_empty());
        assert_eq!(service.status().unwrap().availability, "missing");
        assert_eq!(fixture.state.gets.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.count(), 0);
        stop(client, task, sessions).await;
    }
}

#[tokio::test]
async fn legacy_url_elicitation_acceptance_does_not_claim_a_saved_configuration() {
    let fixture = Fixture::new().await;
    let root = tempfile::tempdir().unwrap();
    let (server, service, sessions) = managed(&fixture, root.path());
    let handler = UrlClient::new(false, true, ElicitationAction::Accept);
    let requests = handler.requests.clone();
    let (client, task) = open_mcp(server, handler, false).await;
    let result = invoke(&client, "configure_tokemula", json!({})).await;
    assert_eq!(metadata(&result)["configuration"]["status"], "active");
    assert_eq!(service.status().unwrap().availability, "missing");
    let requests = requests.lock().unwrap().clone();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0]["mode"], "url");
    assert!(requests[0].get("requestedSchema").is_none());
    assert_eq!(
        requests[0]["url"],
        metadata(&result)["configuration"]["url"]
    );
    assert!(!serde_json::to_string(&requests).unwrap().contains(OLD_KEY));
    assert_eq!(fixture.count(), 0);
    stop(client, task, sessions).await;
}

#[tokio::test]
async fn modern_url_requests_use_mrtr_and_verify_session_correlation_before_cancellation() {
    let fixture = Fixture::new().await;
    let root = tempfile::tempdir().unwrap();
    let (server, service, sessions) = managed(&fixture, root.path());
    let handler = UrlClient::new(true, true, ElicitationAction::Accept);
    let requests = handler.requests.clone();
    let (client, task) = open_mcp(server, handler, true).await;
    let response = client
        .call_tool_once(params("configure_tokemula", json!({})))
        .await
        .unwrap();
    let CallToolResponse::InputRequired(required) = response else {
        panic!("modern protocol must use input_required")
    };
    let state = required.request_state.unwrap();
    let inputs = required.input_requests.unwrap();
    assert_eq!(inputs.len(), 1);
    let input = serde_json::to_value(inputs.get(&state).unwrap()).unwrap();
    assert_eq!(input["method"], "elicitation/create");
    assert_eq!(input["params"]["mode"], "url");
    assert!(input["params"].get("requestedSchema").is_none());
    assert_eq!(sessions.status().await.unwrap().session_id, state);
    assert!(
        requests.lock().unwrap().is_empty(),
        "modern server must not send outbound elicitation"
    );
    let answers = InputResponses::from([(state.clone(), json!({"action":"accept"}))]);
    let wrong = complete(
        client
            .call_tool_once(
                params("configure_tokemula", json!({}))
                    .with_request_state("unrelated-session")
                    .with_input_responses(answers.clone()),
            )
            .await
            .unwrap(),
    );
    assert_eq!(metadata(&wrong)["code"], "configuration_session_mismatch");
    assert_eq!(sessions.status().await.unwrap().session_id, state);
    let accepted = complete(
        client
            .call_tool_once(
                params("configure_tokemula", json!({}))
                    .with_request_state(state.clone())
                    .with_input_responses(answers),
            )
            .await
            .unwrap(),
    );
    assert_eq!(metadata(&accepted)["configuration"]["status"], "active");
    assert_eq!(service.status().unwrap().availability, "missing");
    let cancelled = complete(
        client
            .call_tool_once(
                params("configure_tokemula", json!({}))
                    .with_request_state(state.clone())
                    .with_input_responses(InputResponses::from([(
                        state,
                        json!({"action":"cancel"}),
                    )])),
            )
            .await
            .unwrap(),
    );
    assert_eq!(metadata(&cancelled)["configuration"]["status"], "cancelled");
    assert_eq!(fixture.count(), 0);
    stop(client, task, sessions).await;
}

#[tokio::test]
async fn sdk_fulfils_modern_url_input_without_form_and_cancels_the_real_session() {
    let fixture = Fixture::new().await;
    let root = tempfile::tempdir().unwrap();
    let (server, service, sessions) = managed(&fixture, root.path());
    let handler = UrlClient::new(true, true, ElicitationAction::Decline);
    let requests = handler.requests.clone();
    let (client, task) = open_mcp(server, handler, true).await;
    let result = client
        .call_tool(params("configure_tokemula", json!({"interface":"browser"})))
        .await
        .unwrap();
    assert_eq!(metadata(&result)["configuration"]["status"], "cancelled");
    assert_eq!(requests.lock().unwrap().len(), 1);
    assert_eq!(service.status().unwrap().availability, "missing");
    assert_eq!(fixture.count(), 0);
    stop(client, task, sessions).await;
}

#[derive(Default)]
struct IdentityState {
    events: Mutex<Vec<(&'static str, &'static str)>>,
    delay_first: AtomicBool,
    post_seen: Notify,
    release: Notify,
}
fn identity(request: &Request) -> &'static str {
    match request
        .headers()
        .get("authorization")
        .and_then(|v| v.to_str().ok())
    {
        Some(value) if value == format!("Bearer {OLD_KEY}") => "old",
        Some(value) if value == format!("Bearer {NEW_KEY}") => "new",
        _ => "missing",
    }
}
async fn identity_catalog(
    State(state): State<Arc<IdentityState>>,
    request: Request,
) -> AxumResponse {
    let id = identity(&request);
    assert_ne!(id, "missing");
    state.events.lock().unwrap().push(("catalog", id));
    axum::Json(
        serde_json::from_str::<Value>(include_str!("../tests/fixtures/http-catalog.json")).unwrap(),
    )
    .into_response()
}
async fn identity_generation(
    State(state): State<Arc<IdentityState>>,
    request: Request,
) -> AxumResponse {
    let id = identity(&request);
    state.events.lock().unwrap().push(("post", id));
    let hold = state.delay_first.swap(false, Ordering::SeqCst);
    state.post_seen.notify_one();
    if hold {
        state.release.notified().await;
    }
    AxumResponse::builder()
        .header("content-type", "application/json")
        .header("x-client-request-id", "fixture-request")
        .body(Body::from(
            json!({"model":"synthetic-effective","data":[{"b64_json":STANDARD.encode(png())}]})
                .to_string(),
        ))
        .unwrap()
}
async fn identity_fee(State(state): State<Arc<IdentityState>>, request: Request) -> AxumResponse {
    state
        .events
        .lock()
        .unwrap()
        .push(("fee", identity(&request)));
    axum::Json(
        serde_json::from_str::<Value>(include_str!("../tests/fixtures/http-billing.json")).unwrap(),
    )
    .into_response()
}
fn preferences(model: &str) -> ModelPreferences {
    ModelPreferences {
        default_model: model.into(),
        fallback_models: vec![],
        auto_fallback: false,
    }
}

#[tokio::test]
async fn managed_mcp_refreshes_each_operation_but_inflight_catalog_and_post_keep_one_identity() {
    assert!(std::env::var_os("TOKEMULA_IMAGE_KEY").is_none());
    let state = Arc::new(IdentityState::default());
    state.delay_first.store(true, Ordering::SeqCst);
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let base = format!("http://{}/v1/", listener.local_addr().unwrap());
    let router = Router::new()
        .route("/v1/images/models", get(identity_catalog))
        .route("/v1/images/generations", post(identity_generation))
        .route(
            "/v1/images/requests/{request_ref}/billing",
            get(identity_fee),
        )
        .with_state(state.clone());
    let http_task = tokio::spawn(async move {
        axum::serve(listener, router).await.unwrap();
    });
    let root = tempfile::tempdir().unwrap();
    let service = SettingsService::in_memory_for_tests(root.path().join("settings"), &base);
    let old = service.validate(OLD_KEY.into()).await.unwrap();
    service.save(&old, preferences("sample-26")).unwrap();
    let server = ImageMcp::with_managed_directories(
        ImageClient::build(&base, None, Duration::from_secs(2)).unwrap(),
        service.clone(),
        root.path().join("images"),
        root.path().join("tasks"),
    );
    let sessions = server.configuration_sessions_for_tests().unwrap();
    let (client, mcp_task) = open_mcp(
        server,
        UrlClient::new(true, false, ElicitationAction::Accept),
        true,
    )
    .await;
    let first = {
        let pending =
            client.call_tool_once(params("generate_image", json!({"prompt":"synthetic"})));
        tokio::pin!(pending);
        tokio::select! {
            result = &mut pending => panic!("first operation must wait at its post: {}", result.is_ok()),
            _ = state.post_seen.notified() => (),
        }
        let new = service.validate(NEW_KEY.into()).await.unwrap();
        service.save(&new, preferences("sample-fast")).unwrap();
        state.release.notify_one();
        complete(pending.await.unwrap())
    };
    assert_image(&first);
    assert_eq!(metadata(&first)["selected_model"], "sample-26");
    assert_eq!(metadata(&first)["effective_model"], "synthetic-effective");
    let second = invoke(
        &client,
        "generate_image",
        json!({"prompt":"synthetic second"}),
    )
    .await;
    assert_image(&second);
    assert_eq!(metadata(&second)["selected_model"], "sample-fast");
    let invalid_default = service.validate(NEW_KEY.into()).await.unwrap();
    service
        .save(&invalid_default, preferences("sample-paused"))
        .unwrap();
    let before_history = state.events.lock().unwrap().clone();
    let history = invoke(&client, "list_image_models", json!({})).await;
    assert_ne!(history.is_error, Some(true));
    assert_eq!(state.events.lock().unwrap().len(), before_history.len() + 1);
    assert_eq!(
        state.events.lock().unwrap().last(),
        Some(&("catalog", "new"))
    );
    service.logout().unwrap();
    let before_logout_read = state.events.lock().unwrap().len();
    let logged_out = invoke(&client, "list_image_models", json!({})).await;
    assert_eq!(metadata(&logged_out)["code"], "credential_unavailable");
    assert_eq!(state.events.lock().unwrap().len(), before_logout_read);
    let events = state.events.lock().unwrap().clone();
    assert_eq!(
        events
            .iter()
            .filter(|(kind, _)| *kind == "post")
            .copied()
            .collect::<Vec<_>>(),
        vec![("post", "old"), ("post", "new")]
    );
    assert_eq!(
        events
            .iter()
            .filter(|(kind, _)| *kind == "fee")
            .copied()
            .collect::<Vec<_>>(),
        Vec::<(&str, &str)>::new()
    );
    let safe = serde_json::to_string(&first).unwrap() + &serde_json::to_string(&second).unwrap();
    assert!(!safe.contains(OLD_KEY) && !safe.contains(NEW_KEY));
    stop(client, mcp_task, sessions).await;
    http_task.abort();
}

#[tokio::test]
async fn settings_read_failure_keeps_local_task_reference_without_network_or_fresh_status() {
    assert!(std::env::var_os("TOKEMULA_IMAGE_KEY").is_none());
    for record in ["valid", "missing", "unreadable", "foreign"] {
        let fixture = Fixture::new().await;
        let root = tempfile::tempdir().unwrap();
        let (server, _, sessions) = managed(&fixture, root.path());
        std::fs::create_dir_all(root.path().join("settings")).unwrap();
        std::fs::write(
            root.path().join("settings/settings.json"),
            "invalid preferences",
        )
        .unwrap();
        let store = crate::tasks::TaskStore {
            root: root.path().join("tasks"),
        };
        if record != "missing" {
            store
                .save(&crate::tasks::TaskRecord {
                    task_id: "imgtask_known".into(),
                    request_ref: Some("known-image-request".into()),
                    selected_model: Some("sample-fast".into()),
                    status: crate::tasks::TaskStatus::Completed,
                    created_at: 1,
                    completed_at: Some(2),
                    expires_at: 1000,
                    result_expires_at: None,
                    observed_at: 2,
                })
                .unwrap();
            if record == "unreadable" {
                std::fs::remove_file(store.root.join("imgtask_known.json")).unwrap();
                std::fs::create_dir(store.root.join("imgtask_known.json")).unwrap();
            } else if record == "foreign" {
                let path = store.root.join("imgtask_known.json");
                let mut value: Value =
                    serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
                value["task_id"] = json!("imgtask_other");
                std::fs::write(path, serde_json::to_vec(&value).unwrap()).unwrap();
            }
        }
        let (client, task) = open_mcp(
            server,
            UrlClient::new(true, false, ElicitationAction::Accept),
            true,
        )
        .await;
        let result = invoke(
            &client,
            "get_image_task",
            json!({"task_id":"imgtask_known"}),
        )
        .await;
        let result = metadata(&result).clone();
        assert_eq!(result["code"], "settings_invalid");
        assert_eq!(result["task_id"], "imgtask_known");
        assert_eq!(result["query_state"], "not_obtained");
        assert_eq!(result["submission"], "unknown");
        assert!(result.get("status").is_none());
        assert_eq!(
            result["request_ref"],
            if record == "valid" {
                json!("known-image-request")
            } else {
                Value::Null
            }
        );
        assert_eq!(
            result["request_ref_source"],
            if record == "valid" {
                json!("local_record")
            } else {
                Value::Null
            }
        );
        assert_eq!(fixture.state.posts.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.state.gets.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
        stop(client, task, sessions).await;
    }
    let fixture = Fixture::new().await;
    let root = tempfile::tempdir().unwrap();
    let store = crate::tasks::TaskStore {
        root: root.path().join("tasks"),
    };
    store
        .save(&crate::tasks::TaskRecord {
            task_id: "imgtask_explicit".into(),
            request_ref: Some("explicit-known-request".into()),
            selected_model: None,
            status: crate::tasks::TaskStatus::Processing,
            created_at: 1,
            completed_at: None,
            expires_at: 1000,
            result_expires_at: None,
            observed_at: 2,
        })
        .unwrap();
    let mut explicit = fixture.client(std::time::Duration::from_secs(1));
    explicit.authorization = None;
    explicit.task_store = Some(store);
    let error = explicit
        .task(
            "imgtask_explicit",
            &RequestProgress::for_task("imgtask_explicit"),
        )
        .await
        .unwrap_err();
    let value = serde_json::to_value(error).unwrap();
    assert_eq!(value["code"], "credential_unavailable");
    assert_eq!(value["request_ref"], "explicit-known-request");
    assert_eq!(value["request_ref_source"], "local_record");
    assert_eq!(fixture.state.posts.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.gets.load(Ordering::SeqCst), 0);
    assert_eq!(fixture.state.fees.load(Ordering::SeqCst), 0);
}
