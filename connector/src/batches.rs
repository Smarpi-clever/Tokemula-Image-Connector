//! Batch wire, explicit recovery, and bounded reads. Single-image tasks retain their own format.

use super::*;
use crate::mcp::{BatchGetInput, BatchSubmitInput};
use crate::output::prepare_private_directory;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use std::{collections::HashSet, fs, path::PathBuf, sync::atomic::AtomicU64};

#[derive(Debug, Default, Clone, Serialize)]
pub(crate) struct BatchDiagnostic {
    #[serde(skip_serializing_if = "Option::is_none")]
    batch_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    custom_id: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    image_index: Option<u32>,
    #[serde(skip_serializing_if = "Option::is_none")]
    idempotency_key: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    task_name: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    selected_model: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    query_state: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    cancel_state: Option<&'static str>,
}

impl CallError {
    pub(crate) fn with_batch_metadata(mut self, metadata: &Value) -> Self {
        if let Some(id) = metadata["batch_id"].as_str() {
            self.batch = Some(Box::new(BatchDiagnostic {
                batch_id: Some(id.to_owned()),
                custom_id: metadata["custom_id"].as_str().map(str::to_owned),
                image_index: metadata["image_index"].as_u64().map(|n| n as u32),
                idempotency_key: metadata["idempotency_key"].as_str().map(str::to_owned),
                status: metadata["status"].as_str().map(str::to_owned),
                query_state: Some("retrieved"),
                ..Default::default()
            }));
        }
        self
    }
}

pub(crate) struct BatchProgress {
    diagnostic: Mutex<BatchDiagnostic>,
    submitted: AtomicBool,
    query: bool,
    cancel: bool,
}

impl BatchProgress {
    pub(crate) fn submission() -> Self {
        Self {
            diagnostic: Mutex::default(),
            submitted: AtomicBool::new(false),
            query: false,
            cancel: false,
        }
    }

    pub(crate) fn query(input: &BatchGetInput) -> Self {
        let mut progress = Self::submission();
        progress.query = true;
        *progress.diagnostic.get_mut().expect("batch progress") = BatchDiagnostic {
            batch_id: input
                .batch_id
                .as_ref()
                .filter(|id| valid_request_ref(id))
                .cloned(),
            custom_id: input
                .custom_id
                .as_ref()
                .filter(|id| valid_custom_id(id))
                .cloned(),
            image_index: input.image_index,
            query_state: Some("not_obtained"),
            ..Default::default()
        };
        progress
    }

    pub(crate) fn cancellation(id: &str) -> Self {
        let mut progress = Self::submission();
        progress.cancel = true;
        progress
            .diagnostic
            .get_mut()
            .expect("batch progress")
            .batch_id = valid_request_ref(id).then(|| id.to_owned());
        progress
    }

    pub(crate) fn error(&self, mut error: CallError) -> CallError {
        let mut diagnostic = self.diagnostic.lock().expect("batch progress").clone();
        let rejected = error.submission == Submission::Rejected;
        if self.query || self.cancel {
            error.submission = Submission::Unknown;
        }
        if self.cancel {
            diagnostic.cancel_state = Some(if error.code == "batch_cancel_failed" {
                "failed"
            } else if rejected || error.code == "invalid_parameters" {
                "rejected"
            } else {
                "unknown"
            });
        }
        error.batch = Some(Box::new(diagnostic));
        error
    }

    pub(crate) fn cancelled(&self) -> CallError {
        self.error(CallError::cancelled(self.submitted.load(Ordering::SeqCst)))
    }

    fn observe(&self, batch: &BatchDto) {
        let mut diagnostic = self.diagnostic.lock().expect("batch progress");
        diagnostic.batch_id = Some(batch.id.clone());
        diagnostic.status = Some(batch.status.clone());
        diagnostic.query_state = Some("retrieved");
    }
}

