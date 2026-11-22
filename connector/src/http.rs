//! Formal HTTP catalog boundary; selection stays in the independent B1 core.

use crate::{
    Catalog, CoreError, ExecutionMode, Operation, SelectionRequest,
    performance::OperationTrace,
    tasks::{TaskRecord, TaskStatus, TaskStore},
};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::{ImageDecoder, ImageFormat, ImageReader};
use reqwest::{Client, Response, Url, header::HeaderValue, multipart};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fmt,
    io::Cursor,
    path::Path,
    sync::{
        Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, SystemTime, UNIX_EPOCH},
};
use tokio::io::AsyncReadExt;

pub const API_BASE: &str = "https://api.tokemula.com/v1/";
const MAX_IMAGE_BYTES: usize = 20 * 1024 * 1024;
const MAX_METADATA_RESPONSE_BYTES: usize = 64 * 1024 * 1024;
// shortcut: task GET covers the current 10-output server contract; raise this if that contract expands.
const MAX_TASK_RESPONSE_BYTES: usize =
    10 * (MAX_IMAGE_BYTES.div_ceil(3) * 4 + 16 * 1024) + 1024 * 1024;

#[derive(Clone)]
pub struct ImageClient {
    api: Client,
    download: Client,
    base: Url,
    authorization: Option<HeaderValue>,
    pub(crate) task_store: Option<TaskStore>,
    pub(crate) preferences: Option<crate::settings::ModelPreferences>,
    pub(crate) image_preferences: Option<crate::image_parameters::ImagePreferences>,
    trace: Option<OperationTrace>,
}

impl fmt::Debug for ImageClient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ImageClient")
            .field("credentials_configured", &self.authorization.is_some())
            .finish_non_exhaustive()
    }
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Submission {
    NotSubmitted,
    Accepted,
    Rejected,
    Unknown,
}

#[derive(Debug, Serialize)]
pub struct CallError {
    pub code: &'static str,
    pub message: &'static str,
    pub submission: Submission,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub selection: Option<Box<CoreError>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub request_ref: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub model_selection: Option<Value>,
    #[serde(flatten)]
    task: Option<Box<TaskDiagnostic>>,
    #[serde(flatten)]
    batch: Option<Box<batches::BatchDiagnostic>>,
}

#[derive(Debug, Default, Serialize)]
struct TaskDiagnostic {
    #[serde(skip_serializing_if = "Option::is_none")]
    task_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<TaskStatus>,
    #[serde(skip_serializing_if = "Option::is_none")]
    request_ref_source: Option<&'static str>,
}

impl CallError {
    pub(crate) fn new(code: &'static str, message: &'static str, submission: Submission) -> Self {
        Self {
            code,
            message,
            submission,
            selection: None,
            request_ref: None,
            model_selection: None,
            task: None,
            batch: None,
        }
    }

    pub(crate) fn with_task_reference(
        mut self,
        reference: Option<&str>,
        source: &'static str,
    ) -> Self {
        if self.request_ref.is_none()
            && let Some(reference) = reference
        {
            self.request_ref = Some(reference.to_owned());
            self.task
                .get_or_insert_with(Default::default)
                .request_ref_source = Some(source);
        }
        self
    }

    pub(crate) fn with_task_id(mut self, task_id: Option<&str>) -> Self {
        if let Some(task_id) = task_id {
            self.task
                .get_or_insert_with(Default::default)
                .task_id
                .get_or_insert_with(|| task_id.to_owned());
        }
        self
    }

    pub(crate) fn with_query_status(mut self, status: Option<TaskStatus>) -> Self {
        let task = self.task.get_or_insert_with(Default::default);
        task.status = status;
        task.query_state = Some(if status.is_some() {
            "retrieved"
        } else {
            "not_obtained"
        });
        self
    }

    pub(crate) fn cancelled(submitted: bool) -> Self {
        Self::new(
            "cancelled",
            "已停止本次等待；已提交的请求结果需另行确认。",
            if submitted {
                Submission::Unknown
            } else {
                Submission::NotSubmitted
            },
        )
    }
}

impl fmt::Display for CallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for CallError {}

impl From<CoreError> for CallError {
    fn from(selection: CoreError) -> Self {
        Self {
            code: "selection_rejected",
            message: selection.message,
            submission: Submission::NotSubmitted,
            selection: Some(Box::new(selection)),
            request_ref: None,
            model_selection: None,
            task: None,
            batch: None,
        }
    }
}

#[derive(Default)]
pub(crate) struct RequestProgress {
    submitted: AtomicBool,
    request_ref: Mutex<Option<String>>,
    request_ref_source: Mutex<Option<&'static str>>,
    task_id: Mutex<Option<String>>,
    accepted: AtomicBool,
    query: bool,
    task_status: Mutex<Option<TaskStatus>>,
}

impl RequestProgress {
    pub(crate) fn for_task(task_id: &str) -> Self {
        Self {
            task_id: Mutex::new(valid_request_ref(task_id).then(|| task_id.to_owned())),
            query: true,
            ..Self::default()
        }
    }

