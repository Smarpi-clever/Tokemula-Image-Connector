//! OpenCode V2's ordered JSONC configuration and path-based Skill discovery.
use super::*;
use jsonc_parser::{
    ParseOptions,
    cst::{CstInputValue, CstRootNode},
};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OpenCodePaths {
    pub home: PathBuf,
    pub xdg_config_home: PathBuf,
    pub project_dir: PathBuf,
    pub state_dir: PathBuf,
    pub cli: PathBuf,
    pub executable: PathBuf,
    pub process_path: OsString,
}

/// Effective OpenCode overrides, supplied by the caller rather than ambient process state.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct OpenCodeOverrides {
    pub config_dir: Option<PathBuf>,
    pub config_file: Option<PathBuf>,
    pub config_content: Option<String>,
    pub project_disabled: bool,
    pub claude_skills_disabled: bool,
}

#[derive(Debug, Clone)]
pub struct OpenCodeAdapter {
    scope: Scope,
    paths: OpenCodePaths,
    overrides: OpenCodeOverrides,
}

struct Resolved {
    config: PathBuf,
    skill: PathBuf,
}

impl OpenCodeAdapter {
    pub fn new(scope: Scope, paths: OpenCodePaths, overrides: OpenCodeOverrides) -> Result<Self> {
        for path in [
            &paths.home,
            &paths.xdg_config_home,
            &paths.project_dir,
            &paths.state_dir,
            &paths.cli,
            &paths.executable,
        ]
        .into_iter()
        .chain(overrides.config_dir.iter())
        .chain(overrides.config_file.iter())
        {
            if !path.is_absolute() || path.to_str().is_none() {
                return Err(error("invalid_path", "适配器需要明确的绝对路径。"));
            }
        }
        Ok(Self {
            scope,
            paths,
            overrides,
        })
    }

    pub fn detect(&self) -> Result<String> {
        if !self.paths.cli.is_file() {
            return Err(error("client_missing", "未找到 OpenCode CLI。"));
        }
        let output = self
            .command()
            .arg("--version")
            .output()
            .map_err(|_| error("client_unavailable", "无法执行 OpenCode CLI。"))?;
        let version = std::str::from_utf8(&output.stdout)
            .map_err(|_| configuration_error())?
            .trim();
        if !output.status.success() || !version.starts_with("opencode v") {
            return Err(error("unrecognized_client", "OpenCode 版本输出无法识别。"));
        }
        if version != "opencode v2.3.2" {
            return Err(error(
                "unsupported_version",
                "此适配器适用于 OpenCode V2 2.3.2；保留现有版本及配置。",
            ));
        }
        Ok(version.to_owned())
    }