#[derive(Deserialize, Serialize)]
struct BatchDto {
    id: String,
    object: String,
    task_name: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    parent_batch_id: Option<String>,
    status: String,
    model: String,
    provider: String,
    item_count: u64,
    success_count: u64,
    fail_count: u64,
    created_at: i64,
    submitted_at: Option<i64>,
    settled_at: Option<i64>,
    output_expires_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    downloaded_at: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_deleted_at: Option<i64>,
}

fn invalid_batch() -> CallError {
    CallError::new(
        "invalid_batch_response",
        "批量响应格式无效；请保留已有引用后查询。",
        Submission::Unknown,
    )
}

impl BatchDto {
    fn parse(
        value: Value,
        expected: Option<&str>,
        progress: &BatchProgress,
    ) -> Result<Self, CallError> {
        let id = value["id"].as_str().filter(|id| valid_request_ref(id));
        if id.is_some_and(|id| expected.is_none_or(|e| e == id)) {
            progress.diagnostic.lock().expect("batch progress").batch_id = id.map(str::to_owned);
        }
        let batch: Self = serde_json::from_value(value).map_err(|_| invalid_batch())?;
        if !valid_request_ref(&batch.id)
            || expected.is_some_and(|id| id != batch.id)
            || batch.object != "image.batch"
            || !matches!(
                batch.status.as_str(),
                "queued"
                    | "running"
                    | "processing_results"
                    | "settling"
                    | "completed"
                    | "failed"
                    | "cancelled"
                    | "output_deleted"
            )
        {
            return Err(invalid_batch());
        }
        Ok(batch)
    }

    fn metadata(&self) -> Value {
        let mut value = serde_json::to_value(self).expect("batch DTO");
        value["batch_id"] = json!(self.id);
        value["query_state"] = json!("retrieved");
        value
    }
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct BatchRecord {
    record_type: String,
    idempotency_key: String,
    task_name: String,
    selected_model: String,
    image_size: String,
    response_mime_type: String,
    submission: Submission,
    batch_id: Option<String>,
    status: Option<String>,
    observed_at: u64,
}

struct BatchStore {
    root: PathBuf,
}

fn store_error() -> CallError {
    CallError::new(
        "batch_storage_failed",
        "批量恢复记录无法保存；请保留幂等键及已取得的 batch_id。",
        Submission::NotSubmitted,
    )
}

impl BatchStore {
    fn path(&self, key: &str) -> PathBuf {
        self.root
            .join(format!("{}.json", URL_SAFE_NO_PAD.encode(key)))
    }
    fn load(&self, key: &str) -> Result<Option<BatchRecord>, CallError> {
        match fs::symlink_metadata(&self.root) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Ok(m) if m.is_dir() && !m.file_type().is_symlink() => (),
            _ => return Err(store_error()),
        }
        let path = self.path(key);
        match fs::symlink_metadata(&path) {
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Ok(m) if m.is_file() && !m.file_type().is_symlink() && m.len() <= 16 * 1024 => (),
            _ => return Err(store_error()),
        }
        let record: BatchRecord =
            serde_json::from_slice(&fs::read(path).map_err(|_| store_error())?)
                .map_err(|_| store_error())?;
        if record.record_type != "image_batch_v1"
            || record.idempotency_key != key
            || record
                .batch_id
                .as_deref()
                .is_some_and(|id| !valid_request_ref(id))
        {
            return Err(store_error());
        }
        Ok(Some(record))
    }
    fn save(&self, record: &BatchRecord) -> Result<(), CallError> {
        prepare_private_directory(&self.root).map_err(|_| store_error())?;
        let mut file = tempfile::NamedTempFile::new_in(&self.root).map_err(|_| store_error())?;
        serde_json::to_writer(&mut file, record).map_err(|_| store_error())?;
        file.as_file().sync_all().map_err(|_| store_error())?;
        file.persist(self.path(&record.idempotency_key))
            .map_err(|_| store_error())?;
        Ok(())
    }
}

