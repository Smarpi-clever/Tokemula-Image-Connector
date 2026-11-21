//! D4 uses the real MCP boundary and loopback HTTP uploads, with synthetic credentials only.
use super::*;
use crate::image_parameters::{
    ImagePreference, ImagePreferences, SizeRules, quality_label, size_label,
};

fn d4_rules() -> SizeRules {
    SizeRules {
        multiple_of: 16,
        min_edge: 16,
        max_edge: 3840,
        min_pixels: 655_360,
        max_pixels: 8_294_400,
        max_aspect_ratio: 3,
    }
}
fn d4_capabilities(fixture: &PreferenceFixture) {
    for id in ["ranked", "preferred", "backup-a", "backup-b"] {
        fixture.change_model(id, |model| {
            for capability in model["capabilities"].as_array_mut().unwrap() {
                if capability["mode"] != "batch" {
                    capability["parameters"]["sizes"] =
                        json!(["auto", "1024x1024", "1536x1024", "1024x1536"]);
                    capability["parameters"]["qualities"] =
                        json!(["auto", "low", "medium", "high"]);
                    capability["parameters"]["size_rules"] = json!(d4_rules());
                    capability["parameters"]["complexities"] = json!(["low", "medium", "high"]);
                } else {
                    capability["parameters"]["sizes"] = json!(["1K", "2K"]);
                }
            }
        });
    }
}
fn d4_preferences(
    operation: Operation,
    mode: ExecutionMode,
    size: &str,
    quality: Option<&str>,
) -> ImagePreferences {
    ImagePreferences {
        version: 1,
        profiles: vec![ImagePreference {
            operation,
            mode,
            size: Some(size.into()),
            quality: quality.map(str::to_owned),
        }],
    }
}
fn d4_client(
    fixture: &PreferenceFixture,
    operation: Operation,
    mode: ExecutionMode,
) -> ImageClient {
    let mut client = fixture.client(
        preferences("preferred", &["backup-a", "backup-b"], true),
        Duration::from_secs(2),
    );
    client.image_preferences = Some(d4_preferences(operation, mode, "1536x864", Some("high")));
    client
}

#[tokio::test]
async fn d4_json_multipart_sync_async_use_defaults_and_single_call_overrides() {
    for (operation, mode) in [
        (Operation::Generation, ExecutionMode::Sync),
        (Operation::Edit, ExecutionMode::Sync),
        (Operation::Generation, ExecutionMode::SingleAsync),
        (Operation::Edit, ExecutionMode::SingleAsync),
    ] {
        let fixture = PreferenceFixture::new().await;
        d4_capabilities(&fixture);
        let directory = tempfile::tempdir().unwrap();
        let image = local_image(directory.path(), "d4-image.png", false);
        let (client, task) = connect(d4_client(&fixture, operation, mode), true).await;
        let mut input = json!({"prompt":"D4 parameter path"});
        let tool = if operation == Operation::Edit {
            input["image_path"] = json!(image);
            "edit_image"
        } else {
            "generate_image"
        };
        if mode == ExecutionMode::SingleAsync {
            input["mode"] = json!("single_async");
        }
        let result = call(&client, tool, input.clone()).await;
        assert_ne!(result.is_error, Some(true), "{:?}", metadata(&result));
        let original = fixture.last();
        assert_eq!(original.fields["size"], "1536x864");
        assert_eq!(original.fields["quality"], "high");
        assert_eq!(original.fields["complexity"], "medium");
        assert_eq!(
            original.path.ends_with("/async"),
            mode == ExecutionMode::SingleAsync
        );
        assert_eq!(original.files.is_empty(), operation != Operation::Edit);
        input["size"] = json!("1024x1536");
        input["quality"] = json!("low");
        input["complexity"] = json!("high");
        let overridden = call(&client, tool, input).await;
        assert_ne!(
            overridden.is_error,
            Some(true),
            "{:?}",
            metadata(&overridden)
        );
        let post = fixture.last();
        assert_eq!(post.fields["size"], "1024x1536");
        assert_eq!(post.fields["quality"], "low");
        assert_eq!(post.fields["complexity"], "high");
        assert_eq!(post.files, original.files);
        assert_eq!(fixture.count(), 2);
        close(client, task).await;
    }
}

