//! Verified WorkBuddy macOS local registration; native trust approval remains a separate user action.

use super::*;
use sha2::{Digest, Sha256};

const METADATA: &str = include_str!("../../../plugins/workbuddy/connector-meta.json");
const ICON: &str = include_str!("../../../plugins/workbuddy/icon.svg");

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkBuddyPaths {
    pub home: PathBuf,
    /// Effective WORKBUDDY_CONFIG_DIR, not an Electron userData directory.
    pub config_dir: PathBuf,
    pub state_dir: PathBuf,
    pub app_bundle: PathBuf,
    pub executable: PathBuf,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkBuddyStatus {
    Ready,
    AwaitingConfirmation,
    Installed,
    AlreadyInstalled,
    Removed,
    NotInstalled,
}

#[derive(Debug, Serialize)]
pub struct WorkBuddyPlan {
    pub status: WorkBuddyStatus,
    pub version: String,
    pub config_path: PathBuf,
    pub skill_path: PathBuf,
}

#[derive(Debug, Clone)]
pub struct WorkBuddyAdapter {
    paths: WorkBuddyPaths,
}

/// The verified product identity selects its native default; explicit roots remain caller-owned.
pub(crate) fn detect_bundle(home: &Path, app_bundle: &Path) -> Result<(String, &'static str)> {
    if !cfg!(target_os = "macos") {
        return Err(error(
            "platform_not_verified",
            "WorkBuddy 当前适配入口已验证 macOS 应用包。",
        ));
    }
    let plist = app_bundle.join("Contents/Info.plist");
    if !plist.is_file() {
        return Err(error("client_missing", "未找到 WorkBuddy 应用包。"));
    }
    let read = |key: &str| -> Result<String> {
        let output = Command::new("/usr/libexec/PlistBuddy")
            .args(["-c", &format!("Print :{key}")])
            .arg(&plist)
            .env_clear()
            .env("HOME", home)
            .output()
            .map_err(|_| error("client_unavailable", "无法读取 WorkBuddy 应用元数据。"))?;
        if !output.status.success() {
            return Err(error(
                "unrecognized_client",
                "WorkBuddy 应用元数据无法识别。",
            ));
        }
        String::from_utf8(output.stdout)
            .map(|value| value.trim().to_owned())
            .map_err(|_| configuration_error())
    };
    let identity = read("CFBundleIdentifier")?;
    if !matches!(
        identity.as_str(),
        "com.tencent.workbuddy.mac" | "com.workbuddy.workbuddy-ai"
    ) {
        return Err(error("unrecognized_client", "此应用不是 WorkBuddy。"));
    }
    let version = read("CFBundleShortVersionString")?;
    let folder = match (identity.as_str(), version.as_str()) {
        ("com.tencent.workbuddy.mac", "5.11.3") => ".workbuddy",
        ("com.workbuddy.workbuddy-ai", "5.11.4") => ".workbuddy-ai",
        _ => {
            return Err(error(
                "unsupported_version",
                "WorkBuddy 适配已验证旧版 5.11.3 和 WorkBuddy AI 5.11.4；其他身份/版本组合需核对接入契约。",
            ));
        }
    };
    Ok((version, folder))
}

impl WorkBuddyAdapter {
    pub fn new(scope: Scope, paths: WorkBuddyPaths) -> Result<Self> {
        if scope != Scope::User {
            return Err(error(
                "unsupported_scope",
                "本适配器管理 WorkBuddy 全局连接器，可在任务中按需启用。",
            ));
        }
        for path in [
            &paths.home,
            &paths.config_dir,
            &paths.state_dir,
            &paths.app_bundle,
            &paths.executable,
        ] {
            if !path.is_absolute() || path.to_str().is_none() {
                return Err(error("invalid_path", "适配器需要明确的绝对路径。"));
            }
        }
        Ok(Self { paths })
    }

    pub fn config_path(&self) -> PathBuf {
        self.paths.config_dir.join("mcp.json")
    }

    pub fn skill_path(&self) -> PathBuf {
        self.paths
            .config_dir
            .join("skills")
            .join(REGISTRATION_NAME)
            .join("SKILL.md")
    }

    /// Reads bundle metadata without launching the GUI or changing protocol associations.
    pub fn detect(&self) -> Result<String> {
        detect_bundle(&self.paths.home, &self.paths.app_bundle).map(|(version, _)| version)
    }

    pub fn plan(&self) -> Result<WorkBuddyPlan> {
        let version = self.detect()?;
        self.validate_executable()?;
        let status = self.inspect(true)?;
        Ok(WorkBuddyPlan {
            status: if status == WorkBuddyStatus::NotInstalled {
                WorkBuddyStatus::Ready
            } else {
                status
            },
            version,
            config_path: self.config_path(),
            skill_path: self.skill_path(),
        })
    }

    /// Installed means the owned registration has matching native approval, not a live connection.
    pub fn check(&self) -> Result<WorkBuddyStatus> {
        self.detect()?;
        self.inspect(false)
    }

    pub fn install(&self) -> Result<WorkBuddyStatus> {
        match self.plan()?.status {
            WorkBuddyStatus::Installed => return Ok(WorkBuddyStatus::AlreadyInstalled),
            WorkBuddyStatus::AwaitingConfirmation => {
                return Ok(WorkBuddyStatus::AwaitingConfirmation);
            }
            _ => {}
        }
        let approval = self.approval_status(&self.expected_entry())?;
        let config = self.config_path();
        let skill = self.skill_path();
        let before = read_optional(&config)?;
        let mut state = read_state(&self.paths.state_dir)?;
        let snapshot = save_snapshot(&self.paths.state_dir, before.as_deref())?;
        let operation = (|| {
            self.merge_registration(before.as_deref(), true)?;
            write_private(&skill, SKILL_CONTENT.as_bytes())?;
            state.installations.push(Ownership {
                harness: ManagedHarness::WorkBuddy,
                scope: Scope::User,
                config_path: config.clone(),
                skill_path: skill.clone(),
                entry: self.expected_entry(),
                skill: SKILL_CONTENT.to_owned(),
                snapshot: snapshot.clone(),
                skill_owned: true,
                context: Some(managed::ManagedContext::WorkBuddy {
                    paths: self.paths.clone(),
                }),
            });
            write_state(&self.paths.state_dir, &state)?;
            Ok(approval)
        })();
        if operation.is_err() {
            if restore(&config, before.as_deref())
                .and_then(|_| restore(&skill, None))
                .is_err()
            {
                return Err(error(
                    "rollback_required",
                    "安装失败且自动恢复未完成；私有快照已保留。",
                ));
            }
            let _ = fs::remove_file(&snapshot);
            remove_empty_parent(&skill);
        }
        operation
    }

    pub fn update(&self) -> Result<WorkBuddyStatus> {
        managed::recover_owned_updates(&self.paths.state_dir)?;
        self.detect()?;
        self.validate_executable()?;
        if self.inspect(false)? == WorkBuddyStatus::NotInstalled {
            return Ok(WorkBuddyStatus::NotInstalled);
        }
        let state = read_state(&self.paths.state_dir)?;
        let index = self.owned_index(&state).ok_or_else(conflict)?;
        managed::change_owned(
            &self.paths.state_dir,
            index,
            self.expected_entry(),
            SKILL_CONTENT,
            Some(managed::ManagedContext::WorkBuddy {
                paths: self.paths.clone(),
            }),
        )?;
        self.approval_status(&self.expected_entry())
    }

    pub fn remove(&self) -> Result<WorkBuddyStatus> {
        self.detect()?;
        if self.inspect(false)? == WorkBuddyStatus::NotInstalled {
            return Ok(WorkBuddyStatus::NotInstalled);
        }
        let config = self.config_path();
        let before = read_optional(&config)?;
        let mut state = read_state(&self.paths.state_dir)?;
        let index = self.owned_index(&state).ok_or_else(conflict)?;
        let owned = state.installations.remove(index);
        let operation = (|| {
            self.merge_registration(before.as_deref(), false)?;
            release_skill(&mut state, &owned)?;
            write_state(&self.paths.state_dir, &state)?;
            Ok(WorkBuddyStatus::Removed)
        })();
        if operation.is_err() {
            if restore(&config, before.as_deref())
                .and_then(|_| write_private(&owned.skill_path, owned.skill.as_bytes()))
                .is_err()
            {
                return Err(error(
                    "rollback_required",
                    "移除失败且自动恢复未完成；私有记录已保留。",
                ));
            }
        } else {
            let _ = fs::remove_file(&owned.snapshot);
            remove_empty_parent(&owned.skill_path);
        }
        operation
    }

    /// Exports the official MCP + Skill package to a new directory; this does not register it.
    pub fn export_package(&self, destination: &Path) -> Result<PathBuf> {
        self.validate_executable()?;
        if !destination.is_absolute() || destination.exists() {
            return Err(error(
                "package_destination_conflict",
                "连接器包需要一个尚不存在的绝对目录。",
            ));
        }
        let parent = destination.parent().ok_or_else(storage_error)?;
        fs::create_dir_all(parent).map_err(|_| storage_error())?;
        let temporary = tempfile::Builder::new()
            .prefix(".tokemula-workbuddy-")
            .tempdir_in(parent)
            .map_err(|_| storage_error())?;
        let config = json!({"mcpServers":{REGISTRATION_NAME:{"type":"stdio","command":self.paths.executable}}});
        for (name, bytes) in [
            ("connector-meta.json", METADATA.as_bytes().to_vec()),
            (
                "mcp.json",
                serde_json::to_vec_pretty(&config).map_err(|_| storage_error())?,
            ),
            ("icon.svg", ICON.as_bytes().to_vec()),
            (
                "skills/tokemula-image/SKILL.md",
                SKILL_CONTENT.as_bytes().to_vec(),
            ),
        ] {
            write_private(&temporary.path().join(name), &bytes)?;
        }
        fs::rename(temporary.path(), destination).map_err(|_| storage_error())?;
        Ok(destination.to_owned())
    }

    fn validate_executable(&self) -> Result<()> {
        let metadata = fs::metadata(&self.paths.executable)
            .map_err(|_| error("executable_missing", "未找到独立 MCP 可执行程序。"))?;
        if !metadata.is_file() {
            return Err(error(
                "invalid_executable",
                "MCP 路径必须指向独立可执行文件。",
            ));
        }
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            if metadata.permissions().mode() & 0o111 == 0 {
                return Err(error("invalid_executable", "MCP 文件没有执行权限。"));
            }
        }
        Ok(())
    }

    fn expected_entry(&self) -> Value {
        json!({"command":self.paths.executable})
    }

    fn read_entry(&self, bytes: Option<&[u8]>) -> Result<Option<Value>> {
        let root = parse_json(bytes)?;
        match root.get("mcpServers") {
            None => Ok(None),
            Some(Value::Object(servers)) => Ok(servers.get(REGISTRATION_NAME).cloned()),
            Some(_) => Err(configuration_error()),
        }
    }

    fn merge_registration(&self, before: Option<&[u8]>, install: bool) -> Result<()> {
        let mut root = parse_json(before)?;
        let servers = root
            .as_object_mut()
            .ok_or_else(configuration_error)?
            .entry("mcpServers")
            .or_insert_with(|| json!({}))
            .as_object_mut()
            .ok_or_else(configuration_error)?;
        if install {
            servers.insert(REGISTRATION_NAME.into(), self.expected_entry());
        } else {
            servers.remove(REGISTRATION_NAME);
        }
        let bytes = serde_json::to_vec_pretty(&root).map_err(|_| storage_error())?;
        write_private(&self.config_path(), &bytes)
    }

    fn owned_index(&self, state: &State) -> Option<usize> {
        state.installations.iter().position(|owned| {
            owned.harness == ManagedHarness::WorkBuddy
                && owned.scope == Scope::User
                && owned.config_path == self.config_path()
        })
    }

    fn inspect(&self, for_install: bool) -> Result<WorkBuddyStatus> {
        let entry = self.read_entry(read_optional(&self.config_path())?.as_deref())?;
        let skill = read_optional(&self.skill_path())?;
        let state = read_state(&self.paths.state_dir)?;
        if let Some(index) = self.owned_index(&state) {
            let owned = &state.installations[index];
            if entry.as_ref() != Some(&owned.entry)
                || skill.as_deref() != Some(owned.skill.as_bytes())
                || owned.skill_path != self.skill_path()
            {
                return Err(conflict());
            }
            if for_install && (owned.entry != self.expected_entry() || owned.skill != SKILL_CONTENT)
            {
                return Err(error(
                    "installation_update_required",
                    "当前受管安装与请求的程序或 Skill 版本不同；请使用受管更新入口。",
                ));
            }
            return self.approval_status(&owned.entry);
        }
        if entry.is_some()
            || skill.is_some()
            || (for_install && self.skill_path().parent().is_some_and(Path::exists))
        {
            return Err(conflict());
        }
        Ok(WorkBuddyStatus::NotInstalled)
    }

    fn approval_status(&self, entry: &Value) -> Result<WorkBuddyStatus> {
        let command = entry["command"].as_str().ok_or_else(conflict)?;
        // WorkBuddy's native approval binds this exact stdio transport, not a file-integrity check.
        let fingerprint: String = Sha256::digest(format!("{command}||").as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let path = self.paths.config_dir.join("mcp-approvals.json");
        let approved = read_optional(&path)?.is_some_and(|bytes| {
            serde_json::from_slice::<Value>(&bytes)
                .ok()
                .and_then(|root| {
                    root.get(format!("{fingerprint}::{REGISTRATION_NAME}"))
                        .cloned()
                })
                .is_some_and(|timestamp| timestamp.is_number())
        });
        Ok(if approved {
            WorkBuddyStatus::Installed
        } else {
            WorkBuddyStatus::AwaitingConfirmation
        })
    }
}