    pub(crate) fn cancelled(&self) -> CallError {
        let mut error = CallError::cancelled(self.submitted.load(Ordering::SeqCst))
            .with_task_reference(
                self.request_ref
                    .lock()
                    .expect("request progress")
                    .as_deref(),
                self.request_ref_source
                    .lock()
                    .expect("request progress")
                    .unwrap_or("submission_header"),
            )
            .with_task_id(self.task_id.lock().expect("task progress").as_deref());
        if self.accepted.load(Ordering::SeqCst) {
            error.submission = Submission::Accepted;
            error.message = "任务已受理；已停止本次等待，请按 task_id 查询。";
        } else if self.query {
            error.submission = Submission::Unknown;
            let status = *self.task_status.lock().expect("task progress");
            error = error.with_query_status(status);
            error.message = if status.is_some() {
                "已停止结果读取；可按相同 task_id 再次查询。"
            } else {
                "已停止查询等待，任务状态未取得；可按相同 task_id 再次查询。"
            };
        }
        error
    }
}

#[derive(Debug)]
pub(crate) struct ImageOutput {
    pub images: Vec<(Vec<u8>, &'static str)>,
    pub metadata: Value,
}

#[derive(Deserialize)]
struct ImagesResponse {
    data: Vec<ImageData>,
    model: Option<String>,
}

#[derive(Deserialize)]
struct ImageData {
    b64_json: Option<String>,
    url: Option<String>,
    expires_at: Option<u64>,
}

#[derive(Deserialize)]
struct TaskResponse {
    task_id: String,
    request_ref: Option<String>,
    status: TaskStatus,
    created_at: u64,
    completed_at: Option<u64>,
    expires_at: u64,
    result_expires_at: Option<u64>,
    result: Option<Value>,
    error: Option<Value>,
}

fn invalid_task_response() -> CallError {
    CallError::new(
        "invalid_task_response",
        "任务响应标识或状态无效；请保留引用后查询，生成请求不会重发。",
        Submission::Unknown,
    )
}

fn retry_after(response: &Response) -> Option<u64> {
    response
        .headers()
        .get("retry-after")?
        .to_str()
        .ok()?
        .parse()
        .ok()
}

impl TaskResponse {
    fn parse(value: Value, expected_id: Option<&str>) -> Result<Self, CallError> {
        let task_id = value["task_id"].as_str().filter(|id| valid_request_ref(id));
        let identity_matches = task_id.is_some_and(|id| {
            expected_id.is_none_or(|expected| expected == id)
                && value
                    .get("id")
                    .is_none_or(|alias| alias.is_null() || alias.as_str() == Some(id))
        });
        let request_ref = identity_matches
            .then(|| {
                value["request_ref"]
                    .as_str()
                    .filter(|reference| valid_request_ref(reference))
            })
            .flatten();
        let invalid = invalid_task_response()
            .with_task_id(expected_id.or(task_id))
            .with_task_reference(request_ref, "task_json");
        if !identity_matches {
            return Err(invalid);
        }
        match serde_json::from_value::<Self>(value) {
            Ok(task) if task.request_ref.as_deref().is_none_or(valid_request_ref) => Ok(task),
            _ => Err(invalid),
        }
    }

    fn failure(&self) -> Value {
        let error = self.error.as_ref();
        let kind = error.and_then(|value| value["type"].as_str());
        let code = error.and_then(|value| value["code"].as_str());
        let message = error.and_then(|value| value["message"].as_str());
        let (code, message) = if kind == Some("timeout_error") || code == Some("timeout_error") {
            (
                "image_task_timeout",
                "图片任务执行超时，请到 Tokemula 网站查看使用记录。",
            )
        } else if kind == Some("api_error")
            && message == Some("failed to store generated image to object storage")
        {
            (
                "image_result_storage_failed",
                "图片结果转存失败，请到 Tokemula 网站查看使用记录。",
            )
        } else {
            (
                "image_task_failed",
                "图片任务执行失败，请到 Tokemula 网站查看使用记录。",
            )
        };
        json!({"code":code,"message":message})
    }

    fn record(&self, selected_model: Option<String>, known_ref: Option<&str>) -> TaskRecord {
        TaskRecord {
            task_id: self.task_id.clone(),
            request_ref: self
                .request_ref
                .clone()
                .or_else(|| known_ref.map(str::to_owned)),
            selected_model,
            status: self.status,
            created_at: self.created_at,
            completed_at: self.completed_at,
            expires_at: self.expires_at,
            result_expires_at: self.result_expires_at,
            observed_at: SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("system clock after Unix epoch")
                .as_secs(),
        }
    }

