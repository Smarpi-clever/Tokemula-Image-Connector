//! Minimal private recovery references, retained independently of image-cache expiry.

use crate::{
    http::{CallError, Submission, valid_request_ref},
    output::prepare_private_directory,
};
use serde::{Deserialize, Serialize};
use std::{fs, path::PathBuf};

#[derive(Debug, Clone)]
pub(crate) struct TaskStore {
    pub(crate) root: PathBuf,
}

#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub(crate) enum TaskStatus {
    Processing,
    Completed,
    Failed,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct TaskRecord {
    pub(crate) task_id: String,
    pub(crate) request_ref: Option<String>,
    pub(crate) selected_model: Option<String>,
    pub(crate) status: TaskStatus,
    pub(crate) created_at: u64,
    pub(crate) completed_at: Option<u64>,
    pub(crate) expires_at: u64,
    pub(crate) result_expires_at: Option<u64>,
    pub(crate) observed_at: u64,
}

fn storage_error() -> CallError {
    CallError::new(
        "task_storage_failed",
        "任务恢复记录读写失败；请保存 task_id 和 request_ref。",
        Submission::Accepted,
    )
}

impl TaskStore {
    pub(crate) fn for_current_user() -> Option<Self> {
        #[cfg(windows)]
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from)?;
        #[cfg(not(windows))]
        let base = std::env::var_os("HOME")
            .map(PathBuf::from)?
            .join(".local/state");
        base.is_absolute().then(|| Self {
            root: base.join("tokemula-image-connector").join("tasks"),
        })
    }

    /// Destructive cleanup requires the exact canonical current file, not a same-stem sibling.
    pub(crate) fn owns_file(&self, path: &std::path::Path) -> bool {
        if path.parent() != Some(self.root.as_path())
            || path.extension() != Some(std::ffi::OsStr::new("json"))
        {
            return false;
        }
        let Some(id) = path
            .file_stem()
            .and_then(|s| s.to_str())
            .filter(|id| valid_request_ref(id))
        else {
            return false;
        };
        if self.root.join(format!("{id}.json")) != path {
            return false;
        }
        self.load(id).is_ok_and(|record| record.is_some())
    }
    pub(crate) fn save(&self, record: &TaskRecord) -> Result<(), CallError> {
        prepare_private_directory(&self.root).map_err(|_| storage_error())?;
        let mut file = tempfile::NamedTempFile::new_in(&self.root).map_err(|_| storage_error())?;
        serde_json::to_writer(&mut file, record).map_err(|_| storage_error())?;
        file.as_file().sync_all().map_err(|_| storage_error())?;
        file.persist(self.root.join(format!("{}.json", record.task_id)))
            .map_err(|_| storage_error())?;
        Ok(())
    }

    pub(crate) fn load(&self, task_id: &str) -> Result<Option<TaskRecord>, CallError> {
        let root = match fs::symlink_metadata(&self.root) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(storage_error()),
        };
        if !root.is_dir() || root.file_type().is_symlink() {
            return Err(storage_error());
        }
        let path = self.root.join(format!("{task_id}.json"));
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(storage_error()),
        };
        if !metadata.is_file() || metadata.file_type().is_symlink() || metadata.len() > 16 * 1024 {
            return Err(storage_error());
        }
        let record: TaskRecord =
            serde_json::from_slice(&fs::read(path).map_err(|_| storage_error())?)
                .map_err(|_| storage_error())?;
        if record.task_id != task_id
            || record
                .request_ref
                .as_deref()
                .is_some_and(|value| !valid_request_ref(value))
        {
            return Err(storage_error());
        }
        Ok(Some(record))
    }
}
