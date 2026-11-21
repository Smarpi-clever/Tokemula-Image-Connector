use super::*;
use rmcp::{ServiceExt, model::ClientConfig};
use serde_json::json;

fn png(noise: bool) -> Vec<u8> {
    let mut image =
        image::RgbImage::new(if noise { 2048 } else { 16 }, if noise { 2048 } else { 16 });
    let mut seed = 1u64;
    if noise {
        for byte in image.as_mut() {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            *byte = seed as u8;
        }
    }
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    encoded.into_inner()
}

fn output(images: Vec<Vec<u8>>, reference: &str) -> Result<crate::http::ImageOutput, CallError> {
    Ok(crate::http::ImageOutput {
        images: images
            .into_iter()
            .map(|bytes| (bytes, "image/png"))
            .collect(),
        metadata: json!({"request_ref":reference,"local_files":[]}),
    })
}

#[test]
fn inline_view_keeps_original_bytes_and_never_creates_image_files() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("images");
    let server = ImageMcp::with_directories(
        ImageClient::new(None).unwrap(),
        cache.clone(),
        directory.path().join("tasks"),
    );
    let bytes = png(false);
    let result = server.render(
        output(vec![bytes.clone()], "current-small"),
        &RequestId::Number(1),
        true,
    );
    let result = serde_json::to_value(result).unwrap();
    assert_eq!(
        STANDARD
            .decode(result["content"][0]["data"].as_str().unwrap())
            .unwrap(),
        bytes
    );
    assert!(result["_meta"].is_null());
    assert_eq!(result["structuredContent"]["request_ref"], "current-small");
    assert_eq!(result["structuredContent"]["local_files"], json!([]));
    assert!(!cache.exists());
    let legacy = server.render(output(vec![bytes], "legacy"), &RequestId::Number(2), false);
    assert!(legacy.meta.is_none());
    assert!(!cache.exists());
}

#[test]
fn large_view_chunks_preserve_original_and_correspond_to_current_image() {
    let directory = tempfile::tempdir().unwrap();
    let cache = directory.path().join("images");
    let server = ImageMcp::with_directories(
        ImageClient::new(None).unwrap(),
        cache.clone(),
        directory.path().join("tasks"),
    );
    let bytes = png(true);
    assert!(STANDARD.encode(&bytes).len() > MAX_WIRE_BYTES);
    let small = png(false);
    let result = server.render(
        output(vec![small.clone(), bytes.clone()], "current-large"),
        &RequestId::Number(2),
        true,
    );
    assert!(wire_bytes(&result, &RequestId::Number(2)) <= MAX_WIRE_BYTES);
    let result_json = serde_json::to_value(&result).unwrap();
    assert_eq!(
        STANDARD
            .decode(result_json["content"][0]["data"].as_str().unwrap())
            .unwrap(),
        small
    );
    let descriptors = result_json["_meta"]["tokemula/inlineImages"]
        .as_array()
        .unwrap();
    assert_eq!(descriptors.len(), 1);
    let descriptor = &descriptors[0];
    assert_eq!(descriptor["image_index"], 1);
    assert_eq!(descriptor["mime_type"], "image/png");
    let resource = descriptor["resource_uri"].as_str().unwrap();
    let mut reassembled = Vec::new();
    for chunk in 0..descriptor["chunks"].as_u64().unwrap() {
        let read = server
            .image_view
            .read(&format!("{resource}/{chunk}"))
            .unwrap();
        let wire = ServerJsonRpcMessage::response(
            ServerResult::ReadResourceResult(read.clone()),
            RequestId::Number(10),
        );
        assert!(serde_json::to_vec(&wire).unwrap().len() < MAX_WIRE_BYTES);
        let read = serde_json::to_value(read).unwrap();
        reassembled.extend(
            STANDARD
                .decode(read["contents"][0]["blob"].as_str().unwrap())
                .unwrap(),
        );
    }
    assert_eq!(reassembled, bytes);
    assert_eq!(image::load_from_memory(&reassembled).unwrap().width(), 2048);
    assert_eq!(
        result_json["structuredContent"]["request_ref"],
        "current-large"
    );
    assert_eq!(result_json["structuredContent"]["local_files"], json!([]));
    assert!(!cache.exists());
    let next = server.render(
        output(vec![png(false)], "next-result"),
        &RequestId::Number(3),
        true,
    );
    assert!(next.meta.is_none());
    assert_eq!(
        next.structured_content.as_ref().unwrap()["request_ref"],
        "next-result"
    );
    assert!(
        server
            .image_view
            .read(&format!("{resource}/9999999999999999999"))
            .is_err()
    );
    assert!(
        server
            .image_view
            .read("tokemula-image://result/unrelated/0")
            .is_err()
    );
}

#[tokio::test]
async fn only_negotiated_clients_get_ui_metadata_and_all_eight_tools_remain() {
    for ui in [false, true] {
        let capabilities = if ui {
            serde_json::from_value(json!({"extensions":{"io.modelcontextprotocol/ui":{"mimeTypes":[image_view::MIME]}}})).unwrap()
        } else {
            Default::default()
        };
        let config = ClientConfig::new(
            capabilities,
            rmcp::model::Implementation::new("synthetic-ui-client", "1"),
        )
        .with_protocol_version(ProtocolVersion::V_2025_11_25);
        let (client_io, server_io) = tokio::io::duplex(64 * 1024);
        let server = ImageMcp::with_storage(ImageClient::new(None).unwrap(), None, None);
        let task = tokio::spawn(async move {
            server
                .serve(server_io)
                .await
                .unwrap()
                .waiting()
                .await
                .unwrap();
        });
        let client = config.serve(client_io).await.unwrap();
        let tools = client.list_all_tools().await.unwrap();
        assert_eq!(tools.len(), 8);
        for tool in tools {
            let expected = ui
                && matches!(
                    tool.name.as_ref(),
                    "generate_image" | "edit_image" | "get_image_task" | "get_image_batch"
                );
            assert_eq!(tool.meta.is_some(), expected, "{}", tool.name);
            if expected {
                assert_eq!(
                    serde_json::to_value(tool.meta).unwrap()["ui"]["resourceUri"],
                    image_view::URI
                );
            }
        }
        let resources = client.list_all_resources().await.unwrap();
        assert_eq!(resources.len(), 1);
        assert_eq!(resources[0].uri, image_view::URI);
        let resource = client
            .read_resource(rmcp::model::ReadResourceRequestParams::new(image_view::URI))
            .await
            .unwrap();
        let resource = serde_json::to_value(resource).unwrap();
        assert_eq!(resource["contents"][0]["mimeType"], image_view::MIME);
        assert!(
            resource["contents"][0]["text"]
                .as_str()
                .unwrap()
                .contains("ui/notifications/tool-result")
        );
        client.cancel().await.unwrap();
        task.await.unwrap();
    }
}
