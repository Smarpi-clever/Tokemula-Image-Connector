//! Image tools; transport and protocol lifecycle belong to rmcp.

use crate::{
    configuration::SessionManager,
    http::{BatchProgress, CallError, ImageClient, RequestProgress, Submission},
    image_view::{self, ImageView},
    output::{OutputCache, PersistentOutput},
    performance::OperationTrace,
    settings::SettingsService,
    tasks::TaskStore,
};
use base64::{Engine, engine::general_purpose::STANDARD};
use image::ImageFormat;
use rmcp::{
    ServerHandler,
    handler::server::{
        tool::{InputResponses as ToolInputResponses, RequestState},
        wrapper::Parameters,
    },
    model::{
        CallToolResponse, CallToolResult, ContentBlock, ElicitRequest, ElicitRequestParams,
        InputRequest, InputRequests, InputRequiredResult, ProtocolVersion, RequestId, ResultType,
        ServerCapabilities, ServerConfig, ServerJsonRpcMessage, ServerResult,
    },
    service::{RequestContext, RoleServer},
    tool, tool_handler, tool_router,
};
use schemars::JsonSchema;
use serde::Deserialize;
use serde_json::Value;
use std::sync::Arc;
use std::{io::Cursor, time::SystemTime};

#[derive(Debug, Clone, Copy, Default, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageMode {
    #[default]
    Sync,
    SingleAsync,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageResultMode {
    #[default]
    Inline,
    Files,
}

#[derive(Debug, Clone, Copy, Deserialize, serde::Serialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ImageComplexity {
    Low,
    Medium,
    High,
}
impl ImageComplexity {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Low => "low",
            Self::Medium => "medium",
            Self::High => "high",
        }
    }
}