/// Canonical filename plus the original complete record validator establish cleanup ownership.
pub(crate) fn owned_batch_record_file(root: &std::path::Path, path: &std::path::Path) -> bool {
    if path.parent() != Some(root) || path.extension() != Some(std::ffi::OsStr::new("json")) {
        return false;
    }
    let Some(encoded) = path.file_stem().and_then(|s| s.to_str()) else {
        return false;
    };
    let Ok(raw) = URL_SAFE_NO_PAD.decode(encoded) else {
        return false;
    };
    let Ok(key) = String::from_utf8(raw) else {
        return false;
    };
    if !valid_key(&key) {
        return false;
    }
    let store = BatchStore {
        root: root.to_owned(),
    };
    if store.path(&key) != path {
        return false;
    }
    store.load(&key).is_ok_and(|record| record.is_some())
}

fn valid_key(key: &str) -> bool {
    !key.is_empty() && key.len() <= 128 && key.bytes().all(|b| b.is_ascii_graphic())
}
fn valid_custom_id(id: &str) -> bool {
    !id.is_empty() && id == id.trim()
}

fn routable_custom_id(id: &str) -> bool {
    valid_custom_id(id) && !matches!(id, "." | "..") && !id.chars().any(char::is_control)
}
fn parameter(message: &'static str) -> CallError {
    CallError::new("invalid_parameters", message, Submission::NotSubmitted)
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("Unix clock")
        .as_secs()
}
fn new_key() -> String {
    static SERIAL: AtomicU64 = AtomicU64::new(0);
    format!(
        "tic-batch-{:x}-{:x}-{:x}",
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("Unix clock")
            .as_nanos(),
        std::process::id(),
        SERIAL.fetch_add(1, Ordering::Relaxed)
    )
}

impl ImageClient {
    pub(crate) fn batch_context_error(
        &self,
        input: &BatchSubmitInput,
        error: CallError,
        progress: &BatchProgress,
    ) -> CallError {
        if let Some(key) = input
            .idempotency_key
            .as_deref()
            .filter(|key| valid_key(key))
        {
            let mut diagnostic = progress.diagnostic.lock().expect("batch progress");
            diagnostic.idempotency_key = Some(key.to_owned());
            if let Some(store) = &self.task_store
                && let Ok(Some(record)) = (BatchStore {
                    root: store.root.join("batches"),
                })
                .load(key)
            {
                diagnostic.batch_id = record.batch_id;
                diagnostic.task_name = Some(record.task_name);
                diagnostic.selected_model = Some(record.selected_model);
            }
        }
        progress.error(error)
    }

    pub(crate) async fn submit_batch(
        &self,
        input: &BatchSubmitInput,
        progress: &BatchProgress,
    ) -> Result<ImageOutput, CallError> {
        self.submit_batch_inner(input, progress)
            .await
            .map_err(|e| progress.error(e))
    }

