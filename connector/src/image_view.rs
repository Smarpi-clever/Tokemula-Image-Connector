//! MCP Apps image resources. Originals stay in memory until the view reads them.

use base64::{Engine, engine::general_purpose::STANDARD};
use rmcp::{
    ErrorData,
    model::{ClientCapabilities, ReadResourceResult, Resource, ResourceContents},
};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};

pub(crate) const URI: &str = "ui://tokemula/image-viewer";
pub(crate) const MIME: &str = "text/html;profile=mcp-app";
pub(crate) const CHUNK_BYTES: usize = 3 * 1024 * 1024;
const RETENTION: Duration = Duration::from_secs(10 * 60);
const IMAGE_PREFIX: &str = "tokemula-image://result/";

pub(crate) fn supported(capabilities: Option<ClientCapabilities>) -> bool {
    capabilities
        .and_then(|c| c.extensions)
        .and_then(|extensions| extensions.get("io.modelcontextprotocol/ui").cloned())
        .and_then(|ui| ui["mimeTypes"].as_array().cloned())
        .is_some_and(|types| types.iter().any(|mime| mime == MIME))
}

pub(crate) fn resource() -> Resource {
    Resource::new(URI, "Tokemula 图片")
        .with_mime_type(MIME)
        .with_description("直接展示本次图片结果；保存由用户发起。")
}

struct Original {
    bytes: Vec<u8>,
    created: Instant,
}

#[derive(Clone, Default)]
pub(crate) struct ImageView {
    originals: Arc<Mutex<HashMap<String, Original>>>,
}

impl ImageView {
    pub(crate) fn retain(
        &self,
        bytes: Vec<u8>,
        mime: &'static str,
        index: usize,
    ) -> Result<Value, ErrorData> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random)
            .map_err(|_| ErrorData::internal_error("无法建立本次图片的内存资源。", None))?;
        let token: String = random.iter().map(|byte| format!("{byte:02x}")).collect();
        let descriptor = json!({
            "image_index": index,
            "resource_uri": format!("{IMAGE_PREFIX}{token}"),
            "mime_type": mime,
            "byte_length": bytes.len(),
            "chunks": bytes.len().div_ceil(CHUNK_BYTES),
            "expires_in_seconds": RETENTION.as_secs()
        });
        let mut originals = self.originals.lock().expect("image view lock");
        originals.retain(|_, original| original.created.elapsed() < RETENTION);
        originals.insert(
            token,
            Original {
                bytes,
                created: Instant::now(),
            },
        );
        Ok(descriptor)
    }

    pub(crate) fn read(&self, uri: &str) -> Result<ReadResourceResult, ErrorData> {
        if uri == URI {
            return Ok(ReadResourceResult::new(vec![
                ResourceContents::text(include_str!("image_view.html"), URI).with_mime_type(MIME),
            ]));
        }
        let missing = || ErrorData::resource_not_found("本次图片内存资源已过期或不存在。", None);
        let (token, chunk) = uri
            .strip_prefix(IMAGE_PREFIX)
            .and_then(|suffix| suffix.rsplit_once('/'))
            .ok_or_else(missing)?;
        let chunk: usize = chunk.parse().map_err(|_| missing())?;
        let mut originals = self.originals.lock().expect("image view lock");
        originals.retain(|_, original| original.created.elapsed() < RETENTION);
        let original = originals.get(token).ok_or_else(missing)?;
        let start = chunk
            .checked_mul(CHUNK_BYTES)
            .filter(|start| *start < original.bytes.len())
            .ok_or_else(missing)?;
        let end = (start + CHUNK_BYTES).min(original.bytes.len());
        Ok(ReadResourceResult::new(vec![
            ResourceContents::blob(STANDARD.encode(&original.bytes[start..end]), uri)
                .with_mime_type("application/octet-stream"),
        ]))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expired_resources_stop_without_disk_or_upstream_fallback() {
        let view = ImageView::default();
        let descriptor = view.retain(vec![1, 2, 3], "image/png", 0).unwrap();
        let uri = descriptor["resource_uri"].as_str().unwrap();
        assert!(view.read(&format!("{uri}/0")).is_ok());
        view.originals
            .lock()
            .unwrap()
            .values_mut()
            .next()
            .unwrap()
            .created = Instant::now() - RETENTION;
        assert!(view.read(&format!("{uri}/0")).is_err());
        assert!(view.originals.lock().unwrap().is_empty());
    }

    #[test]
    fn only_the_declared_ui_mime_type_enables_the_view() {
        assert!(!supported(None));
        assert!(!supported(Some(
            serde_json::from_value(json!({
                "experimental":{"io.modelcontextprotocol/ui":{"mimeTypes":[MIME]}}
            }))
            .unwrap()
        )));
        assert!(!supported(Some(
            serde_json::from_value(json!({
                "extensions":{"io.modelcontextprotocol/ui":{"mimeTypes":["text/html"]}}
            }))
            .unwrap()
        )));
        assert!(supported(Some(
            serde_json::from_value(json!({
                "extensions":{"io.modelcontextprotocol/ui":{"mimeTypes":[MIME]}}
            }))
            .unwrap()
        )));
    }
}
