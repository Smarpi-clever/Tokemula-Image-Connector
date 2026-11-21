//! Only oversized MCP results use this private, short-lived original-image cache.

use crate::http::{CallError, Submission};
use serde::Serialize;
use std::{
    fs,
    io::Write,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub(crate) const RETENTION: Duration = Duration::from_secs(24 * 60 * 60);

#[derive(Debug, Clone)]
pub(crate) struct OutputCache {
    pub(crate) root: PathBuf,
}

#[derive(Debug, Serialize)]
pub(crate) struct SavedImage {
    pub(crate) path: PathBuf,
    pub(crate) expires_at: u64,
    pub(crate) mime_type: &'static str,
}

fn storage_error() -> CallError {
    CallError::new(
        "image_storage_failed",
        "无法保存完整原图；生成请求不会重发。",
        Submission::Unknown,
    )
}

fn seconds(now: SystemTime) -> Result<u64, CallError> {
    Ok(now
        .duration_since(UNIX_EPOCH)
        .map_err(|_| storage_error())?
        .as_secs())
}

pub(crate) fn prepare_private_directory(root: &Path) -> std::io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(root)?;
    let metadata = fs::symlink_metadata(root)?;
    if !metadata.is_dir() || metadata.file_type().is_symlink() {
        return Err(std::io::Error::other("private directory required"));
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(root, fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}

impl OutputCache {
    pub(crate) fn for_current_user() -> Option<Self> {
        #[cfg(windows)]
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
        #[cfg(not(windows))]
        let base = std::env::var_os("HOME").map(PathBuf::from)?.join(".cache");
        base.is_absolute().then(|| Self {
            root: base.join("tokemula-image-connector").join("images"),
        })
    }

    pub(crate) fn save(
        &self,
        bytes: &[u8],
        mime: &'static str,
        now: SystemTime,
    ) -> Result<SavedImage, CallError> {
        prepare_private_directory(&self.root).map_err(|_| storage_error())?;
        let expires_at = seconds(now)? + RETENTION.as_secs();
        let extension = match mime {
            "image/png" => "png",
            "image/jpeg" => "jpg",
            "image/webp" => "webp",
            _ => return Err(storage_error()),
        };
        let mut file = tempfile::Builder::new()
            .prefix(&format!("tic-image-{expires_at}-"))
            .rand_bytes(12)
            .suffix(&format!(".{extension}"))
            .tempfile_in(&self.root)
            .map_err(|_| storage_error())?;
        file.write_all(bytes).map_err(|_| storage_error())?;
        let (file, path) = file.keep().map_err(|_| storage_error())?;
        drop(file);
        Ok(SavedImage {
            path,
            expires_at,
            mime_type: mime,
        })
    }

    /// Only this cache's ordinary files with the exact generated name are eligible.
    pub(crate) fn cleanup(&self, now: SystemTime) -> Result<(), CallError> {
        let metadata = match fs::symlink_metadata(&self.root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(()),
            Err(_) => return Err(storage_error()),
        };
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(storage_error());
        }
        let now = seconds(now)?;
        for entry in fs::read_dir(&self.root).map_err(|_| storage_error())? {
            let entry = entry.map_err(|_| storage_error())?;
            if !entry.file_type().map_err(|_| storage_error())?.is_file() {
                continue;
            }
            if managed_expiry(&entry.path()).is_some_and(|expires| expires <= now) {
                fs::remove_file(entry.path()).map_err(|_| storage_error())?;
            }
        }
        Ok(())
    }
}

pub(crate) fn managed_expiry(path: &Path) -> Option<u64> {
    let name = path.file_name()?.to_str()?.strip_prefix("tic-image-")?;
    let (expires, suffix) = name.split_once('-')?;
    let (random, extension) = suffix.rsplit_once('.')?;
    if random.len() != 12
        || !random.bytes().all(|c| c.is_ascii_alphanumeric())
        || !matches!(extension, "png" | "jpg" | "webp")
    {
        return None;
    }
    expires.parse().ok()
}

#[cfg(test)]
#[path = "output_tests.rs"]
mod tests;