    async fn submit_batch_inner(
        &self,
        input: &BatchSubmitInput,
        progress: &BatchProgress,
    ) -> Result<ImageOutput, CallError> {
        let key = input.idempotency_key.clone().unwrap_or_else(new_key);
        if !valid_key(&key) {
            return Err(parameter(
                "幂等键应为 1–128 位可打印 ASCII 字符，不含空格。",
            ));
        }
        progress
            .diagnostic
            .lock()
            .expect("batch progress")
            .idempotency_key = Some(key.clone());
        let store = BatchStore {
            root: self
                .task_store
                .as_ref()
                .ok_or_else(store_error)?
                .root
                .join("batches"),
        };
        let previous = store.load(&key)?;
        if let Some(record) = &previous {
            let mut diagnostic = progress.diagnostic.lock().expect("batch progress");
            diagnostic.batch_id = record.batch_id.clone();
            diagnostic.task_name = Some(record.task_name.clone());
            diagnostic.selected_model = Some(record.selected_model.clone());
        }
        self.authorization()?;
        let task_name = input
            .task_name
            .as_deref()
            .map(str::trim)
            .map(str::to_owned)
            .or_else(|| previous.as_ref().map(|r| r.task_name.clone()))
            .unwrap_or_else(|| format!("Image batch {key}"));
        if task_name.is_empty() || task_name.len() > 255 {
            return Err(parameter("批量名称不能为空且最多 255 个 UTF-8 字节。"));
        }
        let model = input
            .model
            .as_deref()
            .or_else(|| previous.as_ref().map(|r| r.selected_model.as_str()));
        let mut preferred_size = None;
        if input.size.is_none() && previous.is_none() {
            for item in &input.items {
                let operation = if item.reference_image_paths.is_empty() {
                    Operation::Generation
                } else {
                    Operation::Edit
                };
                if let Some(size) = self
                    .image_preferences
                    .as_ref()
                    .and_then(|p| p.profile(operation, ExecutionMode::Batch))
                    .and_then(|p| p.size.as_deref())
                {
                    if preferred_size.is_some_and(|previous| previous != size) {
                        return Err(parameter(
                            "批量生成和编辑的默认尺寸不同，请为本次批量明确指定尺寸。",
                        ));
                    }
                    preferred_size = Some(size);
                }
            }
        }
        let size = input
            .size
            .as_deref()
            .or_else(|| previous.as_ref().map(|r| r.image_size.as_str()))
            .or(preferred_size)
            .unwrap_or("1K");
        if input.items.is_empty() {
            return Err(parameter("批量任务至少需要一个条目。"));
        }
        let (catalog, visible) = self.catalog().await?;
        if !catalog.can_submit() {
            catalog.select(&SelectionRequest::new(
                Operation::Generation,
                ExecutionMode::Batch,
            ))?;
        }
        let mut ids = HashSet::new();
        let mut images = Vec::with_capacity(input.items.len());
        for item in &input.items {
            if !valid_custom_id(&item.custom_id)
                || !ids.insert(&item.custom_id)
                || item.prompt.trim().is_empty()
                || item.output_count == Some(0)
            {
                return Err(parameter(
                    "条目需要唯一的稳定 custom_id、非空提示词及正数 output_count。",
                ));
            }
            let mut refs = Vec::new();
            for path in &item.reference_image_paths {
                refs.push(read_local_image(path, MAX_IMAGE_BYTES as u64).await?);
            }
            images.push(refs);
        }
        let requests: Vec<_> = input
            .items
            .iter()
            .map(|item| SelectionRequest {
                model_id: None,
                operation: if item.reference_image_paths.is_empty() {
                    Operation::Generation
                } else {
                    Operation::Edit
                },
                mode: ExecutionMode::Batch,
                size: Some(size),
                quality: None,
                image_count: item.output_count.unwrap_or(1),
            })
            .collect();
        let wire_items: Vec<_> = input.items.iter().zip(&images).map(|(item, refs)| {
            let references: Vec<_> = refs.iter().map(|(bytes, mime)| json!({"mime_type":mime,"data":STANDARD.encode(bytes)})).collect();
            json!({"custom_id":item.custom_id,"prompt":item.prompt.trim(),"output_count":item.output_count.unwrap_or(1),"reference_images":references})
        }).collect();
        let mut choices = Vec::new();
        let mut explicit_error = None;
        let mut limit_error = None;
        for (index, candidate) in catalog.models().iter().enumerate() {
            let check = (|| {
                for request in &requests {
                    catalog.select(&SelectionRequest {
                        model_id: Some(&candidate.id),
                        ..*request
                    })?;
                }
                let body = json!({"model":candidate.id,"task_name":task_name,"image_size":size,"response_mime_type":"image/png","items":wire_items});
                let encoded = serde_json::to_vec(&body).expect("batch request JSON");
                let total_outputs: u64 = requests.iter().map(|r| u64::from(r.image_count)).sum();
                let total_refs: u64 = images
                    .iter()
                    .zip(&requests)
                    .map(|(refs, r)| refs.len() as u64 * u64::from(r.image_count))
                    .sum();
                let total_bytes: u64 = images
                    .iter()
                    .zip(&requests)
                    .map(|(refs, r)| {
                        refs.iter()
                            .map(|(bytes, _)| bytes.len() as u64)
                            .sum::<u64>()
                            * u64::from(r.image_count)
                    })
                    .sum();
                for ((item, refs), request) in input.items.iter().zip(&images).zip(&requests) {
                    let cap_index = candidate
                        .capabilities
                        .iter()
                        .position(|c| {
                            c.operation == request.operation && c.mode == ExecutionMode::Batch
                        })
                        .expect("selected batch capability");
                    let limits =
                        &visible["models"][index]["capabilities"][cap_index]["input_limits"];
                    for (field, count, message) in [
                        (
                            "max_batch_items",
                            input.items.len() as u64,
                            "批量条目数量超过模型限制。",
                        ),
                        (
                            "max_batch_output_images",
                            total_outputs,
                            "批量总输出数量超过模型限制。",
                        ),
                        (
                            "max_batch_input_images",
                            total_refs,
                            "参考图按输出数量加权后超过批量图片总量限制。",
                        ),
                        (
                            "max_batch_input_bytes",
                            total_bytes,
                            "参考图按输出数量加权后超过批量输入字节限制。",
                        ),
                        (
                            "max_prompt_bytes",
                            item.prompt.trim().len() as u64,
                            "提示词 UTF-8 字节数超过模型限制。",
                        ),
                        (
                            "max_request_bytes",
                            encoded.len() as u64,
                            "编码后的完整批量 JSON 超过请求体限制。",
                        ),
                    ] {
                        if limits[field].as_u64().is_some_and(|limit| count > limit) {
                            return Err(parameter(message));
                        }
                    }
                    if refs.len() as u64
                        > limits["max_images"]
                            .as_u64()
                            .expect("validated image count")
                        || refs.iter().any(|(bytes, mime)| {
                            bytes.len() as u64
                                > limits["max_image_bytes"].as_u64().expect("validated bytes")
                                || !limits["mime_types"]
                                    .as_array()
                                    .expect("validated MIME")
                                    .iter()
                                    .any(|allowed| allowed == mime)
                        })
                    {
                        return Err(parameter("条目参考图数量、格式或单图大小超过模型限制。"));
                    }
                }
                Ok::<_, CallError>(encoded)
            })();
            match check {
                Ok(encoded) => choices.push((candidate, encoded)),
                Err(error) if model == Some(candidate.id.as_str()) => explicit_error = Some(error),
                Err(error) if error.selection.is_none() && limit_error.is_none() => {
                    limit_error = Some(error)
                }
                Err(_) => (),
            }
        }
        let selected = choices
            .iter()
            .enumerate()
            .filter_map(|(i, (m, _))| self.model_priority(m, model).map(|priority| (i, priority)))
            .min_by_key(|(_, priority)| *priority)
            .map(|(i, _)| i);
        let Some(selected) = selected else {
            if self.preferences.is_some() && model.is_none() {
                return Err(self.preference_error(
                    &catalog,
                    choices.iter().map(|(m, _)| m.id.clone()).collect(),
                ));
            }
            let mut error = explicit_error.or(limit_error).unwrap_or_else(|| {
                CallError::from(CoreError::new(
                    if model.is_some() {
                        crate::ErrorCode::ModelNotFound
                    } else {
                        crate::ErrorCode::NoCompatibleModel
                    },
                    if model.is_some() {
                        "目录中没有指定的模型。"
                    } else {
                        "目录中没有同时满足全部条目的批量模型。"
                    },
                ))
            });
            let alternatives: Vec<_> = choices.iter().map(|(m, _)| m.id.clone()).collect();
            match &mut error.selection {
                Some(selection) => selection.alternatives = alternatives,
                None => {
                    error.selection = Some(Box::new(CoreError {
                        code: crate::ErrorCode::UnsupportedParameter,
                        message: error.message,
                        alternatives,
                    }))
                }
            }
            return Err(error);
        };
        let (selected, encoded) = choices.swap_remove(selected);
        {
            let mut d = progress.diagnostic.lock().expect("batch progress");
            d.task_name = Some(task_name.clone());
            d.selected_model = Some(selected.id.clone());
        }
        let mut record = previous.clone().unwrap_or(BatchRecord {
            record_type: "image_batch_v1".into(),
            idempotency_key: key.clone(),
            task_name,
            selected_model: selected.id.clone(),
            image_size: size.to_owned(),
            response_mime_type: "image/png".into(),
            submission: Submission::Unknown,
            batch_id: None,
            status: None,
            observed_at: now(),
        });
        record.submission = Submission::Unknown;
        record.observed_at = now();
        store.save(&record)?;
        progress.submitted.store(true, Ordering::SeqCst);
        let result = async {
            let response = self
                .api
                .post(self.base.join("images/batches").expect("fixed route"))
                .header(reqwest::header::AUTHORIZATION, self.authorization()?)
                .header("Idempotency-Key", &key)
                .header(reqwest::header::CONTENT_TYPE, "application/json")
                .body(encoded)
                .send()
                .await
                .map_err(|e| transport_error(&e, true))?;
            let value = batch_json(response, true).await?;
            let batch = BatchDto::parse(value, None, progress)?;
            progress.observe(&batch);
            Ok::<_, CallError>(batch)
        }
        .await;
        match result {
            Ok(batch) => {
                record.batch_id = Some(batch.id.clone());
                record.status = Some(batch.status.clone());
                record.submission = Submission::Accepted;
                let recovery = match store.save(&record) {
                    Ok(()) => json!({"status":"saved"}),
                    Err(_) => {
                        json!({"status":"failed","code":"batch_storage_failed","message":"任务已受理；请保存 batch_id 和幂等键，后续按 ID 查询。"})
                    }
                };
                let mut metadata = batch.metadata();
                metadata["submission"] = json!("accepted");
                metadata["idempotency_key"] = json!(key);
                metadata["selected_model"] = json!(selected.id);
                let selection = self.preference_metadata(&catalog, &selected.id, model);
                if !selection.is_null() {
                    metadata["model_selection"] = selection;
                }
                metadata["recovery"] = recovery;
                Ok(ImageOutput {
                    images: vec![],
                    metadata,
                })
            }
            Err(mut error) => {
                let selection = self.preference_metadata(&catalog, &selected.id, model);
                if !selection.is_null() {
                    error.model_selection = Some(selection);
                }
                record.submission = error.submission;
                record.batch_id = progress
                    .diagnostic
                    .lock()
                    .expect("batch progress")
                    .batch_id
                    .clone();
                // The pre-POST record already preserves unknown state if this best-effort update fails.
                let _ = store.save(&record);
                Err(error)
            }
        }
    }