    fn metadata(&self, record: &TaskRecord, retry_after: Option<u64>) -> Value {
        json!({
            "task_id":self.task_id, "request_ref":self.request_ref, "status":self.status,
            "created_at":self.created_at, "completed_at":self.completed_at,
            "expires_at":self.expires_at, "result_expires_at":self.result_expires_at,
            "retry_after_seconds":retry_after, "selected_model":record.selected_model
        })
    }
}

pub(crate) fn valid_request_ref(reference: &str) -> bool {
    !reference.is_empty()
        && reference.len() <= 128
        && reference
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'_')
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct InputLimits {
    max_images: u32,
    max_image_bytes: u64,
    mime_types: Vec<String>,
    max_request_bytes: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_prompt_bytes: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_batch_items: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_batch_output_images: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_batch_input_images: Option<u64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    max_batch_input_bytes: Option<u64>,
}

#[derive(Deserialize, Serialize)]
struct CapabilityDetails {
    input_limits: InputLimits,
    result_formats: Vec<String>,
    cancel_supported: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    edit_options: Option<edits::EditOptions>,
}

impl CapabilityDetails {
    fn valid(&self, capability: &crate::Capability) -> bool {
        let limits = &self.input_limits;
        let no_input = limits.max_images == 0;
        let valid_input = if no_input {
            limits.max_image_bytes == 0 && limits.mime_types.is_empty()
        } else {
            limits.max_image_bytes > 0 && !limits.mime_types.is_empty()
        };
        limits
            .mime_types
            .iter()
            .all(|mime| matches!(mime.as_str(), "image/png" | "image/jpeg" | "image/webp"))
            && limits.max_request_bytes > 0
            && valid_input
            && (capability.operation != Operation::Edit || !no_input)
            && !self.result_formats.is_empty()
            && self.result_formats.iter().all(|format| {
                matches!(
                    format.as_str(),
                    "b64_json" | "url" | "authenticated_download"
                )
            })
            && (!self.cancel_supported || capability.mode == ExecutionMode::Batch)
            && self.edit_options.as_ref().is_none_or(|options| {
                capability.operation == Operation::Edit
                    && capability.mode != ExecutionMode::Batch
                    && options.valid()
            })
            && ((limits.max_batch_input_images.is_none() && limits.max_batch_input_bytes.is_none())
                || (capability.operation == Operation::Edit
                    && capability.mode == ExecutionMode::Batch))
            && [
                limits.max_prompt_bytes,
                limits.max_batch_items,
                limits.max_batch_output_images,
                limits.max_batch_input_images,
                limits.max_batch_input_bytes,
            ]
            .into_iter()
            .all(|limit| limit.is_none_or(|n| n > 0))
    }
}

fn bearer(api_key: Option<String>) -> Result<Option<HeaderValue>, CallError> {
    api_key
        .filter(|s| !s.is_empty())
        .map(|key| {
            let mut value = HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| {
                CallError::new(
                    "configuration_invalid",
                    "无法初始化图片连接配置。",
                    Submission::NotSubmitted,
                )
            })?;
            value.set_sensitive(true);
            Ok(value)
        })
        .transpose()
}

impl ImageClient {
    /// The executable supplies only its explicitly named authentication value.
    pub fn new(api_key: Option<String>) -> Result<Self, CallError> {
        Self::build(API_BASE, api_key, Duration::from_secs(240))
    }

