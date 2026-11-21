//! Installer-facing registration and shared private ownership. Every filesystem root is explicit.

pub mod deepseek;
mod managed;
pub mod opencode;
pub mod workbuddy;
pub use managed::{
    ManagedOperationReport, OwnedInstallation, cleanup_empty_owned_state, owned_inventory,
    recover_owned_updates, remove_owned, update_owned,
};

use crate::output::prepare_private_directory;
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    io::Write,
    path::{Path, PathBuf},
    process::Command,
};
use toml_edit::{DocumentMut, Item, Table, value};

pub const REGISTRATION_NAME: &str = "tokemula-image";
pub const SKILL_CONTENT: &str = include_str!("../../skills/tokemula-image/SKILL.md");

#[cfg(test)]
#[path = "adapters/shared_tests.rs"]
mod shared_tests;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    Codex,
    ClaudeCode,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    User,
    Project,
}

/// Resolved roots, supplied by the caller. No ambient credentials or configuration are inherited.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AdapterPaths {
    pub home: PathBuf,
    pub codex_home: PathBuf,
    /// None preserves Claude's default HOME/.claude.json state location.
    pub claude_config_dir: Option<PathBuf>,
    pub project_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cli: PathBuf,
    pub executable: PathBuf,
    /// Needed by installed CLI launchers such as `#!/usr/bin/env node`.
    pub process_path: OsString,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum InstallStatus {
    Ready,
    Installed,
    AlreadyInstalled,
    Removed,
    NotInstalled,
}

#[derive(Debug, Serialize)]
pub struct InstallPlan {
    pub status: InstallStatus,
    pub version: String,
    pub config_path: PathBuf,
    pub skill_path: PathBuf,
}

#[derive(Debug, Clone, Serialize)]
pub struct AdapterError {
    pub code: &'static str,
    pub message: &'static str,
}
impl std::fmt::Display for AdapterError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.message)
    }
}
impl std::error::Error for AdapterError {}
type Result<T> = std::result::Result<T, AdapterError>;

fn error(code: &'static str, message: &'static str) -> AdapterError {
    AdapterError { code, message }
}
fn storage_error() -> AdapterError {
    error("storage_error", "无法读写适配配置或私有恢复记录。")
}
fn configuration_error() -> AdapterError {
    error(
        "unrecognized_configuration",
        "配置格式无法识别；保留原文件。",
    )
}
fn conflict() -> AdapterError {
    error(
        "ownership_conflict",
        "同名注册或 Skill 不属于本产品，或已被用户修改。",
    )
}

#[derive(Debug, Clone)]
pub struct Adapter {
    harness: Harness,
    scope: Scope,
    paths: AdapterPaths,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum ManagedHarness {
    Codex,
    ClaudeCode,
    OpenCode,
    DeepSeek,
    WorkBuddy,
}

impl From<Harness> for ManagedHarness {
    fn from(value: Harness) -> Self {
        match value {
            Harness::Codex => Self::Codex,
            Harness::ClaudeCode => Self::ClaudeCode,
        }
    }
}

#[derive(Clone, Serialize, Deserialize)]
struct Ownership {
    harness: ManagedHarness,
    scope: Scope,
    config_path: PathBuf,
    skill_path: PathBuf,
    entry: Value,
    skill: String,
    snapshot: PathBuf,
    #[serde(default = "owned_skill", skip_serializing_if = "is_owned_skill")]
    skill_owned: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    context: Option<managed::ManagedContext>,
}
#[derive(Clone, Default, Serialize, Deserialize)]
struct State {
    installations: Vec<Ownership>,
}

impl Adapter {
    pub fn new(harness: Harness, scope: Scope, paths: AdapterPaths) -> Result<Self> {
        for path in [
            &paths.home,
            &paths.codex_home,
            &paths.project_dir,
            &paths.state_dir,
            &paths.cli,
            &paths.executable,
        ]
        .into_iter()
        .chain(paths.claude_config_dir.iter())
        {
            if !path.is_absolute() || path.to_str().is_none() {
                return Err(error("invalid_path", "适配器需要明确的绝对路径。"));
            }
        }
        Ok(Self {
            harness,
            scope,
            paths,
        })
    }

