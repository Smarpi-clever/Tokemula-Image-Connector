//! Advanced editing acceptance through loopback HTTP and actual MCP calls.
use super::*;
use std::path::{Path, PathBuf};

#[derive(Clone, Debug)]
struct UploadedFile {
    name: String,
    mime: String,
    filename: String,
    bytes: Vec<u8>,
}

#[derive(Clone, Debug)]
struct EditUpload {
    path: String,
    fields: Value,
    files: Vec<UploadedFile>,
    body: Vec<u8>,
    content_type: String,
}

struct AdvancedState {
    common: Arc<FixtureState>,
    uploads: Mutex<Vec<EditUpload>>,
}

struct AdvancedFixture {
    state: Arc<AdvancedState>,
    base: String,
    server: JoinHandle<()>,
}

impl Drop for AdvancedFixture {
    fn drop(&mut self) {
        self.server.abort();
    }
}

impl AdvancedFixture {
    async fn new(catalog: Value) -> Self {
        let common = Arc::new(FixtureState {
            catalog: Mutex::new(catalog),
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
        let state = Arc::new(AdvancedState {
            common: common.clone(),
            uploads: Mutex::new(Vec::new()),
        });
        let router = Router::new()
            .route("/v1/images/models", get(catalog_handler))
            .route(
                "/v1/images/requests/{request_ref}/billing",
                get(fee_handler),
            )
            .with_state(common)
            .merge(
                Router::new()
                    .route("/v1/images/edits", post(advanced_edit_handler))
                    .route("/v1/images/edits/async", post(advanced_edit_handler))
                    .with_state(state.clone()),
            );
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}/v1/", listener.local_addr().unwrap());
        let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
        Self {
            state,
            base,
            server,
        }
    }

    fn client(&self, timeout: Duration) -> ImageClient {
        ImageClient::build(&self.base, Some(KEY.into()), timeout).unwrap()
    }

    fn posts(&self) -> usize {
        self.state.common.posts.load(Ordering::SeqCst)
    }
}

async fn advanced_edit_handler(
    State(state): State<Arc<AdvancedState>>,
    request: Request,
) -> AxumResponse {
    use axum::extract::FromRequest;
    assert!(auth_ok(&request));
    let mut upload = EditUpload {
        path: request.uri().path().into(),
        content_type: request.headers()["content-type"].to_str().unwrap().into(),
        fields: json!({}),
        files: Vec::new(),
        body: Vec::new(),
    };
    let (parts, body) = request.into_parts();
    upload.body = to_bytes(body, MAX_METADATA_RESPONSE_BYTES)
        .await
        .unwrap()
        .to_vec();
    let request = Request::from_parts(parts, Body::from(upload.body.clone()));
    let mut multipart = Multipart::from_request(request, &()).await.unwrap();
    while let Some(field) = multipart.next_field().await.unwrap() {
        let name = field.name().unwrap().to_owned();
        if let Some(filename) = field.file_name() {
            let filename = filename.to_owned();
            let mime = field.content_type().unwrap().to_owned();
            upload.files.push(UploadedFile {
                name,
                mime,
                filename,
                bytes: field.bytes().await.unwrap().to_vec(),
            });
        } else {
            assert!(upload.fields.get(&name).is_none(), "duplicate scalar field");
            upload.fields[&name] = json!(field.text().await.unwrap());
        }
    }
    state.uploads.lock().unwrap().push(upload);
    respond(&state.common).await
}

fn colored_png(width: u32, height: u32, rgba: Option<[u8; 4]>) -> Vec<u8> {
    let image = match rgba {
        Some(color) => image::DynamicImage::ImageRgba8(image::RgbaImage::from_pixel(
            width,
            height,
            image::Rgba(color),
        )),
        None => image::DynamicImage::ImageRgb8(image::RgbImage::from_pixel(
            width,
            height,
            image::Rgb([40, 70, 90]),
        )),
    };
    let mut bytes = Cursor::new(Vec::new());
    image.write_to(&mut bytes, ImageFormat::Png).unwrap();
    bytes.into_inner()
}

fn local_file(root: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = root.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

fn edit_options() -> Value {
    json!({
        "backgrounds":["auto","opaque","transparent"],
        "output_formats":["png","jpeg","webp"],
        "input_fidelities":["high","low"],
        "output_compression":{"min":0,"max":100,"formats":["jpeg","webp"]},
        "transparent_output_formats":["png","webp"],
        "mask":{"max_images":1,"max_image_bytes":4194303,"mime_types":["image/png"],
            "requires_alpha":true,"same_dimensions_as_first_image":true,"applies_to_image":0}
    })
}

fn advanced_catalog() -> Value {
    let model = |id: &str, rank: u32, native: bool| {
        let caps: Vec<_> = ["sync", "single_async"]
            .into_iter()
            .map(|mode| {
                let mut cap = json!({
                    "operation":"edit","mode":mode,
                    "parameters":{"max_images":2,"sizes":["1024x1024"],"qualities":["high"]},
                    "input_limits":{"max_images":if native {3}else{1},"max_image_bytes":20971520,
                        "max_request_bytes":268435456,"mime_types":["image/png","image/jpeg","image/webp"]},
                    "result_formats":["b64_json","url"],"cancel_supported":false
                });
                if native {
                    cap["edit_options"] = edit_options();
                }
                cap
            })
            .collect();
        json!({"id":id,"display_name":id,"preference_rank":rank,"availability":"configured",
            "capabilities":caps,"pricing":{"status":"unknown"}})
    };
    json!({"schema_version":"1","observed_at":"2026-11-20T00:00:00Z","can_submit":true,
        "models":[model("synthetic-basic",0,false),model("synthetic-native",1,true)]})
}

fn accepted_edit() -> Value {
    json!({"id":"imgtask_advanced","task_id":"imgtask_advanced","request_ref":"fixture-request",
        "status":"processing","created_at":1795156000,"expires_at":1795242400,
        "result_expires_at":null})
}

#[tokio::test]
async fn old_single_image_keeps_basic_selection_and_omits_all_new_wire_fields() {
    let fixture = AdvancedFixture::new(advanced_catalog()).await;
    let directory = tempfile::tempdir().unwrap();
    let bytes = colored_png(3, 2, None);
    let path = local_file(directory.path(), "旧图片.png", &bytes);
    let (client, server) = connect(fixture.client(Duration::from_secs(3)), false).await;
    for asynchronous in [false, true] {
        if asynchronous {
            *fixture.state.common.mode.lock().unwrap() =
                Mode::Error(StatusCode::ACCEPTED, accepted_edit());
        }
        let mut input = json!({"image_path":path,"prompt":"保留原调用"});
        if asynchronous {
            input["mode"] = json!("single_async");
        }
        let result = call(&client, "edit_image", input).await;
        if asynchronous {
            assert_eq!(metadata(&result)["submission"], "accepted");
            assert_eq!(metadata(&result)["task_id"], "imgtask_advanced");
            assert_eq!(metadata(&result)["request_ref"], "fixture-request");
        } else {
            assert_image(&result);
        }
        assert_eq!(metadata(&result)["selected_model"], "synthetic-basic");
        let records = fixture.state.uploads.lock().unwrap();
        let record = records.last().unwrap();
        assert_eq!(
            record.path,
            if asynchronous {
                "/v1/images/edits/async"
            } else {
                "/v1/images/edits"
            }
        );
        assert_eq!(record.files.len(), 1);
        assert_eq!(record.files[0].name, "image");
        assert_eq!(record.files[0].bytes, bytes);
        assert_eq!(
            record.fields,
            json!({"model":"synthetic-basic","prompt":"保留原调用","n":"1"})
        );
    }
    assert_eq!(fixture.posts(), 2);
    assert_eq!(fixture.state.common.fees.load(Ordering::SeqCst), 0);
    close(client, server).await;
}

#[tokio::test]
async fn ordered_references_first_image_mask_and_explicit_options_cross_both_edit_modes() {
    for asynchronous in [false, true] {
        let fixture = AdvancedFixture::new(advanced_catalog()).await;
        let directory = tempfile::tempdir().unwrap();
        let first = colored_png(3, 2, None);
        let second = colored_png(5, 4, Some([20, 40, 180, 255]));
        let mask = colored_png(3, 2, Some([0, 0, 0, 0]));
        let a = local_file(directory.path(), "第一张.png", &first);
        let b = local_file(directory.path(), "第二张.png", &second);
        let m = local_file(directory.path(), "mask.png", &mask);
        if asynchronous {
            *fixture.state.common.mode.lock().unwrap() =
                Mode::Error(StatusCode::ACCEPTED, accepted_edit());
        }
        let (client, server) = connect(fixture.client(Duration::from_secs(3)), true).await;
        let mut input = json!({"image_paths":[a,b],"mask_path":m,"prompt":"有序合成",
            "size":"1024x1024","quality":"high","n":2,
            "background":"transparent","output_format":"webp","output_compression":73,"input_fidelity":"high"});
        if asynchronous {
            input["mode"] = json!("single_async");
        }
        let result = call(&client, "edit_image", input).await;
        assert_ne!(result.is_error, Some(true), "{:?}", metadata(&result));
        assert_eq!(metadata(&result)["selected_model"], "synthetic-native");
        if asynchronous {
            assert_eq!(metadata(&result)["submission"], "accepted");
            assert_eq!(metadata(&result)["recovery"]["status"], "saved");
        } else {
            assert_image(&result);
        }
        let record = fixture.state.uploads.lock().unwrap()[0].clone();
        assert_eq!(
            record.path,
            if asynchronous {
                "/v1/images/edits/async"
            } else {
                "/v1/images/edits"
            }
        );
        assert_eq!(
            record
                .files
                .iter()
                .map(|f| f.name.as_str())
                .collect::<Vec<_>>(),
            vec!["image[]", "image[]", "mask"]
        );
        assert_eq!(
            record
                .files
                .iter()
                .map(|f| f.bytes.clone())
                .collect::<Vec<_>>(),
            vec![first, second, mask]
        );
        assert!(
            record
                .files
                .iter()
                .all(|f| f.mime == "image/png" && f.filename.ends_with(".png"))
        );
        assert_eq!(
            record.fields,
            json!({"model":"synthetic-native","prompt":"有序合成","n":"2",
            "size":"1024x1024","quality":"high","background":"transparent","output_format":"webp",
            "output_compression":"73","input_fidelity":"high"})
        );
        assert_eq!(fixture.posts(), 1);
        assert_eq!(fixture.state.common.fees.load(Ordering::SeqCst), 0);
        close(client, server).await;
    }
}

#[tokio::test]
async fn invalid_edit_inputs_and_unsupported_combinations_never_submit() {
    let fixture = AdvancedFixture::new(advanced_catalog()).await;
    let dir = tempfile::tempdir().unwrap();
    let first = local_file(dir.path(), "primary.png", &colored_png(3, 2, None));
    let second = local_file(dir.path(), "secondary.png", &colored_png(5, 4, None));
    let mask = local_file(
        dir.path(),
        "mask.png",
        &colored_png(3, 2, Some([0, 0, 0, 0])),
    );
    let wrong_dimensions = local_file(
        dir.path(),
        "wrong-size.png",
        &colored_png(5, 4, Some([0, 0, 0, 0])),
    );
    let opaque = local_file(dir.path(), "no-alpha.png", &colored_png(3, 2, None));
    let mut jpeg = Cursor::new(Vec::new());
    image::DynamicImage::new_rgb8(3, 2)
        .write_to(&mut jpeg, ImageFormat::Jpeg)
        .unwrap();
    let jpeg_mask = local_file(dir.path(), "jpeg-mask.png", &jpeg.into_inner());
    let (client, server) = connect(fixture.client(Duration::from_secs(3)), false).await;
    let base = json!({"image_paths":[first.clone(),second.clone()],"prompt":"synthetic"});
    let mut cases = vec![
        json!({"prompt":"synthetic"}),
        json!({"image_paths":[],"prompt":"synthetic"}),
        json!({"image_path":first,"image_paths":[second],"prompt":"synthetic"}),
        json!({"image_paths":["relative.png"],"prompt":"synthetic"}),
        json!({"image_paths":[first.clone(),second.clone(),first.clone(),second.clone()],"prompt":"synthetic"}),
        json!({"image_paths":[first.clone(),second.clone()],"mask_path":wrong_dimensions,"prompt":"synthetic"}),
        json!({"image_path":first,"mask_path":opaque,"prompt":"synthetic"}),
        json!({"image_path":first,"mask_path":jpeg_mask,"prompt":"synthetic"}),
        json!({"image_path":first,"mask_path":"missing.png","prompt":"synthetic"}),
    ];
    for option in [
        json!({"background":"invented"}),
        json!({"output_format":"gif"}),
        json!({"input_fidelity":"medium"}),
        json!({"output_compression":101,"output_format":"jpeg"}),
        json!({"output_compression":-1,"output_format":"jpeg"}),
        json!({"output_compression":50}),
        json!({"output_compression":50,"output_format":"png"}),
        json!({"background":"transparent","output_format":"jpeg"}),
        json!({"extra_params":{"provider":"private"}}),
    ] {
        let mut item = base.clone();
        item.as_object_mut()
            .unwrap()
            .extend(option.as_object().unwrap().clone());
        cases.push(item);
    }
    for (index, input) in cases.into_iter().enumerate() {
        let result = call(&client, "edit_image", input).await;
        assert_eq!(
            result.is_error,
            Some(true),
            "case {index} unexpectedly accepted"
        );
        assert_eq!(
            metadata(&result)["submission"],
            "not_submitted",
            "case {index}"
        );
        assert_eq!(fixture.posts(), 0, "case {index}");
    }
    for field in [
        "backgrounds",
        "output_formats",
        "input_fidelities",
        "output_compression",
        "mask",
    ] {
        let mut catalog = advanced_catalog();
        catalog["models"][1]["capabilities"][0]["edit_options"]
            .as_object_mut()
            .unwrap()
            .remove(field);
        *fixture.state.common.catalog.lock().unwrap() = catalog;
        let mut input = json!({"image_path":first,"prompt":"synthetic","model":"synthetic-native"});
        match field {
            "backgrounds" => input["background"] = json!("opaque"),
            "output_formats" => input["output_format"] = json!("png"),
            "input_fidelities" => input["input_fidelity"] = json!("high"),
            "output_compression" => {
                input["output_format"] = json!("jpeg");
                input["output_compression"] = json!(50);
            }
            "mask" => input["mask_path"] = json!(mask),
            _ => unreachable!(),
        }
        let result = call(&client, "edit_image", input).await;
        assert_eq!(result.is_error, Some(true), "undeclared {field}");
        assert_eq!(fixture.posts(), 0);
    }
    assert_eq!(fixture.state.common.fees.load(Ordering::SeqCst), 0);
    close(client, server).await;
}

#[tokio::test]
async fn whole_request_selection_and_reference_mask_limits_preserve_explicit_choice() {
    let fixture = AdvancedFixture::new(advanced_catalog()).await;
    let dir = tempfile::tempdir().unwrap();
    let image = colored_png(3, 2, None);
    let mask_bytes = colored_png(3, 2, Some([0, 0, 0, 0]));
    let primary = local_file(dir.path(), "primary.png", &image);
    let mask = local_file(dir.path(), "mask.png", &mask_bytes);
    let (client, server) = connect(fixture.client(Duration::from_secs(3)), true).await;
    for input in [
        json!({"image_paths":[primary.clone(),primary.clone()],"model":"synthetic-basic","prompt":"synthetic"}),
        json!({"image_path":primary,"mask_path":mask,"model":"synthetic-basic","prompt":"synthetic"}),
        json!({"image_path":primary,"background":"opaque","model":"synthetic-basic","prompt":"synthetic"}),
    ] {
        let result = call(&client, "edit_image", input).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(
            metadata(&result)["selection"]["alternatives"],
            json!(["synthetic-native"])
        );
        assert_eq!(fixture.posts(), 0);
    }
    for (mask_limit, image_limit, allow) in [
        (mask_bytes.len() - 1, image.len(), false),
        (mask_bytes.len(), image.len() - 1, false),
        (mask_bytes.len(), image.len(), true),
    ] {
        let mut catalog = advanced_catalog();
        let cap = &mut catalog["models"][1]["capabilities"][0];
        cap["edit_options"]["mask"]["max_image_bytes"] = json!(mask_limit);
        cap["input_limits"]["max_image_bytes"] = json!(image_limit);
        *fixture.state.common.catalog.lock().unwrap() = catalog;
        let result = call(
            &client,
            "edit_image",
            json!({"image_paths":[primary.clone(),primary.clone()],"mask_path":mask,
            "prompt":"synthetic","background":"transparent"}),
        )
        .await;
        if allow {
            assert_image(&result);
            let upload = fixture.state.uploads.lock().unwrap()[0].clone();
            assert!(
                upload.fields.get("output_format").is_none(),
                "implicit PNG for combination does not inject a wire default"
            );
        } else {
            assert_eq!(result.is_error, Some(true));
            assert_eq!(fixture.posts(), 0);
        }
    }
    assert_eq!(fixture.posts(), 1);
    close(client, server).await;
}

#[tokio::test]
async fn encoded_multiple_images_and_mask_body_limit_includes_all_multipart_overhead() {
    let fixture = AdvancedFixture::new(advanced_catalog()).await;
    let dir = tempfile::tempdir().unwrap();
    let image = colored_png(3, 2, None);
    let mask_bytes = colored_png(3, 2, Some([0, 0, 0, 0]));
    let primary = local_file(dir.path(), "primary.png", &image);
    let mask = local_file(dir.path(), "mask.png", &mask_bytes);
    let input = json!({"image_paths":[primary.clone(),primary],"mask_path":mask,"model":"synthetic-native",
        "prompt":"引号\"与换行\n","output_format":"webp","output_compression":51});
    let (client, server) = connect(fixture.client(Duration::from_secs(3)), false).await;
    let result = call(&client, "edit_image", input.clone()).await;
    assert_image(&result);
    let size = fixture.state.uploads.lock().unwrap()[0].body.len();
    assert!(size > image.len() * 2 + mask_bytes.len());
    fixture.state.common.catalog.lock().unwrap()["models"][1]["capabilities"][0]["input_limits"]
        ["max_request_bytes"] = json!(size - 1);
    let rejected = call(&client, "edit_image", input.clone()).await;
    assert_eq!(rejected.is_error, Some(true));
    assert_eq!(fixture.posts(), 1);
    fixture.state.common.catalog.lock().unwrap()["models"][1]["capabilities"][0]["input_limits"]
        ["max_request_bytes"] = json!(size);
    let result = call(&client, "edit_image", input).await;
    assert_image(&result);
    assert_eq!(fixture.state.uploads.lock().unwrap()[1].body.len(), size);
    assert_eq!(fixture.posts(), 2);
    close(client, server).await;
}

#[tokio::test]
async fn advanced_post_failure_preserves_one_submission_and_images_without_fee_requests() {
    let directory = tempfile::tempdir().unwrap();
    let path = local_file(directory.path(), "input.png", &colored_png(3, 2, None));
    let input = json!({"image_paths":[path.clone(),path.clone()],"prompt":"synthetic","input_fidelity":"high"});
    for mode in [
        Mode::Delay,
        Mode::Disconnect,
        Mode::Redirect("http://127.0.0.1:1/never".into()),
        Mode::Error(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({"error":{"message":SENSITIVE}}),
        ),
    ] {
        let fixture = AdvancedFixture::new(advanced_catalog()).await;
        *fixture.state.common.mode.lock().unwrap() = mode;
        let (client, server) = connect(fixture.client(Duration::from_millis(100)), false).await;
        let result = call(&client, "edit_image", input.clone()).await;
        assert_eq!(result.is_error, Some(true));
        assert_eq!(metadata(&result)["submission"], "unknown");
        assert_eq!(fixture.posts(), 1);
        assert_eq!(fixture.state.common.fees.load(Ordering::SeqCst), 0);
        assert!(!serde_json::to_string(&result).unwrap().contains(SENSITIVE));
        close(client, server).await;
    }
    let fixture = AdvancedFixture::new(advanced_catalog()).await;
    *fixture.state.common.fee.lock().unwrap() = (
        StatusCode::SERVICE_UNAVAILABLE,
        json!({"error":{"message":SENSITIVE}}),
    );
    let bytes = noise_png(1600, 1600);
    *fixture.state.common.mode.lock().unwrap() = Mode::Error(
        StatusCode::OK,
        json!({"data":[{"b64_json":STANDARD.encode(&bytes)}]}),
    );
    let (client, server) = connect(fixture.client(Duration::from_secs(10)), true).await;
    let result = call(&client, "edit_image", input).await;
    assert_ne!(result.is_error, Some(true));
    assert_eq!(metadata(&result)["request_ref"], "fixture-request");
    assert_no_fees(metadata(&result));
    let local = &metadata(&result)["local_files"][0];
    assert!(local["path"].is_string());
    assert_eq!(
        std::fs::read(local["path"].as_str().unwrap()).unwrap(),
        bytes
    );
    assert!(local["expires_at"].as_u64().unwrap() > 0);
    assert_eq!(local["inline_representation"], "preview");
    assert_eq!(local["image_index"], 0);
    assert!(serde_json::to_vec(&result).unwrap().len() < 8 * 1024 * 1024);
    assert_eq!(fixture.posts(), 1);
    assert_eq!(fixture.state.common.fees.load(Ordering::SeqCst), 0);
    close(client, server).await;
}

#[tokio::test]
#[ignore = "requires actual Go advanced catalog/parser artifact"]
async fn server_generated_advanced_catalog_and_parser_inputs_cross_http_mcp_and_export_wire() {
    let artifact_path = std::env::var_os("TOKEMULA_B3C_GO_ARTIFACT")
        .expect("set TOKEMULA_B3C_GO_ARTIFACT to the real Go synthetic artifact");
    let artifact: Value = serde_json::from_slice(&std::fs::read(artifact_path).unwrap()).unwrap();
    let mut exports = Vec::new();
    for case in artifact["parsing"].as_array().unwrap() {
        let directory = tempfile::tempdir().unwrap();
        let fixture = AdvancedFixture::new(artifact["catalog"].clone()).await;
        let asynchronous = case["name"].as_str().unwrap().ends_with("/async");
        let originals = case["upload_data_base64"]
            .as_array()
            .unwrap()
            .iter()
            .map(|v| STANDARD.decode(v.as_str().unwrap()).unwrap())
            .collect::<Vec<_>>();
        let paths: Vec<_> = originals
            .iter()
            .enumerate()
            .map(|(i, b)| local_file(directory.path(), &format!("image-{i}.png"), b))
            .collect();
        let original_mask = STANDARD
            .decode(case["mask_data_base64"].as_str().unwrap())
            .unwrap();
        let mask = local_file(directory.path(), "mask.png", &original_mask);
        if asynchronous {
            *fixture.state.common.mode.lock().unwrap() =
                Mode::Error(StatusCode::ACCEPTED, accepted_edit());
        }
        let (client, server) = connect(fixture.client(Duration::from_secs(3)), true).await;
        let tools = client.list_tools(None).await.unwrap().tools;
        assert_eq!(tools.len(), 8);
        let edit_schema = serde_json::to_value(
            &tools
                .iter()
                .find(|t| t.name == "edit_image")
                .unwrap()
                .input_schema,
        )
        .unwrap();
        for field in [
            "image_path",
            "image_paths",
            "mask_path",
            "background",
            "output_format",
            "output_compression",
            "input_fidelity",
        ] {
            assert!(
                edit_schema["properties"].get(field).is_some(),
                "missing actual MCP schema field {field}"
            );
        }
        let catalog = call(&client, "list_image_models", json!({})).await;
        assert_ne!(catalog.is_error, Some(true));
        for expected in artifact["catalog"]["models"].as_array().unwrap() {
            let actual = metadata(&catalog)["models"]
                .as_array()
                .unwrap()
                .iter()
                .find(|m| m["id"] == expected["id"])
                .unwrap();
            for cap in expected["capabilities"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|c| c["operation"] == "edit")
            {
                let actual_cap = actual["capabilities"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .find(|c| c["operation"] == cap["operation"] && c["mode"] == cap["mode"])
                    .unwrap();
                assert_eq!(actual_cap["edit_options"], cap["edit_options"]);
                assert_eq!(actual_cap["input_limits"], cap["input_limits"]);
            }
        }
        let mut input = json!({"prompt":"Synthetic ordered edit", "image_paths":paths,"mask_path":mask,
            "background":case["background"], "output_format":case["output_format"],
            "output_compression":case["output_compression"],"input_fidelity":case["input_fidelity"]});
        if asynchronous {
            input["mode"] = json!("single_async");
        }
        let mut incompatible = input.clone();
        incompatible["model"] = json!("gpt-image-fallback");
        let rejected = call(&client, "edit_image", incompatible).await;
        assert_eq!(rejected.is_error, Some(true));
        assert_eq!(
            metadata(&rejected)["selection"]["alternatives"],
            json!(["gpt-image-native"])
        );
        assert_eq!(fixture.posts(), 0);
        let result = call(&client, "edit_image", input).await;
        assert_ne!(result.is_error, Some(true), "{:?}", metadata(&result));
        assert_eq!(metadata(&result)["selected_model"], case["model"]);
        assert_eq!(metadata(&result)["request_ref"], "fixture-request");
        if asynchronous {
            assert_eq!(metadata(&result)["submission"], "accepted");
        } else {
            assert_image(&result);
        }
        let upload = fixture.state.uploads.lock().unwrap()[0].clone();
        assert_eq!(upload.path, case["name"]);
        assert_eq!(
            upload
                .files
                .iter()
                .filter(|f| f.name == "image[]")
                .map(|f| f.bytes.clone())
                .collect::<Vec<_>>(),
            originals
        );
        assert_eq!(
            upload
                .files
                .iter()
                .find(|f| f.name == "mask")
                .unwrap()
                .bytes,
            original_mask
        );
        for field in ["background", "output_format", "input_fidelity"] {
            assert_eq!(upload.fields[field], case[field]);
        }
        assert_eq!(
            upload.fields["output_compression"],
            case["output_compression"].as_u64().unwrap().to_string()
        );
        assert_eq!(fixture.posts(), 1);
        assert_eq!(fixture.state.common.fees.load(Ordering::SeqCst), 0);
        exports.push(
            json!({"endpoint":upload.path,"content_type":upload.content_type,
            "body_base64":STANDARD.encode(upload.body),"expected":case}),
        );
        close(client, server).await;
    }
    if let Some(path) = std::env::var_os("TOKEMULA_B3C_REQUEST_EXPORT") {
        std::fs::write(
            path,
            serde_json::to_vec_pretty(&json!({"requests":exports})).unwrap(),
        )
        .unwrap();
    }
    eprintln!(
        "real Go catalog/parser inputs consumed: sync + async; each 1 POST, total 2; 6 catalog GET, 1 fee GET; 0 task GET"
    );
}