    pub(crate) fn build(
        base: &str,
        api_key: Option<String>,
        timeout: Duration,
    ) -> Result<Self, CallError> {
        let invalid = || {
            CallError::new(
                "configuration_invalid",
                "无法初始化图片连接配置。",
                Submission::NotSubmitted,
            )
        };
        let authorization = bearer(api_key)?;
        let builder = || {
            Client::builder()
                .no_proxy()
                .retry(reqwest::retry::never())
                .connect_timeout(Duration::from_secs(5))
                .timeout(timeout)
        };
        let api = builder()
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| invalid())?;
        let download = builder()
            .redirect(reqwest::redirect::Policy::limited(5))
            .build()
            .map_err(|_| invalid())?;
        Ok(Self {
            api,
            download,
            base: Url::parse(base).map_err(|_| invalid())?,
            authorization,
            task_store: None,
            preferences: None,
            image_preferences: None,
            trace: None,
        })
    }

    pub(crate) fn with_operation_context(
        &self,
        mut context: crate::settings::OperationContext,
    ) -> Result<Self, CallError> {
        let mut client = self.clone();
        client.authorization = bearer(context.api_key.take())?;
        client.preferences = context.preferences.take();
        client.image_preferences = context.image_preferences.take();
        Ok(client)
    }

    pub(crate) fn with_trace(&self, trace: OperationTrace) -> Self {
        let mut client = self.clone();
        client.trace = Some(trace);
        client
    }

    fn mark(&self, stage: &'static str) {
        if let Some(trace) = &self.trace {
            trace.mark(stage);
        }
    }

    fn counts(&self, stage: &'static str, counts: &[(&'static str, usize)]) {
        if let Some(trace) = &self.trace {
            trace.counts(stage, counts);
        }
    }

    fn authorization(&self) -> Result<HeaderValue, CallError> {
        self.authorization.clone().ok_or_else(|| {
            CallError::new(
                "credential_unavailable",
                "尚未配置图片 API 凭据。",
                Submission::NotSubmitted,
            )
        })
    }

    pub(crate) async fn catalog(&self) -> Result<(Catalog, Value), CallError> {
        let auth = self.authorization()?;
        self.mark("catalog_get_start");
        let response = self
            .api
            .get(
                self.base
                    .join("images/models")
                    .expect("fixed relative route"),
            )
            .header(reqwest::header::AUTHORIZATION, auth)
            .send()
            .await
            .map_err(|e| transport_error(&e, false))?;
        self.counts(
            "catalog_headers",
            &[("http_status", response.status().as_u16() as usize)],
        );
        self.mark("catalog_body_start");
        let wire = read_json_traced(
            response,
            false,
            MAX_METADATA_RESPONSE_BYTES,
            self.trace.as_ref(),
            "catalog_body_complete",
            "catalog_json_complete",
        )
        .await?;
        self.mark("catalog_validation_start");
        let invalid = || {
            CallError::new(
                "invalid_catalog",
                "图片能力目录不符合正式契约。",
                Submission::NotSubmitted,
            )
        };
        if wire.get("schema_version").and_then(Value::as_str) != Some("1") {
            return Err(invalid());
        }
        let observed_at = wire
            .get("observed_at")
            .and_then(Value::as_str)
            .filter(|s| !s.trim().is_empty())
            .ok_or_else(invalid)?;
        let models = wire
            .get("models")
            .and_then(Value::as_array)
            .ok_or_else(invalid)?;
        let mut core_models = models.clone();
        // The offline selection contract keeps its placeholder; HTTP results omit prices.
        for model in &mut core_models {
            model
                .as_object_mut()
                .ok_or_else(invalid)?
                .insert("pricing".into(), json!({"status":"unknown"}));
        }
        let core = json!({"can_submit":wire.get("can_submit"),"reason_code":wire.get("reason_code"),"data":core_models});
        let catalog = Catalog::from_json(&core.to_string())?;
        let mut visible_models = serde_json::to_value(catalog.models()).map_err(|_| invalid())?;
        for ((source, selected), visible) in models.iter().zip(catalog.models()).zip(
            visible_models
                .as_array_mut()
                .expect("serialized model array"),
        ) {
            visible
                .as_object_mut()
                .expect("serialized model")
                .remove("pricing");
            for ((source_capability, capability), visible_capability) in source["capabilities"]
                .as_array()
                .expect("core validated capabilities")
                .iter()
                .zip(&selected.capabilities)
                .zip(
                    visible["capabilities"]
                        .as_array_mut()
                        .expect("serialized capabilities"),
                )
            {
                let details: CapabilityDetails =
                    serde_json::from_value(source_capability.clone()).map_err(|_| invalid())?;
                if !details.valid(capability) {
                    return Err(invalid());
                }
                visible_capability
                    .as_object_mut()
                    .expect("serialized capability")
                    .extend(
                        serde_json::to_value(details)
                            .map_err(|_| invalid())?
                            .as_object()
                            .expect("serialized capability details")
                            .clone(),
                    );
            }
        }
        let restriction = if catalog.can_submit() {
            Value::Null
        } else {
            let request = SelectionRequest::new(Operation::Generation, ExecutionMode::Sync);
            serde_json::to_value(catalog.select(&request).expect_err("submission disabled"))
                .map_err(|_| invalid())?
        };
        let visible = json!({"schema_version":"1","observed_at":observed_at,"can_submit":catalog.can_submit(),"submission_restriction":restriction,"models":visible_models});
        self.mark("catalog_validation_complete");
        Ok((catalog, visible))
    }

    pub(crate) async fn execute(
        &self,
        input: &crate::mcp::GenerateInput,
        edit: Option<&crate::mcp::EditInput>,
        progress: &RequestProgress,
    ) -> Result<ImageOutput, CallError> {
        self.authorization()?;
        if input.prompt.trim().is_empty() {
            return Err(CallError::new(
                "invalid_parameters",
                "图片提示词不能为空。",
                Submission::NotSubmitted,
            ));
        }
        let (catalog, visible) = self.catalog().await?;
        let operation = if edit.is_some() {
            Operation::Edit
        } else {
            Operation::Generation
        };
        let mode = match input.mode.unwrap_or_default() {
            crate::mcp::ImageMode::Sync => ExecutionMode::Sync,
            crate::mcp::ImageMode::SingleAsync => ExecutionMode::SingleAsync,
        };
        let mut resolved = input.clone();
        if let Some(profile) = self
            .image_preferences
            .as_ref()
            .and_then(|p| p.profile(operation, mode))
        {
            if resolved.size.is_none() {
                resolved.size = profile.size.clone();
            }
            if resolved.quality.is_none() {
                resolved.quality = profile.quality.clone();
            }
        }
        let input = &resolved;
        let request = SelectionRequest {
            model_id: input.model.as_deref(),
            operation,
            mode,
            size: input.size.as_deref(),
            quality: input.quality.as_deref(),
            image_count: input.n.unwrap_or(1),
        };
        // A successful image reply must fit all requested originals after base64 encoding.
        let response_limit = (request.image_count as usize)
            .checked_mul(MAX_IMAGE_BYTES.div_ceil(3) * 4 + 16 * 1024)
            .and_then(|bytes| bytes.checked_add(1024 * 1024))
            .ok_or_else(|| {
                CallError::new(
                    "invalid_parameters",
                    "请求图片数量超过当前平台可处理范围。",
                    Submission::NotSubmitted,
                )
            })?;
        let asynchronous = request.mode == ExecutionMode::SingleAsync;
        self.mark("candidate_selection_start");
        let (selected_id, request, request_parameters) = if let Some(edit) = edit {
            self.edit_request(&catalog, &visible, &request, input, edit)
                .await?
        } else {
            self.generation_request(&catalog, &visible, &request, input, asynchronous)?
        };
        self.mark("candidate_selection_complete");
        let model_selection =
            self.preference_metadata(&catalog, &selected_id, input.model.as_deref());
        progress.submitted.store(true, Ordering::SeqCst);
        if let Some(trace) = &self.trace {
            trace.submission(Submission::Unknown);
        }
        self.mark("image_post_start");
        let response = self.api.execute(request).await.map_err(|e| {
            let mut error = transport_error(&e, !e.is_connect());
            if !model_selection.is_null() {
                error.model_selection = Some(model_selection.clone());
            }
            error
        })?;
        self.counts(
            "image_post_headers",
            &[("http_status", response.status().as_u16() as usize)],
        );
        let request_ref = response
            .headers()
            .get("x-client-request-id")
            .and_then(|v| v.to_str().ok())
            .filter(|s| valid_request_ref(s))
            .map(str::to_owned);
        *progress.request_ref.lock().expect("request progress") = request_ref.clone();
        *progress
            .request_ref_source
            .lock()
            .expect("request progress") = Some("submission_header");
        async {
            let status = response.status();
            let retry_after = retry_after(&response);
            self.mark("image_post_body_start");
            let value = read_json_traced(
                response,
                true,
                if asynchronous {
                    MAX_METADATA_RESPONSE_BYTES
                } else {
                    response_limit
                },
                self.trace.as_ref(),
                "image_post_body_complete",
                "image_post_json_complete",
            )
            .await?;
            if asynchronous {
                let task = TaskResponse::parse(value, None)?;
                if status != reqwest::StatusCode::ACCEPTED
                    || task.request_ref.is_none()
                    || task.request_ref != request_ref
                    || task.status != TaskStatus::Processing
                {
                    return Err(invalid_task_response()
                        .with_task_id(Some(&task.task_id))
                        .with_task_reference(task.request_ref.as_deref(), "task_json"));
                }
                *progress.task_id.lock().expect("task progress") = Some(task.task_id.clone());
                progress.accepted.store(true, Ordering::SeqCst);
                if let Some(trace) = &self.trace {
                    trace.submission(Submission::Accepted);
                }
                let record = task.record(Some(selected_id), None);
                let mut metadata = task.metadata(&record, retry_after);
                metadata["submission"] = json!("accepted");
                if !model_selection.is_null() {
                    metadata["model_selection"] = model_selection.clone();
                }
                // No await between accepting identifiers and persisting them: cancellation cannot skip this write.
                metadata["recovery"] = self.save_task(&record);
                return Ok(ImageOutput {
                    images: vec![],
                    metadata,
                });
            }
            let mut metadata = json!({
                "selected_model":selected_id, "request_ref":request_ref,
                "request_parameters":request_parameters,
                "connector_version":env!("CARGO_PKG_VERSION")
            });
            if !model_selection.is_null() {
                metadata["model_selection"] = model_selection.clone();
            }
            self.image_output(value, metadata).await
        }
        .await
        .map_err(|mut error: CallError| {
            if !model_selection.is_null() {
                error.model_selection = Some(model_selection.clone());
            }
            error.with_task_reference(request_ref.as_deref(), "submission_header")
        })
    }

    async fn image_output(
        &self,
        value: Value,
        mut metadata: Value,
    ) -> Result<ImageOutput, CallError> {
        let response: ImagesResponse = serde_json::from_value(value).map_err(|_| {
            CallError::new(
                "invalid_response",
                "图片服务返回了无效结果，提交结果需确认。",
                Submission::Unknown,
            )
        })?;
        if response.data.is_empty() {
            return Err(CallError::new(
                "invalid_response",
                "图片服务未返回图片，提交结果需确认。",
                Submission::Unknown,
            ));
        }
        let mut images = Vec::new();
        let mut remote_expiries = Vec::new();
        let mut dimensions = Vec::new();
        for (image_index, item) in response.data.into_iter().enumerate() {
            let bytes = match (
                item.b64_json.filter(|encoded| !encoded.trim().is_empty()),
                item.url,
            ) {
                (Some(encoded), _) => {
                    self.counts(
                        "image_source_b64",
                        &[
                            ("image_index", image_index),
                            ("encoded_bytes", encoded.len()),
                        ],
                    );
                    self.mark("image_decode_start");
                    let bytes = STANDARD.decode(encoded).map_err(|_| {
                        CallError::new("invalid_image", "图片结果编码无效。", Submission::Unknown)
                    })?;
                    self.counts(
                        "image_decode_complete",
                        &[("image_index", image_index), ("bytes", bytes.len())],
                    );
                    bytes
                }
                (None, Some(url)) => {
                    self.counts("image_source_url", &[("image_index", image_index)]);
                    let url = Url::parse(&url).map_err(|_| {
                        CallError::new("invalid_image", "图片下载地址无效。", Submission::Unknown)
                    })?;
                    if !matches!(url.scheme(), "http" | "https")
                        || !url.username().is_empty()
                        || url.password().is_some()
                    {
                        return Err(CallError::new(
                            "invalid_image",
                            "图片下载地址无效。",
                            Submission::Unknown,
                        ));
                    }
                    self.mark("image_download_start");
                    let download = self
                        .download
                        .get(url)
                        .send()
                        .await
                        .map_err(|e| transport_error(&e, true))?;
                    self.counts(
                        "image_download_headers",
                        &[("http_status", download.status().as_u16() as usize)],
                    );
                    if !download.status().is_success() {
                        return Err(CallError::new(
                            "image_download_failed",
                            "图片下载未成功，生成请求不会重发。",
                            Submission::Unknown,
                        ));
                    }
                    self.mark("image_download_body_start");
                    let bytes = read_bytes(download, MAX_IMAGE_BYTES, true).await?;
                    self.counts(
                        "image_download_body_complete",
                        &[("image_index", image_index), ("bytes", bytes.len())],
                    );
                    bytes
                }
                _ => {
                    return Err(CallError::new(
                        "invalid_response",
                        "图片结果必须提供一种有效图片来源。",
                        Submission::Unknown,
                    ));
                }
            };
            self.counts("image_validation_start", &[("image_index", image_index)]);
            let (mime, width, height) = image_info(&bytes, Submission::Unknown)?;
            self.counts(
                "image_validation_complete",
                &[("image_index", image_index), ("bytes", bytes.len())],
            );
            dimensions.push(json!({"width":width,"height":height}));
            images.push((bytes, mime));
            remote_expiries.push(item.expires_at);
        }
        metadata["image_dimensions"] = json!(dimensions);
        metadata["effective_model"] = json!(response.model);
        metadata["effective_model_source"] = json!(if response.model.is_some() {
            Some("server_response")
        } else {
            None
        });
        metadata["remote_expires_at"] = json!(remote_expiries);
        metadata["local_files"] = json!([]);
        if let Some(trace) = &self.trace {
            trace.submission(Submission::Accepted);
        }
        Ok(ImageOutput { images, metadata })
    }

    fn save_task(&self, record: &TaskRecord) -> Value {
        let result = self
            .task_store
            .as_ref()
            .ok_or_else(|| {
                CallError::new(
                    "task_storage_unavailable",
                    "未找到当前用户的任务恢复目录；请保存 task_id 和 request_ref。",
                    Submission::Accepted,
                )
            })
            .and_then(|store| store.save(record));
        match result {
            Ok(()) => json!({"status":"saved"}),
            Err(error) => {
                json!({"status":"failed", "error":{"code":error.code,"message":error.message}})
            }
        }
    }

    pub(crate) fn task_context_error(&self, task_id: &str, mut error: CallError) -> CallError {
        let safe_id = valid_request_ref(task_id).then_some(task_id);
        let local = safe_id.and_then(|id| {
            self.task_store
                .as_ref()
                .and_then(|store| store.load(id).ok().flatten())
        });
        error.submission = Submission::Unknown;
        error
            .with_task_id(safe_id)
            .with_query_status(None)
            .with_task_reference(
                local
                    .as_ref()
                    .and_then(|record| record.request_ref.as_deref()),
                "local_record",
            )
    }

    pub(crate) async fn task(
        &self,
        task_id: &str,
        progress: &RequestProgress,
    ) -> Result<ImageOutput, CallError> {
        if !valid_request_ref(task_id) {
            return Err(CallError::new(
                "invalid_parameters",
                "任务 ID 格式无效。",
                Submission::NotSubmitted,
            ));
        }
        let (local, storage_error) = match self.task_store.as_ref().map(|store| store.load(task_id))
        {
            Some(Ok(record)) => (record, None),
            Some(Err(error)) => (None, Some(error)),
            None => (None, None),
        };
        let local_ref = local
            .as_ref()
            .and_then(|record| record.request_ref.as_deref());
        *progress.request_ref.lock().expect("request progress") = local_ref.map(str::to_owned);
        *progress
            .request_ref_source
            .lock()
            .expect("request progress") = Some("local_record");
        let result = async {
            let auth = self.authorization()?;
            let mut url = self.base.join("images/tasks/").expect("fixed route");
            url.path_segments_mut()
                .expect("HTTP URL")
                .pop_if_empty()
                .push(task_id);
            let response = self
                .api
                .get(url)
                .header(reqwest::header::AUTHORIZATION, auth)
                .send()
                .await
                .map_err(|_| {
                    CallError::new(
                        "task_query_failed",
                        "任务状态未取得；可按相同 task_id 再次查询。",
                        Submission::Unknown,
                    )
                })?;
            if response.status() == reqwest::StatusCode::NOT_FOUND {
                return Err(CallError::new(
                    "task_not_found",
                    "任务记录已到期或当前 Key 无法读取；请保留已有的 request_ref。",
                    Submission::Unknown,
                ));
            }
            let retry_after = retry_after(&response);
            let value = read_json(response, false, MAX_TASK_RESPONSE_BYTES)
                .await
                .map_err(|_| {
                    CallError::new(
                        "task_query_failed",
                        "任务状态未取得；可按相同 task_id 再次查询。",
                        Submission::Unknown,
                    )
                })?;
            let task = TaskResponse::parse(value, Some(task_id))?;
            *progress.task_status.lock().expect("task progress") = Some(task.status);
            *progress.request_ref.lock().expect("request progress") = task
                .request_ref
                .clone()
                .or_else(|| local_ref.map(str::to_owned));
            *progress
                .request_ref_source
                .lock()
                .expect("request progress") = Some(if task.request_ref.is_some() {
                "task_json"
            } else {
                "local_record"
            });
            let record = task.record(
                local.as_ref().and_then(|r| r.selected_model.clone()),
                local_ref,
            );
            let mut metadata = task.metadata(&record, retry_after);
            metadata["query_state"] = json!("retrieved");
            metadata["recovery"] = self.save_task(&record);
            if task.request_ref.is_none()
                && let Some(reference) = local_ref
            {
                metadata["recovery"]["request_ref"] = json!(reference);
                metadata["recovery"]["request_ref_source"] = json!("local_record");
            }
            if let Some(error) = &storage_error {
                metadata["recovery"]["read_error"] =
                    json!({"code":error.code,"message":error.message});
            }
            match task.status {
                TaskStatus::Processing => Ok(ImageOutput {
                    images: vec![],
                    metadata,
                }),
                TaskStatus::Failed => {
                    metadata["failure"] = task.failure();
                    Ok(ImageOutput {
                        images: vec![],
                        metadata,
                    })
                }
                TaskStatus::Completed => {
                    self.image_output(task.result.ok_or_else(invalid_task_response)?, metadata)
                        .await
                }
            }
        }
        .await;
        result.map_err(|mut error: CallError| {
            error.submission = Submission::Unknown;
            error = error
                .with_task_id(Some(task_id))
                .with_query_status(*progress.task_status.lock().expect("task progress"));
            error = error.with_task_reference(
                progress
                    .request_ref
                    .lock()
                    .expect("request progress")
                    .as_deref(),
                progress
                    .request_ref_source
                    .lock()
                    .expect("request progress")
                    .unwrap_or("local_record"),
            );
            error
        })
    }
}