    pub fn config_path(&self) -> PathBuf {
        match (self.harness, self.scope) {
            (Harness::Codex, Scope::User) => self.paths.codex_home.join("config.toml"),
            (Harness::Codex, Scope::Project) => self.paths.project_dir.join(".codex/config.toml"),
            (Harness::ClaudeCode, Scope::User) => self
                .paths
                .claude_config_dir
                .as_ref()
                .map(|root| root.join(".claude.json"))
                .unwrap_or_else(|| self.paths.home.join(".claude.json")),
            (Harness::ClaudeCode, Scope::Project) => self.paths.project_dir.join(".mcp.json"),
        }
    }

    pub fn skill_path(&self) -> PathBuf {
        let root = match (self.harness, self.scope) {
            (Harness::Codex, Scope::User) => self.paths.home.join(".agents/skills"),
            (Harness::Codex, Scope::Project) => self.paths.project_dir.join(".agents/skills"),
            (Harness::ClaudeCode, Scope::User) => self
                .paths
                .claude_config_dir
                .clone()
                .unwrap_or_else(|| self.paths.home.join(".claude"))
                .join("skills"),
            (Harness::ClaudeCode, Scope::Project) => self.paths.project_dir.join(".claude/skills"),
        };
        root.join(REGISTRATION_NAME).join("SKILL.md")
    }

    /// Reads the installed CLI's version; does not start a model session or connect MCP servers.
    pub fn detect(&self) -> Result<String> {
        if !self.paths.cli.is_file() {
            return Err(error("client_missing", "未找到指定客户端 CLI。"));
        }
        let mut command = self.command();
        if self.harness == Harness::ClaudeCode {
            command.arg("--bare");
        }
        let output = command
            .arg("--version")
            .output()
            .map_err(|_| error("client_unavailable", "无法执行指定客户端 CLI。"))?;
        if !output.status.success() {
            return Err(error("client_unavailable", "客户端版本检测失败。"));
        }
        let version = String::from_utf8(output.stdout).map_err(|_| configuration_error())?;
        let version = version.trim();
        let expected = match self.harness {
            Harness::Codex => version.starts_with("codex-cli "),
            Harness::ClaudeCode => version.ends_with("(Claude Code)"),
        };
        if !expected || version.len() > 128 {
            return Err(error("unrecognized_client", "客户端版本输出无法识别。"));
        }
        Ok(version.to_owned())
    }

    pub fn plan(&self) -> Result<InstallPlan> {
        let version = self.detect()?;
        self.validate_executable()?;
        let status = self.inspect(true)?;
        Ok(InstallPlan {
            status: if status == InstallStatus::NotInstalled {
                InstallStatus::Ready
            } else {
                status
            },
            version,
            config_path: self.config_path(),
            skill_path: self.skill_path(),
        })
    }

    /// Checks registration and ownership only. Harness connection diagnostics are a separate gate.
    pub fn check(&self) -> Result<InstallStatus> {
        self.detect()?;
        self.inspect(false)
    }