#[derive(Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("not" = {
    "required": ["result_mode", "mode"],
    "properties": {"result_mode": {"const": "files"}, "mode": {"const": "single_async"}}
}))]
pub struct GenerateInput {
    /// 图片生成或编辑的具体要求。
    pub prompt: String,
    /// 精确的客户模型 ID；省略时沿用已存默认模型和有序备用，未配置时按目录选择。
    pub model: Option<String>,
    /// 输出尺寸；省略时使用此操作/模式的已存偏好或模型默认。像素填写 WIDTHxHEIGHT。
    pub size: Option<String>,
    /// 输出画质；省略时使用已存偏好或模型默认，与图片思考复杂度分开。
    pub quality: Option<String>,
    /// 图片模型自身的思考复杂度；支持的路径默认 medium，单次可覆盖。与 Harness 推理强度分开。
    pub complexity: Option<ImageComplexity>,
    /// 输出图片数量，默认 1，受模型能力限制。
    pub n: Option<u32>,
    /// 执行方式，省略时同步；single_async 立即返回任务引用。
    pub mode: Option<ImageMode>,
    /// MCP交付方式；省略/inline保持内联图，files仅同步且需macOS产品的持久原图输出。不会传给图片上游。
    pub result_mode: Option<ImageResultMode>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
#[schemars(extend("not" = {
    "required": ["result_mode", "mode"],
    "properties": {"result_mode": {"const": "files"}, "mode": {"const": "single_async"}}
}))]
pub struct EditInput {
    pub prompt: String,
    pub model: Option<String>,
    /// 输出尺寸；省略时使用此操作/模式的已存偏好或模型默认。像素填写 WIDTHxHEIGHT。
    pub size: Option<String>,
    /// 输出画质；省略时使用已存偏好或模型默认，与图片思考复杂度分开。
    pub quality: Option<String>,
    /// 图片模型自身的思考复杂度；支持的路径默认 medium，单次可覆盖。与 Harness 推理强度分开。
    pub complexity: Option<ImageComplexity>,
    pub n: Option<u32>,
    /// 执行方式，省略时同步；single_async 立即返回任务引用。
    pub mode: Option<ImageMode>,
    /// MCP交付方式；省略/inline保持内联图，files仅同步且需macOS产品的持久原图输出。不会传给图片上游。
    pub result_mode: Option<ImageResultMode>,
    /// 用户明确提供的单个本地绝对路径，与 image_paths 二选一。
    pub image_path: Option<String>,
    /// 有顺序的本地绝对路径；只读取这些文件，与 image_path 二选一。
    pub image_paths: Option<Vec<String>>,
    /// 作用于第一张参考图的本地 PNG mask，须符合目录声明的尺寸和透明通道规则。
    pub mask_path: Option<String>,
    pub background: Option<String>,
    pub output_format: Option<String>,
    /// 仅在目录允许的输出格式下发送，通常为 JPEG/WebP 的 0–100。
    pub output_compression: Option<u32>,
    pub input_fidelity: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct TaskInput {
    /// 图片任务 ID；每次只查询一次，使用当前配置的 Key。
    pub task_id: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchItemInput {
    /// 稳定条目标识；结果使用服务器返回的实际 custom_id。
    pub custom_id: String,
    pub prompt: String,
    /// 每项输出数量，默认 1；由服务端展开，不复制条目。
    pub output_count: Option<u32>,
    /// 用户明确提供的本地绝对路径；只上传这些图片。
    #[serde(default)]
    pub reference_image_paths: Vec<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchSubmitInput {
    pub model: Option<String>,
    pub task_name: Option<String>,
    /// 正式目录声明的批量尺寸，例如 1K。
    pub size: Option<String>,
    pub items: Vec<BatchItemInput>,
    /// 显式重放原提交时使用同一键和相同输入；服务端判断重放或冲突。
    pub idempotency_key: Option<String>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchGetInput {
    pub batch_id: Option<String>,
    /// 必须使用条目列表返回的真实 custom_id。
    pub custom_id: Option<String>,
    /// 取图时默认 0；图片分支不接受分页参数。
    pub image_index: Option<u32>,
    /// 数字偏移游标；使用上页返回的 next_cursor。
    pub cursor: Option<String>,
    /// 每页默认 20；任务列表最多 100，条目列表最多 500。
    pub limit: Option<u32>,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct BatchCancelInput {
    pub batch_id: String,
}

#[derive(Deserialize, JsonSchema)]
#[serde(rename_all = "snake_case")]
pub enum ConfigurationInterface {
    Browser,
    Terminal,
}

#[derive(Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct ConfigureInput {
    /// 明确选择本机网页或自己的交互终端；省略时返回两条指引，并遵循客户端URL能力。
    pub interface: Option<ConfigurationInterface>,
}

#[derive(Clone)]
pub struct ImageMcp {
    client: Arc<ImageClient>,
    output_cache: Option<OutputCache>,
    settings: Option<SettingsService>,
    sessions: Option<SessionManager>,
    image_view: ImageView,
    persistent_output: Option<PersistentOutput>,
}

impl ImageMcp {
    pub fn new(client: ImageClient) -> Self {
        Self::with_storage(
            client,
            OutputCache::for_current_user(),
            TaskStore::for_current_user(),
        )
    }

    /// Managed operations obtain one fresh credential/preference snapshot per call.
    pub fn with_settings(client: ImageClient, settings: SettingsService) -> Self {
        let mut server = Self::new(client);
        server.sessions = Some(SessionManager::new(settings.clone()));
        server.settings = Some(settings);
        server
    }

    pub fn for_current_user() -> Result<Self, crate::settings::SettingsError> {
        let settings = SettingsService::for_current_user()?;
        let client = ImageClient::new(None).map_err(|_| crate::settings::SettingsError {
            code: "configuration_invalid",
            message: "无法初始化图片连接配置。",
        })?;
        let server = Self::with_settings(client, settings);
        #[cfg(target_os = "macos")]
        let server = Self {
            persistent_output: Some(PersistentOutput::for_current_user().map_err(|error| {
                crate::settings::SettingsError {
                    code: error.code,
                    message: error.message,
                }
            })?),
            ..server
        };
        Ok(server)
    }

    async fn operation_client(&self) -> Result<ImageClient, CallError> {
        let Some(settings) = self.settings.clone() else {
            return Ok((*self.client).clone());
        };
        let context = tokio::task::spawn_blocking(move || settings.snapshot())
            .await
            .map_err(|_| {
                CallError::new(
                    "settings_unavailable",
                    "无法读取图片连接设置。",
                    Submission::NotSubmitted,
                )
            })?
            .map_err(settings_error)?;
        self.client.with_operation_context(context)
    }

    async fn operation_client_traced(
        &self,
        trace: &OperationTrace,
    ) -> Result<ImageClient, CallError> {
        let Some(settings) = self.settings.clone() else {
            return Ok(self.client.with_trace(trace.clone()));
        };
        trace.mark("snapshot_dispatched");
        let blocking_trace = trace.clone();
        let context = tokio::task::spawn_blocking(move || {
            blocking_trace.mark("snapshot_blocking_enter");
            settings.snapshot_traced(&blocking_trace)
        })
        .await
        .map_err(|_| {
            CallError::new(
                "settings_unavailable",
                "无法读取图片连接设置。",
                Submission::NotSubmitted,
            )
        })?
        .map_err(settings_error)?;
        Ok(self
            .client
            .with_operation_context(context)?
            .with_trace(trace.clone()))
    }

    async fn configuration_status(&self) -> Value {
        let Some(settings) = self.settings.clone() else {
            return serde_json::json!({"source":"explicit_injection","management":"not_attached"});
        };
        match tokio::task::spawn_blocking(move || settings.status()).await {
            Ok(Ok(status)) => serde_json::to_value(status).expect("nonsecret settings status"),
            Ok(Err(error)) => serde_json::json!({"code":error.code,"message":error.message}),
            Err(_) => {
                serde_json::json!({"code":"settings_unavailable","message":"无法读取图片连接状态。"})
            }
        }
    }

    fn with_storage(
        mut client: ImageClient,
        output_cache: Option<OutputCache>,
        task_store: Option<TaskStore>,
    ) -> Self {
        client.task_store = task_store;
        let server = Self {
            client: Arc::new(client),
            output_cache,
            settings: None,
            sessions: None,
            image_view: ImageView::default(),
            persistent_output: None,
        };
        server.cleanup_outputs();
        server
    }

    #[cfg(test)]
    pub(crate) fn with_persistent_output_for_tests(mut self, root: std::path::PathBuf) -> Self {
        self.persistent_output = Some(PersistentOutput { root });
        self
    }

    fn preflight_output(&self) -> Result<(), CallError> {
        if let Some(output) = &self.persistent_output {
            output.preflight()?;
        }
        Ok(())
    }

    fn preflight_delivery(
        &self,
        mode: Option<ImageMode>,
        result_mode: ImageResultMode,
    ) -> Result<(), CallError> {
        if result_mode == ImageResultMode::Files {
            if matches!(mode, Some(ImageMode::SingleAsync)) {
                return Err(CallError::new(
                    "invalid_parameters",
                    "文件交付仅支持同步生成或编辑。",
                    Submission::NotSubmitted,
                ));
            }
            if self.persistent_output.is_none() {
                return Err(CallError::new(
                    "image_storage_unavailable",
                    "文件交付需要已启用的持久原图输出。",
                    Submission::NotSubmitted,
                ));
            }
        }
        self.preflight_output()
    }

    #[cfg(test)]
    pub(crate) fn with_directories(
        client: ImageClient,
        output_dir: std::path::PathBuf,
        task_dir: std::path::PathBuf,
    ) -> Self {
        Self::with_storage(
            client,
            Some(OutputCache { root: output_dir }),
            Some(TaskStore { root: task_dir }),
        )
    }

    #[cfg(test)]
    pub(crate) fn with_managed_directories(
        client: ImageClient,
        settings: SettingsService,
        output_dir: std::path::PathBuf,
        task_dir: std::path::PathBuf,
    ) -> Self {
        let mut server = Self::with_directories(client, output_dir, task_dir);
        server.sessions = Some(SessionManager::new(settings.clone()));
        server.settings = Some(settings);
        server
    }

    #[cfg(test)]
    pub(crate) fn configuration_sessions_for_tests(&self) -> Option<SessionManager> {
        self.sessions.clone()
    }

    fn cleanup_outputs(&self) {
        if self
            .output_cache
            .as_ref()
            .is_some_and(|cache| cache.cleanup(SystemTime::now()).is_err())
        {
            eprintln!("部分临时图片尚未完成清理。");
        }
    }
}

#[tool_router]
impl ImageMcp {
    #[tool(input_schema = rmcp::handler::server::common::schema_for_input::<ConfigureInput>().expect("input schema"), description = "配置 Tokemula 图片连接、默认模型与备用顺序。此工具只返回安全连接入口或本机终端命令，不接收 API Key；Key 仅在本机网页或用户自己的隐藏输入终端中填写。启动和工具枚举不会打开页面。")]
    async fn configure_tokemula(
        &self,
        Parameters(arguments): Parameters<Value>,
        RequestState(request_state): RequestState,
        ToolInputResponses(responses): ToolInputResponses,
        context: RequestContext<RoleServer>,
    ) -> CallToolResponse {
        let Ok(input) = serde_json::from_value::<ConfigureInput>(arguments) else {
            return invalid_parameters().into();
        };
        let command = terminal_command();
        if matches!(input.interface, Some(ConfigurationInterface::Terminal))
            || self.sessions.is_none()
        {
            return CallToolResult::structured(serde_json::json!({
                "status":"terminal_required", "terminal_command":command,
                "settings":self.configuration_status().await,
                "message":"在你自己的交互终端运行此命令，隐藏输入 API Key。不要在聊天或工具参数中提供 Key。"
            })).into();
        }
        let manager = self.sessions.as_ref().expect("managed configuration");
        let info = if let Some(state) = request_state.as_deref() {
            let Some(info) = manager.status().await else {
                return failure(CallError::new(
                    "configuration_session_expired",
                    "连接会话已结束，请重新发起配置。",
                    Submission::NotSubmitted,
                ))
                .into();
            };
            if state != info.session_id {
                return failure(CallError::new(
                    "configuration_session_mismatch",
                    "连接会话标识不匹配，请重新发起配置。",
                    Submission::NotSubmitted,
                ))
                .into();
            }
            let action = responses
                .as_ref()
                .and_then(|map| map.get(&info.session_id))
                .and_then(|v| v["action"].as_str());
            if matches!(action, Some("decline" | "cancel")) {
                let _ = manager.cancel().await;
            }
            manager.status().await.unwrap_or(info)
        } else {
            match manager.start().await {
                Ok(info) => info,
                Err(error) => return failure(settings_error(error)).into(),
            }
        };
        let supports_url = context
            .client_capabilities()
            .and_then(|c| c.elicitation)
            .is_some_and(|e| e.url.is_some());
        if request_state.is_none() && supports_url {
            if context
                .protocol_version()
                .is_some_and(|v| v >= ProtocolVersion::V_2026_07_28)
            {
                let mut requests = InputRequests::new();
                requests.insert(
                    info.session_id.clone(),
                    InputRequest::Elicitation(ElicitRequest::new(
                        ElicitRequestParams::UrlElicitationParams {
                            meta: None,
                            message: "在本机连接页设置图片 API Key 和模型偏好。".into(),
                            url: info.url.clone(),
                            elicitation_id: info.session_id.clone(),
                        },
                    )),
                );
                return InputRequiredResult::new(Some(requests), Some(info.session_id)).into();
            }
            if let Ok(url) = reqwest::Url::parse(&info.url) {
                let action = tokio::select! {
                    biased;
                    _ = context.ct.cancelled() => None,
                    result = context.peer.elicit_url_with_timeout(
                        "在本机连接页设置图片 API Key 和模型偏好。", url, &info.session_id,
                        Some(std::time::Duration::from_secs(300)),
                    ) => result.ok(),
                };
                if matches!(
                    action,
                    Some(
                        rmcp::model::ElicitationAction::Decline
                            | rmcp::model::ElicitationAction::Cancel
                    )
                ) {
                    let _ = manager.cancel().await;
                }
            }
        }
        let current = manager.status().await.unwrap_or(info);
        let mut result = CallToolResult::success(vec![ContentBlock::text(format!(
            "[打开本机连接页]({})，或在自己的终端运行：{}。API Key 只在本机输入。",
            current.url, command
        ))]);
        result.structured_content = Some(serde_json::json!({
            "configuration":current,"terminal_command":command,
            "settings":self.configuration_status().await,
            "message":"选择本机连接页或交互终端；配置完成状态以本机保存结果为准。"
        }));
        bounded(result, &context.id, Submission::NotSubmitted).into()
    }

    #[tool(input_schema = rmcp::handler::server::common::schema_for_input::<BatchSubmitInput>().expect("input schema"), description = "提交一个批量图片任务，先校验整个批次并保存幂等恢复信息，再发送一次可能收费的 POST。未知结果不自动重发；显式恢复使用同一 idempotency_key 和相同输入。")]
    async fn submit_image_batch(
        &self,
        Parameters(arguments): Parameters<Value>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let Ok(input) = serde_json::from_value::<BatchSubmitInput>(arguments) else {
            return invalid_parameters();
        };
        if let Err(error) = self.preflight_output() {
            return failure(self.client.batch_context_error(
                &input,
                error,
                &BatchProgress::submission(),
            ));
        }
        let progress = BatchProgress::submission();
        let client = match self.operation_client().await {
            Ok(client) => client,
            Err(error) => {
                return failure(self.client.batch_context_error(&input, error, &progress));
            }
        };
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => failure(progress.cancelled()),
            result = client.submit_batch(&input, &progress) => self.render(result, &context.id, image_view::supported(context.client_capabilities()))
        }
    }

    #[tool(input_schema = rmcp::handler::server::common::schema_for_input::<BatchGetInput>().expect("input schema"), description = "只读恢复批量结果：无 batch_id 取一页任务列表；仅 batch_id 取详情和一页条目；加真实 custom_id 取指定 image_index 图片（默认0）。不自动翻页、轮询或下载全部结果。")]
    async fn get_image_batch(
        &self,
        Parameters(arguments): Parameters<Value>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let Ok(input) = serde_json::from_value::<BatchGetInput>(arguments) else {
            return invalid_parameters();
        };
        self.cleanup_outputs();
        let progress = BatchProgress::query(&input);
        let client = match self.operation_client().await {
            Ok(client) => client,
            Err(error) => return failure(progress.error(error)),
        };
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => failure(progress.cancelled()),
            result = client.get_batch(&input, &progress) => self.render(result, &context.id, image_view::supported(context.client_capabilities()))
        }
    }

    #[tool(input_schema = rmcp::handler::server::common::schema_for_input::<BatchCancelInput>().expect("input schema"), description = "仅按 batch_id 发送一次批量业务取消。单异步任务不支持业务取消；取消请求可能尚未完成，已完成部分可能已经计费。MCP 停止等待与业务取消分别处理；不重试或轮询，后续用 get_image_batch 查询。")]
    async fn cancel_image_task(
        &self,
        Parameters(arguments): Parameters<Value>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let Ok(input) = serde_json::from_value::<BatchCancelInput>(arguments) else {
            return invalid_parameters();
        };
        let progress = BatchProgress::cancellation(&input.batch_id);
        let client = match self.operation_client().await {
            Ok(client) => client,
            Err(error) => return failure(progress.error(error)),
        };
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => failure(progress.cancelled()),
            result = client.cancel_batch(&input.batch_id, &progress) => self.render(result, &context.id, image_view::supported(context.client_capabilities()))
        }
    }

    #[tool(input_schema = rmcp::handler::server::common::schema_for_input::<TaskInput>().expect("input schema"), description = "按 task_id 只读查询一次图片任务；完成后交付图片。无需本地登记，不重新提交、不自动轮询。")]
    async fn get_image_task(
        &self,
        Parameters(arguments): Parameters<Value>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let Ok(input) = serde_json::from_value::<TaskInput>(arguments) else {
            return invalid_parameters();
        };
        self.cleanup_outputs();
        let progress = RequestProgress::for_task(&input.task_id);
        let client = match self.operation_client().await {
            Ok(client) => client,
            Err(error) => return failure(self.client.task_context_error(&input.task_id, error)),
        };
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => failure(progress.cancelled()),
            result = client.task(&input.task_id, &progress) => self.render(result, &context.id, image_view::supported(context.client_capabilities()))
        }
    }