    fn batch_url(
        &self,
        batch_id: Option<&str>,
        custom_id: Option<&str>,
        suffix: Option<&str>,
    ) -> Url {
        let mut url = self.base.join("images/batches").expect("fixed route");
        let mut path = url.path_segments_mut().expect("HTTP URL");
        if let Some(id) = batch_id {
            path.push(id);
        }
        if let Some(id) = custom_id {
            path.push("items").push(id);
        }
        if let Some(suffix) = suffix {
            path.push(suffix);
        }
        drop(path);
        url
    }

    pub(crate) async fn get_batch(
        &self,
        input: &BatchGetInput,
        progress: &BatchProgress,
    ) -> Result<ImageOutput, CallError> {
        self.get_batch_inner(input, progress)
            .await
            .map_err(|e| progress.error(e))
    }
    async fn get_batch_inner(
        &self,
        input: &BatchGetInput,
        progress: &BatchProgress,
    ) -> Result<ImageOutput, CallError> {
        if input
            .batch_id
            .as_deref()
            .is_some_and(|id| !valid_request_ref(id))
            || input
                .custom_id
                .as_deref()
                .is_some_and(|id| !routable_custom_id(id))
            || (input.batch_id.is_none()
                && (input.custom_id.is_some() || input.image_index.is_some()))
            || (input.custom_id.is_none() && input.image_index.is_some())
            || (input.custom_id.is_some() && (input.cursor.is_some() || input.limit.is_some()))
        {
            return Err(parameter("批量查询参数组合或标识无效。"));
        }
        let limit = input.limit.unwrap_or(20);
        let max_limit = if input.batch_id.is_some() { 500 } else { 100 };
        if limit == 0 || limit > max_limit {
            return Err(parameter("分页 limit 超过此接口允许范围。"));
        }
        let offset = match input.cursor.as_deref() {
            None => 0,
            Some(v) => v
                .parse::<u64>()
                .ok()
                .filter(|_| !v.is_empty() && v.bytes().all(|b| b.is_ascii_digit()))
                .ok_or_else(|| parameter("cursor 必须是非负数字偏移。"))?,
        };
        if offset > i64::MAX as u64 - u64::from(limit) {
            return Err(parameter("cursor 超过有效偏移范围。"));
        }
        let auth = self.authorization()?;
        let batch = if let Some(id) = input.batch_id.as_deref() {
            let response = self
                .api
                .get(self.batch_url(Some(id), None, None))
                .header(reqwest::header::AUTHORIZATION, auth.clone())
                .send()
                .await
                .map_err(|e| transport_error(&e, false))?;
            let batch = BatchDto::parse(batch_json(response, false).await?, Some(id), progress)?;
            progress.observe(&batch);
            Some(batch)
        } else {
            None
        };
        if let Some(custom_id) = input.custom_id.as_deref() {
            let batch = batch.expect("validated batch ID");
            let index = input.image_index.unwrap_or(0);
            {
                progress
                    .diagnostic
                    .lock()
                    .expect("batch progress")
                    .image_index = Some(index);
            }
            let mut url = self.batch_url(Some(&batch.id), Some(custom_id), Some("content"));
            url.query_pairs_mut()
                .append_pair("image_index", &index.to_string());
            let response = self
                .api
                .get(url)
                .header(reqwest::header::AUTHORIZATION, auth)
                .send()
                .await
                .map_err(|e| transport_error(&e, false))?;
            if response.status() != reqwest::StatusCode::OK {
                batch_json(response, false).await?;
                return Err(invalid_batch());
            }
            let declared = response
                .headers()
                .get(reqwest::header::CONTENT_TYPE)
                .and_then(|v| v.to_str().ok())
                .map(|v| v.split(';').next().unwrap_or("").trim().to_owned());
            let bytes = read_bytes(response, MAX_IMAGE_BYTES, false).await?;
            let mime = image_mime(&bytes, Submission::Unknown)?;
            if declared.as_deref() != Some(mime) {
                return Err(CallError::new(
                    "invalid_image",
                    "图片内容与声明格式不一致。",
                    Submission::Unknown,
                ));
            }
            let mut metadata = batch.metadata();
            metadata["custom_id"] = json!(custom_id);
            metadata["image_index"] = json!(index);
            metadata["local_files"] = json!([]);
            return Ok(ImageOutput {
                images: vec![(bytes, mime)],
                metadata,
            });
        }
        let mut url = self.batch_url(
            input.batch_id.as_deref(),
            None,
            batch.as_ref().map(|_| "items"),
        );
        url.query_pairs_mut()
            .append_pair("limit", &limit.to_string())
            .append_pair("cursor", &offset.to_string());
        let response = self
            .api
            .get(url)
            .header(reqwest::header::AUTHORIZATION, auth)
            .send()
            .await
            .map_err(|e| transport_error(&e, false))?;
        let mut page = batch_json(response, false).await?;
        let data = page["data"].as_array().ok_or_else(invalid_batch)?;
        if page["object"] != "list" || !page["has_more"].is_boolean() || data.len() > limit as usize
        {
            return Err(invalid_batch());
        }
        let data: Vec<Value> = if batch.is_some() {
            data.iter().map(batch_item).collect::<Result<_, _>>()?
        } else {
            data.iter()
                .map(|v| {
                    BatchDto::parse(v.clone(), None, &BatchProgress::submission())
                        .map(|b| b.metadata())
                })
                .collect::<Result<_, _>>()?
        };
        page = json!({"data":data,"has_more":page["has_more"],"cursor":offset.to_string(),"limit":limit,"next_cursor":if page["has_more"]==true {Some((offset+data.len() as u64).to_string())}else{None}});
        let metadata = match batch {
            Some(batch) => {
                let mut m = batch.metadata();
                m["items"] = page;
                m
            }
            None => {
                page["query_state"] = json!("retrieved");
                page
            }
        };
        Ok(ImageOutput {
            images: vec![],
            metadata,
        })
    }