#[tokio::test]
async fn d4_operation_and_mode_profiles_are_distinct_and_absence_is_model_default() {
    let fixture = PreferenceFixture::new().await;
    d4_capabilities(&fixture);
    let mut implementation = d4_client(&fixture, Operation::Edit, ExecutionMode::SingleAsync);
    implementation.image_preferences = Some(d4_preferences(
        Operation::Edit,
        ExecutionMode::SingleAsync,
        "1536x864",
        Some("high"),
    ));
    let (client, task) = connect(implementation, true).await;
    for input in [
        json!({"prompt":"generation sync"}),
        json!({"prompt":"generation async", "mode":"single_async"}),
    ] {
        let result = call(&client, "generate_image", input).await;
        assert_ne!(result.is_error, Some(true), "{:?}", metadata(&result));
        assert_eq!(fixture.last().fields["size"], "auto");
        assert_eq!(fixture.last().fields["quality"], "auto");
    }
    close(client, task).await;
    let legacy = PreferenceFixture::new().await;
    let (client, task) = connect(
        legacy.client(preferences("preferred", &[], false), Duration::from_secs(2)),
        false,
    )
    .await;
    let result = call(
        &client,
        "generate_image",
        json!({"prompt":"legacy directory"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true));
    let fields = legacy.last().fields;
    assert_eq!(
        metadata(&result)["request_parameters"],
        json!({"model":"preferred", "size":null, "quality":null, "complexity":null})
    );
    for name in ["size", "quality", "complexity"] {
        assert!(fields.get(name).is_none(), "{name}: {fields}");
    }
    close(client, task).await;
}

#[tokio::test]
async fn d4_invalid_sizes_and_unsupported_explicit_complexity_stop_before_post() {
    let fixture = PreferenceFixture::new().await;
    d4_capabilities(&fixture);
    let (client, task) = connect(
        d4_client(&fixture, Operation::Generation, ExecutionMode::Sync),
        true,
    )
    .await;
    for size in [
        "1537x864",
        "4000x2048",
        "256x256",
        "3200x800",
        "3840x3840",
        "0x1024",
        "1536×864",
    ] {
        let result = call(
            &client,
            "generate_image",
            json!({"prompt":"reject dimensions","model":"preferred","size":size}),
        )
        .await;
        assert_eq!(
            result.is_error,
            Some(true),
            "size {size}: {:?}",
            metadata(&result)
        );
        assert_eq!(fixture.count(), 0);
    }
    fixture.change_model("preferred", |model| {
        for capability in model["capabilities"].as_array_mut().unwrap() {
            capability["parameters"]
                .as_object_mut()
                .unwrap()
                .remove("complexities");
        }
    });
    for value in ["low", "medium", "high", "xhigh"] {
        let result = call(
            &client,
            "generate_image",
            json!({"prompt":"reject complexity","model":"preferred","complexity":value}),
        )
        .await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(fixture.count(), 0);
    }
    let defaulted = call(
        &client,
        "generate_image",
        json!({"prompt":"supported base request","model":"preferred"}),
    )
    .await;
    assert_ne!(defaulted.is_error, Some(true), "{:?}", metadata(&defaulted));
    assert!(fixture.last().fields.get("complexity").is_none());
    assert_eq!(fixture.count(), 1);
    close(client, task).await;
}

#[tokio::test]
async fn d4_fallback_preserves_dimensions_quality_complexity_references_and_mask() {
    let fixture = PreferenceFixture::new().await;
    d4_capabilities(&fixture);
    fixture.change_model("preferred", |model| {
        for capability in model["capabilities"].as_array_mut().unwrap() {
            capability["parameters"]
                .as_object_mut()
                .unwrap()
                .remove("size_rules");
        }
    });
    fixture.change_model("backup-a", |model| {
        for capability in model["capabilities"].as_array_mut().unwrap() {
            capability["parameters"]["qualities"] = json!(["low"]);
        }
    });
    let directory = tempfile::tempdir().unwrap();
    let first = local_image(directory.path(), "first-d4.png", false);
    let second = local_image(directory.path(), "second-d4.png", false);
    let mask = local_image(directory.path(), "mask-d4.png", true);
    let expected = vec![
        ("image[]".into(), std::fs::read(&first).unwrap()),
        ("image[]".into(), std::fs::read(&second).unwrap()),
        ("mask".into(), std::fs::read(&mask).unwrap()),
    ];
    let (client, task) = connect(
        d4_client(&fixture, Operation::Edit, ExecutionMode::Sync),
        true,
    )
    .await;
    let result = call(&client, "edit_image", json!({"prompt":"preserve full requirements", "image_paths":[first,second], "mask_path":mask, "complexity":"high", "background":"transparent", "output_format":"webp", "input_fidelity":"high", "n":2})).await;
    assert_selected(&result, "backup-b", true);
    let post = fixture.last();
    assert_eq!(post.fields["size"], "1536x864");
    assert_eq!(post.fields["quality"], "high");
    assert_eq!(post.fields["complexity"], "high");
    assert_eq!(post.fields["n"], "2");
    assert_eq!(post.fields["background"], "transparent");
    assert_eq!(post.fields["output_format"], "webp");
    assert_eq!(post.fields["input_fidelity"], "high");
    assert_eq!(post.files, expected);
    assert_eq!(fixture.count(), 1);
    close(client, task).await;
}

#[tokio::test]
async fn d4_complexity_filters_unmanaged_catalog_ranking_before_submission() {
    let fixture = PreferenceFixture::new().await;
    d4_capabilities(&fixture);
    fixture.change_model("ranked", |model| {
        for capability in model["capabilities"].as_array_mut().unwrap() {
            capability["parameters"]["complexities"] = json!([]);
        }
    });
    let implementation =
        ImageClient::build(&fixture.base, Some(KEY.into()), Duration::from_secs(2)).unwrap();
    let (client, task) = connect(implementation, true).await;
    let result = call(
        &client,
        "generate_image",
        json!({"prompt":"complexity participates in ranking", "complexity":"high"}),
    )
    .await;
    assert_ne!(result.is_error, Some(true), "{:?}", metadata(&result));
    assert_eq!(fixture.last().fields["model"], "backup-a");
    assert_eq!(fixture.last().fields["complexity"], "high");
    assert_eq!(fixture.count(), 1);
    close(client, task).await;
}

#[tokio::test]
async fn d4_post_failure_does_not_resubmit_and_old_async_recovery_ignores_new_defaults() {
    let fixture = PreferenceFixture::new().await;
    d4_capabilities(&fixture);
    *fixture.state.common.mode.lock().unwrap() = Mode::Delay;
    let mut implementation = d4_client(&fixture, Operation::Generation, ExecutionMode::Sync);
    implementation = ImageClient::build(&fixture.base, Some(KEY.into()), Duration::from_millis(50))
        .unwrap()
        .with_operation_context(crate::settings::OperationContext {
            api_key: Some(KEY.into()),
            preferences: implementation.preferences.clone(),
            image_preferences: implementation.image_preferences.clone(),
            source: crate::settings::CredentialSource::SystemStore,
        })
        .unwrap();
    let input = serde_json::from_value(json!({"prompt":"one post with defaults"})).unwrap();
    let failed = implementation
        .execute(&input, None, &RequestProgress::default())
        .await
        .unwrap_err();
    assert_eq!(failed.submission, Submission::Unknown);
    assert_eq!(fixture.count(), 1);
    assert_eq!(fixture.last().fields["size"], "1536x864");
    implementation.image_preferences = Some(d4_preferences(
        Operation::Generation,
        ExecutionMode::Sync,
        "invalid-new-size",
        Some("unsupported-new-quality"),
    ));
    fixture.state.common.catalog.lock().unwrap()["can_submit"] = json!(false);
    let previous_gets = fixture.state.common.gets.load(Ordering::SeqCst);
    let recovered = implementation
        .task(
            "imgtask_preference",
            &RequestProgress::for_task("imgtask_preference"),
        )
        .await
        .unwrap();
    assert_eq!(recovered.metadata["status"], "processing");
    assert_eq!(recovered.metadata["request_ref"], "fixture-request");
    assert_eq!(
        fixture.state.common.gets.load(Ordering::SeqCst),
        previous_gets
    );
    assert_eq!(fixture.count(), 1);
}

#[tokio::test]
async fn d4_batch_replay_keeps_original_resolution_after_image_defaults_change() {
    let fixture = PreferenceFixture::new().await;
    d4_capabilities(&fixture);
    let directory = tempfile::tempdir().unwrap();
    let mut implementation = fixture.client(
        preferences("preferred", &["backup-a"], true),
        Duration::from_secs(2),
    );
    implementation.task_store = Some(TaskStore {
        root: directory.path().join("tasks"),
    });
    implementation.image_preferences = Some(d4_preferences(
        Operation::Generation,
        ExecutionMode::Batch,
        "2K",
        None,
    ));
    let input = serde_json::from_value(json!({"idempotency_key":"d4-stable-batch", "items":[{"custom_id":"a", "prompt":"first"},{"custom_id":"b", "prompt":"second"}]})).unwrap();
    implementation
        .submit_batch(&input, &BatchProgress::submission())
        .await
        .unwrap();
    let original = fixture.last().fields;
    assert_eq!(original["image_size"], "2K");
    assert!(original.get("quality").is_none());
    assert!(original.get("complexity").is_none());
    implementation.image_preferences = Some(d4_preferences(
        Operation::Generation,
        ExecutionMode::Batch,
        "1K",
        None,
    ));
    implementation.preferences = Some(preferences("backup-b", &[], false));
    implementation
        .submit_batch(&input, &BatchProgress::submission())
        .await
        .unwrap();
    assert_eq!(fixture.last().fields, original);
    assert_eq!(fixture.count(), 2);
    assert_eq!(fixture.state.creates.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn d4_mcp_keeps_eight_tools_and_exposes_optional_complexity_only_for_single_requests() {
    let fixture = PreferenceFixture::new().await;
    let (client, task) = connect(
        fixture.client(preferences("preferred", &[], false), Duration::from_secs(2)),
        true,
    )
    .await;
    let tools = client.list_tools(None).await.unwrap().tools;
    assert_eq!(tools.len(), 8);
    for name in ["generate_image", "edit_image"] {
        let tool = tools.iter().find(|t| t.name == name).unwrap();
        let schema = serde_json::to_value(&tool.input_schema).unwrap();
        assert!(schema["properties"].get("complexity").is_some());
        let required = schema["required"].as_array().unwrap();
        assert!(
            !required
                .iter()
                .any(|v| v == "complexity" || v == "size" || v == "quality")
        );
        let text = schema.to_string();
        for value in ["low", "medium", "high"] {
            assert!(text.contains(value));
        }
    }
    let batch = tools
        .iter()
        .find(|t| t.name == "submit_image_batch")
        .unwrap();
    let schema = serde_json::to_value(&batch.input_schema).unwrap();
    assert!(schema["properties"].get("quality").is_none());
    assert!(schema["properties"].get("complexity").is_none());
    assert_eq!(fixture.count(), 0);
    close(client, task).await;
}

#[test]
fn d4_dimension_labels_rules_and_versioned_preferences_keep_exact_meanings() {
    for (value, expected) in [
        ("1024x1024", "1024×1024 · 1:1"),
        ("1536x1024", "1536×1024 · 3:2"),
        ("1024x1536", "1024×1536 · 2:3"),
        ("1536x864", "1536×864 · 16:9"),
        ("1K", "1K · 分辨率等级"),
        ("auto", "自动 (auto)"),
    ] {
        assert_eq!(size_label(value), expected);
    }
    assert_eq!(quality_label("high"), "高 (high)");
    let rules = d4_rules();
    for size in ["1536x864", "1024x1536", "3840x2160", "2160x3840"] {
        assert!(rules.allows(size), "{size}");
    }
    for size in [
        "1537x864",
        "4000x2048",
        "256x256",
        "3200x800",
        "3840x3840",
        "0x1024",
        "1536×864",
        "-1536x864",
    ] {
        assert!(!rules.allows(size), "{size}");
    }
    assert!(ImagePreferences::default().validate().is_ok());
    let mut preferences = d4_preferences(
        Operation::Generation,
        ExecutionMode::Sync,
        "1536x864",
        Some("high"),
    );
    preferences.profiles.push(preferences.profiles[0].clone());
    assert!(preferences.validate().is_err());
    preferences.profiles.pop();
    preferences.version = 2;
    assert!(preferences.validate().is_err());
    assert!(
        d4_preferences(
            Operation::Generation,
            ExecutionMode::Batch,
            "1K",
            Some("high")
        )
        .validate()
        .is_err()
    );
}

#[tokio::test]
async fn sync_observability_matches_wire_parameters_and_preserves_mismatched_original() {
    let mut original = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(1254, 1254)
        .write_to(&mut original, ImageFormat::Png)
        .unwrap();
    let original = original.into_inner();
    for operation in [Operation::Generation, Operation::Edit] {
        let fixture = PreferenceFixture::new().await;
        d4_capabilities(&fixture);
        *fixture.state.common.mode.lock().unwrap() = Mode::Error(
            StatusCode::OK,
            json!({
                "model":"fixture-effective",
                "data":[{"b64_json":STANDARD.encode(&original)}],
                "actual_fee":SENSITIVE, "api_key":KEY
            }),
        );
        let mut implementation = fixture.client(
            preferences("preferred", &["backup-a"], true),
            Duration::from_secs(2),
        );
        implementation.image_preferences = Some(d4_preferences(
            operation,
            ExecutionMode::Sync,
            "1024x1024",
            Some("medium"),
        ));
        let directory = tempfile::tempdir().unwrap();
        let mut input = json!({"prompt":"observe original dimensions","n":1});
        let tool = if operation == Operation::Edit {
            input["image_path"] = json!(local_image(directory.path(), "original.png", false));
            "edit_image"
        } else {
            "generate_image"
        };
        let (client, task) = connect(implementation, true).await;
        let result = call(&client, tool, input).await;
        assert_ne!(result.is_error, Some(true), "{:?}", metadata(&result));
        let meta = metadata(&result);
        let expected = json!({
            "model":"preferred", "size":"1024x1024", "quality":"medium", "complexity":"medium"
        });
        assert_eq!(meta["connector_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(meta["request_parameters"], expected);
        assert_eq!(
            meta["image_dimensions"],
            json!([{"width":1254,"height":1254}])
        );
        for name in ["model", "size", "quality", "complexity"] {
            assert_eq!(fixture.last().fields[name], expected[name]);
        }
        let wire = serde_json::to_value(&result).unwrap();
        let blocks = wire["content"].as_array().unwrap();
        let image = blocks.iter().find(|b| b["type"] == "image").unwrap();
        assert_eq!(
            STANDARD.decode(image["data"].as_str().unwrap()).unwrap(),
            original
        );
        let caption = blocks.iter().find(|b| b["type"] == "text").unwrap()["text"]
            .as_str()
            .unwrap();
        let summary: Value = serde_json::from_str(caption.split_once('\n').unwrap().1).unwrap();
        assert_eq!(summary["connector_version"], env!("CARGO_PKG_VERSION"));
        assert_eq!(summary["request_parameters"], meta["request_parameters"]);
        assert_eq!(summary["image_dimensions"], meta["image_dimensions"]);
        assert_eq!(summary["selected_model"], meta["selected_model"]);
        assert_eq!(summary["model_selection"], meta["model_selection"]);
        assert_eq!(summary["effective_model"], "fixture-effective");
        assert_eq!(summary["request_ref"], "fixture-request");
        assert_no_fees(meta);
        assert_no_fees(&summary);
        assert!(!caption.contains(KEY));
        assert!(!caption.contains(SENSITIVE));
        assert_eq!(fixture.state.common.fees.load(Ordering::SeqCst), 0);
        assert_eq!(fixture.count(), 1);
        close(client, task).await;
    }
}