    pub fn plan(&self) -> Result<InstallPlan> {
        let version = self.detect()?;
        self.validate_executable()?;
        let resolved = self.resolve()?;
        let status = self.inspect(&resolved, true)?;
        Ok(InstallPlan {
            status: if status == InstallStatus::NotInstalled {
                InstallStatus::Ready
            } else {
                status
            },
            version,
            config_path: resolved.config,
            skill_path: resolved.skill,
        })
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

    pub(super) fn managed_paths(&self) -> Result<(PathBuf, PathBuf)> {
        let resolved = self.resolve()?;
        Ok((resolved.config, resolved.skill))
    }

    pub fn check(&self) -> Result<InstallStatus> {
        self.detect()?;
        self.inspect(&self.resolve()?, false)
    }

    pub fn install(&self) -> Result<InstallStatus> {
        let plan = self.plan()?;
        if plan.status == InstallStatus::Installed {
            return Ok(InstallStatus::AlreadyInstalled);
        }
        let before = read_optional(&plan.config_path)?;
        let skill_before = read_optional(&plan.skill_path)?;
        let mut state = read_state(&self.paths.state_dir)?;
        prepare_private_directory(&self.paths.state_dir).map_err(|_| storage_error())?;
        fs::create_dir_all(self.paths.state_dir.join("tmp")).map_err(|_| storage_error())?;
        let snapshot = save_snapshot(&self.paths.state_dir, before.as_deref())?;
        let operation = (|| {
            self.register(&plan.config_path, before.as_deref(), true)?;
            if skill_before.is_none() {
                write_private(&plan.skill_path, SKILL_CONTENT.as_bytes())?;
            }
            state.installations.push(Ownership {
                harness: ManagedHarness::OpenCode,
                scope: self.scope,
                config_path: plan.config_path.clone(),
                skill_path: plan.skill_path.clone(),
                entry: self.entry(),
                skill: SKILL_CONTENT.into(),
                snapshot: snapshot.clone(),
                skill_owned: skill_before.is_none(),
                context: self.managed_context(),
            });
            write_state(&self.paths.state_dir, &state)?;
            Ok(InstallStatus::Installed)
        })();
        if operation.is_err() {
            if restore(&plan.config_path, before.as_deref())
                .and_then(|_| restore(&plan.skill_path, skill_before.as_deref()))
                .is_err()
            {
                return Err(error(
                    "rollback_required",
                    "安装失败且自动恢复未完成；私有快照已保留。",
                ));
            }
            let _ = fs::remove_file(snapshot);
            if skill_before.is_none() {
                remove_empty_parent(&plan.skill_path);
            }
        }
        operation
    }

    fn managed_context(&self) -> Option<managed::ManagedContext> {
        // Inline overrides may contain unrelated credentials; never persist their contents.
        self.overrides
            .config_content
            .is_none()
            .then(|| managed::ManagedContext::OpenCode {
                scope: self.scope,
                paths: self.paths.clone(),
                overrides: self.overrides.clone(),
            })
    }

    pub fn update(&self) -> Result<InstallStatus> {
        managed::recover_owned_updates(&self.paths.state_dir)?;
        self.detect()?;
        self.validate_executable()?;
        let resolved = self.resolve()?;
        if self.inspect(&resolved, false)? == InstallStatus::NotInstalled {
            return Ok(InstallStatus::NotInstalled);
        }
        let state = read_state(&self.paths.state_dir)?;
        let index = self
            .owned_index(&state, &resolved.config)
            .ok_or_else(conflict)?;
        managed::change_owned(
            &self.paths.state_dir,
            index,
            self.entry(),
            SKILL_CONTENT,
            self.managed_context(),
        )
    }

    pub fn remove(&self) -> Result<InstallStatus> {
        self.detect()?;
        let resolved = self.resolve()?;
        if self.inspect(&resolved, false)? == InstallStatus::NotInstalled {
            return Ok(InstallStatus::NotInstalled);
        }
        let before = read_optional(&resolved.config)?;
        let mut state = read_state(&self.paths.state_dir)?;
        let index = self
            .owned_index(&state, &resolved.config)
            .ok_or_else(conflict)?;
        let owned = state.installations.remove(index);
        let operation = (|| {
            self.register(&resolved.config, before.as_deref(), false)?;
            release_skill(&mut state, &owned)?;
            write_state(&self.paths.state_dir, &state)?;
            Ok(InstallStatus::Removed)
        })();
        if operation.is_err() {
            if restore(&resolved.config, before.as_deref())
                .and_then(|_| write_private(&owned.skill_path, owned.skill.as_bytes()))
                .is_err()
            {
                return Err(error(
                    "rollback_required",
                    "移除失败且自动恢复未完成；私有记录已保留。",
                ));
            }
        } else {
            let _ = fs::remove_file(owned.snapshot);
            remove_empty_parent(&owned.skill_path);
        }
        operation
    }

    fn owned_index(&self, state: &State, config: &Path) -> Option<usize> {
        state.installations.iter().position(|o| {
            o.harness == ManagedHarness::OpenCode
                && o.scope == self.scope
                && o.config_path == config
        })
    }

    fn inspect(&self, resolved: &Resolved, for_install: bool) -> Result<InstallStatus> {
        let entry = read_entry(read_optional(&resolved.config)?.as_deref())?;
        let skill = read_optional(&resolved.skill)?;
        let state = read_state(&self.paths.state_dir)?;
        if let Some(index) = self.owned_index(&state, &resolved.config) {
            let owned = &state.installations[index];
            if entry.as_ref() != Some(&owned.entry)
                || skill.as_deref() != Some(owned.skill.as_bytes())
                || owned.skill_path != resolved.skill
            {
                return Err(conflict());
            }
            if for_install && (owned.entry != self.entry() || owned.skill != SKILL_CONTENT) {
                return Err(error(
                    "installation_update_required",
                    "受管程序或 Skill 版本不同；请使用受管更新入口。",
                ));
            }
            return Ok(InstallStatus::Installed);
        }
        if entry.is_some() {
            return Err(conflict());
        }
        if let Some(bytes) = skill {
            if bytes != SKILL_CONTENT.as_bytes()
                || !state
                    .installations
                    .iter()
                    .any(|o| o.skill_path == resolved.skill && o.skill.as_bytes() == bytes)
            {
                return Err(conflict());
            }
        } else if resolved.skill.parent().is_some_and(Path::exists) {
            return Err(conflict());
        }
        Ok(InstallStatus::NotInstalled)
    }

    fn entry(&self) -> Value {
        json!({"type":"local", "command":[self.paths.executable.to_string_lossy()]})
    }

    fn global_dir(&self) -> PathBuf {
        self.overrides
            .config_dir
            .clone()
            .unwrap_or_else(|| self.paths.xdg_config_home.join("opencode"))
    }

    fn project_ancestors(&self) -> Vec<PathBuf> {
        if self.overrides.project_disabled || self.paths.project_dir == self.global_dir() {
            return vec![];
        }
        let mut dirs: Vec<_> = self
            .paths
            .project_dir
            .ancestors()
            .map(Path::to_path_buf)
            .collect();
        dirs.reverse();
        dirs
    }

    fn resolve(&self) -> Result<Resolved> {
        if self.scope == Scope::Project && self.overrides.project_disabled {
            return Err(error(
                "project_configuration_disabled",
                "OpenCode 当前禁用项目配置；未修改安装作用域。",
            ));
        }
        let global = self.global_dir();
        let ancestors = self.project_ancestors();
        let mut global_files = config_files(&global);
        if let Some(explicit) = &self.overrides.config_file {
            global_files.push(explicit.clone());
        }
        let mut project_files: Vec<_> = ancestors
            .iter()
            .flat_map(|p| config_files(p))
            .filter(|p| !global_files.contains(p))
            .collect();
        project_files.extend(
            ancestors
                .iter()
                .flat_map(|p| config_files(&p.join(".opencode"))),
        );
        let mut documents = vec![];
        for path in global_files.iter().chain(project_files.iter()) {
            if let Some(bytes) = read_optional(path)? {
                documents.push((path.clone(), parse_jsonc(&bytes)?));
            }
        }
        if let Some(content) = &self.overrides.config_content {
            let value = parse_jsonc(content.as_bytes())?;
            if value.pointer("/mcp/servers/tokemula-image").is_some() {
                return Err(error(
                    "read_only_override",
                    "OPENCODE_CONFIG_CONTENT 定义了同名服务器；请先在原覆盖来源处理。",
                ));
            }
            documents.push((PathBuf::new(), value));
        }
        // Managed files are never writable installation targets.
        let system = if cfg!(target_os = "macos") {
            PathBuf::from("/Library/Application Support/opencode")
        } else if cfg!(target_os = "windows") {
            PathBuf::from("C:/ProgramData/opencode")
        } else {
            PathBuf::from("/etc/opencode")
        };
        for path in config_files(&system) {
            if let Some(bytes) = read_optional(&path)? {
                let value = parse_jsonc(&bytes)?;
                if value.pointer("/mcp/servers/tokemula-image").is_some() {
                    return Err(error(
                        "managed_configuration",
                        "同名服务器由系统管理配置提供；保留管理策略。",
                    ));
                }
                documents.push((path, value));
            }
        }
        #[cfg(target_os = "macos")]
        if Path::new("/Library/Managed Preferences/ai.opencode.managed.plist").exists()
            || fs::read_dir("/Library/Managed Preferences")
                .ok()
                .into_iter()
                .flatten()
                .flatten()
                .any(|e| e.path().join("ai.opencode.managed.plist").exists())
        {
            return Err(error(
                "managed_configuration",
                "检测到 OpenCode 管理策略；需要在其实际作用域确认安装。",
            ));
        }
        let writable = if self.scope == Scope::User {
            &global_files
        } else {
            &project_files
        };
        let state = read_state(&self.paths.state_dir)?;
        let defined = documents
            .iter()
            .rfind(|(_, v)| v.pointer("/mcp/servers/tokemula-image").is_some());
        if let Some((path, document)) = defined {
            let entry = document
                .pointer("/mcp/servers/tokemula-image")
                .expect("defined server entry");
            let owned = state.installations.iter().any(|record| {
                record.harness == ManagedHarness::OpenCode
                    && record.config_path == *path
                    && &record.entry == entry
            });
            if !owned {
                if !writable.contains(path) && self.scope == Scope::User {
                    return Err(error(
                        "configuration_shadowed",
                        "同名服务器由更高优先级项目配置定义；请在该项目作用域处理。",
                    ));
                }
                return Err(conflict());
            }
        }
        let recorded = state
            .installations
            .iter()
            .rev()
            .find(|o| {
                o.harness == ManagedHarness::OpenCode
                    && o.scope == self.scope
                    && writable.contains(&o.config_path)
            })
            .map(|o| o.config_path.clone());
        let config = defined
            .filter(|(p, _)| writable.contains(p))
            .map(|(p, _)| p.clone())
            .or(recorded)
            .or_else(|| writable.iter().rev().find(|p| p.is_file()).cloned())
            .unwrap_or_else(|| {
                if self.scope == Scope::User {
                    self.overrides
                        .config_file
                        .clone()
                        .unwrap_or_else(|| global.join("opencode.json"))
                } else {
                    self.paths.project_dir.join("opencode.json")
                }
            });
        let mut roots = vec![];
        if !self.overrides.claude_skills_disabled {
            roots.push(self.paths.home.join(".claude/skills"));
            roots.extend(ancestors.iter().map(|p| p.join(".claude/skills")));
        }
        roots.push(self.paths.home.join(".agents/skills"));
        roots.extend(ancestors.iter().map(|p| p.join(".agents/skills")));
        for dir in std::iter::once(global).chain(ancestors.iter().map(|p| p.join(".opencode"))) {
            roots.push(dir.join("skill"));
            roots.push(dir.join("skills"));
        }
        roots.push(system.join("skill"));
        roots.push(system.join("skills"));
        for (_, doc) in &documents {
            if let Some(sources) = doc.get("skills") {
                for source in sources.as_array().ok_or_else(configuration_error)? {
                    let s = source.as_str().ok_or_else(configuration_error)?;
                    if s.starts_with("https://") || s.starts_with("http://") {
                        return Err(error(
                            "remote_skill_source",
                            "存在远程 Skill 源；请先由客户端确认同名 Skill 的有效来源。",
                        ));
                    }
                    roots.push(if let Some(p) = s.strip_prefix("~/") {
                        self.paths.home.join(p)
                    } else if Path::new(s).is_absolute() {
                        PathBuf::from(s)
                    } else {
                        self.paths.project_dir.join(s)
                    });
                }
            }
        }
        let mut skill = None;
        for root in roots {
            let mut files = vec![];
            scan_skills(&root, 0, &mut files)?;
            files.sort();
            for p in files {
                let id = if p.parent() == Some(root.as_path())
                    && p.file_name().is_some_and(|n| n != "SKILL.md")
                {
                    p.file_stem()
                } else {
                    p.parent().and_then(Path::file_name)
                };
                if id.is_some_and(|v| v == REGISTRATION_NAME) {
                    skill = Some(p);
                }
            }
        }
        Ok(Resolved {
            config,
            skill: skill.unwrap_or_else(|| {
                if self.scope == Scope::User {
                    self.global_dir()
                } else {
                    self.paths.project_dir.join(".opencode")
                }
                .join("skills")
                .join(REGISTRATION_NAME)
                .join("SKILL.md")
            }),
        })
    }

    fn register(&self, path: &Path, before: Option<&[u8]>, add: bool) -> Result<()> {
        let official_dir = if self.scope == Scope::User {
            self.global_dir()
        } else {
            self.paths.project_dir.clone()
        };
        let choices = [
            official_dir.join("opencode.json"),
            official_dir.join("opencode.jsonc"),
            official_dir.join(".opencode/opencode.json"),
            official_dir.join(".opencode/opencode.jsonc"),
        ];
        let official_target = choices.iter().find(|p| p.is_file()).unwrap_or(&choices[0]);
        if add && official_target == path {
            fs::create_dir_all(path.parent().ok_or_else(storage_error)?)
                .map_err(|_| storage_error())?;
            let mut command = self.command();
            command.args(["mcp", "add", REGISTRATION_NAME]);
            if self.scope == Scope::User {
                command.arg("--global");
            }
            let output = command
                .arg("--")
                .arg(&self.paths.executable)
                .output()
                .map_err(|_| error("cli_failed", "OpenCode 注册命令无法执行。"))?;
            if !output.status.success() {
                return Err(error("cli_failed", "OpenCode 拒绝注册；原配置将恢复。"));
            }
        }
        let text =
            std::str::from_utf8(before.unwrap_or(b"{}")).map_err(|_| configuration_error())?;
        parse_jsonc(text.as_bytes())?;
        let root = CstRootNode::parse(text, &options()).map_err(|_| configuration_error())?;
        let obj = root
            .object_value_or_create()
            .ok_or_else(configuration_error)?;
        if add {
            let mcp = obj
                .object_value_or_create("mcp")
                .ok_or_else(configuration_error)?;
            let servers = mcp
                .object_value_or_create("servers")
                .ok_or_else(configuration_error)?;
            servers.append(
                REGISTRATION_NAME,
                CstInputValue::Object(vec![
                    ("type".into(), CstInputValue::String("local".into())),
                    (
                        "command".into(),
                        CstInputValue::Array(vec![CstInputValue::String(
                            self.paths.executable.to_string_lossy().into_owned(),
                        )]),
                    ),
                ]),
            );
        } else if let Some(servers) = obj
            .object_value("mcp")
            .and_then(|m| m.object_value("servers"))
            && let Some(prop) = servers.get(REGISTRATION_NAME)
        {
            prop.remove();
        }
        write_private(path, root.to_string().as_bytes())
    }

    fn command(&self) -> Command {
        let mut c = Command::new(&self.paths.cli);
        c.env_clear()
            .current_dir(&self.paths.project_dir)
            .env("HOME", &self.paths.home)
            .env("USERPROFILE", &self.paths.home)
            .env("PATH", &self.paths.process_path)
            .env("XDG_CONFIG_HOME", &self.paths.xdg_config_home)
            .env("XDG_DATA_HOME", self.paths.home.join(".local/share"))
            .env("XDG_CACHE_HOME", self.paths.home.join(".cache"))
            .env("XDG_STATE_HOME", self.paths.home.join(".local/state"))
            .env("TMPDIR", self.paths.state_dir.join("tmp"))
            .env("OPENCODE_DISABLE_MODELS_FETCH", "1")
            .env("OPENCODE_DISABLE_AUTOUPDATE", "1")
            .env("HTTP_PROXY", "http://127.0.0.1:9")
            .env("HTTPS_PROXY", "http://127.0.0.1:9")
            .env("ALL_PROXY", "http://127.0.0.1:9")
            .env("NO_PROXY", "localhost,127.0.0.1,::1");
        if let Some(v) = &self.overrides.config_dir {
            c.env("OPENCODE_CONFIG_DIR", v);
        }
        if let Some(v) = &self.overrides.config_file {
            c.env("OPENCODE_CONFIG", v);
        }
        if let Some(v) = &self.overrides.config_content {
            c.env("OPENCODE_CONFIG_CONTENT", v);
        }
        if self.overrides.project_disabled {
            c.env("OPENCODE_CONFIG_PROJECT_DISABLE", "1");
        }
        if self.overrides.claude_skills_disabled {
            c.env("OPENCODE_DISABLE_CLAUDE_CODE_SKILLS", "1");
        }
        c
    }
}

pub(super) fn patch_managed_entry(path: &Path, entry: Option<&Value>) -> Result<()> {
    fn input(value: &Value) -> Result<CstInputValue> {
        Ok(match value {
            Value::String(s) => CstInputValue::String(s.clone()),
            Value::Array(values) => {
                CstInputValue::Array(values.iter().map(input).collect::<Result<_>>()?)
            }
            Value::Object(values) => CstInputValue::Object(
                values
                    .iter()
                    .map(|(key, value)| Ok((key.clone(), input(value)?)))
                    .collect::<Result<_>>()?,
            ),
            _ => return Err(configuration_error()),
        })
    }
    let before = read_optional(path)?.ok_or_else(conflict)?;
    let text = std::str::from_utf8(&before).map_err(|_| configuration_error())?;
    parse_jsonc(&before)?;
    let root = CstRootNode::parse(text, &options()).map_err(|_| configuration_error())?;
    let servers = root
        .object_value()
        .and_then(|obj| obj.object_value("mcp"))
        .and_then(|obj| obj.object_value("servers"))
        .ok_or_else(configuration_error)?;
    if let Some(property) = servers.get(REGISTRATION_NAME) {
        property.remove();
    }
    if let Some(entry) = entry {
        servers.append(REGISTRATION_NAME, input(entry)?);
    }
    write_private(path, root.to_string().as_bytes())
}

pub(super) fn managed_entry(path: &Path) -> Result<Option<Value>> {
    read_entry(read_optional(path)?.as_deref())
}

fn config_files(root: &Path) -> Vec<PathBuf> {
    vec![root.join("opencode.json"), root.join("opencode.jsonc")]
}
fn options() -> ParseOptions {
    ParseOptions {
        allow_comments: true,
        allow_trailing_commas: true,
        allow_loose_object_property_names: false,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}
fn parse_jsonc(bytes: &[u8]) -> Result<Value> {
    let text = std::str::from_utf8(bytes).map_err(|_| configuration_error())?;
    let value: Value =
        jsonc_parser::parse_to_serde_value(text, &options()).map_err(|_| configuration_error())?;
    if !value.is_object() {
        return Err(configuration_error());
    }
    if let Some(mcp) = value.get("mcp")
        && (!mcp.is_object() || mcp.get("servers").is_some_and(|s| !s.is_object()))
    {
        return Err(configuration_error());
    }
    Ok(value)
}
fn read_entry(bytes: Option<&[u8]>) -> Result<Option<Value>> {
    Ok(parse_jsonc(bytes.unwrap_or(b"{}"))?
        .pointer("/mcp/servers/tokemula-image")
        .cloned())
}
fn scan_skills(root: &Path, depth: usize, out: &mut Vec<PathBuf>) -> Result<()> {
    let entries = match fs::read_dir(root) {
        Ok(v) => v,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(_) => return Err(storage_error()),
    };
    let mut children = vec![];
    for entry in entries {
        let e = entry.map_err(|_| storage_error())?;
        let name = e.file_name();
        let name = name.to_string_lossy();
        if name.starts_with('.') || ["node_modules", "venv", "__pycache__"].contains(&name.as_ref())
        {
            continue;
        }
        children.push(e.path());
    }
    let skill = root.join("SKILL.md");
    if children.contains(&skill) && skill.is_file() {
        out.push(skill);
        return Ok(());
    }
    for p in children {
        if depth == 0 && p.is_file() && p.extension().is_some_and(|v| v == "md") {
            out.push(p);
        } else if depth < 6 && p.is_dir() {
            scan_skills(&p, depth + 1, out)?;
        }
    }
    Ok(())
}