    pub(crate) async fn cancel_batch(
        &self,
        id: &str,
        progress: &BatchProgress,
    ) -> Result<ImageOutput, CallError> {
        async {
            if !valid_request_ref(id) {
                return Err(parameter("batch_id 格式无效。"));
            }
            let auth = self.authorization()?;
            progress.submitted.store(true, Ordering::SeqCst);
            let response = self
                .api
                .post(self.batch_url(Some(id), None, Some("cancel")))
                .header(reqwest::header::AUTHORIZATION, auth)
                .send()
                .await
                .map_err(|e| transport_error(&e, true))?;
            let batch = BatchDto::parse(batch_json(response, true).await?, Some(id), progress)?;
            progress.observe(&batch);
            let mut metadata = batch.metadata();
            metadata["cancel_state"] = json!(match batch.status.as_str() {
                "cancelled" => "completed",
                "completed" | "failed" | "output_deleted" => "already_finished",
                _ => "requested",
            });
            Ok(ImageOutput {
                images: vec![],
                metadata,
            })
        }
        .await
        .map_err(|e| progress.error(e))
    }
}

async fn batch_json(response: Response, submitted: bool) -> Result<Value, CallError> {
    let status = response.status();
    let special = match status.as_u16() {
        502 => Some(("upstream_failed", "批量服务请求未完成，结果需确认。")),
        409 => Some((
            "batch_conflict",
            "批量操作冲突；保留原幂等键和 batch_id，按状态或原输入恢复。",
        )),
        404 => Some((
            "batch_not_found",
            "批量任务或指定条目未取得，请核对当前 Key 与服务器返回的 ID。",
        )),
        410 => Some((
            "batch_output_unavailable",
            "批量图片已过期或删除；请保留 batch_id，不要重新提交。",
        )),
        _ => None,
    };
    if let Some((mut code, mut message)) = special {
        let bytes = read_bytes(response, MAX_METADATA_RESPONSE_BYTES, submitted).await?;
        let value = serde_json::from_slice::<Value>(&bytes).ok();
        let server_code = value
            .as_ref()
            .and_then(|v| v["error"]["code"].as_str().or_else(|| v["code"].as_str()));
        if status.as_u16() == 409 && server_code == Some("BATCH_IMAGE_IDEMPOTENCY_CONFLICT") {
            code = "idempotency_conflict";
            message = "同一幂等键对应不同请求；请保留原键和原输入，不会换键重发。";
        } else if status.as_u16() == 502 && server_code == Some("BATCH_IMAGE_CANCEL_FAILED") {
            code = "batch_cancel_failed";
            message = "批量取消失败；请保留 batch_id 并查询任务状态。";
        }
        return Err(CallError::new(
            code,
            message,
            if submitted && status.is_client_error() {
                Submission::Rejected
            } else {
                Submission::Unknown
            },
        ));
    }
    let value = read_json(response, submitted, MAX_METADATA_RESPONSE_BYTES).await?;
    if status != reqwest::StatusCode::OK {
        return Err(invalid_batch());
    }
    Ok(value)
}

fn batch_item(value: &Value) -> Result<Value, CallError> {
    let id = value["custom_id"]
        .as_str()
        .filter(|id| valid_custom_id(id))
        .ok_or_else(invalid_batch)?;
    let status = value["status"]
        .as_str()
        .filter(|s| matches!(*s, "pending" | "succeeded" | "failed"))
        .ok_or_else(invalid_batch)?;
    let count = value["image_count"].as_u64().ok_or_else(invalid_batch)?;
    Ok(
        json!({"custom_id":id,"status":status,"image_count":count,"mime_type":value["mime_type"],"file_extension":value["file_extension"],"error":if status=="failed"{json!({"code":"batch_item_failed","message":"此条目执行或结果处理失败。"})}else{Value::Null}}),
    )
}
