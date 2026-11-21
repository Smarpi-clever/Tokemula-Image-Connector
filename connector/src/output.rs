//! User-owned desktop originals and the separate short-lived compatibility cache.

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

#[derive(Debug, Clone)]
pub(crate) struct PersistentOutput {
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

fn save_unique(root: &Path, prefix: &str, bytes: &[u8], mime: &str) -> Result<PathBuf, CallError> {
    let extension = match mime {
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/webp" => "webp",
        _ => return Err(storage_error()),
    };
    let mut file = tempfile::Builder::new()
        .prefix(prefix)
        .rand_bytes(12)
        .suffix(&format!(".{extension}"))
        .tempfile_in(root)
        .map_err(|_| storage_error())?;
    file.write_all(bytes).map_err(|_| storage_error())?;
    let (file, path) = file.keep().map_err(|_| storage_error())?;
    drop(file);
    Ok(path)
}

impl PersistentOutput {
    #[cfg(target_os = "macos")]
    pub(crate) fn for_current_user() -> Result<Self, CallError> {
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .ok_or_else(Self::unavailable)?;
        Ok(Self {
            root: home.join("Desktop").join("Tokemula-Images"),
        })
    }

    fn unavailable() -> CallError {
        CallError::new(
            "image_storage_unavailable",
            "桌面 Tokemula-Images 目录不可写；请允许当前客户端访问桌面后再提交。",
            Submission::NotSubmitted,
        )
    }

    fn prepare(&self) -> std::io::Result<PathBuf> {
        // Resolving Desktop also follows the user's symlink/iCloud location without changing it.
        let parent = fs::canonicalize(
            self.root
                .parent()
                .ok_or_else(|| std::io::Error::other("desktop parent required"))?,
        )?;
        let root = parent.join(
            self.root
                .file_name()
                .ok_or_else(|| std::io::Error::other("image output directory required"))?,
        );
        let mut builder = fs::DirBuilder::new();
        #[cfg(unix)]
        {
            use std::os::unix::fs::DirBuilderExt;
            builder.mode(0o700);
        }
        match builder.create(&root) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
        let metadata = fs::symlink_metadata(&root)?;
        if !metadata.is_dir() || metadata.file_type().is_symlink() {
            return Err(std::io::Error::other("image output directory required"));
        }
        Ok(root)
    }

    pub(crate) fn preflight(&self) -> Result<(), CallError> {
        let root = self.prepare().map_err(|_| Self::unavailable())?;
        let mut probe = tempfile::Builder::new()
            .prefix(".tokemula-write-check-")
            .tempfile_in(root)
            .map_err(|_| Self::unavailable())?;
        probe.write_all(b"\0").map_err(|_| Self::unavailable())?;
        Ok(())
    }

    pub(crate) fn save(&self, bytes: &[u8], mime: &'static str) -> Result<PathBuf, CallError> {
        let root = self.prepare().map_err(|_| storage_error())?;
        save_unique(&root, "tokemula-", bytes, mime)
    }
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
        let path = save_unique(&self.root, &format!("tic-image-{expires_at}-"), bytes, mime)?;
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
