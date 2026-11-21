//! Shared image capability core. The original offline APIs remain independent of HTTP/MCP.

pub mod adapters;
pub mod configuration;
pub mod distribution;
pub mod http;
pub mod image_parameters;
pub mod management;
pub mod mcp;
mod output;
pub mod release_package;
pub mod settings;
mod tasks;

use serde::{Deserialize, Serialize};
use std::{collections::HashSet, error::Error, fmt};

/// Development-contract marker, to be agreed with the server before HTTP integration.
pub const IMAGE_26_FAMILY: &str = "image-2.6";

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Operation {
    Generation,
    Edit,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionMode {
    Sync,
    SingleAsync,
    Batch,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Availability {
    Configured,
    TemporarilyUnavailable,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ParameterConstraints {
    pub max_images: u32,
    #[serde(default)]
    pub sizes: Vec<String>,
    #[serde(default)]
    pub qualities: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub size_rules: Option<image_parameters::SizeRules>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub complexities: Vec<String>,
}

/// Each row describes one operation/mode pair, not their Cartesian product.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Capability {
    pub operation: Operation,
    pub mode: ExecutionMode,
    pub parameters: ParameterConstraints,
}

/// Exact decimal text, with no currency inference or arithmetic in this core.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct Amount {
    pub ledger_amount: String,
    pub ledger_unit: String,
    pub display: Option<DisplayAmount>,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
pub struct DisplayAmount {
    pub amount: String,
    pub currency: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize, Serialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum Pricing {
    Available { amount: Amount },
    Hidden {},
    Unknown {},
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Model {
    pub id: String,
    pub display_name: String,
    pub family: Option<String>,
    pub preference_rank: Option<u32>,
    pub availability: Availability,
    pub capabilities: Vec<Capability>,
    pub pricing: Pricing,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SubmissionReason {
    ImageAccessDisabled,
    InsufficientBalance,
    QuotaExhausted,
    #[serde(other)]
    Other,
}

#[derive(Debug, Deserialize)]
struct CatalogData {
    can_submit: bool,
    reason_code: Option<SubmissionReason>,
    data: Vec<Model>,
}

/// Parsed and validated at the input boundary; its models cannot be mutated in place.
#[derive(Debug)]
pub struct Catalog(CatalogData);

impl Catalog {
    pub fn from_json(json: &str) -> Result<Self, CoreError> {
        let data: CatalogData = serde_json::from_str(json).map_err(|_| invalid_catalog())?;
        let mut ids = HashSet::new();
        for model in &data.data {
            if model.id.trim().is_empty()
                || model.display_name.trim().is_empty()
                || model.family.as_ref().is_some_and(|s| s.trim().is_empty())
                || !ids.insert(&model.id)
            {
                return Err(invalid_catalog());
            }
            for (index, capability) in model.capabilities.iter().enumerate() {
                let parameters = &capability.parameters;
                if parameters.max_images == 0
                    || parameters.sizes.iter().any(|s| s.trim().is_empty())
                    || parameters.qualities.iter().any(|s| s.trim().is_empty())
                    || parameters
                        .size_rules
                        .as_ref()
                        .is_some_and(|rules| !rules.valid())
                    || parameters
                        .complexities
                        .iter()
                        .any(|v| !matches!(v.as_str(), "low" | "medium" | "high"))
                    || model.capabilities[..index].iter().any(|previous| {
                        previous.operation == capability.operation
                            && previous.mode == capability.mode
                    })
                {
                    return Err(invalid_catalog());
                }
            }
            if let Pricing::Available { amount } = &model.pricing
                && !valid_amount(amount)
            {
                return Err(invalid_catalog());
            }
        }
        Ok(Self(data))
    }

    pub fn can_submit(&self) -> bool {
        self.0.can_submit
    }

    pub(crate) fn reason_code(&self) -> Option<&str> {
        self.0.reason_code.as_ref().map(|reason| match reason {
            SubmissionReason::ImageAccessDisabled => "image_access_disabled",
            SubmissionReason::InsufficientBalance => "insufficient_balance",
            SubmissionReason::QuotaExhausted => "quota_exhausted",
            SubmissionReason::Other => "submission_restricted",
        })
    }

    pub fn models(&self) -> &[Model] {
        &self.0.data
    }

    /// Selects only from this snapshot, preserving an explicit model choice.
    pub fn select(&self, request: &SelectionRequest<'_>) -> Result<&Model, CoreError> {
        if !self.0.can_submit {
            let message = match self.0.reason_code {
                Some(SubmissionReason::ImageAccessDisabled) => "当前 Key 未启用图片提交权限。",
                Some(SubmissionReason::InsufficientBalance) => "当前 Key 可用余额不足。",
                Some(SubmissionReason::QuotaExhausted) => "当前 Key 可用额度已耗尽。",
                Some(SubmissionReason::Other) | None => "当前 Key 不允许提交图片请求。",
            };
            return Err(CoreError::new(ErrorCode::SubmissionNotAllowed, message));
        }
        if let Some(id) = request.model_id {
            let failure = match self.0.data.iter().find(|model| model.id == id) {
                Some(model) => match incompatibility(model, request) {
                    None => return Ok(model),
                    Some(failure) => failure,
                },
                None => (ErrorCode::ModelNotFound, "目录中没有指定的模型。"),
            };
            let mut error = CoreError::new(failure.0, failure.1);
            error.alternatives = self
                .0
                .data
                .iter()
                .filter(|model| incompatibility(model, request).is_none())
                .map(|model| model.id.clone())
                .collect();
            return Err(error);
        }
        self.0
            .data
            .iter()
            .filter(|model| incompatibility(model, request).is_none())
            .min_by_key(|model| {
                (
                    model.family.as_deref() != Some(IMAGE_26_FAMILY),
                    model.preference_rank.is_none(),
                    model.preference_rank.unwrap_or(u32::MAX),
                )
            })
            .ok_or_else(|| {
                CoreError::new(
                    ErrorCode::NoCompatibleModel,
                    "目录中没有适合本次请求的模型。",
                )
            })
    }
}

#[derive(Debug, Clone, Copy)]
pub struct SelectionRequest<'a> {
    pub model_id: Option<&'a str>,
    pub operation: Operation,
    pub mode: ExecutionMode,
    pub size: Option<&'a str>,
    pub quality: Option<&'a str>,
    pub image_count: u32,
}

impl SelectionRequest<'_> {
    pub fn new(operation: Operation, mode: ExecutionMode) -> Self {
        Self {
            model_id: None,
            operation,
            mode,
            size: None,
            quality: None,
            image_count: 1,
        }
    }
}

fn incompatibility(
    model: &Model,
    request: &SelectionRequest<'_>,
) -> Option<(ErrorCode, &'static str)> {
    if model.availability != Availability::Configured {
        return Some((ErrorCode::ModelUnavailable, "指定模型暂不可用。"));
    }
    if !model
        .capabilities
        .iter()
        .any(|capability| capability.operation == request.operation)
    {
        return Some((
            ErrorCode::UnsupportedOperation,
            "指定模型不支持此图片操作。",
        ));
    }
    let Some(capability) = model.capabilities.iter().find(|capability| {
        capability.operation == request.operation && capability.mode == request.mode
    }) else {
        return Some((
            ErrorCode::UnsupportedMode,
            "指定模型的此操作不支持该执行模式。",
        ));
    };
    let parameters = &capability.parameters;
    if request.image_count == 0 || request.image_count > parameters.max_images {
        return Some((
            ErrorCode::UnsupportedParameter,
            "请求的图片数量不在支持范围内。",
        ));
    }
    if request
        .size
        .is_some_and(|size| !parameters.supports_size(size))
    {
        return Some((ErrorCode::UnsupportedParameter, "请求的图片尺寸不受支持。"));
    }
    if request
        .quality
        .is_some_and(|quality| !parameters.supports_quality(quality))
    {
        return Some((ErrorCode::UnsupportedParameter, "请求的图片质量不受支持。"));
    }
    None
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FeeStatus {
    Pending,
    Settled,
    Unavailable,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Visibility {
    Visible,
    Hidden,
}

#[derive(Debug, Deserialize, Serialize)]
struct FeeData {
    status: FeeStatus,
    visibility: Visibility,
    amount: Option<Amount>,
}

#[derive(Debug, Serialize)]
pub struct FeeReceipt(FeeData);

impl FeeReceipt {
    pub fn from_json(json: &str) -> Result<Self, CoreError> {
        let data: FeeData = serde_json::from_str(json).map_err(|_| invalid_receipt())?;
        match (data.status, data.visibility, &data.amount) {
            (FeeStatus::Settled, Visibility::Visible, Some(amount)) if valid_amount(amount) => {}
            (FeeStatus::Settled, Visibility::Visible, _) | (_, _, Some(_)) => {
                return Err(invalid_receipt());
            }
            _ => {}
        }
        Ok(Self(data))
    }

    pub fn status(&self) -> FeeStatus {
        self.0.status
    }

    pub fn visibility(&self) -> Visibility {
        self.0.visibility
    }

    pub fn amount(&self) -> Option<&Amount> {
        self.0.amount.as_ref()
    }
}

fn valid_amount(amount: &Amount) -> bool {
    decimal(&amount.ledger_amount)
        && !amount.ledger_unit.trim().is_empty()
        && amount
            .display
            .as_ref()
            .is_none_or(|display| decimal(&display.amount) && !display.currency.trim().is_empty())
}

fn decimal(value: &str) -> bool {
    let (whole, fraction) = value.split_once('.').unwrap_or((value, "0"));
    !whole.is_empty()
        && !fraction.is_empty()
        && whole.bytes().all(|c| c.is_ascii_digit())
        && fraction.bytes().all(|c| c.is_ascii_digit())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ErrorCode {
    InvalidCatalog,
    InvalidFeeReceipt,
    SubmissionNotAllowed,
    ModelNotFound,
    ModelUnavailable,
    UnsupportedOperation,
    UnsupportedMode,
    UnsupportedParameter,
    NoCompatibleModel,
}

/// Diagnostics contain stable, bounded explanations, never the raw parse error or input.
#[derive(Debug, Serialize)]
pub struct CoreError {
    pub code: ErrorCode,
    pub message: &'static str,
    pub alternatives: Vec<String>,
}

impl CoreError {
    fn new(code: ErrorCode, message: &'static str) -> Self {
        Self {
            code,
            message,
            alternatives: Vec::new(),
        }
    }
}

impl fmt::Display for CoreError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.message)
    }
}

impl Error for CoreError {}

fn invalid_catalog() -> CoreError {
    CoreError::new(ErrorCode::InvalidCatalog, "模型目录格式或字段约束无效。")
}

fn invalid_receipt() -> CoreError {
    CoreError::new(ErrorCode::InvalidFeeReceipt, "费用回执格式或状态组合无效。")
}

#[cfg(test)]
mod distribution_acceptance_tests;
