//! Versioned user installation: a single current pointer activates both programs.
use crate::{
    output::prepare_private_directory,
    release_package::{self, ReleaseManifest},
    settings::SettingsService,
};
use fs2::FileExt;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::{Duration, SystemTime, UNIX_EPOCH},
};

pub const PRODUCT: &str = "tokemula-image-connector";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const TARGET: &str = env!("TOKEMULA_BUILD_TARGET");
const STATE: &str = "installation.json";
const JOURNAL: &str = "transaction.json";
const PROGRAMS: [&str; 2] = ["tokemula-image", "tokemula-image-mcp"];

#[derive(Debug, Clone, Serialize)]
pub struct DistributionError {
    pub code: &'static str,
    pub message: &'static str,
}
impl std::fmt::Display for DistributionError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for DistributionError {}
type Result<T> = std::result::Result<T, DistributionError>;
fn error(code: &'static str, message: &'static str) -> DistributionError {
    DistributionError { code, message }
}
fn io_error(_: std::io::Error) -> DistributionError {
    error(
        "installation_io_error",
        "安装文件操作未完成；原版本与恢复入口保留。",
    )
}
fn pkg_error(e: release_package::ReleaseError) -> DistributionError {
    error(e.code, e.message)
}
fn conflict() -> DistributionError {
    error(
        "installation_conflict",
        "安装路径含非本产品或被修改的内容，已保留原文件。",
    )
}
fn now() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Installation {
    schema_version: u32,
    product: String,
    current: String,
    previous: Option<String>,
    update_url: Option<String>,
    public_key: String,
    adapter_state: PathBuf,
    data_paths: DataPaths,
    versions: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DataPaths {
    pub tasks: PathBuf,
    pub cache: PathBuf,
}
impl DataPaths {
    /// Non-secret image defaults live alongside the managed task store.
    pub fn image_preferences_path(&self) -> PathBuf {
        self.tasks
            .parent()
            .unwrap_or(&self.tasks)
            .join("image-preferences.json")
    }
    pub(crate) fn for_current_process() -> Result<Self> {
        if let Some(root) = installed_root() {
            let installation: Installation = read_json(&root.join(STATE))?;
            validate_installation(&installation)?;
            return Ok(installation.data_paths);
        }
        Self::current()
    }
    fn current() -> Result<Self> {
        #[cfg(windows)]
        {
            let base = std::env::var_os("LOCALAPPDATA")
                .map(PathBuf::from)
                .ok_or_else(conflict)?
                .join(PRODUCT);
            Ok(Self {
                tasks: base.join("tasks"),
                cache: base.join("images"),
            })
        }
        #[cfg(not(windows))]
        {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .ok_or_else(conflict)?;
            Ok(Self {
                tasks: home.join(".local/state").join(PRODUCT).join("tasks"),
                cache: home.join(".cache").join(PRODUCT).join("images"),
            })
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Transaction {
    from: Option<Installation>,
    to: Installation,
    phase: String,
    #[serde(default)]
    staging: Option<PathBuf>,
    #[serde(default)]
    files: Vec<release_package::ReleaseFile>,
}

type PhaseHook = Arc<dyn Fn(&str) + Send + Sync>;
pub struct Installer {
    root: PathBuf,
    adapter_state: PathBuf,
    data_paths: DataPaths,
    hook: Option<PhaseHook>,
}
impl Installer {
    pub fn new(root: PathBuf, adapter_state: PathBuf) -> Result<Self> {
        if !root.is_absolute() || !adapter_state.is_absolute() {
            return Err(error(
                "invalid_installation_path",
                "安装与归属目录必须是绝对路径。",
            ));
        }
        Ok(Self {
            root,
            adapter_state,
            data_paths: DataPaths::current()?,
            hook: None,
        })
    }
    #[doc(hidden)]
    pub fn with_test_hook(mut self, hook: PhaseHook) -> Self {
        self.hook = Some(hook);
        self
    }
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn with_data_paths(mut self, paths: DataPaths) -> Result<Self> {
        if !paths.tasks.is_absolute() || !paths.cache.is_absolute() {
            return Err(conflict());
        }
        self.data_paths = paths;
        Ok(self)
    }
    fn phase(&self, p: &str) {
        if let Some(h) = &self.hook {
            h(p)
        }
    }
    fn state(&self) -> Result<Installation> {
        let s: Installation = read_json(&self.root.join(STATE))?;
        validate_installation(&s)?;
        Ok(s)
    }
    fn lock(&self) -> Result<fs::File> {
        prepare_private_directory(&self.root).map_err(io_error)?;
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(self.root.join("operation.lock"))
            .map_err(io_error)?;
        f.try_lock_exclusive().map_err(|_| {
            error(
                "installation_busy",
                "本产品另一安装操作尚未结束，请稍后重试。",
            )
        })?;
        Ok(f)
    }
    pub fn status(&self) -> Result<Value> {
        if !self.root.join(STATE).exists() {
            return Ok(json!({"status":"not_installed","install_root":self.root}));
        }
        let s = self.state()?;
        Ok(
            json!({"status":if self.root.join(JOURNAL).exists(){"recovery_required"}else{"installed"},"version":s.current,"target":TARGET,"previous_version":s.previous,"management_program":self.root.join("bin/tokemula-image"),"mcp_program":self.root.join("bin/tokemula-image-mcp"),"recovery_command":format!("{} recover --install-root {}",shell_quote(&self.root.join("recover")),shell_quote(&self.root)),"update_source":if s.update_url.is_some(){"configured"}else{"not_configured"}}),
        )
    }
    pub async fn setup(
        &self,
        source: &Path,
        trust: &Path,
        update_url: Option<String>,
    ) -> Result<Value> {
        supported()?;
        if !self.root.join(STATE).exists() {
            for name in [
                "current",
                "recover",
                "bin/tokemula-image",
                "bin/tokemula-image-mcp",
            ] {
                if fs::symlink_metadata(self.root.join(name)).is_ok() {
                    return Err(conflict());
                }
            }
        }
        if let Some(url) = &update_url {
            validate_update_url(url)?;
        }
        let public_key = fs::read_to_string(trust).map_err(io_error)?;
        let pinned_trust = tempfile::NamedTempFile::new().map_err(io_error)?;
        fs::write(pinned_trust.path(), &public_key).map_err(io_error)?;
        let trust = pinned_trust.path();
        let manifest = release_package::verify_directory(source, trust).map_err(pkg_error)?;
        let _lock = self.lock()?;
        if self.root.join(JOURNAL).exists() {
            return Err(error(
                "recovery_required",
                "上次版本切换未完成，请先运行独立恢复入口。",
            ));
        }
        let previous = if self.root.join(STATE).exists() {
            Some(self.state()?)
        } else {
            None
        };
        if previous.is_some() {
            return Err(error(
                "already_installed",
                "程序已安装，请使用 update 或 recover；不会覆盖已有安装。",
            ));
        }
        let next = Installation {
            schema_version: 1,
            product: PRODUCT.into(),
            current: manifest.version.clone(),
            previous: None,
            update_url,
            public_key,
            adapter_state: self.adapter_state.clone(),
            data_paths: self.data_paths.clone(),
            versions: vec![manifest.version.clone()],
        };
        write_json(
            &self.root.join(JOURNAL),
            &Transaction {
                from: previous.clone(),
                to: next.clone(),
                phase: "preparing".into(),
                staging: None,
                files: manifest.files.clone(),
            },
        )?;
        if let Err(failure) = self.prepare(source, &manifest, trust).await {
            let _ = self.cleanup_initial(&next, None);
            return Err(failure);
        }
        self.switch(previous, next)?;
        self.status()
    }
    async fn prepare(&self, source: &Path, manifest: &ReleaseManifest, trust: &Path) -> Result<()> {
        prepare_private_directory(&self.root.join("releases")).map_err(io_error)?;
        let destination = self.root.join("releases").join(&manifest.version);
        if destination.exists() {
            if fs::symlink_metadata(&destination)
                .map_err(io_error)?
                .file_type()
                .is_symlink()
            {
                return Err(conflict());
            }
            let existing =
                release_package::verify_directory(&destination, trust).map_err(pkg_error)?;
            if &existing != manifest {
                return Err(conflict());
            }
            health(&destination).await?;
            return Ok(());
        }
        let temporary = tempfile::Builder::new()
            .prefix(".prepare-")
            .tempdir_in(self.root.join("releases"))
            .map_err(io_error)?;
        if self.root.join(JOURNAL).exists() {
            let mut tx: Transaction = read_json(&self.root.join(JOURNAL))?;
            tx.staging = Some(
                temporary
                    .path()
                    .strip_prefix(&self.root)
                    .map_err(|_| conflict())?
                    .to_owned(),
            );
            tx.files = manifest.files.clone();
            write_json(&self.root.join(JOURNAL), &tx)?;
        }
        copy_release(source, temporary.path(), manifest)?;
        let staged =
            release_package::verify_directory(temporary.path(), trust).map_err(pkg_error)?;
        if &staged != manifest {
            return Err(conflict());
        }
        health(temporary.path()).await?;
        fs::rename(temporary.path(), &destination).map_err(io_error)?;
        Ok(())
    }
    fn validate_layout(&self, s: &Installation) -> Result<()> {
        if fs::read_link(self.root.join("current")).map_err(io_error)?
            != PathBuf::from("releases").join(&s.current)
        {
            return Err(conflict());
        }
        for name in PROGRAMS {
            if fs::read_link(self.root.join("bin").join(name)).map_err(io_error)?
                != PathBuf::from("../current").join(name)
            {
                return Err(conflict());
            }
        }
        let recover = fs::read_link(self.root.join("recover")).map_err(io_error)?;
        let expected = s.previous.as_deref().unwrap_or(&s.current);
        if recover
            != PathBuf::from("releases")
                .join(expected)
                .join("tokemula-image")
        {
            return Err(conflict());
        }
        Ok(())
    }
    fn ensure_links(&self) -> Result<()> {
        prepare_private_directory(&self.root.join("bin")).map_err(io_error)?;
        for name in PROGRAMS {
            let path = self.root.join("bin").join(name);
            let target = PathBuf::from("../current").join(name);
            match fs::symlink_metadata(&path) {
                Ok(m)
                    if m.file_type().is_symlink()
                        && fs::read_link(&path).map_err(io_error)? == target => {}
                Ok(_) => return Err(conflict()),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => atomic_link(&target, &path)?,
                Err(e) => return Err(io_error(e)),
            }
        }
        Ok(())
    }
    fn switch(&self, from: Option<Installation>, to: Installation) -> Result<()> {
        self.ensure_links()?;
        let mut tx = Transaction {
            from: from.clone(),
            to: to.clone(),
            phase: "prepared".into(),
            staging: None,
            files: Vec::new(),
        };
        write_json(&self.root.join(JOURNAL), &tx)?;
        self.phase("journal_prepared");
        let result = (|| {
            atomic_link(
                &PathBuf::from("releases").join(&to.current),
                &self.root.join("current"),
            )?;
            tx.phase = "pointer_switched".into();
            write_json(&self.root.join(JOURNAL), &tx)?;
            self.phase("pointer_switched");
            // Managed assets update only after the programs are staged; failure restores their original owner records.
            if from.is_some() {
                refresh_managed(
                    &self
                        .root
                        .join("releases")
                        .join(&to.current)
                        .join("tokemula-image"),
                    &to.adapter_state,
                    &self.root.join("bin/tokemula-image-mcp"),
                )?;
            }
            write_json(&self.root.join(STATE), &to)?;
            let recovery = to.previous.as_deref().unwrap_or(&to.current);
            atomic_link(
                &PathBuf::from("releases")
                    .join(recovery)
                    .join("tokemula-image"),
                &self.root.join("recover"),
            )?;
            fs::remove_file(self.root.join(JOURNAL)).map_err(io_error)?;
            sync_dir(&self.root)?;
            Ok(())
        })();
        if result.is_err()
            && let Some(mut old) = from
        {
            for version in &to.versions {
                if !old.versions.contains(version) {
                    old.versions.push(version.clone());
                }
            }
            let restored = atomic_link(
                &PathBuf::from("releases").join(&old.current),
                &self.root.join("current"),
            )
            .and_then(|_| {
                crate::adapters::recover_owned_updates(&old.adapter_state)
                    .map_err(|e| error(e.code, e.message))
            })
            .and_then(|_| {
                refresh_managed(
                    &self
                        .root
                        .join("releases")
                        .join(&old.current)
                        .join("tokemula-image"),
                    &old.adapter_state,
                    &self.root.join("bin/tokemula-image-mcp"),
                )
            })
            .and_then(|_| write_json(&self.root.join(STATE), &old))
            .and_then(|_| {
                atomic_link(
                    &PathBuf::from("releases")
                        .join(old.previous.as_deref().unwrap_or(&old.current))
                        .join("tokemula-image"),
                    &self.root.join("recover"),
                )
            });
            if restored.is_ok() {
                let _ = fs::remove_file(self.root.join(JOURNAL));
            } else {
                return Err(error(
                    "recovery_required",
                    "版本切换失败且自动恢复未完成，请使用保留的 recover 入口。",
                ));
            }
        }
        result
    }
    fn cleanup_staging(&self, tx: &Transaction) -> Result<()> {
        let Some(relative) = &tx.staging else {
            return Ok(());
        };
        if relative
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
            || !((relative.parent() == Some(Path::new("releases"))
                && relative
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().starts_with(".prepare-")))
                || (relative.components().count() == 1
                    && relative.file_name().is_some_and(|n| {
                        n.to_string_lossy().starts_with(".uninstall-controller-")
                    })))
        {
            return Err(conflict());
        }
        let stage = self.root.join(relative);
        if !stage.exists() {
            return Ok(());
        }
        if fs::symlink_metadata(&stage)
            .map_err(io_error)?
            .file_type()
            .is_symlink()
        {
            return Err(conflict());
        }
        for item in &tx.files {
            let path = Path::new(&item.path);
            if path
                .components()
                .any(|c| !matches!(c, std::path::Component::Normal(_)))
            {
                return Err(conflict());
            }
            let file = stage.join(path);
            if fs::symlink_metadata(&file).is_ok_and(|m| m.is_file() && !m.file_type().is_symlink())
            {
                fs::remove_file(file).map_err(io_error)?;
            }
        }
        for name in ["release.json", "release.sig"] {
            let p = stage.join(name);
            if p.is_file() {
                fs::remove_file(p).map_err(io_error)?;
            }
        }
        remove_empty_tree(&stage);
        Ok(())
    }
    fn cleanup_initial(&self, to: &Installation, staging: Option<&Transaction>) -> Result<()> {
        let expected = [
            ("current", PathBuf::from("releases").join(&to.current)),
            (
                "recover",
                PathBuf::from("releases")
                    .join(to.previous.as_deref().unwrap_or(&to.current))
                    .join("tokemula-image"),
            ),
            (
                "bin/tokemula-image",
                PathBuf::from("../current/tokemula-image"),
            ),
            (
                "bin/tokemula-image-mcp",
                PathBuf::from("../current/tokemula-image-mcp"),
            ),
        ];
        // Preflight the complete layout before removing any staged/published payload or entry.
        for (name, target) in &expected {
            let path = self.root.join(name);
            match fs::symlink_metadata(&path) {
                Ok(metadata)
                    if metadata.file_type().is_symlink()
                        && fs::read_link(&path).map_err(io_error)? == *target => {}
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
                _ => return Err(conflict()),
            }
        }
        if let Some(tx) = staging {
            self.cleanup_staging(tx)?;
        }
        let trust = tempfile::NamedTempFile::new().map_err(io_error)?;
        fs::write(trust.path(), &to.public_key).map_err(io_error)?;
        let release = self.root.join("releases").join(&to.current);
        if release.exists() {
            remove_release_files(&release, trust.path())?;
        }
        for name in [
            "current",
            "recover",
            "bin/tokemula-image",
            "bin/tokemula-image-mcp",
        ] {
            let path = self.root.join(name);
            if fs::symlink_metadata(&path).is_ok() {
                if !fs::symlink_metadata(&path)
                    .map_err(io_error)?
                    .file_type()
                    .is_symlink()
                {
                    return Err(conflict());
                }
                fs::remove_file(path).map_err(io_error)?;
            }
        }
        for name in [STATE, JOURNAL] {
            let p = self.root.join(name);
            if p.is_file() {
                fs::remove_file(p).map_err(io_error)?;
            }
        }
        for name in ["bin", "releases"] {
            let _ = fs::remove_dir(self.root.join(name));
        }
        Ok(())
    }
    pub async fn recover(&self) -> Result<Value> {
        supported()?;
        let _lock = self.lock()?;
        if let Some(value) = self.cleanup_last_controller()? {
            drop(_lock);
            let result = Ok(value);
            self.cleanup_finished_root(&result);
            return result;
        }
        if let Some(tx) = self.uninstall_transaction()? {
            let result = self.continue_uninstall(tx);
            drop(_lock);
            self.cleanup_finished_root(&result);
            return result;
        }
        let (old, failed) = if self.root.join(JOURNAL).exists() {
            let tx: Transaction = read_json(&self.root.join(JOURNAL))?;
            validate_installation(&tx.to)?;
            if let Some(old) = &tx.from {
                validate_installation(old)?;
            }
            if tx.from.is_none() && tx.phase != "recovering" {
                self.cleanup_initial(&tx.to, Some(&tx))?;
                return Ok(
                    json!({"status":"not_installed","message":"首次安装中断已恢复；可从分发包重新 setup。"}),
                );
            }
            self.cleanup_staging(&tx)?;
            let mut old = if tx.phase == "recovering" {
                tx.to.clone()
            } else {
                tx.from.clone().ok_or_else(conflict)?
            };
            for v in &tx.to.versions {
                if !old.versions.contains(v) {
                    old.versions.push(v.clone());
                }
            }
            (old, Some(tx.to.current))
        } else {
            let s = self.state()?;
            let prior = s
                .previous
                .clone()
                .ok_or_else(|| error("no_previous_version", "当前没有上一可用版本。"))?;
            (
                Installation {
                    current: prior,
                    previous: None,
                    ..s.clone()
                },
                Some(s.current),
            )
        };
        write_json(
            &self.root.join(JOURNAL),
            &Transaction {
                from: None,
                to: old.clone(),
                phase: "recovering".into(),
                staging: None,
                files: Vec::new(),
            },
        )?;
        let trust = tempfile::NamedTempFile::new().map_err(io_error)?;
        fs::write(trust.path(), &old.public_key).map_err(io_error)?;
        let source = self.root.join("releases").join(&old.current);
        if fs::symlink_metadata(&source)
            .map_err(io_error)?
            .file_type()
            .is_symlink()
        {
            return Err(conflict());
        }
        release_package::verify_directory(&source, trust.path()).map_err(pkg_error)?;
        health(&source).await?;
        atomic_link(
            &PathBuf::from("releases").join(&old.current),
            &self.root.join("current"),
        )?;
        crate::adapters::recover_owned_updates(&old.adapter_state)
            .map_err(|e| error(e.code, e.message))?;
        refresh_managed(
            &source.join("tokemula-image"),
            &old.adapter_state,
            &self.root.join("bin/tokemula-image-mcp"),
        )?;
        let _failed_version = failed;
        let restored = old;
        write_json(&self.root.join(STATE), &restored)?;
        atomic_link(
            &PathBuf::from("releases")
                .join(restored.previous.as_deref().unwrap_or(&restored.current))
                .join("tokemula-image"),
            &self.root.join("recover"),
        )?;
        if self.root.join(JOURNAL).exists() {
            fs::remove_file(self.root.join(JOURNAL)).map_err(io_error)?;
        }
        self.status()
    }
    pub async fn check(&self) -> Result<Value> {
        if self.root.join(JOURNAL).exists() {
            return Err(error(
                "recovery_required",
                "产品操作尚未完成，请按恢复指引继续。",
            ));
        }
        let s = self.state()?;
        let Some(url) = s.update_url.as_deref() else {
            return Ok(
                json!({"status":"not_configured","current_version":s.current,"message":"尚未配置可信更新渠道。"}),
            );
        };
        let bytes = download(url, 1024 * 1024).await?;
        let trust = tempfile::NamedTempFile::new().map_err(io_error)?;
        fs::write(trust.path(), &s.public_key).map_err(io_error)?;
        let update = release_package::verify_update(&bytes, trust.path()).map_err(pkg_error)?;
        let current = semver::Version::parse(&s.current).map_err(|_| conflict())?;
        let latest = semver::Version::parse(&update.version).map_err(|_| conflict())?;
        Ok(
            json!({"status":if latest>current{"update_available"}else{"up_to_date"},"current_version":s.current,"available_version":update.version,"target":update.target,"reconnect_required":false}),
        )
    }
    pub async fn apply(&self, confirm: bool) -> Result<Value> {
        supported()?;
        if !confirm {
            return Err(error(
                "confirmation_required",
                "升级需要用户明确确认，请添加 --confirm。",
            ));
        }
        let _lock = self.lock()?;
        if self.root.join(JOURNAL).exists() {
            return Err(error(
                "recovery_required",
                "上次版本切换未完成，请先 recover。",
            ));
        }
        let s = self.state()?;
        self.validate_layout(&s)?;
        let url = s
            .update_url
            .as_deref()
            .ok_or_else(|| error("update_not_configured", "尚未配置可信更新渠道。"))?;
        let bytes = download(url, 1024 * 1024).await?;
        let trust = tempfile::NamedTempFile::new().map_err(io_error)?;
        fs::write(trust.path(), &s.public_key).map_err(io_error)?;
        let update = release_package::verify_update(&bytes, trust.path()).map_err(pkg_error)?;
        if semver::Version::parse(&update.version).map_err(|_| conflict())?
            <= semver::Version::parse(&s.current).map_err(|_| conflict())?
        {
            return Err(error(
                "update_not_newer",
                "更新版本必须高于当前版本；回退请使用 recover。",
            ));
        }
        let archive = download(&update.archive_url, 1024 * 1024 * 1024).await?;
        let temp = tempfile::tempdir_in(&self.root).map_err(io_error)?;
        let archive_path = temp.path().join("release.tar.gz");
        fs::write(&archive_path, archive).map_err(io_error)?;
        let unpacked = temp.path().join("unpacked");
        let manifest =
            release_package::unpack_verified(&archive_path, &unpacked, trust.path(), &update)
                .map_err(pkg_error)?;
        let mut versions = s.versions.clone();
        if !versions.contains(&manifest.version) {
            versions.push(manifest.version.clone())
        }
        let next = Installation {
            current: manifest.version.clone(),
            previous: Some(s.current.clone()),
            versions,
            ..s.clone()
        };
        write_json(
            &self.root.join(JOURNAL),
            &Transaction {
                from: Some(s.clone()),
                to: next.clone(),
                phase: "preparing".into(),
                staging: None,
                files: manifest.files.clone(),
            },
        )?;
        if let Err(failure) = self.prepare(&unpacked, &manifest, trust.path()).await {
            let _ = fs::remove_file(self.root.join(JOURNAL));
            return Err(failure);
        }
        self.switch(Some(s), next)?;
        let mut status = self.status()?;
        status["reconnect_required"] = json!(true);
        status["message"] = json!("新版已安装；现有 MCP 可完成在途请求，重连后使用新版。");
        Ok(status)
    }
    pub async fn automatic_check(&self) -> Result<Option<Value>> {
        if !self.root.join(STATE).exists() || self.root.join(JOURNAL).exists() {
            return Ok(None);
        }
        let _lock = self.lock()?;
        let stamp = self.root.join("update-check.json");
        if let Ok(value) = read_json::<Value>(&stamp)
            && value["attempted_at"]
                .as_u64()
                .is_some_and(|t| now().saturating_sub(t) < 24 * 60 * 60)
        {
            return Ok(None);
        }
        write_json(&stamp, &json!({"attempted_at":now()}))?;
        drop(_lock);
        self.check().await.map(Some)
    }
    pub fn uninstall(
        &self,
        service: &SettingsService,
        purge: bool,
        confirm: bool,
    ) -> Result<Value> {
        supported()?;
        if purge && !confirm {
            return Err(error(
                "confirmation_required",
                "彻底清理需要明确 --purge --confirm；默认保留设置。",
            ));
        }
        let _lock = self.lock()?;
        if let Some(value) = self.cleanup_last_controller()? {
            drop(_lock);
            let result = Ok(value);
            self.cleanup_finished_root(&result);
            return result;
        }
        if let Some(tx) = self.uninstall_transaction()? {
            let result = self.continue_uninstall(tx);
            drop(_lock);
            self.cleanup_finished_root(&result);
            return result;
        }
        if self.root.join(".uninstall-recovery").exists() {
            return Err(conflict());
        }
        let s = self.state()?;
        self.validate_layout(&s)?;
        let trust = tempfile::NamedTempFile::new().map_err(io_error)?;
        fs::write(trust.path(), &s.public_key).map_err(io_error)?;
        let releases: Vec<PathBuf> = s
            .versions
            .iter()
            .map(|v| self.root.join("releases").join(v))
            .collect();
        for release in &releases {
            if fs::symlink_metadata(release)
                .map_err(io_error)?
                .file_type()
                .is_symlink()
            {
                return Err(conflict());
            }
            if release.is_dir() {
                release_package::verify_owned_directory(release, trust.path())
                    .map_err(pkg_error)?;
            }
        }
        let probe = tempfile::NamedTempFile::new_in(&self.root).map_err(io_error)?;
        drop(probe);
        let removed = crate::adapters::remove_owned(&s.adapter_state)
            .map_err(|e| error(e.code, e.message))?;
        if !removed.completed {
            return Ok(
                json!({"status":"uninstall_incomplete","registrations":removed,"recovery_program":self.root.join("recover"),"programs_retained":true}),
            );
        }
        // Credentials and user files are never removed until all external registrations are clean.
        if purge {
            service
                .with_data_paths(&s.data_paths)
                .purge()
                .map_err(|e| error(e.code, e.message))?;
            purge_task_records(&s.data_paths.tasks)?;
        }
        clear_managed_cache(&s.data_paths.cache)?;
        crate::adapters::cleanup_empty_owned_state(&s.adapter_state)
            .map_err(|e| error(e.code, e.message))?;
        let tx = Transaction {
            from: Some(s.clone()),
            to: s,
            phase: if purge {
                "uninstall_preparing_purge"
            } else {
                "uninstall_preparing_keep"
            }
            .into(),
            staging: None,
            files: Vec::new(),
        };
        write_json(&self.root.join(JOURNAL), &tx)?;
        let result = self.continue_uninstall(tx);
        drop(_lock);
        self.cleanup_finished_root(&result);
        result
    }
    fn uninstall_transaction(&self) -> Result<Option<Transaction>> {
        let path = if self.root.join(JOURNAL).exists() {
            self.root.join(JOURNAL)
        } else {
            self.root.join(".uninstall-recovery/control.json")
        };
        if !path.exists() {
            return Ok(None);
        }
        let tx: Transaction = read_json(&path)?;
        if tx.phase.starts_with("uninstall_") {
            validate_installation(&tx.to)?;
            if !matches!(
                tx.phase.as_str(),
                "uninstall_preparing_keep"
                    | "uninstall_preparing_purge"
                    | "uninstall_programs_keep"
                    | "uninstall_programs_purge"
                    | "uninstall_finalize_keep"
                    | "uninstall_finalize_purge"
            ) {
                return Err(conflict());
            }
            Ok(Some(tx))
        } else {
            Ok(None)
        }
    }
    fn controller(&self) -> PathBuf {
        self.root.join(".uninstall-recovery/tokemula-image")
    }
    fn controller_metadata(&self, version: &str) -> PathBuf {
        self.root
            .join(".uninstall-recovery/manifests")
            .join(version)
    }
    fn continue_uninstall(&self, mut tx: Transaction) -> Result<Value> {
        let purge = tx.phase.ends_with("_purge");
        let outcome = (|| {
            if tx.phase.starts_with("uninstall_preparing_") {
                self.prepare_uninstall_controller(&mut tx)?;
                tx.phase = if purge {
                    "uninstall_programs_purge"
                } else {
                    "uninstall_programs_keep"
                }
                .into();
                write_json(&self.root.join(JOURNAL), &tx)?;
                write_json(&self.root.join(".uninstall-recovery/control.json"), &tx)?;
            }
            let trust = tempfile::NamedTempFile::new().map_err(io_error)?;
            fs::write(trust.path(), &tx.to.public_key).map_err(io_error)?;
            if tx.phase.starts_with("uninstall_programs_") {
                if !self.controller().is_file() {
                    return Err(error(
                        "uninstall_recovery_missing",
                        "独立卸载恢复程序尚未准备完成。",
                    ));
                }
                // Its bytes remain bound to the retained publisher-signed manifest.
                release_package::verify_removal_directory(
                    &self.root.join(".uninstall-recovery"),
                    &self.controller_metadata(&tx.to.current),
                    trust.path(),
                )
                .map_err(pkg_error)?;
                for version in &tx.to.versions {
                    let payload = self.root.join("releases").join(version);
                    if fs::symlink_metadata(&payload).is_ok_and(|m| m.file_type().is_symlink()) {
                        return Err(conflict());
                    }
                    let manifest = release_package::verify_removal_directory(
                        &payload,
                        &self.controller_metadata(version),
                        trust.path(),
                    )
                    .map_err(pkg_error)?;
                    for item in manifest.files {
                        let path = payload.join(item.path);
                        if path.exists() {
                            fs::remove_file(path).map_err(io_error)?;
                        }
                    }
                    for name in ["release.json", "release.sig"] {
                        let path = payload.join(name);
                        if path.exists() {
                            fs::remove_file(path).map_err(io_error)?;
                        }
                    }
                    remove_empty_tree(&payload);
                }
                for name in ["bin/tokemula-image", "bin/tokemula-image-mcp", "current"] {
                    let path = self.root.join(name);
                    if fs::symlink_metadata(&path).is_ok() {
                        if !fs::symlink_metadata(&path)
                            .map_err(io_error)?
                            .file_type()
                            .is_symlink()
                        {
                            return Err(conflict());
                        }
                        fs::remove_file(path).map_err(io_error)?;
                    }
                }
                tx.phase = if purge {
                    "uninstall_finalize_purge"
                } else {
                    "uninstall_finalize_keep"
                }
                .into();
                write_json(&self.root.join(JOURNAL), &tx)?;
                write_json(&self.root.join(".uninstall-recovery/control.json"), &tx)?;
            }
            // The standalone controller is deleted last; any earlier failure can be resumed.
            for version in &tx.to.versions {
                let meta = self.controller_metadata(version);
                for name in ["release.json", "release.sig"] {
                    let p = meta.join(name);
                    if p.exists() {
                        fs::remove_file(p).map_err(io_error)?;
                    }
                }
                let _ = fs::remove_dir(meta);
            }
            let _ = fs::remove_dir(self.root.join(".uninstall-recovery/manifests"));
            for name in [STATE, "update-check.json", JOURNAL] {
                let p = self.root.join(name);
                if p.exists() {
                    fs::remove_file(p).map_err(io_error)?;
                }
            }
            let link = self.root.join("recover");
            if fs::symlink_metadata(&link).is_ok() {
                if fs::read_link(&link).map_err(io_error)?
                    != Path::new(".uninstall-recovery/tokemula-image")
                {
                    return Err(conflict());
                }
                fs::remove_file(link).map_err(io_error)?;
            }
            let record = self.root.join(".uninstall-recovery/control.json");
            if record.exists() {
                fs::remove_file(record).map_err(io_error)?;
            }
            fs::remove_file(self.controller()).map_err(io_error)?;
            let _ = fs::remove_dir(self.root.join(".uninstall-recovery"));
            Ok(
                json!({"status":"uninstalled","settings":if purge{"purged"}else{"retained"},"user_files":"preserved"}),
            )
        })();
        match outcome {
            Ok(value) => Ok(value),
            Err(failure) => Ok(
                json!({"status":"uninstall_incomplete","phase":tx.phase,"code":failure.code,"message":failure.message,"programs_retained":self.controller().is_file()||self.root.join("bin/tokemula-image").is_file(),"resume_command":format!("{} recover --install-root {}",shell_quote(&if self.controller().is_file(){self.controller()}else{self.root.join("bin/tokemula-image")}),shell_quote(&self.root)),"external_cleanup_repeated":false}),
            ),
        }
    }
    fn prepare_uninstall_controller(&self, tx: &mut Transaction) -> Result<()> {
        let destination = self.root.join(".uninstall-recovery");
        if destination.exists() {
            let trust = tempfile::NamedTempFile::new().map_err(io_error)?;
            fs::write(trust.path(), &tx.to.public_key).map_err(io_error)?;
            if !self.controller().is_file() {
                return Err(conflict());
            }
            release_package::verify_removal_directory(
                &destination,
                &self.controller_metadata(&tx.to.current),
                trust.path(),
            )
            .map_err(pkg_error)?;
        } else {
            self.cleanup_staging(tx)?;
            let stage = tempfile::Builder::new()
                .prefix(".uninstall-controller-")
                .tempdir_in(&self.root)
                .map_err(io_error)?;
            tx.staging = Some(
                stage
                    .path()
                    .strip_prefix(&self.root)
                    .map_err(|_| conflict())?
                    .to_owned(),
            );
            tx.files = vec![release_package::ReleaseFile {
                path: "tokemula-image".into(),
                sha256: String::new(),
                size: 0,
            }];
            for version in &tx.to.versions {
                for name in ["release.json", "release.sig"] {
                    tx.files.push(release_package::ReleaseFile {
                        path: format!("manifests/{version}/{name}"),
                        sha256: String::new(),
                        size: 0,
                    });
                }
            }
            write_json(&self.root.join(JOURNAL), tx)?;
            let trust = tempfile::NamedTempFile::new().map_err(io_error)?;
            fs::write(trust.path(), &tx.to.public_key).map_err(io_error)?;
            for version in &tx.to.versions {
                let source = self.root.join("releases").join(version);
                release_package::verify_owned_directory(&source, trust.path())
                    .map_err(pkg_error)?;
                let meta = stage.path().join("manifests").join(version);
                fs::create_dir_all(&meta).map_err(io_error)?;
                for name in ["release.json", "release.sig"] {
                    fs::copy(source.join(name), meta.join(name)).map_err(io_error)?;
                }
            }
            fs::copy(
                self.root
                    .join("releases")
                    .join(&tx.to.current)
                    .join("tokemula-image"),
                stage.path().join("tokemula-image"),
            )
            .map_err(io_error)?;
            fs::File::open(stage.path().join("tokemula-image"))
                .and_then(|f| f.sync_all())
                .map_err(io_error)?;
            release_package::verify_removal_directory(
                stage.path(),
                &stage.path().join("manifests").join(&tx.to.current),
                trust.path(),
            )
            .map_err(pkg_error)?;
            fs::rename(stage.path(), &destination).map_err(io_error)?;
            sync_dir(&self.root)?;
        }
        tx.staging = None;
        tx.files.clear();
        atomic_link(
            Path::new(".uninstall-recovery/tokemula-image"),
            &self.root.join("recover"),
        )?;
        Ok(())
    }
    fn cleanup_last_controller(&self) -> Result<Option<Value>> {
        if self.root.join(STATE).exists()
            || self.root.join(JOURNAL).exists()
            || self.root.join(".uninstall-recovery/control.json").exists()
        {
            return Ok(None);
        }
        let own = std::env::current_exe().and_then(fs::canonicalize).ok();
        if own.is_none() || own != fs::canonicalize(self.controller()).ok() {
            return Ok(None);
        }
        for name in ["bin/tokemula-image", "bin/tokemula-image-mcp", "current"] {
            if fs::symlink_metadata(self.root.join(name)).is_ok() {
                return Ok(None);
            }
        }
        if self.root.join("releases").is_dir() {
            for entry in fs::read_dir(self.root.join("releases")).map_err(io_error)? {
                let entry = entry.map_err(io_error)?;
                if entry.file_type().map_err(io_error)?.is_symlink() {
                    return Ok(None);
                }
                for name in PROGRAMS {
                    if fs::symlink_metadata(entry.path().join(name)).is_ok() {
                        return Ok(None);
                    }
                }
            }
        }
        // All installed programs were removed before this final self-unlink was attempted.
        fs::remove_file(self.controller()).map_err(io_error)?;
        let _ = fs::remove_dir(self.root.join(".uninstall-recovery"));
        Ok(Some(
            json!({"status":"uninstalled","settings":"already_processed","user_files":"preserved","remaining_controller_removed":true}),
        ))
    }
    fn cleanup_finished_root(&self, result: &Result<Value>) {
        if result.as_ref().is_ok_and(|v| v["status"] == "uninstalled") {
            let _ = fs::remove_file(self.root.join("operation.lock"));
            for name in ["bin", "releases", ""] {
                let _ = fs::remove_dir(self.root.join(name));
            }
        }
    }
}

pub fn default_install_root() -> Result<PathBuf> {
    if let Some(root) = installed_root() {
        return Ok(root);
    }
    let home = std::env::var_os(if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "HOME"
    })
    .map(PathBuf::from)
    .ok_or_else(|| {
        error(
            "installation_path_unavailable",
            "无法定位当前用户安装目录。",
        )
    })?;
    Ok(if cfg!(windows) {
        home.join(PRODUCT)
    } else {
        home.join(".local/share").join(PRODUCT)
    })
}
pub fn default_adapter_state() -> Result<PathBuf> {
    let root = std::env::var_os(if cfg!(windows) {
        "LOCALAPPDATA"
    } else {
        "XDG_STATE_HOME"
    })
    .map(PathBuf::from)
    .or_else(|| std::env::var_os("HOME").map(|p| PathBuf::from(p).join(".local/state")))
    .ok_or_else(conflict)?;
    Ok(root.join(PRODUCT).join("adapters"))
}
pub(crate) fn installation_root_for(exe: &Path) -> Option<PathBuf> {
    exe.ancestors()
        .skip(1)
        .take(4)
        .find(|root| {
            let Ok(state) = read_json::<Installation>(&root.join(STATE)) else {
                return false;
            };
            if state.product != PRODUCT || state.schema_version != 1 {
                return false;
            }
            if exe == root.join("recover") {
                return fs::read_link(exe)
                    .is_ok_and(|p| p.starts_with("releases") && p.ends_with("tokemula-image"));
            }
            if exe.parent() == Some(root.join(".uninstall-recovery").as_path())
                || exe.parent() == Some(root.join("bin").as_path())
                || exe.parent() == Some(root.join("current").as_path())
            {
                return exe
                    .file_name()
                    .is_some_and(|n| PROGRAMS.iter().any(|p| n == std::ffi::OsStr::new(p)));
            }
            let Ok(relative) = exe.strip_prefix(root.join("releases")) else {
                return false;
            };
            let mut parts = relative.components();
            let Some(std::path::Component::Normal(version)) = parts.next() else {
                return false;
            };
            let Some(std::path::Component::Normal(binary)) = parts.next() else {
                return false;
            };
            parts.next().is_none()
                && state
                    .versions
                    .iter()
                    .any(|v| version == std::ffi::OsStr::new(v))
                && PROGRAMS.iter().any(|p| binary == std::ffi::OsStr::new(p))
        })
        .map(Path::to_owned)
}
pub(crate) fn adapter_state_for(exe: &Path) -> Option<PathBuf> {
    let root = installation_root_for(exe)?;
    read_json::<Installation>(&root.join(STATE))
        .ok()
        .map(|s| s.adapter_state)
}
pub fn installed_root() -> Option<PathBuf> {
    installation_root_for(&std::env::current_exe().ok()?)
}
pub fn management_program() -> Option<PathBuf> {
    installed_root()
        .map(|r| r.join("bin/tokemula-image"))
        .or_else(|| {
            std::env::current_exe().ok().map(|p| {
                p.with_file_name(if cfg!(windows) {
                    "tokemula-image.exe"
                } else {
                    "tokemula-image"
                })
            })
        })
}
pub fn shell_quote(path: &Path) -> String {
    if cfg!(windows) {
        format!("\"{}\"", path.display())
    } else {
        format!("'{}'", path.to_string_lossy().replace('\'', "'\\''"))
    }
}
pub fn management_command(arguments: &str) -> String {
    management_program()
        .map(|p| format!("{} {arguments}", shell_quote(&p)))
        .unwrap_or_else(|| "请从分发目录运行管理程序。".into())
}
pub fn build_info(binary: &str) -> Value {
    json!({"product":PRODUCT,"version":VERSION,"target":TARGET,"binary":binary})
}
pub fn embedded_health() -> Result<Value> {
    let metadata: Value =
        serde_json::from_str(include_str!("../../plugins/workbuddy/connector-meta.json"))
            .map_err(|_| conflict())?;
    if metadata["name"].as_str().is_none()
        || !crate::adapters::SKILL_CONTENT.starts_with("---")
        || !include_str!("../../integrations/deepseek/index.js").contains("mcpManager.register")
        || !include_str!("../../integrations/deepseek/recover.mjs")
            .contains("activateNewBundles: false")
        || !include_str!("../assets/configure/index.html").contains("Tokemula")
        || include_str!("../assets/configure/app.js").is_empty()
        || !include_str!("../assets/configure/app.css").contains("520")
    {
        return Err(error(
            "embedded_assets_invalid",
            "内嵌页面或插件资源不完整。",
        ));
    }
    let temp = tempfile::tempdir().map_err(io_error)?;
    let mcp = std::env::current_exe()
        .map_err(io_error)?
        .with_file_name(if cfg!(windows) {
            "tokemula-image-mcp.exe"
        } else {
            "tokemula-image-mcp"
        });
    let adapter = crate::adapters::workbuddy::WorkBuddyAdapter::new(
        crate::adapters::Scope::User,
        crate::adapters::workbuddy::WorkBuddyPaths {
            home: temp.path().to_owned(),
            config_dir: temp.path().join("config"),
            state_dir: temp.path().join("state"),
            app_bundle: temp.path().join("WorkBuddy.app"),
            executable: mcp,
        },
    )
    .map_err(|e| error(e.code, e.message))?;
    let exported = adapter
        .export_package(&temp.path().join("connector"))
        .map_err(|e| error(e.code, e.message))?;
    let exported_meta: Value = read_json(&exported.join("connector-meta.json"))?;
    let exported_mcp: Value = read_json(&exported.join("mcp.json"))?;
    if exported_meta["source"] != "tokemula-image"
        || exported_mcp["mcpServers"]["tokemula-image"].is_null()
        || !fs::read_to_string(exported.join("icon.svg"))
            .map_err(io_error)?
            .starts_with("<svg")
        || fs::read_to_string(exported.join("skills/tokemula-image/SKILL.md")).map_err(io_error)?
            != crate::adapters::SKILL_CONTENT
    {
        return Err(error("embedded_assets_invalid", "内嵌连接器资源生成失败。"));
    }
    let deepseek = crate::adapters::deepseek::DeepSeekAdapter::new(
        crate::adapters::Scope::User,
        crate::adapters::deepseek::DeepSeekPaths {
            home: temp.path().to_owned(),
            dsh_home: temp.path().join("dsh"),
            project_dir: temp.path().to_owned(),
            state_dir: temp.path().join("state"),
            cli: std::env::current_exe().map_err(io_error)?,
            executable: std::env::current_exe().map_err(io_error)?.with_file_name(
                if cfg!(windows) {
                    "tokemula-image-mcp.exe"
                } else {
                    "tokemula-image-mcp"
                },
            ),
            process_path: Default::default(),
            profile: "web".into(),
            runtime: crate::adapters::deepseek::DeepSeekRuntime::CliWeb,
        },
    )
    .map_err(|e| error(e.code, e.message))?;
    let materialized = deepseek
        .materialize_assets(&temp.path().join("plugin"))
        .map_err(|e| error(e.code, e.message))?;
    let plugin: Value = read_json(&materialized.join("package.json"))?;
    let patch: serde_yaml_ng::Value = serde_yaml_ng::from_slice(
        &fs::read(materialized.join("cordis.patch.yml")).map_err(io_error)?,
    )
    .map_err(|_| conflict())?;
    if plugin["name"] != "tokemula-image-connector"
        || patch.is_null()
        || fs::read(materialized.join("index.js"))
            .map_err(io_error)?
            .is_empty()
        || fs::read(materialized.join("recover.mjs"))
            .map_err(io_error)?
            .is_empty()
    {
        return Err(error("embedded_assets_invalid", "内嵌插件资源生成失败。"));
    }
    Ok(
        json!({"status":"healthy","assets":["configuration_html","configuration_css","configuration_script","skill","deepseek_plugin","deepseek_recovery","workbuddy_metadata","workbuddy_icon"]}),
    )
}
fn supported() -> Result<()> {
    if cfg!(unix) {
        Ok(())
    } else {
        Err(error(
            "platform_lifecycle_pending",
            "Windows 成套替换仍需原生文件占用与恢复验收；原安装保持。",
        ))
    }
}
fn write_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().ok_or_else(conflict)?;
    prepare_private_directory(parent).map_err(io_error)?;
    let mut f = tempfile::NamedTempFile::new_in(parent).map_err(io_error)?;
    serde_json::to_writer(&mut f, value).map_err(|_| conflict())?;
    f.as_file().sync_all().map_err(io_error)?;
    f.persist(path).map_err(|e| io_error(e.error))?;
    sync_dir(parent)
}
fn read_json<T: serde::de::DeserializeOwned>(path: &Path) -> Result<T> {
    let m = fs::symlink_metadata(path).map_err(io_error)?;
    if !m.is_file() || m.file_type().is_symlink() || m.len() > 1024 * 1024 {
        return Err(conflict());
    }
    serde_json::from_slice(&fs::read(path).map_err(io_error)?).map_err(|_| conflict())
}
fn sync_dir(path: &Path) -> Result<()> {
    fs::File::open(path)
        .and_then(|f| f.sync_all())
        .map_err(io_error)
}
fn atomic_link(target: &Path, path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        let parent = path.parent().ok_or_else(conflict)?;
        let temp = tempfile::Builder::new()
            .prefix(".switch-")
            .tempdir_in(parent)
            .map_err(io_error)?;
        let link = temp.path().join("pointer");
        std::os::unix::fs::symlink(target, &link).map_err(io_error)?;
        fs::rename(link, path).map_err(io_error)?;
        sync_dir(parent)
    }
    #[cfg(not(unix))]
    {
        let _ = (target, path);
        supported()
    }
}
fn copy_release(source: &Path, dest: &Path, m: &ReleaseManifest) -> Result<()> {
    for item in &m.files {
        let to = dest.join(&item.path);
        if let Some(parent) = to.parent() {
            fs::create_dir_all(parent).map_err(io_error)?;
        }
        fs::copy(source.join(&item.path), &to).map_err(io_error)?;
    }
    for name in ["release.json", "release.sig"] {
        fs::copy(source.join(name), dest.join(name)).map_err(io_error)?;
    }
    Ok(())
}
async fn health(root: &Path) -> Result<()> {
    for name in PROGRAMS {
        let output = tokio::time::timeout(
            Duration::from_secs(10),
            tokio::process::Command::new(root.join(name))
                .kill_on_drop(true)
                .arg("--build-info")
                .env_clear()
                .output(),
        )
        .await
        .map_err(|_| error("release_health_failed", "候选程序启动检查超时。"))?
        .map_err(io_error)?;
        if !output.status.success() {
            return Err(error(
                "release_health_failed",
                "候选程序无法启动，未切换版本。",
            ));
        }
        let value: Value = serde_json::from_slice(&output.stdout).map_err(|_| conflict())?;
        if value["product"] != PRODUCT
            || value["version"] != root_manifest_version(root)?
            || value["target"] != TARGET
            || value["binary"] != name
        {
            return Err(error(
                "release_version_mismatch",
                "程序实际版本或目标与发行清单不一致。",
            ));
        }
    }
    let output = tokio::time::timeout(
        Duration::from_secs(10),
        tokio::process::Command::new(root.join("tokemula-image"))
            .kill_on_drop(true)
            .arg("health")
            .env_clear()
            .output(),
    )
    .await
    .map_err(|_| error("release_health_failed", "候选资源检查超时。"))?
    .map_err(io_error)?;
    if !output.status.success() {
        return Err(error("release_health_failed", "候选内嵌资源检查失败。"));
    }
    use rmcp::ServiceExt;
    let home = tempfile::tempdir().map_err(io_error)?;
    let mut child = tokio::process::Command::new(root.join("tokemula-image-mcp"))
        .env_clear()
        .env("HOME", home.path())
        .env("TOKEMULA_IMAGE_KEY", "")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null())
        .kill_on_drop(true)
        .spawn()
        .map_err(io_error)?;
    let stdin = child.stdin.take().ok_or_else(conflict)?;
    let stdout = child.stdout.take().ok_or_else(conflict)?;
    let client = tokio::time::timeout(
        Duration::from_secs(10),
        rmcp::model::ClientConfig::new(
            Default::default(),
            rmcp::model::Implementation::new("release-health", "1"),
        )
        .serve((stdout, stdin)),
    )
    .await
    .map_err(|_| conflict())?
    .map_err(|_| error("release_protocol_failed", "候选 MCP 协议初始化失败。"))?;
    let tools = tokio::time::timeout(Duration::from_secs(10), client.list_tools(None))
        .await
        .map_err(|_| conflict())?
        .map_err(|_| conflict())?;
    let version = root_manifest_version(root)?;
    let names: Vec<&str> = tools.tools.iter().map(|tool| tool.name.as_ref()).collect();
    if !release_tools_match(version.as_str().ok_or_else(conflict)?, &names)? {
        return Err(error(
            "release_protocol_failed",
            "候选 MCP 工具目录不完整。",
        ));
    }
    let _ = client.cancel().await;
    let _ = tokio::time::timeout(Duration::from_secs(3), child.wait()).await;
    Ok(())
}
fn release_tools_match(version: &str, names: &[&str]) -> Result<bool> {
    let version = semver::Version::parse(version).map_err(|_| conflict())?;
    let mut expected = vec![
        "configure_tokemula",
        "submit_image_batch",
        "get_image_batch",
        "cancel_image_task",
        "get_image_task",
        "list_image_models",
        "generate_image",
        "edit_image",
    ];
    // The target version owns its protocol, including rollback to signed legacy packages.
    if version < semver::Version::parse("0.1.0-e1.1").expect("fixed version") {
        expected.push("get_image_billing");
    }
    Ok(names.len() == expected.len() && expected.iter().all(|name| names.contains(name)))
}
fn root_manifest_version(root: &Path) -> Result<Value> {
    let v: Value = read_json(&root.join("release.json"))?;
    Ok(v["version"].clone())
}
fn validate_update_url(url: &str) -> Result<()> {
    let u =
        reqwest::Url::parse(url).map_err(|_| error("update_source_invalid", "更新地址无效。"))?;
    if u.username() != ""
        || u.password().is_some()
        || u.fragment().is_some()
        || !(u.scheme() == "https"
            || u.scheme() == "http"
                && matches!(u.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")))
    {
        return Err(error(
            "update_source_invalid",
            "更新源需要 HTTPS 或明确的本机 loopback，不能含认证信息。",
        ));
    }
    Ok(())
}
async fn download(url: &str, max: usize) -> Result<Vec<u8>> {
    validate_update_url(url)?;
    let client = reqwest::Client::builder()
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .retry(reqwest::retry::never())
        .timeout(Duration::from_secs(15))
        .build()
        .map_err(|_| conflict())?;
    let mut response = client
        .get(url)
        .send()
        .await
        .map_err(|_| error("update_network_failed", "更新源读取失败；当前程序保持。"))?;
    if !response.status().is_success() {
        return Err(error(
            "update_source_unavailable",
            "更新源尚不可用；当前程序保持。",
        ));
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| error("update_network_failed", "更新下载未完成，当前程序保持。"))?
    {
        if bytes.len().saturating_add(chunk.len()) > max {
            return Err(error("update_too_large", "更新数据超过允许大小。"));
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(bytes)
}
fn remove_release_files(root: &Path, trust: &Path) -> Result<()> {
    let m = release_package::verify_owned_directory(root, trust).map_err(pkg_error)?;
    for item in &m.files {
        let path = root.join(&item.path);
        if path.is_file()
            && !fs::symlink_metadata(&path)
                .map_err(io_error)?
                .file_type()
                .is_symlink()
        {
            fs::remove_file(path).map_err(io_error)?;
        }
    }
    for name in ["release.json", "release.sig"] {
        let p = root.join(name);
        if p.is_file() {
            fs::remove_file(p).map_err(io_error)?;
        }
    }
    // Only empty parents are removed; user-added files keep their directories alive.
    remove_empty_tree(root);
    Ok(())
}

fn clear_managed_cache(root: &Path) -> Result<()> {
    if fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(conflict());
    }
    if root.is_dir() {
        for e in fs::read_dir(root).map_err(io_error)? {
            let e = e.map_err(io_error)?;
            if e.file_type().map_err(io_error)?.is_file()
                && crate::output::managed_expiry(&e.path()).is_some()
            {
                fs::remove_file(e.path()).map_err(io_error)?;
            }
        }
    }
    Ok(())
}
fn purge_task_records(root: &Path) -> Result<()> {
    if fs::symlink_metadata(root).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(conflict());
    }
    let store = crate::tasks::TaskStore {
        root: root.to_owned(),
    };
    if !store.root.is_dir() {
        return Ok(());
    }
    for e in fs::read_dir(&store.root).map_err(io_error)? {
        let e = e.map_err(io_error)?;
        if !e.file_type().map_err(io_error)?.is_file() {
            continue;
        }
        if store.owns_file(&e.path()) {
            fs::remove_file(e.path()).map_err(io_error)?;
        }
    }
    let batches = store.root.join("batches");
    if fs::symlink_metadata(&batches).is_ok_and(|m| m.file_type().is_symlink()) {
        return Err(conflict());
    }
    if batches.is_dir() {
        for e in fs::read_dir(&batches).map_err(io_error)? {
            let e = e.map_err(io_error)?;
            if e.file_type().map_err(io_error)?.is_file()
                && crate::http::owned_batch_record_file(&batches, &e.path())
            {
                fs::remove_file(e.path()).map_err(io_error)?;
            }
        }
        let _ = fs::remove_dir(batches);
    }
    let _ = fs::remove_dir(store.root);
    Ok(())
}

fn refresh_managed(program: &Path, state: &Path, mcp: &Path) -> Result<()> {
    let output = std::process::Command::new(program)
        .args(["managed-refresh", "--state-dir"])
        .arg(state)
        .arg("--mcp")
        .arg(mcp)
        .env_clear()
        .env("HOME", program.parent().unwrap_or(Path::new("/")))
        .output()
        .map_err(io_error)?;
    if !output.status.success() {
        return Err(error(
            "managed_update_incomplete",
            "受管接入更新未完成，版本和恢复材料保留。",
        ));
    }
    Ok(())
}
pub fn start_automatic_check() {
    let Some(root) = installed_root() else { return };
    let Ok(installer) = Installer::new(root, default_adapter_state().unwrap_or_default()) else {
        return;
    };
    tokio::spawn(async move {
        if let Ok(Some(value)) = installer.automatic_check().await
            && value["status"] == "update_available"
        {
            eprintln!(
                "图片连接器有可用更新，请运行 {}。",
                management_command("update check")
            );
        }
    });
}

fn remove_empty_tree(path: &Path) {
    if let Ok(entries) = fs::read_dir(path) {
        for e in entries.flatten() {
            if e.file_type().is_ok_and(|t| t.is_dir()) {
                remove_empty_tree(&e.path());
            }
        }
    }
    let _ = fs::remove_dir(path);
}

#[cfg(test)]
#[path = "distribution_tests.rs"]
mod tests;

fn validate_installation(s: &Installation) -> Result<()> {
    if s.schema_version != 1
        || s.product != PRODUCT
        || !s.adapter_state.is_absolute()
        || !s.data_paths.tasks.is_absolute()
        || !s.data_paths.cache.is_absolute()
        || semver::Version::parse(&s.current).is_err()
        || !s.versions.contains(&s.current)
        || s.versions
            .iter()
            .any(|v| semver::Version::parse(v).is_err())
        || s.previous
            .as_ref()
            .is_some_and(|p| semver::Version::parse(p).is_err() || !s.versions.contains(p))
    {
        return Err(conflict());
    }
    Ok(())
}