    pub fn install(&self) -> Result<InstallStatus> {
        let plan = self.plan()?;
        if plan.status == InstallStatus::Installed {
            return Ok(InstallStatus::AlreadyInstalled);
        }
        let config = self.config_path();
        let skill = self.skill_path();
        let before = read_optional(&config)?;
        let mut state = read_state(&self.paths.state_dir)?;
        prepare_private_directory(&self.paths.state_dir).map_err(|_| storage_error())?;
        fs::create_dir_all(self.paths.state_dir.join("tmp")).map_err(|_| storage_error())?;
        let snapshot = save_snapshot(&self.paths.state_dir, before.as_deref())?;
        let operation = (|| {
            self.registration(true, before.as_deref())?;
            let entry = self
                .read_entry(read_optional(&config)?.as_deref())?
                .ok_or_else(conflict)?;
            if !self.is_expected_entry(&entry) {
                return Err(conflict());
            }
            write_private(&skill, SKILL_CONTENT.as_bytes())?;
            state.installations.push(Ownership {
                harness: self.harness.into(),
                scope: self.scope,
                config_path: config.clone(),
                skill_path: skill.clone(),
                entry,
                skill: SKILL_CONTENT.to_owned(),
                snapshot: snapshot.clone(),
                skill_owned: true,
                context: Some(managed::ManagedContext::Standard {
                    harness: self.harness,
                    scope: self.scope,
                    paths: self.paths.clone(),
                }),
            });
            write_state(&self.paths.state_dir, &state)?;
            Ok(InstallStatus::Installed)
        })();
        if operation.is_err() {
            let restored = restore(&config, before.as_deref()).and_then(|_| restore(&skill, None));
            if restored.is_err() {
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

    /// Updates only a still-owned registration and its shared Skill references.
    pub fn update(&self) -> Result<InstallStatus> {
        managed::recover_owned_updates(&self.paths.state_dir)?;
        self.detect()?;
        self.validate_executable()?;
        if self.inspect(false)? == InstallStatus::NotInstalled {
            return Ok(InstallStatus::NotInstalled);
        }
        let state = read_state(&self.paths.state_dir)?;
        let index = self.owned_index(&state).ok_or_else(conflict)?;
        let mut entry = state.installations[index].entry.clone();
        entry["command"] = json!(self.paths.executable);
        managed::change_owned(
            &self.paths.state_dir,
            index,
            entry,
            SKILL_CONTENT,
            Some(managed::ManagedContext::Standard {
                harness: self.harness,
                scope: self.scope,
                paths: self.paths.clone(),
            }),
        )
    }

    pub fn remove(&self) -> Result<InstallStatus> {
        self.detect()?;
        if self.inspect(false)? == InstallStatus::NotInstalled {
            return Ok(InstallStatus::NotInstalled);
        }
        let config = self.config_path();
        let skill = self.skill_path();
        let before = read_optional(&config)?;
        let mut state = read_state(&self.paths.state_dir)?;
        let index = self.owned_index(&state).ok_or_else(conflict)?;
        let owned = state.installations.remove(index);
        let operation = (|| {
            self.registration(false, before.as_deref())?;
            if self
                .read_entry(read_optional(&config)?.as_deref())?
                .is_some()
            {
                return Err(conflict());
            }
            release_skill(&mut state, &owned)?;
            write_state(&self.paths.state_dir, &state)?;
            Ok(InstallStatus::Removed)
        })();
        if operation.is_err() {
            if restore(&config, before.as_deref())
                .and_then(|_| write_private(&skill, owned.skill.as_bytes()))
                .is_err()
            {
                return Err(error(
                    "rollback_required",
                    "移除失败且自动恢复未完成；私有记录已保留。",
                ));
            }
        } else {
            let _ = fs::remove_file(&owned.snapshot);
            remove_empty_parent(&skill);
        }
        operation
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

    fn inspect(&self, for_install: bool) -> Result<InstallStatus> {
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
            if for_install
                && (!self.is_expected_entry(&owned.entry) || owned.skill != SKILL_CONTENT)
            {
                return Err(error(
                    "installation_update_required",
                    "当前受管安装与请求的程序或 Skill 版本不同；请使用受管更新入口。",
                ));
            }
            return Ok(InstallStatus::Installed);
        }
        if entry.is_some()
            || skill.is_some()
            || self.skill_path().parent().is_some_and(Path::exists)
        {
            return Err(conflict());
        }
        Ok(InstallStatus::NotInstalled)
    }

    fn owned_index(&self, state: &State) -> Option<usize> {
        state.installations.iter().position(|s| {
            s.harness == self.harness.into()
                && s.scope == self.scope
                && s.config_path == self.config_path()
        })
    }

    fn is_expected_entry(&self, entry: &Value) -> bool {
        let Some(object) = entry.as_object() else {
            return false;
        };
        let keys = match self.harness {
            Harness::Codex => &["command", "args"][..],
            Harness::ClaudeCode => &["command", "type", "args", "env"][..],
        };
        object.keys().all(|key| keys.contains(&key.as_str()))
            && entry["command"] == self.paths.executable.to_string_lossy().as_ref()
            && entry
                .get("args")
                .is_none_or(|v| v.as_array().is_some_and(Vec::is_empty))
            && entry
                .get("env")
                .is_none_or(|v| v.as_object().is_some_and(serde_json::Map::is_empty))
            && entry.get("type").is_none_or(|v| v == "stdio")
    }

    fn read_entry(&self, bytes: Option<&[u8]>) -> Result<Option<Value>> {
        match self.harness {
            Harness::Codex => {
                let doc = parse_toml(bytes)?;
                let Some(servers) = doc.get("mcp_servers") else {
                    return Ok(None);
                };
                let servers = servers.as_table_like().ok_or_else(configuration_error)?;
                let Some(item) = servers.get(REGISTRATION_NAME) else {
                    return Ok(None);
                };
                let mut entry_doc = DocumentMut::new();
                entry_doc["entry"] = item.clone();
                let parsed: Value = toml_edit::de::from_str(&entry_doc.to_string())
                    .map_err(|_| configuration_error())?;
                Ok(Some(parsed["entry"].clone()))
            }
            Harness::ClaudeCode => {
                let doc = parse_json(bytes)?;
                match doc.get("mcpServers") {
                    None => Ok(None),
                    Some(servers) => Ok(servers
                        .as_object()
                        .ok_or_else(configuration_error)?
                        .get(REGISTRATION_NAME)
                        .cloned()),
                }
            }
        }
    }

    fn registration(&self, add: bool, before: Option<&[u8]>) -> Result<()> {
        if self.harness == Harness::Codex && self.scope == Scope::Project {
            return self.patch_codex(add, before);
        }
        let parent = self
            .config_path()
            .parent()
            .ok_or_else(storage_error)?
            .to_path_buf();
        fs::create_dir_all(parent).map_err(|_| storage_error())?;
        let mut cmd = self.command();
        if self.harness == Harness::ClaudeCode {
            cmd.arg("--bare");
        }
        cmd.args(["mcp", if add { "add" } else { "remove" }]);
        if self.harness == Harness::ClaudeCode {
            cmd.args([
                "--scope",
                if self.scope == Scope::User {
                    "user"
                } else {
                    "project"
                },
            ]);
        }
        cmd.arg(REGISTRATION_NAME);
        if add {
            cmd.arg("--").arg(&self.paths.executable);
        }
        let output = cmd
            .output()
            .map_err(|_| error("cli_failed", "客户端配置命令无法执行。"))?;
        if !output.status.success() {
            return Err(error("cli_failed", "客户端拒绝配置操作；原配置将恢复。"));
        }
        if self.harness == Harness::Codex {
            // Codex CLI rewrites other MCP comments; retain the original document around our entry.
            return self.patch_codex(add, before);
        }
        Ok(())
    }

    fn patch_codex(&self, add: bool, before: Option<&[u8]>) -> Result<()> {
        let mut doc = parse_toml(before)?;
        if add {
            if doc.get("mcp_servers").is_none() {
                doc["mcp_servers"] = Item::Table(Table::new());
            }
            let servers = doc["mcp_servers"]
                .as_table_like_mut()
                .ok_or_else(configuration_error)?;
            let mut entry = Table::new();
            entry["command"] = value(self.paths.executable.to_string_lossy().as_ref());
            servers.insert(REGISTRATION_NAME, Item::Table(entry));
        } else if let Some(servers) = doc.get_mut("mcp_servers") {
            servers
                .as_table_like_mut()
                .ok_or_else(configuration_error)?
                .remove(REGISTRATION_NAME);
        }
        write_private(&self.config_path(), doc.to_string().as_bytes())
    }

    fn command(&self) -> Command {
        let mut command = Command::new(&self.paths.cli);
        command
            .env_clear()
            .current_dir(&self.paths.project_dir)
            .env("PATH", &self.paths.process_path)
            .env("HOME", &self.paths.home)
            .env("USERPROFILE", &self.paths.home)
            .env("CODEX_HOME", &self.paths.codex_home)
            .env("XDG_CONFIG_HOME", self.paths.home.join(".config"))
            .env("XDG_CACHE_HOME", self.paths.home.join(".cache"))
            .env("XDG_STATE_HOME", self.paths.home.join(".local/state"))
            .env("XDG_DATA_HOME", self.paths.home.join(".local/share"))
            .env("APPDATA", self.paths.home.join("AppData/Roaming"))
            .env("LOCALAPPDATA", self.paths.home.join("AppData/Local"))
            .env("TMPDIR", self.paths.state_dir.join("tmp"))
            .env("CLAUDE_CODE_DISABLE_NONESSENTIAL_TRAFFIC", "1")
            .env("DISABLE_AUTOUPDATER", "1")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .env("NO_PROXY", "localhost,127.0.0.1,::1");
        if let Some(root) = &self.paths.claude_config_dir {
            command.env("CLAUDE_CONFIG_DIR", root);
        }
        command
    }
}

fn owned_skill() -> bool {
    true
}
fn is_owned_skill(owned: &bool) -> bool {
    *owned
}

fn read_state(root: &Path) -> Result<State> {
    match read_optional(&root.join("ownership.json"))? {
        None => Ok(State::default()),
        Some(bytes) => serde_json::from_slice(&bytes).map_err(|_| {
            error(
                "invalid_ownership",
                "本产品归属记录无法识别；保留当前配置。",
            )
        }),
    }
}

fn write_state(root: &Path, state: &State) -> Result<()> {
    prepare_private_directory(root).map_err(|_| storage_error())?;
    let bytes = serde_json::to_vec_pretty(state).map_err(|_| storage_error())?;
    write_private(&root.join("ownership.json"), &bytes)
}

fn save_snapshot(root: &Path, original: Option<&[u8]>) -> Result<PathBuf> {
    prepare_private_directory(root).map_err(|_| storage_error())?;
    let root = root.join("snapshots");
    prepare_private_directory(&root).map_err(|_| storage_error())?;
    let mut file = tempfile::Builder::new()
        .prefix("configuration-")
        .suffix(".private")
        .tempfile_in(root)
        .map_err(|_| storage_error())?;
    file.write_all(original.unwrap_or_default())
        .map_err(|_| storage_error())?;
    file.as_file().sync_all().map_err(|_| storage_error())?;
    file.keep().map(|(_, p)| p).map_err(|_| storage_error())
}

// Shared Skill users keep the file; its last managed reference owns eventual cleanup.
fn release_skill(state: &mut State, removed: &Ownership) -> Result<()> {
    if let Some(remaining) = state
        .installations
        .iter_mut()
        .find(|entry| entry.skill_path == removed.skill_path)
    {
        remaining.skill_owned |= removed.skill_owned;
    } else if removed.skill_owned {
        fs::remove_file(&removed.skill_path).map_err(|_| storage_error())?;
    }
    Ok(())
}

fn parse_toml(bytes: Option<&[u8]>) -> Result<DocumentMut> {
    std::str::from_utf8(bytes.unwrap_or_default())
        .map_err(|_| configuration_error())?
        .parse()
        .map_err(|_| configuration_error())
}
fn parse_json(bytes: Option<&[u8]>) -> Result<Value> {
    let value = match bytes {
        None => json!({}),
        Some(bytes) => serde_json::from_slice(bytes).map_err(|_| configuration_error())?,
    };
    if value.is_object() {
        Ok(value)
    } else {
        Err(configuration_error())
    }
}
fn read_optional(path: &Path) -> Result<Option<Vec<u8>>> {
    match fs::symlink_metadata(path) {
        Ok(metadata) if metadata.is_file() && !metadata.file_type().is_symlink() => {
            fs::read(path).map(Some).map_err(|_| storage_error())
        }
        Ok(_) => Err(storage_error()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(_) => Err(storage_error()),
    }
}
fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let parent = path.parent().ok_or_else(storage_error)?;
    fs::create_dir_all(parent).map_err(|_| storage_error())?;
    if fs::symlink_metadata(path).is_ok_and(|m| !m.is_file() || m.file_type().is_symlink()) {
        return Err(storage_error());
    }
    let mut file = tempfile::NamedTempFile::new_in(parent).map_err(|_| storage_error())?;
    file.write_all(bytes).map_err(|_| storage_error())?;
    file.as_file().sync_all().map_err(|_| storage_error())?;
    file.persist(path).map_err(|_| storage_error())?;
    Ok(())
}
fn restore(path: &Path, before: Option<&[u8]>) -> Result<()> {
    match before {
        Some(bytes) => write_private(path, bytes),
        None => match fs::remove_file(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(storage_error()),
        },
    }
}
fn remove_empty_parent(path: &Path) {
    if let Some(parent) = path.parent() {
        let _ = fs::remove_dir(parent);
    }
}
