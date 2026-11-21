//! Optional edit capabilities and ordered multipart uploads; base catalog callers stay unchanged.
use super::*;
use crate::mcp::{EditInput, GenerateInput};

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(super) struct EditOptions {
    #[serde(default)]
    backgrounds: Vec<String>,
    #[serde(default)]
    output_formats: Vec<String>,
    #[serde(default)]
    input_fidelities: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    output_compression: Option<Compression>,
    #[serde(default)]
    transparent_output_formats: Vec<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    mask: Option<Mask>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Compression {
    min: u32,
    max: u32,
    formats: Vec<String>,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Mask {
    max_images: u32,
    max_image_bytes: u64,
    mime_types: Vec<String>,
    requires_alpha: bool,
    same_dimensions_as_first_image: bool,
    applies_to_image: u32,
}

impl EditOptions {
    pub(super) fn valid(&self) -> bool {
        self.backgrounds
            .iter()
            .all(|v| matches!(v.as_str(), "auto" | "opaque" | "transparent"))
            && self
                .output_formats
                .iter()
                .all(|v| matches!(v.as_str(), "png" | "jpeg" | "webp"))
            && self
                .input_fidelities
                .iter()
                .all(|v| matches!(v.as_str(), "high" | "low"))
            && self
                .transparent_output_formats
                .iter()
                .all(|v| matches!(v.as_str(), "png" | "webp"))
            && self.output_compression.as_ref().is_none_or(|c| {
                c.min <= c.max
                    && c.max <= 100
                    && !c.formats.is_empty()
                    && c.formats
                        .iter()
                        .all(|v| matches!(v.as_str(), "jpeg" | "webp"))
            })
            && self.mask.as_ref().is_none_or(|m| {
                m.max_images == 1
                    && m.max_image_bytes > 0
                    && m.mime_types == ["image/png"]
                    && m.requires_alpha
                    && m.same_dimensions_as_first_image
                    && m.applies_to_image == 0
            })
    }
}

struct EditFiles {
    images: Vec<(Vec<u8>, &'static str)>,
    mask: Option<(Vec<u8>, &'static str)>,
}

impl EditFiles {
    async fn read(input: &EditInput) -> Result<Self, CallError> {
        let paths = match (&input.image_path, &input.image_paths) {
            (Some(path), None) => vec![path],
            (None, Some(paths)) if !paths.is_empty() => paths.iter().collect(),
            _ => return Err(parameter("image_path 与非空 image_paths 必须二选一。")),
        };
        let mut images = Vec::with_capacity(paths.len());
        for path in paths {
            images.push(read_local_image(path, MAX_IMAGE_BYTES as u64).await?);
        }
        let mask = match &input.mask_path {
            Some(path) => {
                let mask = read_local_image(path, MAX_IMAGE_BYTES as u64).await?;
                if mask.1 != "image/png" {
                    return Err(parameter("mask 必须是含透明通道的 PNG 图片。"));
                }
                let decoder = ImageReader::new(Cursor::new(&mask.0))
                    .with_guessed_format()
                    .expect("validated image format")
                    .into_decoder()
                    .expect("validated image decoder");
                if !decoder.color_type().has_alpha() {
                    return Err(parameter("mask 必须是含透明通道的 PNG 图片。"));
                }
                let first = ImageReader::new(Cursor::new(&images[0].0))
                    .with_guessed_format()
                    .expect("validated image format")
                    .into_decoder()
                    .expect("validated image decoder");
                if decoder.dimensions() != first.dimensions() {
                    return Err(parameter("mask 尺寸必须与第一张参考图一致。"));
                }
                drop(decoder);
                Some(mask)
            }
            None => None,
        };
        Ok(Self { images, mask })
    }

    fn validate(&self, input: &EditInput, details: &CapabilityDetails) -> Result<(), CallError> {
        let limits = &details.input_limits;
        if self.images.len() > limits.max_images as usize {
            return Err(parameter("参考图数量超过此模型的编辑能力。"));
        }
        if self.images.iter().any(|(bytes, mime)| {
            bytes.len() as u64 > limits.max_image_bytes
                || !limits.mime_types.iter().any(|m| m == mime)
        }) {
            return Err(CallError::new(
                "invalid_image",
                "指定图片不符合模型的大小或格式限制。",
                Submission::NotSubmitted,
            ));
        }
        let option = details.edit_options.as_ref();
        for (value, allowed, message) in [
            (
                input.background.as_ref(),
                option.map(|o| &o.backgrounds),
                "此模型未声明支持所选背景参数。",
            ),
            (
                input.output_format.as_ref(),
                option.map(|o| &o.output_formats),
                "此模型未声明支持所选输出格式。",
            ),
            (
                input.input_fidelity.as_ref(),
                option.map(|o| &o.input_fidelities),
                "此模型未声明支持所选输入保真度。",
            ),
        ] {
            if value.is_some_and(|value| allowed.is_none_or(|values| !values.contains(value))) {
                return Err(parameter(message));
            }
        }
        if input.background.as_deref() == Some("transparent")
            && option.is_none_or(|o| {
                !o.transparent_output_formats
                    .iter()
                    .any(|v| v == input.output_format.as_deref().unwrap_or("png"))
            })
        {
            return Err(parameter("透明背景需要目录支持的 PNG 或 WebP 输出格式。"));
        }
        if let Some(value) = input.output_compression {
            let supported = option.and_then(|o| o.output_compression.as_ref());
            if supported.is_none_or(|c| {
                value < c.min
                    || value > c.max
                    || input
                        .output_format
                        .as_ref()
                        .is_none_or(|format| !c.formats.contains(format))
            }) {
                return Err(parameter(
                    "压缩参数须在声明范围内，并显式选择允许压缩的输出格式。",
                ));
            }
        }
        if let Some((bytes, _)) = &self.mask
            && option
                .and_then(|o| o.mask.as_ref())
                .is_none_or(|m| bytes.len() as u64 > m.max_image_bytes)
        {
            return Err(parameter(
                "此模型未声明 mask 支持，或 mask 超过单图大小限制。",
            ));
        }
        Ok(())
    }

    fn form(&self, input: &GenerateInput, edit: &EditInput, model: &str) -> multipart::Form {
        let mut form = multipart::Form::new()
            .text("model", model.to_owned())
            .text("prompt", input.prompt.clone())
            .text("n", input.n.unwrap_or(1).to_string());
        for (index, (bytes, mime)) in self.images.iter().enumerate() {
            let extension = match *mime {
                "image/png" => "png",
                "image/jpeg" => "jpg",
                _ => "webp",
            };
            let filename = if self.images.len() == 1 {
                format!("image.{extension}")
            } else {
                format!("image{index}.{extension}")
            };
            let part = multipart::Part::bytes(bytes.clone())
                .file_name(filename)
                .mime_str(mime)
                .expect("known MIME");
            form = form.part(
                if self.images.len() == 1 {
                    "image"
                } else {
                    "image[]"
                },
                part,
            );
        }
        if let Some((bytes, mime)) = &self.mask {
            form = form.part(
                "mask",
                multipart::Part::bytes(bytes.clone())
                    .file_name("mask.png")
                    .mime_str(mime)
                    .expect("known MIME"),
            );
        }
        for (name, value) in [
            ("size", &input.size),
            ("quality", &input.quality),
            ("background", &edit.background),
            ("output_format", &edit.output_format),
            ("input_fidelity", &edit.input_fidelity),
        ] {
            if let Some(value) = value {
                form = form.text(name, value.clone());
            }
        }
        if let Some(complexity) = input.complexity {
            form = form.text("complexity", complexity.as_str().to_owned());
        }
        if let Some(compression) = edit.output_compression {
            form = form.text("output_compression", compression.to_string());
        }
        form
    }
}

fn parameter(message: &'static str) -> CallError {
    CallError::new("invalid_parameters", message, Submission::NotSubmitted)
}

impl ImageClient {
    pub(super) async fn edit_request(
        &self,
        catalog: &Catalog,
        visible: &Value,
        selection: &SelectionRequest<'_>,
        input: &GenerateInput,
        edit: &EditInput,
    ) -> Result<(String, reqwest::Request, Value), CallError> {
        if !catalog.can_submit() {
            catalog.select(selection)?;
        }
        // Keep the original single-image rejection order: unsupported models need no readable file.
        if edit.image_path.is_some()
            && edit.image_paths.is_none()
            && edit.mask_path.is_none()
            && edit.background.is_none()
            && edit.output_format.is_none()
            && edit.output_compression.is_none()
            && edit.input_fidelity.is_none()
        {
            catalog.select(selection)?;
        }
        let files = EditFiles::read(edit).await?;
        let mut alternatives = Vec::new();
        let mut selected = None;
        let mut chosen_priority = None;
        let mut selected_error = None;
        for (index, model) in catalog.models().iter().enumerate() {
            let request = SelectionRequest {
                model_id: Some(&model.id),
                ..*selection
            };
            let candidate = (|| {
                catalog.select(&request)?;
                let capability_index = model
                    .capabilities
                    .iter()
                    .position(|c| c.operation == request.operation && c.mode == request.mode)
                    .expect("selected capability");
                let details: CapabilityDetails = serde_json::from_value(
                    visible["models"][index]["capabilities"][capability_index].clone(),
                )
                .expect("validated capability details");
                files.validate(edit, &details)?;
                let resolved =
                    input.for_capability(&model.capabilities[capability_index].parameters)?;
                let builder = self
                    .api
                    .post(
                        self.base
                            .join(if request.mode == ExecutionMode::SingleAsync {
                                "images/edits/async"
                            } else {
                                "images/edits"
                            })
                            .expect("fixed route"),
                    )
                    .header(reqwest::header::AUTHORIZATION, self.authorization()?)
                    .multipart(files.form(&resolved, edit, &model.id));
                let parameters = json!({
                    "model": model.id, "size": resolved.size,
                    "quality": resolved.quality, "complexity": resolved.complexity
                });
                bounded_request(builder, details.input_limits.max_request_bytes)
                    .map(|request| (request, parameters))
            })();
            match candidate {
                Ok((request, parameters)) => {
                    alternatives.push(model.id.clone());
                    if let Some(priority) = self.model_priority(model, selection.model_id)
                        && chosen_priority.is_none_or(|current| priority < current)
                    {
                        chosen_priority = Some(priority);
                        selected = Some((model.id.clone(), request, parameters));
                    }
                }
                Err(error)
                    if selection.model_id == Some(model.id.as_str())
                        || selected_error.is_none() =>
                {
                    selected_error = Some(error);
                }
                Err(_) => {}
            }
        }
        if let Some(selected) = selected {
            return Ok(selected);
        }
        if self.preferences.is_some() && selection.model_id.is_none() {
            return Err(self.preference_error(catalog, alternatives));
        }
        let mut error = if selection
            .model_id
            .is_some_and(|id| !catalog.models().iter().any(|m| m.id == id))
        {
            CoreError::new(crate::ErrorCode::ModelNotFound, "目录中没有指定的模型。").into()
        } else {
            selected_error.unwrap_or_else(|| {
                CoreError::new(
                    crate::ErrorCode::NoCompatibleModel,
                    "目录中没有满足整次编辑请求的模型。",
                )
                .into()
            })
        };
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
        Err(error)
    }
}
