use super::*;
use rmcp::{ServiceExt, model::ClientConfig};
use serde_json::json;
use std::{fs, path::PathBuf};

fn png(noise: bool) -> Vec<u8> {
    let width = if noise { 2048 } else { 32 };
    let mut image = image::RgbImage::new(width, width);
    let mut seed = 1u64;
    for (index, byte) in image.as_mut().iter_mut().enumerate() {
        seed ^= seed << 13;
        seed ^= seed >> 7;
        seed ^= seed << 17;
        *byte = if noise {
            seed as u8
        } else {
            (index % 251) as u8
        };
    }
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(image)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    encoded.into_inner()
}

fn output(images: Vec<Vec<u8>>, reference: &str) -> Result<crate::http::ImageOutput, CallError> {
    let dimensions: Vec<_> = images
        .iter()
        .map(|bytes| {
            let decoded = image::load_from_memory(bytes).unwrap();
            json!({"width":decoded.width(),"height":decoded.height()})
        })
        .collect();
    Ok(crate::http::ImageOutput {
        images: images
            .into_iter()
            .map(|bytes| (bytes, "image/png"))
            .collect(),
        metadata: json!({"request_ref":reference,"image_dimensions":dimensions,"local_files":[]}),
    })
}

#[test]
fn persistent_result_saves_small_large_and_edit_outputs_in_order_without_overwrite() {
    let directory = tempfile::tempdir().unwrap();
    let desktop = directory.path().join("桌面 空格");
    fs::create_dir(&desktop).unwrap();
    let cache = directory.path().join("cache");
    let server = ImageMcp::with_directories(
        ImageClient::new(None).unwrap(),
        cache.clone(),
        directory.path().join("tasks"),
    )
    .with_persistent_output_for_tests(desktop.join("Tokemula-Images"));
    let small = png(false);
    let large = png(true);
    let result = server.render(
        output(vec![small.clone(), large.clone()], "current-desktop"),
        &RequestId::Number(1),
        true,
    );
    assert!(wire_bytes(&result, &RequestId::Number(1)) <= MAX_WIRE_BYTES);
    assert!(result.meta.is_none());
    assert!(!cache.exists());
    let metadata = result.structured_content.as_ref().unwrap();
    let files = metadata["local_files"].as_array().unwrap();
    assert_eq!(files.len(), 2);
    for (index, bytes) in [small.clone(), large].iter().enumerate() {
        let file = &files[index];
        let path = PathBuf::from(file["path"].as_str().unwrap());
        assert_eq!(fs::read(&path).unwrap(), *bytes);
        assert_eq!(file["image_index"], index);
        assert_eq!(file["persistent"], true);
        assert_eq!(file["retention"], "user_managed");
        assert!(file["expires_at"].is_null());
        assert_eq!(file["mime_type"], "image/png");
        assert_eq!(file["width"], if index == 0 { 32 } else { 2048 });
        assert_eq!(file["height"], file["width"]);
        let text = serde_json::to_string(&result.content).unwrap();
        assert!(text.contains(path.to_str().unwrap()));
    }
    assert_eq!(files[0]["inline_representation"], "original");
    assert_eq!(files[1]["inline_representation"], "preview");
    let original = PathBuf::from(files[0]["path"].as_str().unwrap());
    let edited = server.render(
        output(vec![fs::read(&original).unwrap()], "edited-desktop"),
        &RequestId::Number(2),
        false,
    );
    let edit_path = PathBuf::from(
        edited.structured_content.as_ref().unwrap()["local_files"][0]["path"]
            .as_str()
            .unwrap(),
    );
    assert_ne!(original, edit_path);
    assert_eq!(fs::read(original).unwrap(), small);
    assert_eq!(fs::read(edit_path).unwrap(), small);
}

#[test]
fn failed_desktop_write_keeps_accepted_state_and_original_reference() {
    let directory = tempfile::tempdir().unwrap();
    let blocked = directory.path().join("not-a-directory");
    fs::write(&blocked, b"keep").unwrap();
    let server = ImageMcp::with_storage(ImageClient::new(None).unwrap(), None, None)
        .with_persistent_output_for_tests(blocked);
    let result = server.render(
        output(vec![png(false)], "already-returned"),
        &RequestId::Number(1),
        false,
    );
    assert_eq!(result.is_error, Some(true));
    let metadata = result.structured_content.unwrap();
    assert_eq!(metadata["code"], "image_storage_failed");
    assert_eq!(metadata["submission"], "accepted");
    assert_eq!(metadata["request_ref"], "already-returned");
    assert_eq!(metadata["local_files"], json!([]));
}

#[tokio::test]
async fn persistent_delivery_has_no_ui_tool_binding_even_when_client_supports_apps() {
    let directory = tempfile::tempdir().unwrap();
    let server = ImageMcp::with_storage(ImageClient::new(None).unwrap(), None, None)
        .with_persistent_output_for_tests(directory.path().join("Tokemula-Images"));
    let config = ClientConfig::new(
        serde_json::from_value(
            json!({"extensions":{"io.modelcontextprotocol/ui":{"mimeTypes":[image_view::MIME]}}}),
        )
        .unwrap(),
        rmcp::model::Implementation::new("persistent-fixture", "1"),
    )
    .with_protocol_version(ProtocolVersion::V_2025_11_25);
    let (client_io, server_io) = tokio::io::duplex(64 * 1024);
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
    assert!(tools.iter().all(|t| t.meta.is_none()));
    assert!(!directory.path().join("Tokemula-Images").exists());
    client.cancel().await.unwrap();
    task.await.unwrap();
}

#[test]
#[ignore = "Explicit exporter: writes only to TOKEMULA_WB3_EXPORT_DIR supplied by the authorized caller"]
fn export_desktop_fixture_through_real_delivery() {
    let destination = PathBuf::from(
        std::env::var_os("TOKEMULA_WB3_EXPORT_DIR").expect("explicit output directory"),
    );
    let receipt = PathBuf::from(
        std::env::var_os("TOKEMULA_WB3_EXPORT_RESULT").expect("explicit result file"),
    );
    assert!(destination.is_absolute() && receipt.is_absolute());
    let server = ImageMcp::with_storage(ImageClient::new(None).unwrap(), None, None)
        .with_persistent_output_for_tests(destination);
    server.preflight_output().unwrap();
    let mut picture = image::RgbImage::new(1024, 1024);
    for (x, y, pixel) in picture.enumerate_pixels_mut() {
        *pixel = image::Rgb(if x < 512 && y < 512 {
            [111, 87, 171]
        } else if x >= 512 && y < 512 {
            [236, 171, 99]
        } else if x < 512 {
            [95, 156, 169]
        } else {
            [226, 225, 198]
        });
    }
    let mut encoded = Cursor::new(Vec::new());
    image::DynamicImage::ImageRgb8(picture)
        .write_to(&mut encoded, ImageFormat::Png)
        .unwrap();
    let result = server.render(
        output(vec![encoded.into_inner()], "wb3-synthetic-desktop-fixture"),
        &RequestId::Number(1),
        false,
    );
    assert_ne!(result.is_error, Some(true));
    fs::write(receipt, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
}