    #[tool(
        description = "查询当前 Key 的图片模型、操作能力和参数限制。不生成图片；余额不足仍可查询目录。"
    )]
    async fn list_image_models(&self, context: RequestContext<RoleServer>) -> CallToolResult {
        self.cleanup_outputs();
        let client = match self.operation_client().await {
            Ok(client) => client,
            Err(error) => return failure(error),
        };
        let result = tokio::select! {
            biased;
            _ = context.ct.cancelled() => failure(CallError::cancelled(false)),
            result = client.catalog() => match result {
                Ok((_, mut visible)) => {
                    visible["connector_version"] = serde_json::json!(env!("CARGO_PKG_VERSION"));
                    CallToolResult::structured(visible)
                }
                Err(error) => failure(error)
            }
        };
        bounded(result, &context.id, Submission::NotSubmitted)
    }

    #[tool(input_schema = rmcp::handler::server::common::schema_for_input::<GenerateInput>().expect("input schema"), description = "生成图片，尺寸和画质沿用对应操作/模式的已存偏好，单次参数可覆盖；支持的图片思考复杂度默认 medium。默认同步；mode=single_async 受理后立即返回任务引用。result_mode=files仅同步且需持久原图输出，省略保持内联。先查询目录并校验，再提交一次可能收费的请求。超时、断线或取消只停止等待；不自动重发。")]
    async fn generate_image(
        &self,
        Parameters(arguments): Parameters<Value>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let trace = OperationTrace::new("generate_image");
        trace.mark("parameters_start");
        let Ok(input) = serde_json::from_value::<GenerateInput>(arguments) else {
            trace.finish("error", Submission::NotSubmitted, "invalid_parameters");
            return invalid_parameters();
        };
        trace.mark("parameters_end");
        let result_mode = input.result_mode.unwrap_or_default();
        trace.mark("desktop_preflight_start");
        if let Err(error) = self.preflight_delivery(input.mode, result_mode) {
            return traced_failure(&trace, error);
        }
        trace.mark("desktop_preflight_end");
        trace.mark("cache_cleanup_start");
        self.cleanup_outputs();
        trace.mark("cache_cleanup_end");
        let client = match self.operation_client_traced(&trace).await {
            Ok(client) => client,
            Err(error) => return traced_failure(&trace, error),
        };
        let progress = RequestProgress::default();
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => traced_failure(&trace, progress.cancelled()),
            result = client.execute(&input, None, &progress) => self.render_traced(
                result,
                &context.id,
                image_view::supported(context.client_capabilities()),
                result_mode,
                &trace,
            )
        }
    }

    #[tool(input_schema = rmcp::handler::server::common::schema_for_input::<EditInput>().expect("input schema"), description = "按目录能力编辑图片：image_path 或有序 image_paths 二选一，可选 mask 作用于第一图。默认同步，single_async 立即返回任务引用；result_mode=files仅同步且需持久原图输出，省略保持内联。尺寸画质沿用对应偏好，支持的图片思考复杂度默认 medium。只读取指定文件，完整校验后提交一次；停止等待不重新生成。")]
    async fn edit_image(
        &self,
        Parameters(arguments): Parameters<Value>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let trace = OperationTrace::new("edit_image");
        trace.mark("parameters_start");
        let Ok(input) = serde_json::from_value::<EditInput>(arguments) else {
            trace.finish("error", Submission::NotSubmitted, "invalid_parameters");
            return invalid_parameters();
        };
        trace.mark("parameters_end");
        let result_mode = input.result_mode.unwrap_or_default();
        trace.mark("desktop_preflight_start");
        if let Err(error) = self.preflight_delivery(input.mode, result_mode) {
            return traced_failure(&trace, error);
        }
        trace.mark("desktop_preflight_end");
        let request = GenerateInput {
            prompt: input.prompt.clone(),
            model: input.model.clone(),
            size: input.size.clone(),
            quality: input.quality.clone(),
            complexity: input.complexity,
            n: input.n,
            mode: input.mode,
            result_mode: input.result_mode,
        };
        trace.mark("cache_cleanup_start");
        self.cleanup_outputs();
        trace.mark("cache_cleanup_end");
        let client = match self.operation_client_traced(&trace).await {
            Ok(client) => client,
            Err(error) => return traced_failure(&trace, error),
        };
        let progress = RequestProgress::default();
        tokio::select! {
            biased;
            _ = context.ct.cancelled() => traced_failure(&trace, progress.cancelled()),
            result = client.execute(&request, Some(&input), &progress) => self.render_traced(
                result,
                &context.id,
                image_view::supported(context.client_capabilities()),
                result_mode,
                &trace,
            )
        }
    }
}