fn bounded_request(
    builder: reqwest::RequestBuilder,
    max_bytes: u64,
) -> Result<reqwest::Request, CallError> {
    let request = builder.build().map_err(|_| {
        CallError::new(
            "invalid_parameters",
            "无法编码图片请求。",
            Submission::NotSubmitted,
        )
    })?;
    // reqwest supplies the full multipart length for our text/bytes-only form.
    let body_bytes = match request.body().and_then(reqwest::Body::as_bytes) {
        Some(bytes) => bytes.len() as u64,
        None => request
            .headers()
            .get(reqwest::header::CONTENT_LENGTH)
            .expect("text/bytes multipart has a known length")
            .to_str()
            .expect("reqwest-generated content length")
            .parse::<u64>()
            .expect("reqwest-generated byte count"),
    };
    if body_bytes > max_bytes {
        return Err(CallError::new(
            "invalid_parameters",
            "编码后的图片请求超过模型请求大小限制。",
            Submission::NotSubmitted,
        ));
    }
    Ok(request)
}

async fn read_local_image(
    path: &str,
    max_bytes: u64,
) -> Result<(Vec<u8>, &'static str), CallError> {
    let invalid = || {
        CallError::new(
            "invalid_image",
            "无法读取符合大小限制的指定图片文件。",
            Submission::NotSubmitted,
        )
    };
    let path = Path::new(path);
    if !path.is_absolute() {
        return Err(CallError::new(
            "invalid_parameters",
            "图片需要用户提供的本地绝对路径。",
            Submission::NotSubmitted,
        ));
    }
    let file = tokio::fs::File::open(path).await.map_err(|_| invalid())?;
    let metadata = file.metadata().await.map_err(|_| invalid())?;
    if !metadata.is_file() || metadata.len() > max_bytes {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .await
        .map_err(|_| invalid())?;
    if bytes.len() as u64 > max_bytes {
        return Err(invalid());
    }
    let mime = image_mime(&bytes, Submission::NotSubmitted)?;
    Ok((bytes, mime))
}

#[path = "batches.rs"]
mod batches;
pub(crate) use batches::{BatchProgress, owned_batch_record_file};

#[path = "edits.rs"]
mod edits;

#[path = "preferences.rs"]
mod preferences;

fn transport_error(error: &reqwest::Error, submitted: bool) -> CallError {
    let state = if submitted {
        Submission::Unknown
    } else {
        Submission::NotSubmitted
    };
    if error.is_timeout() {
        CallError::new("timeout", "图片服务请求超时；不会自动再次提交。", state)
    } else {
        CallError::new(
            "transport_failed",
            "图片服务连接中断；不会自动再次提交。",
            state,
        )
    }
}

async fn read_bytes(
    mut response: Response,
    limit: usize,
    submitted: bool,
) -> Result<Vec<u8>, CallError> {
    let state = if submitted {
        Submission::Unknown
    } else {
        Submission::NotSubmitted
    };
    let invalid = || CallError::new("invalid_response", "服务响应超过大小限制。", state);
    if response.content_length().is_some_and(|n| n > limit as u64) {
        return Err(invalid());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|e| transport_error(&e, submitted))?
    {
        if bytes.len() + chunk.len() > limit {
            return Err(invalid());
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}

async fn read_json(
    response: Response,
    submitted: bool,
    success_limit: usize,
) -> Result<Value, CallError> {
    read_json_traced(
        response,
        submitted,
        success_limit,
        None,
        "body_complete",
        "json_complete",
    )
    .await
}

async fn read_json_traced(
    response: Response,
    submitted: bool,
    success_limit: usize,
    trace: Option<&OperationTrace>,
    body_stage: &'static str,
    json_stage: &'static str,
) -> Result<Value, CallError> {
    let status = response.status();
    let limit = if status.is_success() {
        success_limit
    } else {
        MAX_METADATA_RESPONSE_BYTES
    };
    let bytes = read_bytes(response, limit, submitted).await?;
    if let Some(trace) = trace {
        trace.counts(body_stage, &[("bytes", bytes.len())]);
    }
    if !status.is_success() {
        let code = serde_json::from_slice::<Value>(&bytes).ok().and_then(|v| {
            v.get("error")
                .and_then(|e| e.get("code"))
                .and_then(Value::as_str)
                .map(str::to_owned)
        });
        let (code, message) = match (status.as_u16(), code.as_deref()) {
            (401, _) => ("authentication_failed", "图片 API 认证失败。"),
            (_, Some("insufficient_balance")) => ("insufficient_balance", "图片账户余额不足。"),
            (_, Some("quota_exhausted")) => ("quota_exhausted", "图片额度已耗尽。"),
            (403, _) => ("permission_denied", "当前 Key 没有此操作权限。"),
            (400 | 422, _) => ("invalid_parameters", "图片服务拒绝了请求参数。"),
            (429, _) => ("rate_limited", "图片服务当前限制请求速率或额度。"),
            (300..=399, _) => (
                "redirect_refused",
                "图片 API 重定向已拒绝，提交结果需确认。",
            ),
            _ => ("upstream_failed", "图片服务未完成请求，结果需确认。"),
        };
        let state = if !submitted {
            Submission::NotSubmitted
        } else if status.is_client_error() {
            Submission::Rejected
        } else {
            Submission::Unknown
        };
        return Err(CallError::new(code, message, state));
    }
    let result = serde_json::from_slice(&bytes).map_err(|_| {
        CallError::new(
            "invalid_response",
            "服务响应格式无效。",
            if submitted {
                Submission::Unknown
            } else {
                Submission::NotSubmitted
            },
        )
    });
    if let Some(trace) = trace {
        trace.mark(json_stage);
    }
    result
}

fn image_mime(bytes: &[u8], submission: Submission) -> Result<&'static str, CallError> {
    image_info(bytes, submission).map(|(mime, _, _)| mime)
}

fn image_info(bytes: &[u8], submission: Submission) -> Result<(&'static str, u32, u32), CallError> {
    let invalid = || {
        CallError::new(
            "invalid_image",
            "图片必须是有效的 PNG、JPEG 或 WebP，并符合大小限制。",
            submission,
        )
    };
    if bytes.len() > MAX_IMAGE_BYTES {
        return Err(invalid());
    }
    let reader = ImageReader::new(Cursor::new(bytes))
        .with_guessed_format()
        .map_err(|_| invalid())?;
    let mime = match reader.format() {
        Some(ImageFormat::Png) => "image/png",
        Some(ImageFormat::Jpeg) => "image/jpeg",
        Some(ImageFormat::WebP) => "image/webp",
        _ => return Err(invalid()),
    };
    let decoder = reader.into_decoder().map_err(|_| invalid())?;
    let (width, height) = decoder.dimensions();
    if u64::from(width) * u64::from(height) > 64_000_000 {
        return Err(invalid());
    }
    image::DynamicImage::from_decoder(decoder).map_err(|_| invalid())?;
    Ok((mime, width, height))
}

#[cfg(test)]
#[path = "http_tests.rs"]
mod tests;
