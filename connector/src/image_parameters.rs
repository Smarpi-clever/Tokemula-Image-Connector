//! Image defaults are separate from credential/model settings and resolved before model fallback.
use crate::{ExecutionMode, Operation, ParameterConstraints};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImagePreferences {
    pub version: u8,
    pub profiles: Vec<ImagePreference>,
}
impl Default for ImagePreferences {
    fn default() -> Self {
        Self {
            version: 1,
            profiles: Vec::new(),
        }
    }
}
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImagePreference {
    pub operation: Operation,
    pub mode: ExecutionMode,
    pub size: Option<String>,
    pub quality: Option<String>,
}
impl ImagePreferences {
    pub fn validate(&self) -> Result<(), &'static str> {
        if self.version != 1
            || self.profiles.iter().enumerate().any(|(index, profile)| {
                profile.size.as_ref().is_some_and(|v| v.trim().is_empty())
                    || profile
                        .quality
                        .as_ref()
                        .is_some_and(|v| v.trim().is_empty())
                    || (profile.mode == ExecutionMode::Batch && profile.quality.is_some())
                    || self.profiles[..index]
                        .iter()
                        .any(|p| p.operation == profile.operation && p.mode == profile.mode)
            })
        {
            return Err("图片参数偏好格式无效；原文件已保留。");
        }
        Ok(())
    }
    pub fn profile(&self, operation: Operation, mode: ExecutionMode) -> Option<&ImagePreference> {
        self.profiles
            .iter()
            .find(|p| p.operation == operation && p.mode == mode)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SizeRules {
    pub multiple_of: u32,
    pub min_edge: u32,
    pub max_edge: u32,
    pub min_pixels: u64,
    pub max_pixels: u64,
    pub max_aspect_ratio: u32,
}
impl SizeRules {
    pub fn valid(&self) -> bool {
        self.multiple_of > 0
            && self.min_edge > 0
            && self.min_edge <= self.max_edge
            && self.min_pixels > 0
            && self.min_pixels <= self.max_pixels
            && self.max_aspect_ratio > 0
    }
    pub fn allows(&self, size: &str) -> bool {
        let Some((width, height)) = dimensions(size) else {
            return false;
        };
        let pixels = u64::from(width) * u64::from(height);
        self.valid()
            && width >= self.min_edge
            && height >= self.min_edge
            && width <= self.max_edge
            && height <= self.max_edge
            && width.is_multiple_of(self.multiple_of)
            && height.is_multiple_of(self.multiple_of)
            && pixels >= self.min_pixels
            && pixels <= self.max_pixels
            && u64::from(width.max(height))
                <= u64::from(width.min(height)) * u64::from(self.max_aspect_ratio)
    }
}
fn dimensions(size: &str) -> Option<(u32, u32)> {
    let (w, h) = size.split_once('x')?;
    if w.is_empty()
        || h.is_empty()
        || !w.bytes().all(|b| b.is_ascii_digit())
        || !h.bytes().all(|b| b.is_ascii_digit())
    {
        return None;
    }
    Some((w.parse().ok()?, h.parse().ok()?))
}
impl ParameterConstraints {
    pub fn supports_size(&self, size: &str) -> bool {
        self.sizes.iter().any(|allowed| allowed == size)
            || self.size_rules.as_ref().is_some_and(|r| r.allows(size))
    }
    pub fn supports_quality(&self, quality: &str) -> bool {
        self.qualities.iter().any(|allowed| allowed == quality)
    }
}
/// Validate selected non-secret settings using the same constraints as request selection.
pub fn validate_preferences(
    preferences: &ImagePreferences,
    model: &serde_json::Value,
) -> Result<(), &'static str> {
    preferences.validate()?;
    for profile in &preferences.profiles {
        let capabilities = model["capabilities"]
            .as_array()
            .ok_or("模型缺少图片能力。")?;
        let capability = capabilities
            .iter()
            .find(|c| {
                c["operation"] == serde_json::json!(profile.operation)
                    && c["mode"] == serde_json::json!(profile.mode)
            })
            .ok_or("模型不支持所选操作和执行模式。")?;
        let parameters: ParameterConstraints =
            serde_json::from_value(capability["parameters"].clone())
                .map_err(|_| "模型参数能力无效。")?;
        if profile
            .size
            .as_deref()
            .is_some_and(|s| !parameters.supports_size(s))
            || profile
                .quality
                .as_deref()
                .is_some_and(|q| !parameters.supports_quality(q))
        {
            return Err("模型不支持所选尺寸或画质。");
        }
    }
    Ok(())
}
pub fn size_label(size: &str) -> String {
    if size == "auto" {
        return "自动 (auto)".into();
    }
    let Some((w, h)) = dimensions(size).filter(|(w, h)| *w > 0 && *h > 0) else {
        return format!("{size} · 分辨率等级");
    };
    let (mut a, mut b) = (w, h);
    while b != 0 {
        (a, b) = (b, a % b);
    }
    format!("{w}×{h} · {}:{}", w / a, h / a)
}
pub fn quality_label(quality: &str) -> String {
    let label = match quality {
        "auto" => "自动",
        "low" => "低",
        "medium" => "中",
        "high" => "高",
        "xhigh" => "超高",
        "max" => "最高",
        "standard" => "标准",
        "hd" => "高清",
        _ => return quality.into(),
    };
    format!("{label} ({quality})")
}