#[tool_handler]
impl ServerHandler for ImageMcp {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_tools()
                .enable_resources()
                .build(),
        )
        .with_server_info(rmcp::model::Implementation::new(
            "tokemula-image-mcp",
            crate::distribution::VERSION,
        ))
    }

    async fn list_tools(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListToolsResult, rmcp::ErrorData> {
        let mut tools = Self::tool_router().list_all();
        if self.persistent_output.is_none() && image_view::supported(context.client_capabilities())
        {
            for tool in &mut tools {
                if matches!(
                    tool.name.as_ref(),
                    "generate_image" | "edit_image" | "get_image_task" | "get_image_batch"
                ) {
                    tool.meta = Some(
                        serde_json::from_value(serde_json::json!({
                            "ui": { "resourceUri": image_view::URI }
                        }))
                        .expect("UI tool metadata"),
                    );
                }
            }
        }
        let mut result = rmcp::model::ListToolsResult::with_all_items(tools);
        if context
            .protocol_version()
            .is_some_and(|version| version >= ProtocolVersion::V_2026_07_28)
        {
            result.ttl_ms = Some(0);
            result.cache_scope = Some(rmcp::model::CacheScope::Public);
        }
        Ok(result)
    }

    async fn list_resources(
        &self,
        _request: Option<rmcp::model::PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ListResourcesResult, rmcp::ErrorData> {
        Ok(rmcp::model::ListResourcesResult::with_all_items(vec![
            image_view::resource(),
        ]))
    }

    async fn read_resource(
        &self,
        request: rmcp::model::ReadResourceRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<rmcp::model::ReadResourceResponse, rmcp::ErrorData> {
        self.image_view.read(&request.uri).map(Into::into)
    }
}

fn terminal_command() -> String {
    crate::distribution::management_command("configure --terminal")
}

fn settings_error(error: crate::settings::SettingsError) -> CallError {
    CallError::new(error.code, error.message, Submission::NotSubmitted)
}

fn traced_failure(trace: &OperationTrace, error: CallError) -> CallToolResult {
    trace.finish(
        if error.code == "cancelled" {
            "cancelled"
        } else {
            "error"
        },
        error.submission,
        error.code,
    );
    failure(error)
}

fn failure(error: CallError) -> CallToolResult {
    CallToolResult::structured_error(serde_json::to_value(error).expect("serializable tool error"))
}

// The budget includes the official serialized response envelope, all images and metadata.
// It stays below rmcp's default 16 MiB line limit without changing client configuration.
const MAX_WIRE_BYTES: usize = 8 * 1024 * 1024;

fn wire_bytes(result: &CallToolResult, id: &RequestId) -> usize {
    let mut complete = result.clone();
    complete.result_type = Some(ResultType::COMPLETE);
    let response =
        ServerJsonRpcMessage::response(ServerResult::CallToolResult(complete), id.clone());
    serde_json::to_vec(&response)
        .expect("serializable MCP result")
        .len()
        + 1
}

fn with_output_reference(mut error: CallError, metadata: &Value) -> CallError {
    if let Some(reference) = metadata["request_ref"].as_str() {
        let source = match metadata["request_ref_source"].as_str() {
            Some("local_record") => "local_record",
            Some("submission_header") => "submission_header",
            _ if metadata["task_id"].is_string() => "task_json",
            _ => "submission_header",
        };
        error = error.with_task_reference(Some(reference), source);
    } else {
        error =
            error.with_task_reference(metadata["recovery"]["request_ref"].as_str(), "local_record");
    }
    if metadata["task_id"].is_string()
        && let Some(status) = metadata.get("status")
    {
        error = error.with_query_status(Some(
            serde_json::from_value(status.clone()).expect("validated task status"),
        ));
    }
    error
        .with_task_id(metadata["task_id"].as_str())
        .with_batch_metadata(metadata)
}

fn bounded(result: CallToolResult, id: &RequestId, submission: Submission) -> CallToolResult {
    if wire_bytes(&result, id) <= MAX_WIRE_BYTES {
        result
    } else {
        let error = CallError::new(
            "response_too_large",
            "结果信息超过单次协议消息容量。",
            submission,
        );
        failure(match result.structured_content.as_ref() {
            Some(metadata) => with_output_reference(error, metadata),
            None => error,
        })
    }
}

fn image_delivery_failure(error: CallError, metadata: &Value) -> CallToolResult {
    let mut result = failure(with_output_reference(error, metadata));
    result.structured_content.as_mut().expect("error metadata")["local_files"] =
        metadata["local_files"].clone();
    for file in metadata["local_files"]
        .as_array()
        .expect("saved image paths")
    {
        result.content.push(ContentBlock::text(format!(
            "已保存的本次原图：{}",
            file["path"].as_str().expect("saved path")
        )));
    }
    result
}

impl ImageMcp {
    fn render_traced(
        &self,
        result: Result<crate::http::ImageOutput, CallError>,
        id: &RequestId,
        inline_ui: bool,
        result_mode: ImageResultMode,
        trace: &OperationTrace,
    ) -> CallToolResult {
        let output = match result {
            Ok(output) => output,
            Err(error) => {
                let submission = error.submission;
                trace.finish("error", submission, error.code);
                return bounded(failure(error), id, submission);
            }
        };
        trace.mark("render_start");
        let response = self.render_with_mode(Ok(output), id, inline_ui, result_mode, Some(trace));
        if response.is_error == Some(true) {
            let metadata = response
                .structured_content
                .as_ref()
                .expect("tool error metadata");
            let submission = serde_json::from_value(metadata["submission"].clone())
                .expect("validated submission state");
            let code = match metadata["code"].as_str() {
                Some("image_storage_failed") => "image_storage_failed",
                Some("preview_failed") => "preview_failed",
                Some("image_view_unavailable") => "image_view_unavailable",
                Some("image_storage_unavailable") => "image_storage_unavailable",
                Some("response_too_large") => "response_too_large",
                _ => "delivery_failed",
            };
            trace.finish("error", submission, code);
        } else {
            trace.mark("render_return");
            trace.finish("success", Submission::Accepted, "ok");
        }
        response
    }

    fn render(
        &self,
        result: Result<crate::http::ImageOutput, CallError>,
        id: &RequestId,
        inline_ui: bool,
    ) -> CallToolResult {
        self.render_with_mode(result, id, inline_ui, ImageResultMode::Inline, None)
    }

    fn render_with_mode(
        &self,
        result: Result<crate::http::ImageOutput, CallError>,
        id: &RequestId,
        inline_ui: bool,
        result_mode: ImageResultMode,
        trace: Option<&OperationTrace>,
    ) -> CallToolResult {
        let mut output = match result {
            Ok(output) => output,
            Err(error) => {
                let submission = error.submission;
                return bounded(failure(error), id, submission);
            }
        };
        if output.images.is_empty() {
            return bounded(
                CallToolResult::structured(output.metadata),
                id,
                Submission::Accepted,
            );
        }
        let inline_ui = inline_ui && self.persistent_output.is_none();
        let mut desktop_paths: Vec<Value> = Vec::new();
        if let Some(trace) = trace {
            trace.mark("original_save_start");
        }
        if let Some(destination) = &self.persistent_output {
            for (index, (bytes, mime)) in output.images.iter().enumerate() {
                let path = match destination.save(bytes, mime) {
                    Ok(path) => path,
                    Err(_) => {
                        output.metadata["local_files"] = serde_json::json!(desktop_paths);
                        let failed = image_delivery_failure(
                            CallError::new(
                                "image_storage_failed",
                                "图片已返回，但无法保存桌面原图；请求不会重发。",
                                Submission::Accepted,
                            ),
                            &output.metadata,
                        );
                        return bounded(failed, id, Submission::Accepted);
                    }
                };
                let dimensions = &output.metadata["image_dimensions"][index];
                desktop_paths.push(serde_json::json!({
                    "image_index": index,
                    "path": path,
                    "mime_type": mime,
                    "width": dimensions["width"],
                    "height": dimensions["height"],
                    "persistent": true,
                    "retention": "user_managed",
                    "expires_at": null,
                    "inline_representation": if result_mode == ImageResultMode::Files { "none" } else { "original" }
                }));
            }
            output.metadata["local_files"] = serde_json::json!(desktop_paths);
        }
        if let Some(trace) = trace {
            trace.counts("original_save_end", &[("image_count", desktop_paths.len())]);
        }
        let caption = if output.metadata["batch_id"].is_string() {
            "图片已返回；任务状态见结果信息。"
        } else {
            "图片已返回；模型和结果状态见结果信息。"
        };
        let mut caption = if output.metadata["request_parameters"].is_object() {
            format!(
                "{caption}\n{}",
                serde_json::json!({
                    "connector_version": output.metadata["connector_version"],
                    "request_parameters": output.metadata["request_parameters"],
                    "image_dimensions": output.metadata["image_dimensions"],
                    "selected_model": output.metadata["selected_model"],
                    "effective_model": output.metadata["effective_model"],
                    "model_selection": output.metadata["model_selection"],
                    "request_ref": output.metadata["request_ref"]
                })
            )
        } else {
            caption.to_owned()
        };
        for file in &desktop_paths {
            caption.push_str(&format!(
                "\n第 {} 张完整原图：{}（用户产物，无自动过期）。",
                file["image_index"].as_u64().expect("image index") + 1,
                file["path"].as_str().expect("saved path")
            ));
        }
        if result_mode == ImageResultMode::Files {
            let mut result = CallToolResult::success(vec![ContentBlock::text(caption)]);
            result.structured_content = Some(output.metadata);
            if let Some(trace) = trace {
                trace.counts(
                    "image_encoding_skipped",
                    &[("image_count", output.images.len())],
                );
                trace.mark("message_measure_start");
            }
            let result = bounded(result, id, Submission::Accepted);
            if let Some(trace) = trace {
                trace.mark("message_measure_end");
            }
            return result;
        }
        if let Some(trace) = trace {
            trace.mark("image_encoding_start");
        }
        let content = output
            .images
            .iter()
            .map(|(bytes, mime)| ContentBlock::image(STANDARD.encode(bytes), *mime))
            .chain(std::iter::once(ContentBlock::text(caption)))
            .collect();
        if let Some(trace) = trace {
            trace.counts(
                "image_encoding_end",
                &[("image_count", output.images.len())],
            );
        }
        let mut result = CallToolResult::success(content);
        result.structured_content = Some(output.metadata);
        let mut previews = vec![false; output.images.len()];
        loop {
            if let Some(trace) = trace {
                trace.mark("message_measure_start");
            }
            let bytes = wire_bytes(&result, id);
            if let Some(trace) = trace {
                trace.counts("message_measure_end", &[("wire_bytes", bytes)]);
            }
            if bytes <= MAX_WIRE_BYTES {
                break;
            }
            let Some(index) = output
                .images
                .iter()
                .enumerate()
                .filter(|(index, _)| !previews[*index])
                .max_by_key(|(_, (bytes, _))| bytes.len())
                .map(|(index, _)| index)
            else {
                return bounded(result, id, Submission::Unknown);
            };
            if self.persistent_output.is_some() {
                if let Some(trace) = trace {
                    trace.mark("preview_encoding_start");
                }
                let mut preview = Cursor::new(Vec::new());
                let decoded = image::load_from_memory(&output.images[index].0)
                    .expect("HTTP boundary validated image bytes");
                if decoded
                    .thumbnail(256, 256)
                    .write_to(&mut preview, ImageFormat::Png)
                    .is_err()
                {
                    return image_delivery_failure(
                        CallError::new(
                            "preview_failed",
                            "桌面原图已保存，但协议预览编码失败。",
                            Submission::Accepted,
                        ),
                        result.structured_content.as_ref().expect("image metadata"),
                    );
                }
                result.content[index] =
                    ContentBlock::image(STANDARD.encode(preview.into_inner()), "image/png");
                if let Some(trace) = trace {
                    trace.mark("preview_encoding_end");
                }
                result.structured_content.as_mut().expect("image metadata")["local_files"][index]
                    ["inline_representation"] = serde_json::json!("preview");
                result.content.push(ContentBlock::text(format!(
                    "第 {} 张协议图块是预览；请展示上方本次桌面原图路径。",
                    index + 1
                )));
                previews[index] = true;
                continue;
            }
            if inline_ui {
                let (bytes, mime) = (&output.images[index].0, output.images[index].1);
                if let Some(trace) = trace {
                    trace.mark("preview_encoding_start");
                }
                let mut preview = Cursor::new(Vec::new());
                let decoded =
                    image::load_from_memory(bytes).expect("HTTP boundary validated image bytes");
                if decoded
                    .thumbnail(256, 256)
                    .write_to(&mut preview, ImageFormat::Png)
                    .is_err()
                {
                    return failure(with_output_reference(
                        CallError::new(
                            "preview_failed",
                            "原图预览编码失败；生成请求不会重发。",
                            Submission::Unknown,
                        ),
                        result.structured_content.as_ref().expect("image metadata"),
                    ));
                }
                result.content[index] =
                    ContentBlock::image(STANDARD.encode(preview.into_inner()), "image/png");
                if let Some(trace) = trace {
                    trace.mark("preview_encoding_end");
                }
                let original = std::mem::take(&mut output.images[index].0);
                let descriptor = match self.image_view.retain(original, mime, index) {
                    Ok(descriptor) => descriptor,
                    Err(_) => {
                        return failure(with_output_reference(
                            CallError::new(
                                "image_view_unavailable",
                                "无法建立本次图片的内存资源；生成请求不会重发。",
                                Submission::Unknown,
                            ),
                            result.structured_content.as_ref().expect("image metadata"),
                        ));
                    }
                };
                let meta = result.meta.get_or_insert_with(Default::default);
                meta.entry("tokemula/inlineImages".to_owned())
                    .or_insert_with(|| serde_json::json!([]))
                    .as_array_mut()
                    .expect("inline image descriptors")
                    .push(descriptor);
                result.content.push(ContentBlock::text(format!(
                    "第 {} 张的协议内联图为预览；内嵌图片视图从本次内存资源显示完整原图，未自动写入图片文件。", index + 1
                )));
                previews[index] = true;
                continue;
            }
            let Some(cache) = &self.output_cache else {
                return failure(with_output_reference(
                    CallError::new(
                        "image_storage_unavailable",
                        "未找到当前用户的私有缓存目录，无法交还完整大图。",
                        Submission::Unknown,
                    ),
                    result.structured_content.as_ref().expect("image metadata"),
                ));
            };
            let (bytes, mime) = &output.images[index];
            if let Some(trace) = trace {
                trace.mark("cache_original_save_start");
            }
            let original = match cache.save(bytes, mime, SystemTime::now()) {
                Ok(original) => original,
                Err(error) => {
                    return failure(with_output_reference(
                        error,
                        result.structured_content.as_ref().expect("image metadata"),
                    ));
                }
            };
            if let Some(trace) = trace {
                trace.mark("cache_original_save_end");
                trace.mark("preview_encoding_start");
            }
            let mut preview = Cursor::new(Vec::new());
            let decoded =
                image::load_from_memory(bytes).expect("HTTP boundary validated image bytes");
            if decoded
                .thumbnail(256, 256)
                .write_to(&mut preview, ImageFormat::Png)
                .is_err()
            {
                return failure(with_output_reference(
                    CallError::new(
                        "preview_failed",
                        "完整原图已保存，但预览编码失败。",
                        Submission::Unknown,
                    ),
                    result.structured_content.as_ref().expect("image metadata"),
                ));
            }
            result.content[index] =
                ContentBlock::image(STANDARD.encode(preview.into_inner()), "image/png");
            if let Some(trace) = trace {
                trace.mark("preview_encoding_end");
            }
            result.content.push(ContentBlock::text(format!(
                "第 {} 张为预览。完整原图：{}；本地有效至 Unix 秒 {}，请在到期前另存。",
                index + 1,
                original.path.display(),
                original.expires_at
            )));
            let mut file = serde_json::to_value(original).expect("serializable original image");
            file["image_index"] = serde_json::json!(index);
            file["inline_representation"] = serde_json::json!("preview");
            result.structured_content.as_mut().expect("image metadata")["local_files"]
                .as_array_mut()
                .expect("file metadata array")
                .push(file);
            previews[index] = true;
        }
        result
    }
}

fn invalid_parameters() -> CallToolResult {
    failure(CallError::new(
        "invalid_parameters",
        "工具参数不符合要求。",
        crate::http::Submission::NotSubmitted,
    ))
}

#[cfg(test)]
#[path = "image_view_tests.rs"]
mod image_view_tests;

#[cfg(test)]
#[path = "persistent_output_tests.rs"]
mod persistent_output_tests;
