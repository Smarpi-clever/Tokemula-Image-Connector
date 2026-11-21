//! DeepSeek's native profile plugin manager owns dependency and bundle changes.

use super::*;

const PACKAGE: &str = "tokemula-image-connector";
const VERSION: &str = "0.2.2-rc.5";
const PLUGIN: &str = include_str!("../../../integrations/deepseek/index.js");
const RECOVER: &str = include_str!("../../../integrations/deepseek/recover.mjs");
const FILES: [&str; 3] = ["package.json", "cordis.patch.yml", "index.js"];

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum DeepSeekRuntime {
    CliWeb,
    DesktopBundled,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepSeekPaths {
    pub home: PathBuf,
    pub dsh_home: PathBuf,
    pub project_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cli: PathBuf,
    pub executable: PathBuf,
    pub process_path: OsString,
    pub profile: String,
    pub runtime: DeepSeekRuntime,
}

#[derive(Debug, Serialize)]
pub struct DeepSeekPlan {
    pub status: InstallStatus,
    pub version: String,
    pub profile_path: PathBuf,
    pub package_path: PathBuf,
    pub skill_path: PathBuf,
    pub bundle_enabled: bool,
}

#[derive(Debug, Serialize, Deserialize, PartialEq)]
struct NativeRegistration {
    dependency: Value,
    bundle_index: Option<usize>,
}

#[derive(Serialize, Deserialize)]
struct RemovalRecovery {
    profile: String,
    before: NativeRegistration,
    owned: Ownership,
    snapshot: PathBuf,
}

#[derive(Debug, Clone)]
pub struct DeepSeekAdapter {
    paths: DeepSeekPaths,
}

impl DeepSeekAdapter {
    pub fn new(scope: Scope, paths: DeepSeekPaths) -> Result<Self> {
        if scope == Scope::Project {
            return Err(error(
                "unsupported_scope",
                "DeepSeek MCP 按 profile 全局注册；项目 Skill 与 MCP 作用域分开。",
            ));
        }
        for path in [
            &paths.home,
            &paths.dsh_home,
            &paths.project_dir,
            &paths.state_dir,
            &paths.cli,
            &paths.executable,
        ] {
            if !path.is_absolute() || path.to_str().is_none() {
                return Err(error("invalid_path", "适配器需要明确的绝对路径。"));
            }
        }
        if paths.profile.is_empty()
            || !paths
                .profile
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        {
            return Err(error(
                "invalid_profile",
                "请选择一个明确的 DeepSeek profile 名称。",
            ));
        }
        if paths.profile == "desktop" && paths.runtime != DeepSeekRuntime::DesktopBundled {
            return Err(error(
                "desktop_runtime_required",
                "desktop profile 只由已初始化并退出的 Desktop 自带 CLI 管理。",
            ));
        }
        if paths.runtime == DeepSeekRuntime::DesktopBundled
            && !paths.cli.ends_with("runtime/cli/bin/dsh")
            && !paths.cli.ends_with("runtime/cli/bin/dsh.cmd")
        {
            return Err(error(
                "desktop_runtime_required",
                "请提供 Desktop 安装内的 bundled CLI，保留其运行时和 profile。",
            ));
        }
        Ok(Self { paths })
    }

    pub fn profile_path(&self) -> PathBuf {
        self.paths
            .dsh_home
            .join("profiles")
            .join(&self.paths.profile)
    }
    pub fn config_path(&self) -> PathBuf {
        self.profile_path().join("package.json")
    }
    pub fn package_path(&self) -> PathBuf {
        self.paths
            .state_dir
            .join("deepseek-packages")
            .join(&self.paths.profile)
    }
    pub fn skill_path(&self) -> PathBuf {
        self.paths
            .dsh_home
            .join("skills")
            .join(REGISTRATION_NAME)
            .join("SKILL.md")
    }

    /// Uses the same embedded factories as installation; does not start a Harness runtime.
    pub(crate) fn materialize_assets(&self, destination: &Path) -> Result<PathBuf> {
        self.validate_executable()?;
        if !destination.is_absolute() || destination.exists() {
            return Err(error("invalid_path", "资源导出需要不存在的绝对目录。"));
        }
        prepare_private_directory(destination).map_err(|_| storage_error())?;
        for (name, text) in self.expected_files()? {
            write_private(&destination.join(name), text.as_bytes())?;
        }
        write_private(&destination.join("recover.mjs"), RECOVER.as_bytes())?;
        Ok(destination.to_path_buf())
    }

    pub fn detect(&self) -> Result<String> {
        if !self.paths.cli.is_file() {
            return Err(error("client_missing", "未找到指定的 DeepSeek CLI。"));
        }
        let output = self
            .command()
            .arg("--version")
            .output()
            .map_err(|_| error("client_unavailable", "无法执行 DeepSeek CLI。"))?;
        if !output.status.success() {
            return Err(error("client_unavailable", "DeepSeek 版本检测失败。"));
        }
        let value = String::from_utf8(output.stdout).map_err(|_| configuration_error())?;
        if value.trim() != VERSION {
            return Err(error(
                "unsupported_client_version",
                "此适配包使用 DeepSeek Harness 0.2.2-rc.5 契约。",
            ));
        }
        Ok(value.trim().to_owned())
    }

    pub fn plan(&self) -> Result<DeepSeekPlan> {
        let version = self.detect()?;
        self.require_profile()?;
        if self.paths.runtime == DeepSeekRuntime::DesktopBundled {
            return Err(error(
                "desktop_validation_required",
                "Desktop 需在完整隔离环境完成原生接入确认；当前仅完成 CLI/Web 适配。",
            ));
        }
        self.validate_executable()?;
        let status = self.inspect(true)?;
        self.check_native_namespace(status != InstallStatus::NotInstalled)?;
        Ok(DeepSeekPlan {
            status: if status == InstallStatus::NotInstalled {
                InstallStatus::Ready
            } else {
                status
            },
            version,
            profile_path: self.profile_path(),
            package_path: self.package_path(),
            skill_path: self.skill_path(),
            bundle_enabled: self.bundle_enabled()?,
        })
    }

    pub fn check(&self) -> Result<InstallStatus> {
        self.detect()?;
        self.require_profile()?;
        let status = self.inspect(false)?;
        if status == InstallStatus::Installed {
            self.check_native_namespace(true)?;
        }
        Ok(status)
    }

    pub fn install(&self) -> Result<InstallStatus> {
        if self.plan()?.status == InstallStatus::Installed {
            return Ok(InstallStatus::AlreadyInstalled);
        }
        let before = read_optional(&self.config_path())?;
        let mut state = read_state(&self.paths.state_dir)?;
        prepare_private_directory(&self.paths.state_dir).map_err(|_| storage_error())?;
        let snapshot = save_snapshot(&self.paths.state_dir, before.as_deref())?;
        let skill_before = read_optional(&self.skill_path())?;
        let files = self.expected_files()?;
        let operation = (|| {
            for (name, content) in &files {
                write_private(&self.package_path().join(name), content.as_bytes())?;
            }
            self.manage(true)?;
            let entry = self.entry()?.ok_or_else(conflict)?;
            if !self.bundle_enabled()? {
                return Err(error(
                    "plugin_not_enabled",
                    "DeepSeek 尚未选择本产品 bundle。",
                ));
            }
            if skill_before.is_none() {
                write_private(&self.skill_path(), SKILL_CONTENT.as_bytes())?;
            }
            state.installations.push(Ownership {
                harness: ManagedHarness::DeepSeek,
                scope: Scope::User,
                config_path: self.config_path(),
                skill_path: self.skill_path(),
                entry,
                skill: SKILL_CONTENT.into(),
                snapshot: snapshot.clone(),
                skill_owned: skill_before.is_none(),
                context: Some(managed::ManagedContext::DeepSeek {
                    paths: self.paths.clone(),
                }),
            });
            write_state(&self.paths.state_dir, &state)?;
            Ok(InstallStatus::Installed)
        })();
        if operation.is_err() {
            let rollback: Result<()> = (|| {
                if self.dependency()?.is_some() {
                    self.manage(false)?;
                }
                if skill_before.is_none() {
                    restore(&self.skill_path(), None)?;
                    remove_empty_parent(&self.skill_path());
                }
                self.remove_package_files(&files)?;
                Ok(())
            })();
            if rollback.is_err() {
                return Err(error(
                    "rollback_required",
                    "DeepSeek 安装失败且原生撤回未完成；私有记录与插件包已保留。",
                ));
            }
            let _ = fs::remove_file(snapshot);
        }
        operation
    }

    pub fn update(&self) -> Result<InstallStatus> {
        managed::recover_owned_updates(&self.paths.state_dir)?;
        self.detect()?;
        self.require_profile()?;
        if self.paths.runtime == DeepSeekRuntime::DesktopBundled {
            return Err(error(
                "desktop_validation_required",
                "Desktop 仍需独立环境验收。",
            ));
        }
        self.validate_executable()?;
        if self.inspect(false)? == InstallStatus::NotInstalled {
            return Ok(InstallStatus::NotInstalled);
        }
        self.check_native_namespace(true)?;
        let state = read_state(&self.paths.state_dir)?;
        let index = self.owned_index(&state).ok_or_else(conflict)?;
        let mut entry = state.installations[index].entry.clone();
        entry["files"] = json!(self.expected_files()?);
        managed::change_owned(
            &self.paths.state_dir,
            index,
            entry,
            SKILL_CONTENT,
            Some(managed::ManagedContext::DeepSeek {
                paths: self.paths.clone(),
            }),
        )
    }

    pub(super) fn refresh_managed_files(&self, entry: &Value) -> Result<()> {
        let before = self.native_registration()?;
        let files: std::collections::BTreeMap<String, String> =
            serde_json::from_value(entry["files"].clone()).map_err(|_| configuration_error())?;
        if files
            .keys()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>()
            != FILES.into_iter().collect()
        {
            return Err(conflict());
        }
        for (name, value) in &files {
            write_private(&self.package_path().join(name), value.as_bytes())?;
        }
        self.recover_native(&before)
    }

    pub fn remove(&self) -> Result<InstallStatus> {
        if self.check()? == InstallStatus::NotInstalled {
            return Ok(InstallStatus::NotInstalled);
        }
        let mut state = read_state(&self.paths.state_dir)?;
        let before_state = read_optional(&self.paths.state_dir.join("ownership.json"))?;
        let index = self.owned_index(&state).ok_or_else(conflict)?;
        let owned = state.installations.remove(index);
        let files: std::collections::BTreeMap<String, String> =
            serde_json::from_value(owned.entry["files"].clone())
                .map_err(|_| configuration_error())?;
        let before_native = self.native_registration()?;
        let native_snapshot = save_snapshot(
            &self.paths.state_dir,
            read_optional(&self.config_path())?.as_deref(),
        )?;
        // A nonzero package-manager exit can follow partial writes; inspect and recover its own entry.
        let operation = (|| {
            release_skill(&mut state, &owned)?;
            write_state(&self.paths.state_dir, &state)?;
            self.manage(false)?;
            if self.dependency()?.is_some() || self.bundle_enabled()? {
                return Err(conflict());
            }
            Ok(InstallStatus::Removed)
        })();
        if operation.is_err() {
            let changed = self.native_registration().as_ref().ok() != Some(&before_native);
            let native_restored = !changed || self.recover_native(&before_native).is_ok();
            if write_private(&owned.skill_path, owned.skill.as_bytes())
                .and_then(|_| {
                    restore(
                        &self.paths.state_dir.join("ownership.json"),
                        before_state.as_deref(),
                    )
                })
                .is_err()
                || !native_restored
            {
                let evidence = RemovalRecovery {
                    profile: self.paths.profile.clone(),
                    before: before_native,
                    owned,
                    snapshot: native_snapshot,
                };
                let _ = write_private(
                    &self.recovery_path(),
                    &serde_json::to_vec_pretty(&evidence).map_err(|_| storage_error())?,
                );
                return Err(error(
                    "rollback_required",
                    "DeepSeek 移除失败且自动恢复未完成；私有归属、快照与恢复记录保留。修复运行时或文件权限后调用 recover，再检查或移除。",
                ));
            }
        } else {
            self.remove_package_files(&files)?;
            remove_empty_parent(&owned.skill_path);
            let _ = fs::remove_file(owned.snapshot);
        }
        let _ = fs::remove_file(native_snapshot);
        let _ = fs::remove_file(self.recovery_path());
        operation
    }

    /// Private recovery evidence for a native operation that could not be restored automatically.
    pub fn recovery_path(&self) -> PathBuf {
        self.paths
            .state_dir
            .join("deepseek-recovery")
            .join(&self.paths.profile)
            .join("remove.json")
    }

    /// Explicitly retry the recorded recovery after the package manager or storage is available again.
    pub fn recover(&self) -> Result<InstallStatus> {
        self.detect()?;
        self.require_profile()?;
        let Some(bytes) = read_optional(&self.recovery_path())? else {
            return self.check();
        };
        let mut recovery: RemovalRecovery =
            serde_json::from_slice(&bytes).map_err(|_| configuration_error())?;
        if recovery.profile != self.paths.profile
            || recovery.owned.harness != ManagedHarness::DeepSeek
            || recovery.owned.scope != Scope::User
            || recovery.owned.config_path != self.config_path()
            || recovery.owned.skill_path != self.skill_path()
            || recovery.owned.entry["package_path"] != json!(self.package_path())
        {
            return Err(conflict());
        }
        for name in FILES {
            if read_optional(&self.package_path().join(name))?.as_deref()
                != recovery.owned.entry["files"][name]
                    .as_str()
                    .map(str::as_bytes)
            {
                return Err(conflict());
            }
        }
        if recovery.before.dependency != recovery.owned.entry["dependency"] {
            return Err(conflict());
        }
        let mut state = read_state(&self.paths.state_dir)?;
        let existing = self.owned_index(&state);
        if let Some(index) = existing
            && (state.installations[index].entry != recovery.owned.entry
                || state.installations[index].skill != recovery.owned.skill)
        {
            return Err(conflict());
        }
        match read_optional(&recovery.owned.skill_path)? {
            Some(bytes) if bytes != recovery.owned.skill.as_bytes() => return Err(conflict()),
            None => recovery.owned.skill_owned = true,
            _ => {}
        }
        let restored = self.recover_native(&recovery.before).and_then(|_| {
            write_private(&recovery.owned.skill_path, recovery.owned.skill.as_bytes())
        });
        let result = restored.and_then(|_| {
            if existing.is_none() {
                state.installations.push(recovery.owned);
            }
            write_state(&self.paths.state_dir, &state).and_then(|_| self.check())
        });
        if result.is_err() {
            return Err(error(
                "rollback_required",
                "DeepSeek 恢复仍未完成；保留私有恢复记录，修复运行时或文件权限后再调用 recover。",
            ));
        }
        let _ = fs::remove_file(recovery.snapshot);
        let _ = fs::remove_file(self.recovery_path());
        result
    }

    fn native_registration(&self) -> Result<NativeRegistration> {
        let doc = self.profile_document()?;
        Ok(NativeRegistration {
            dependency: doc["dependencies"]
                .get(PACKAGE)
                .cloned()
                .unwrap_or(Value::Null),
            bundle_index: doc["dsh"]["profile"]["bundles"]
                .as_array()
                .ok_or_else(configuration_error)?
                .iter()
                .position(|value| value == PACKAGE),
        })
    }

    fn recover_native(&self, before: &NativeRegistration) -> Result<()> {
        use std::process::Stdio;
        let configured = self.command();
        let mut command = Command::new("node");
        command
            .env_clear()
            .envs(
                configured
                    .get_envs()
                    .filter_map(|(key, value)| value.map(|value| (key, value))),
            )
            .current_dir(&self.paths.project_dir)
            .args(["--input-type=module", "--eval", RECOVER])
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null());
        let payload = json!({"cli":self.paths.cli,"profile":self.paths.profile,"profile_path":self.profile_path(),"package_path":self.package_path(),"cwd":self.paths.project_dir,"dsh_home":self.paths.dsh_home,"before":before});
        let mut child = command.spawn().map_err(|_| storage_error())?;
        let written = child
            .stdin
            .take()
            .ok_or_else(storage_error)?
            .write_all(&serde_json::to_vec(&payload).map_err(|_| storage_error())?);
        let status = child.wait().map_err(|_| storage_error())?;
        written.map_err(|_| storage_error())?;
        if !status.success() || self.native_registration()? != *before {
            return Err(storage_error());
        }
        Ok(())
    }

    fn require_profile(&self) -> Result<()> {
        if !self.config_path().is_file() {
            return Err(error(
                "profile_initialization_required",
                "请先用对应 Harness 初始化 profile 并退出，再安装本产品。",
            ));
        }
        self.profile_document()?;
        Ok(())
    }
    fn validate_executable(&self) -> Result<()> {
        let metadata = fs::metadata(&self.paths.executable)
            .map_err(|_| error("executable_missing", "未找到独立 MCP 可执行程序。"))?;
        if !metadata.is_file() {
            return Err(error(
                "invalid_executable",
                "MCP 路径必须为独立可执行文件。",
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
    fn profile_document(&self) -> Result<Value> {
        let doc = parse_json(read_optional(&self.config_path())?.as_deref())?;
        if doc
            .get("dependencies")
            .is_some_and(|value| !value.is_object())
            || !doc["dsh"]["profile"]["bundles"].is_array()
        {
            return Err(configuration_error());
        }
        Ok(doc)
    }
    fn dependency(&self) -> Result<Option<Value>> {
        Ok(self.profile_document()?["dependencies"]
            .get(PACKAGE)
            .cloned())
    }
    fn bundle_enabled(&self) -> Result<bool> {
        Ok(self.profile_document()?["dsh"]["profile"]["bundles"]
            .as_array()
            .ok_or_else(configuration_error)?
            .iter()
            .any(|x| x == PACKAGE))
    }
    fn expected_files(&self) -> Result<std::collections::BTreeMap<String, String>> {
        let manifest = json!({"name":PACKAGE,"version":"0.1.0","private":true,"type":"module","main":"index.js","exports":{".":"./index.js"},"engines":{"dsh":format!("={VERSION}")},"dsh":{"bundle":{"patch":"./cordis.patch.yml"}}});
        let patch = json!([{"insert":[{"id":REGISTRATION_NAME,"name":PACKAGE,"inject":["mcpManager"],"config":{"command":self.paths.executable}}]}]);
        Ok(std::collections::BTreeMap::from([
            (
                "package.json".into(),
                serde_json::to_string_pretty(&manifest).map_err(|_| configuration_error())? + "\n",
            ),
            (
                "cordis.patch.yml".into(),
                serde_json::to_string_pretty(&patch).map_err(|_| configuration_error())? + "\n",
            ),
            ("index.js".into(), PLUGIN.into()),
        ]))
    }
    pub(super) fn entry(&self) -> Result<Option<Value>> {
        let Some(dependency) = self.dependency()? else {
            return Ok(None);
        };
        let mut files = serde_json::Map::new();
        for name in FILES {
            let value = read_optional(&self.package_path().join(name))?.ok_or_else(conflict)?;
            files.insert(
                name.into(),
                Value::String(String::from_utf8(value).map_err(|_| conflict())?),
            );
        }
        Ok(Some(
            json!({"package":PACKAGE,"dependency":dependency,"package_path":self.package_path(),"files":files}),
        ))
    }
    fn owned_index(&self, state: &State) -> Option<usize> {
        state.installations.iter().position(|x| {
            x.harness == ManagedHarness::DeepSeek
                && x.scope == Scope::User
                && x.config_path == self.config_path()
        })
    }
    fn inspect(&self, for_install: bool) -> Result<InstallStatus> {
        let state = read_state(&self.paths.state_dir)?;
        let dependency = self.dependency()?;
        let skill = read_optional(&self.skill_path())?;
        if let Some(index) = self.owned_index(&state) {
            let owned = &state.installations[index];
            if self.entry()?.as_ref() != Some(&owned.entry)
                || owned.skill_path != self.skill_path()
                || skill.as_deref() != Some(owned.skill.as_bytes())
            {
                return Err(conflict());
            }
            if for_install
                && (owned.entry["files"] != json!(self.expected_files()?)
                    || owned.skill != SKILL_CONTENT)
            {
                return Err(error(
                    "installation_update_required",
                    "受管插件与请求程序或Skill版本不同；请使用受管更新入口。",
                ));
            }
            return Ok(InstallStatus::Installed);
        }
        if dependency.is_some()
            || self.bundle_enabled()?
            || (for_install && self.package_path().exists())
        {
            return Err(conflict());
        }
        if let Some(bytes) = skill {
            if bytes != SKILL_CONTENT.as_bytes()
                || !state
                    .installations
                    .iter()
                    .any(|x| x.skill_path == self.skill_path() && x.skill == SKILL_CONTENT)
            {
                return Err(conflict());
            }
        } else if for_install && self.skill_path().parent().is_some_and(Path::exists) {
            return Err(conflict());
        }
        Ok(InstallStatus::NotInstalled)
    }
    fn check_native_namespace(&self, owned: bool) -> Result<()> {
        let output = self
            .command()
            .args(["--profile", &self.paths.profile, "--dump-config"])
            .output()
            .map_err(|_| error("cli_failed", "DeepSeek 配置检查无法执行。"))?;
        if !output.status.success() {
            return Err(configuration_error());
        }
        let doc: serde_yaml_ng::Value =
            serde_yaml_ng::from_slice(&output.stdout).map_err(|_| configuration_error())?;
        let rows = doc.as_sequence().ok_or_else(configuration_error)?;
        let manager = rows
            .iter()
            .find(|row| row["name"].as_str() == Some("@deepseek-ai/dsh-mcp-manager"));
        if !owned
            && manager.is_none_or(|row| {
                row["disabled"].as_bool() == Some(true)
                    || matches!(row["disabled"], serde_yaml_ng::Value::Tagged(_))
            })
        {
            return Err(error(
                "mcp_manager_required",
                "此profile需先启用原生mcp-manager；web和已初始化Desktop提供该服务。",
            ));
        }
        if let Some(manager) = manager {
            let servers = &manager["config"]["servers"];
            if !servers.is_null() && !servers.is_mapping() {
                return Err(configuration_error());
            }
            if servers.get(REGISTRATION_NAME).is_some() {
                return Err(conflict());
            }
        }
        let row = rows
            .iter()
            .find(|row| row["id"].as_str() == Some(REGISTRATION_NAME));
        if owned {
            if self.bundle_enabled()? {
                let row = row.ok_or_else(conflict)?;
                let state = read_state(&self.paths.state_dir)?;
                let record = &state.installations[self.owned_index(&state).ok_or_else(conflict)?];
                let patch: Value = serde_json::from_str(
                    record.entry["files"]["cordis.patch.yml"]
                        .as_str()
                        .ok_or_else(conflict)?,
                )
                .map_err(|_| conflict())?;
                let config: Value = serde_yaml_ng::from_value(row["config"].clone())
                    .map_err(|_| configuration_error())?;
                if row["name"].as_str() != Some(PACKAGE)
                    || row["disabled"].as_bool() == Some(true)
                    || matches!(row["disabled"], serde_yaml_ng::Value::Tagged(_))
                    || config != patch[0]["insert"][0]["config"]
                {
                    return Err(conflict());
                }
            } else if row.is_some() {
                return Err(conflict());
            }
        } else if row.is_some() {
            return Err(conflict());
        }
        Ok(())
    }
    fn manage(&self, add: bool) -> Result<()> {
        fs::create_dir_all(self.paths.state_dir.join("tmp")).map_err(|_| storage_error())?;
        let mut command = self.command();
        command.args([
            "plugin",
            "--profile",
            &self.paths.profile,
            if add { "add" } else { "remove" },
        ]);
        if add {
            command.arg(self.package_path());
        } else {
            command.arg(PACKAGE);
        }
        command.arg("--workspace-root");
        if add {
            command.args(["--offline", "--ignore-scripts"]);
        }
        let out = command
            .output()
            .map_err(|_| error("cli_failed", "DeepSeek 原生插件操作无法执行。"))?;
        if !out.status.success() {
            return Err(error(
                "cli_failed",
                "DeepSeek 原生插件管理未完成；保留其他profile内容。",
            ));
        }
        Ok(())
    }
    fn remove_package_files(
        &self,
        files: &std::collections::BTreeMap<String, String>,
    ) -> Result<()> {
        for (name, expected) in files {
            let path = self.package_path().join(name);
            match read_optional(&path)? {
                Some(bytes) if bytes == expected.as_bytes() => {
                    fs::remove_file(path).map_err(|_| storage_error())?
                }
                None => {}
                Some(_) => return Err(conflict()),
            }
        }
        let _ = fs::remove_dir(self.package_path());
        Ok(())
    }
    fn command(&self) -> Command {
        let mut command = Command::new(&self.paths.cli);
        command
            .env_clear()
            .current_dir(&self.paths.project_dir)
            .env("PATH", &self.paths.process_path)
            .env("HOME", &self.paths.home)
            .env("USERPROFILE", &self.paths.home)
            .env("DSH_HOME", &self.paths.dsh_home)
            .env("XDG_CONFIG_HOME", self.paths.home.join(".config"))
            .env("XDG_CACHE_HOME", self.paths.home.join(".cache"))
            .env("XDG_DATA_HOME", self.paths.home.join(".local/share"))
            .env("XDG_STATE_HOME", self.paths.home.join(".local/state"))
            .env("APPDATA", self.paths.home.join("AppData/Roaming"))
            .env("LOCALAPPDATA", self.paths.home.join("AppData/Local"))
            .env("TMPDIR", self.paths.state_dir.join("tmp"))
            .env(
                "NPM_CONFIG_USERCONFIG",
                self.paths.state_dir.join("empty.npmrc"),
            )
            .env(
                "NPM_CONFIG_GLOBALCONFIG",
                self.paths.state_dir.join("empty-global.npmrc"),
            )
            .env("DSH_AGENTS_HOME", self.paths.home.join(".agents"))
            .env("DSH_TELEMETRY_MODE", "DISABLED")
            .env("DSH_TELEMETRY_DISABLED", "1")
            .env("COREPACK_ENABLE_DOWNLOAD_PROMPT", "0")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .env("NO_PROXY", "localhost,127.0.0.1,::1");
        command
    }
}
