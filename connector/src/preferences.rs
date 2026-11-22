//! User-confirmed ordering is applied only to candidates valid for the complete request.
use super::*;

impl ImageClient {
    pub(super) fn model_priority(
        &self,
        model: &crate::Model,
        explicit: Option<&str>,
    ) -> Option<(usize, bool, bool, u32)> {
        if let Some(id) = explicit {
            return (id == model.id).then_some((0, false, false, 0));
        }
        if let Some(preferences) = &self.preferences {
            let position = if model.id == preferences.default_model {
                Some(0)
            } else if preferences.auto_fallback {
                preferences
                    .fallback_models
                    .iter()
                    .position(|id| id == &model.id)
                    .map(|i| i + 1)
            } else {
                None
            };
            return position.map(|i| (i, false, false, 0));
        }
        Some((
            0,
            model.family.as_deref() != Some(crate::IMAGE_26_FAMILY),
            model.preference_rank.is_none(),
            model.preference_rank.unwrap_or(u32::MAX),
        ))
    }

    pub(super) fn preference_metadata(
        &self,
        catalog: &Catalog,
        selected: &str,
        explicit: Option<&str>,
    ) -> Value {
        let Some(preferences) = self.preferences.as_ref().filter(|_| explicit.is_none()) else {
            return Value::Null;
        };
        let fallback = selected != preferences.default_model;
        let (code, message) = if !fallback {
            ("preferred_model", "使用已设置的默认模型。")
        } else {
            self.preference_reason(catalog)
        };
        json!({"default_model":preferences.default_model,"selected_model":selected,"used_fallback":fallback,"reason_code":code,"message":message})
    }

    fn preference_reason(&self, catalog: &Catalog) -> (&'static str, &'static str) {
        let preferences = self.preferences.as_ref().expect("user preferences");
        match catalog
            .models()
            .iter()
            .find(|model| model.id == preferences.default_model)
        {
            None => ("preferred_model_missing", "默认模型不在当前目录中。"),
            Some(model) if model.availability != crate::Availability::Configured => {
                ("preferred_model_unavailable", "默认模型暂不可用。")
            }
            Some(_) => ("preferred_request_unsupported", "默认模型不支持完整请求。"),
        }
    }

    pub(super) fn preference_error(
        &self,
        catalog: &Catalog,
        alternatives: Vec<String>,
    ) -> CallError {
        let preferences = self.preferences.as_ref().expect("user preferences");
        let mut error = CallError::new(
            "preferred_model_unavailable",
            if preferences.auto_fallback {
                "默认模型及已设置的备用均不适用，请选择可用模型或更新模型偏好。"
            } else {
                "默认模型不适用，自动降级已关闭；请指定模型或更新模型偏好。"
            },
            Submission::NotSubmitted,
        );
        let (reason_code, reason) = self.preference_reason(catalog);
        error.model_selection = Some(
            json!({"default_model":preferences.default_model,"selected_model":null,"used_fallback":false,"reason_code":reason_code,"message":reason}),
        );
        error.selection = Some(Box::new(CoreError {
            code: crate::ErrorCode::NoCompatibleModel,
            message: error.message,
            alternatives,
        }));
        error
    }
}

impl ImageClient {
    pub(super) fn generation_request(
        &self,
        catalog: &Catalog,
        visible: &Value,
        selection: &SelectionRequest<'_>,
        input: &crate::mcp::GenerateInput,
        asynchronous: bool,
    ) -> Result<(String, reqwest::Request, Value), CallError> {
        if !catalog.can_submit() {
            catalog.select(selection)?;
        }
        if self.preferences.is_none() || selection.model_id.is_some() {
            catalog.select(selection)?;
        }
        let exact = selection.model_id;
        let mut selected = None;
        let mut best_priority = None;
        let mut alternatives = Vec::new();
        let mut exact_error = None;
        let mut construction_count = 0;
        for (model_index, model) in catalog.models().iter().enumerate() {
            let candidate = (|| {
                catalog.select(&SelectionRequest {
                    model_id: Some(&model.id),
                    ..*selection
                })?;
                let capability_index = model
                    .capabilities
                    .iter()
                    .position(|capability| {
                        capability.operation == selection.operation
                            && capability.mode == selection.mode
                    })
                    .expect("selected capability");
                let capability = &visible["models"][model_index]["capabilities"][capability_index];
                let resolved =
                    input.for_capability(&model.capabilities[capability_index].parameters)?;
                let input = &resolved;
                construction_count += 1;
                self.counts(
                    "candidate_construct_start",
                    &[("construction_count", construction_count)],
                );
                let mut body =
                    json!({"model":model.id,"prompt":input.prompt,"n":selection.image_count});
                if let Some(complexity) = input.complexity {
                    body["complexity"] = json!(complexity);
                }
                if let Some(size) = &input.size {
                    body["size"] = json!(size);
                }
                if let Some(quality) = &input.quality {
                    body["quality"] = json!(quality);
                }
                let parameters = json!({
                    "model": model.id, "size": input.size,
                    "quality": input.quality, "complexity": input.complexity
                });
                bounded_request(
                    self.api
                        .post(
                            self.base
                                .join(if asynchronous {
                                    "images/generations/async"
                                } else {
                                    "images/generations"
                                })
                                .expect("fixed route"),
                        )
                        .header(reqwest::header::AUTHORIZATION, self.authorization()?)
                        .json(&body),
                    capability["input_limits"]["max_request_bytes"]
                        .as_u64()
                        .expect("validated request limit"),
                )
                .map(|request| (request, parameters))
            })();
            match candidate {
                Ok((request, parameters)) => {
                    alternatives.push(model.id.clone());
                    if let Some(priority) = self.model_priority(model, exact)
                        && best_priority.is_none_or(|best| priority < best)
                    {
                        best_priority = Some(priority);
                        selected = Some((model.id.clone(), request, parameters));
                    }
                }
                Err(error) if exact == Some(model.id.as_str()) || exact_error.is_none() => {
                    exact_error = Some(error)
                }
                Err(_) => (),
            }
        }
        self.counts(
            "candidate_counts",
            &[
                ("candidate_count", catalog.models().len()),
                ("construction_count", construction_count),
            ],
        );
        selected.ok_or_else(|| {
            if self.preferences.is_some() && exact.is_none() {
                self.preference_error(catalog, alternatives)
            } else {
                exact_error.unwrap_or_else(|| {
                    CoreError::new(
                        crate::ErrorCode::NoCompatibleModel,
                        "目录中没有适合完整图片请求的模型。",
                    )
                    .into()
                })
            }
        })
    }
}

impl crate::mcp::GenerateInput {
    pub(super) fn for_capability(
        &self,
        parameters: &crate::ParameterConstraints,
    ) -> Result<Self, CallError> {
        let mut resolved = self.clone();
        if let Some(complexity) = self.complexity {
            if !parameters
                .complexities
                .iter()
                .any(|value| value == complexity.as_str())
            {
                return Err(CallError::new(
                    "invalid_parameters",
                    "此模型路径不支持所选图片思考复杂度。",
                    Submission::NotSubmitted,
                ));
            }
        } else if parameters
            .complexities
            .iter()
            .any(|value| value == "medium")
        {
            resolved.complexity = Some(crate::mcp::ImageComplexity::Medium);
        }
        if resolved.size.is_none() && parameters.sizes.iter().any(|v| v == "auto") {
            resolved.size = Some("auto".into());
        }
        if resolved.quality.is_none() && parameters.qualities.iter().any(|v| v == "auto") {
            resolved.quality = Some("auto".into());
        }
        Ok(resolved)
    }
}
