//! Explicit synthetic D4 settings fixture. All credentials and state use an owned namespace/root.
use axum::{
    Json, Router,
    extract::State,
    http::{HeaderMap, Method, StatusCode},
    response::IntoResponse,
    routing::get,
};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    },
};
use tokemula_image_connector::{
    distribution::DataPaths,
    management,
    settings::{ModelPreferences, SettingsService},
};
const KEY: &str = "D4_SYNTHETIC_SETTINGS_KEY";
#[derive(Serialize, Deserialize)]
struct Context {
    namespace: String,
    base: String,
}
struct Api {
    catalog: Value,
    gets: AtomicUsize,
    posts: AtomicUsize,
}
#[tokio::main]
async fn main() {
    assert!(std::env::var_os("TOKEMULA_IMAGE_KEY").is_none());
    let root =
        PathBuf::from(std::env::var_os("D4_SETTINGS_ROOT").expect("explicit D4_SETTINGS_ROOT"));
    assert!(root.is_absolute());
    std::fs::create_dir_all(&root).unwrap();
    let mut args = std::env::args_os().skip(1);
    let action = args
        .next()
        .expect("serve / seed / cleanup / management arguments");
    if action == "serve" {
        let mut catalog: Value =
            serde_json::from_str(include_str!("../tests/fixtures/http-catalog.json")).unwrap();
        let mut model = catalog["models"][0].clone();
        model["capabilities"][0]["parameters"]["sizes"] =
            json!(["auto", "1024x1024", "1536x1024", "1024x1536"]);
        model["capabilities"][0]["parameters"]["qualities"] =
            json!(["auto", "low", "medium", "high", "xhigh", "max"]);
        model["capabilities"][0]["parameters"]["complexities"] = json!(["low", "medium", "high"]);
        model["capabilities"][0]["parameters"]["size_rules"] = json!({"multiple_of":16,"min_edge":16,"max_edge":3840,"min_pixels":262144,"max_pixels":8294400,"max_aspect_ratio":3});
        let mut capabilities = Vec::new();
        for operation in ["generation", "edit"] {
            for mode in ["sync", "single_async"] {
                let mut cap = model["capabilities"][0].clone();
                cap["operation"] = json!(operation);
                cap["mode"] = json!(mode);
                if operation == "edit" {
                    cap["input_limits"]["max_images"] = json!(1);
                    cap["input_limits"]["max_image_bytes"] = json!(20971520);
                    cap["input_limits"]["mime_types"] =
                        json!(["image/png", "image/jpeg", "image/webp"]);
                }
                capabilities.push(cap);
            }
        }
        let mut batch = capabilities[0].clone();
        batch["mode"] = json!("batch");
        batch["parameters"] = json!({"max_images":1,"sizes":["1K","2K"],"qualities":[]});
        capabilities.push(batch);
        model["capabilities"] = json!(capabilities);
        model["id"] = json!("image-studio");
        model["display_name"] = json!("图像工作室");
        model.as_object_mut().unwrap().remove("family");
        let mut other = model.clone();
        other["id"] = json!("image-fast");
        other["display_name"] = json!("快速图片");
        for capability in other["capabilities"].as_array_mut().unwrap() {
            capability["parameters"]
                .as_object_mut()
                .unwrap()
                .remove("size_rules");
            capability["parameters"]
                .as_object_mut()
                .unwrap()
                .remove("complexities");
            if capability["mode"] != "batch" {
                capability["parameters"]["sizes"] = json!(["1024x1024"]);
                capability["parameters"]["qualities"] = json!(["low"]);
            }
        }
        // The fast model intentionally lacks edit/async modes for model-change acceptance.
        other["capabilities"] = json!([other["capabilities"][0].clone()]);
        catalog["models"] = json!([model, other]);
        let state = Arc::new(Api {
            catalog,
            gets: AtomicUsize::new(0),
            posts: AtomicUsize::new(0),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let context = Context {
            namespace: format!("tokemula-image-test-d4-settings-{}", std::process::id()),
            base: format!("http://{}/v1/", listener.local_addr().unwrap()),
        };
        std::fs::write(
            root.join("context.json"),
            serde_json::to_vec(&context).unwrap(),
        )
        .unwrap();
        let app = Router::new()
            .route(
                "/v1/images/models",
                get(|State(s): State<Arc<Api>>, headers: HeaderMap| async move {
                    s.gets.fetch_add(1, Ordering::SeqCst);
                    if headers.get("authorization").and_then(|h| h.to_str().ok())
                        != Some(concat!("Bearer ", "D4_SYNTHETIC_SETTINGS_KEY"))
                    {
                        return (
                            StatusCode::UNAUTHORIZED,
                            Json(json!({"error":{"type":"authentication_error"}})),
                        )
                            .into_response();
                    }
                    Json(s.catalog.clone()).into_response()
                }),
            )
            .fallback(|State(s): State<Arc<Api>>, method: Method| async move {
                if method == Method::POST {
                    s.posts.fetch_add(1, Ordering::SeqCst);
                }
                StatusCode::NOT_FOUND
            })
            .with_state(state);
        println!("D4 synthetic settings upstream ready; no image endpoint is enabled.");
        axum::serve(listener, app).await.unwrap();
        return;
    }
    let context: Context =
        serde_json::from_slice(&std::fs::read(root.join("context.json")).unwrap()).unwrap();
    assert!(
        context
            .namespace
            .starts_with("tokemula-image-test-d4-settings-")
    );
    let paths = if let Some(install_root) =
        std::env::var_os("D4_SETTINGS_INSTALL_ROOT").map(PathBuf::from)
    {
        assert!(
            install_root.starts_with(&root),
            "owned install root required"
        );
        let record: Value =
            serde_json::from_slice(&std::fs::read(install_root.join("installation.json")).unwrap())
                .unwrap();
        let paths: DataPaths = serde_json::from_value(record["data_paths"].clone()).unwrap();
        assert!(paths.tasks.starts_with(&root) && paths.cache.starts_with(&root));
        paths
    } else {
        DataPaths {
            tasks: root.join("state/tasks"),
            cache: root.join("cache"),
        }
    };
    let service = SettingsService::for_isolated_tests(
        root.join("settings"),
        &context.namespace,
        &context.base,
    )
    .unwrap()
    .with_data_paths(&paths);
    if action == "seed" {
        let validated = service.validate(KEY.into()).await.unwrap();
        service
            .save(
                &validated,
                ModelPreferences {
                    default_model: "image-studio".into(),
                    fallback_models: vec!["image-fast".into()],
                    auto_fallback: true,
                },
            )
            .unwrap();
        println!("D4 synthetic credential seeded; no key value is displayed.");
        return;
    }
    if action == "cleanup" {
        service.purge().unwrap();
        println!("D4 synthetic credential and known settings removed.");
        return;
    }
    let code = management::run_with_service(
        std::iter::once("tokemula-image".into())
            .chain(std::iter::once(action))
            .chain(args),
        service,
    )
    .await;
    std::process::exit(code);
}
