//! Resolve user-facing target options, then reuse the existing Harness adapters.
use crate::adapters::{
    Adapter, AdapterError, AdapterPaths, Harness, Scope,
    deepseek::{DeepSeekAdapter, DeepSeekPaths, DeepSeekRuntime},
    opencode::{OpenCodeAdapter, OpenCodeOverrides, OpenCodePaths},
    workbuddy::{WorkBuddyAdapter, WorkBuddyPaths, WorkBuddyStatus},
};
use clap::{Args, ValueEnum};
use serde_json::{Value, json};
use std::{
    collections::HashMap,
    ffi::OsString,
    path::{Path, PathBuf},
};

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum HarnessChoice {
    Codex,
    Claude,
    Opencode,
    Deepseek,
    Workbuddy,
}
impl HarnessChoice {
    pub const ALL: [Self; 5] = [
        Self::Codex,
        Self::Claude,
        Self::Opencode,
        Self::Deepseek,
        Self::Workbuddy,
    ];
    pub fn name(self) -> &'static str {
        match self {
            Self::Codex => "codex",
            Self::Claude => "claude",
            Self::Opencode => "opencode",
            Self::Deepseek => "deepseek",
            Self::Workbuddy => "workbuddy",
        }
    }
}
#[derive(Debug, Clone, Copy, Default, ValueEnum)]
pub enum ScopeChoice {
    #[default]
    User,
    Project,
}
impl From<ScopeChoice> for Scope {
    fn from(scope: ScopeChoice) -> Self {
        match scope {
            ScopeChoice::User => Self::User,
            ScopeChoice::Project => Self::Project,
        }
    }
}
#[derive(Debug, Clone, Default, Args)]
pub struct TargetArgs {
    /// 要接入的客户端。
    #[arg(long)]
    pub harness: Option<HarnessChoice>,
    /// 原生安装作用域，默认 user。
    #[arg(long, value_enum, default_value = "user")]
    pub scope: ScopeChoice,
    /// DeepSeek 已初始化的 CLI/Web profile。
    #[arg(long)]
    pub profile: Option<String>,
    /// Harness 用户目录覆盖；不改变系统凭据所属用户。
    #[arg(long)]
    pub home: Option<PathBuf>,
    /// 项目位置；省略时使用当前目录。
    #[arg(long)]
    pub project: Option<PathBuf>,
    /// 非默认客户端 CLI 的位置。
    #[arg(long)]
    pub client: Option<PathBuf>,
    /// 独立 MCP 程序；默认使用管理程序旁的 tokemula-image-mcp。
    #[arg(long)]
    pub mcp: Option<PathBuf>,
    /// 本产品的私有安装归属目录。
    #[arg(long)]
    pub state_dir: Option<PathBuf>,
    /// WorkBuddy 应用包位置。
    #[arg(long)]
    pub workbuddy_app: Option<PathBuf>,
}
#[derive(Clone, Copy)]
pub enum TargetAction {
    Install,
    Update,
    Status,
    Remove,
}

fn error(code: &'static str, message: &'static str) -> AdapterError {
    AdapterError { code, message }
}

struct Environment {
    values: HashMap<&'static str, OsString>,
    cwd: PathBuf,
    executable: PathBuf,
}
impl Environment {
    fn current() -> Result<Self, AdapterError> {
        const KEYS: &[&str] = &[
            "HOME",
            "USERPROFILE",
            "PATH",
            "CODEX_HOME",
            "CLAUDE_CONFIG_DIR",
            "XDG_CONFIG_HOME",
            "XDG_STATE_HOME",
            "LOCALAPPDATA",
            "OPENCODE_CONFIG_DIR",
            "OPENCODE_CONFIG",
            "OPENCODE_CONFIG_CONTENT",
            "OPENCODE_DISABLE_PROJECT_CONFIG",
            "OPENCODE_DISABLE_CLAUDE_SKILLS",
            "DSH_HOME",
            "WORKBUDDY_CONFIG_DIR",
            "CODEBUDDY_CONFIG_DIR",
        ];
        Ok(Self {
            values: KEYS
                .iter()
                .filter_map(|key| std::env::var_os(key).map(|v| (*key, v)))
                .collect(),
            cwd: std::env::current_dir()
                .map_err(|_| error("current_directory_unavailable", "无法读取当前项目目录。"))?,
            executable: std::env::current_exe()
                .map_err(|_| error("executable_missing", "无法定位管理程序。"))?,
        })
    }
    fn value(&self, name: &str) -> Option<&OsString> {
        self.values.get(name).filter(|v| !v.is_empty())
    }
    fn path(&self, name: &str) -> Option<PathBuf> {
        self.value(name)
            .map(PathBuf::from)
            .map(|p| absolute(&self.cwd, &p))
    }
    fn enabled(&self, name: &str) -> bool {
        self.value(name).is_some_and(|v| v == "1" || v == "true")
    }
    fn home(&self) -> Result<PathBuf, AdapterError> {
        let keys = if cfg!(windows) {
            ["USERPROFILE", "HOME"]
        } else {
            ["HOME", "USERPROFILE"]
        };
        keys.into_iter()
            .find_map(|name| self.path(name))
            .ok_or_else(|| error("home_unavailable", "请在有明确用户目录的终端运行。"))
    }
    fn client(&self, name: &str, explicit: Option<&Path>) -> Result<PathBuf, AdapterError> {
        if let Some(path) = explicit {
            return Ok(absolute(&self.cwd, path));
        }
        let path = self
            .value("PATH")
            .ok_or_else(|| error("client_missing", "未找到客户端；可用 --client 指定其 CLI。"))?;
        for directory in std::env::split_paths(path) {
            let directory = absolute(&self.cwd, &directory);
            let names = if cfg!(windows) {
                vec![
                    format!("{name}.exe"),
                    format!("{name}.cmd"),
                    name.to_owned(),
                ]
            } else {
                vec![name.to_owned()]
            };
            for name in names {
                let candidate = directory.join(name);
                if candidate.is_file() {
                    return Ok(candidate);
                }
            }
        }
        Err(error(
            "client_missing",
            "未找到客户端；可用 --client 指定其 CLI。",
        ))
    }
}
fn absolute(cwd: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_owned()
    } else {
        cwd.join(path)
    }
}

struct Resolved {
    home: PathBuf,
    project: PathBuf,
    state: PathBuf,
    mcp: PathBuf,
    path: OsString,
    codex_home: PathBuf,
    claude_config: Option<PathBuf>,
    xdg_config: PathBuf,
    opencode_overrides: OpenCodeOverrides,
    dsh_home: PathBuf,
    workbuddy_config: PathBuf,
    workbuddy_app: PathBuf,
}
fn resolve(args: &TargetArgs, env: &Environment) -> Result<Resolved, AdapterError> {
    let home = args
        .home
        .as_deref()
        .map(|p| absolute(&env.cwd, p))
        .map(Ok)
        .unwrap_or_else(|| env.home())?;
    let state_root = if cfg!(windows) {
        env.path("LOCALAPPDATA")
            .unwrap_or_else(|| home.join("AppData/Local"))
    } else {
        env.path("XDG_STATE_HOME")
            .unwrap_or_else(|| home.join(".local/state"))
    };
    let state = args
        .state_dir
        .as_deref()
        .map(|p| absolute(&env.cwd, p))
        .unwrap_or_else(|| {
            crate::distribution::adapter_state_for(&env.executable)
                .unwrap_or_else(|| state_root.join("tokemula-image-connector/adapters"))
        });
    let mcp = args
        .mcp
        .as_deref()
        .map(|p| absolute(&env.cwd, p))
        .unwrap_or_else(|| {
            crate::distribution::installation_root_for(&env.executable)
                .map(|root| {
                    root.join("bin").join(if cfg!(windows) {
                        "tokemula-image-mcp.exe"
                    } else {
                        "tokemula-image-mcp"
                    })
                })
                .unwrap_or_else(|| {
                    env.executable.with_file_name(if cfg!(windows) {
                        "tokemula-image-mcp.exe"
                    } else {
                        "tokemula-image-mcp"
                    })
                })
        });
    let workbuddy_app = args
        .workbuddy_app
        .as_deref()
        .map(|p| absolute(&env.cwd, p))
        .unwrap_or_else(|| discover_workbuddy_app(&home, Path::new("/Applications")));
    let workbuddy_config = match env
        .path("WORKBUDDY_CONFIG_DIR")
        .or_else(|| env.path("CODEBUDDY_CONFIG_DIR"))
    {
        Some(path) => path,
        None if matches!(args.harness, Some(HarnessChoice::Workbuddy)) => {
            let (_, folder) = crate::adapters::workbuddy::detect_bundle(&home, &workbuddy_app)?;
            home.join(folder)
        }
        None => home.join(".workbuddy"),
    };
    Ok(Resolved {
        codex_home: env
            .path("CODEX_HOME")
            .unwrap_or_else(|| home.join(".codex")),
        claude_config: env.path("CLAUDE_CONFIG_DIR"),
        xdg_config: env
            .path("XDG_CONFIG_HOME")
            .unwrap_or_else(|| home.join(".config")),
        opencode_overrides: OpenCodeOverrides {
            config_dir: env.path("OPENCODE_CONFIG_DIR"),
            config_file: env.path("OPENCODE_CONFIG"),
            config_content: env
                .value("OPENCODE_CONFIG_CONTENT")
                .map(|v| v.to_string_lossy().into_owned()),
            project_disabled: env.enabled("OPENCODE_DISABLE_PROJECT_CONFIG"),
            claude_skills_disabled: env.enabled("OPENCODE_DISABLE_CLAUDE_SKILLS"),
        },
        dsh_home: env.path("DSH_HOME").unwrap_or_else(|| home.join(".dsh")),
        workbuddy_config,
        project: args
            .project
            .as_deref()
            .map(|p| absolute(&env.cwd, p))
            .unwrap_or_else(|| env.cwd.clone()),
        state,
        mcp,
        path: env.value("PATH").cloned().unwrap_or_default(),
        home,
        workbuddy_app,
    })
}

fn discover_workbuddy_app(home: &Path, applications: &Path) -> PathBuf {
    ["WorkBuddy AI.app", "WorkBuddy.app"]
        .into_iter()
        .flat_map(|name| {
            [
                home.join("Applications").join(name),
                applications.join(name),
            ]
        })
        .find(|path| path.is_dir())
        .unwrap_or_else(|| applications.join("WorkBuddy.app"))
}

pub fn execute(action: TargetAction, args: &TargetArgs) -> Result<Value, AdapterError> {
    execute_in(action, args, &Environment::current()?)
}
pub fn default_statuses(common: &TargetArgs) -> Value {
    Value::Array(HarnessChoice::ALL.into_iter().map(|harness| {
        let mut args = common.clone();
        args.harness = Some(harness);
        args.client = None;
        if !matches!(harness, HarnessChoice::Deepseek) { args.profile = None; }
        execute(TargetAction::Status, &args).unwrap_or_else(|failure| {
            json!({"harness": harness.name(), "scope": Scope::from(args.scope), "code": failure.code, "message": failure.message})
        })
    }).collect())
}

fn execute_in(
    action: TargetAction,
    args: &TargetArgs,
    env: &Environment,
) -> Result<Value, AdapterError> {
    let harness = args.harness.ok_or_else(|| {
        error(
            "harness_required",
            "请选择 --harness codex、claude、opencode、deepseek 或 workbuddy。",
        )
    })?;
    if !matches!(harness, HarnessChoice::Deepseek) && args.profile.is_some() {
        return Err(error(
            "profile_not_applicable",
            "--profile 只用于 DeepSeek Harness。",
        ));
    }
    if matches!(harness, HarnessChoice::Deepseek | HarnessChoice::Workbuddy)
        && matches!(args.scope, ScopeChoice::Project)
    {
        return Err(error(
            "unsupported_scope",
            "此客户端 MCP 按用户或 profile 全局注册，不支持项目 MCP。",
        ));
    }
    if matches!(harness, HarnessChoice::Deepseek) && args.profile.as_deref() == Some("desktop") {
        return Err(error(
            "desktop_validation_required",
            "DeepSeek Desktop 留待独立环境验收；请选择已初始化的 CLI/Web profile。",
        ));
    }
    if matches!(harness, HarnessChoice::Workbuddy) && !cfg!(target_os = "macos") {
        return Err(error(
            "platform_not_verified",
            "WorkBuddy 当前管理入口已验证 macOS 应用包。",
        ));
    }
    let paths = resolve(args, env)?;
    let scope = args.scope.into();
    let status = match harness {
        HarnessChoice::Codex | HarnessChoice::Claude => {
            let adapter = Adapter::new(
                if matches!(harness, HarnessChoice::Codex) {
                    Harness::Codex
                } else {
                    Harness::ClaudeCode
                },
                scope,
                AdapterPaths {
                    home: paths.home,
                    codex_home: paths.codex_home,
                    claude_config_dir: paths.claude_config,
                    project_dir: paths.project,
                    state_dir: paths.state,
                    cli: env.client(harness.name(), args.client.as_deref())?,
                    executable: paths.mcp,
                    process_path: paths.path,
                },
            )?;
            serde_json::to_value(match action {
                TargetAction::Install => adapter.install()?,
                TargetAction::Update => adapter.update()?,
                TargetAction::Status => adapter.check()?,
                TargetAction::Remove => adapter.remove()?,
            })
            .expect("status serializes")
        }
        HarnessChoice::Opencode => {
            let adapter = OpenCodeAdapter::new(
                scope,
                OpenCodePaths {
                    home: paths.home,
                    xdg_config_home: paths.xdg_config,
                    project_dir: paths.project,
                    state_dir: paths.state,
                    cli: env.client("opencode", args.client.as_deref())?,
                    executable: paths.mcp,
                    process_path: paths.path,
                },
                paths.opencode_overrides,
            )?;
            serde_json::to_value(match action {
                TargetAction::Install => adapter.install()?,
                TargetAction::Update => adapter.update()?,
                TargetAction::Status => adapter.check()?,
                TargetAction::Remove => adapter.remove()?,
            })
            .expect("status serializes")
        }
        HarnessChoice::Deepseek => {
            let profile = args.profile.clone().ok_or_else(|| {
                error(
                    "profile_required",
                    "请用 --profile 选择已初始化的 DeepSeek CLI/Web profile，例如 web。",
                )
            })?;
            let adapter = DeepSeekAdapter::new(
                scope,
                DeepSeekPaths {
                    home: paths.home,
                    dsh_home: paths.dsh_home,
                    project_dir: paths.project,
                    state_dir: paths.state,
                    cli: env.client("dsh", args.client.as_deref())?,
                    executable: paths.mcp,
                    process_path: paths.path,
                    profile,
                    runtime: DeepSeekRuntime::CliWeb,
                },
            )?;
            serde_json::to_value(match action {
                TargetAction::Install => adapter.install()?,
                TargetAction::Update => adapter.update()?,
                TargetAction::Status => adapter.check()?,
                TargetAction::Remove => adapter.remove()?,
            })
            .expect("status serializes")
        }
        HarnessChoice::Workbuddy => {
            let adapter = WorkBuddyAdapter::new(
                scope,
                WorkBuddyPaths {
                    home: paths.home,
                    config_dir: paths.workbuddy_config,
                    state_dir: paths.state,
                    app_bundle: paths.workbuddy_app,
                    executable: paths.mcp,
                },
            )?;
            let result = match action {
                TargetAction::Install => adapter.install()?,
                TargetAction::Update => adapter.update()?,
                TargetAction::Status => adapter.check()?,
                TargetAction::Remove => adapter.remove()?,
            };
            return Ok(
                json!({"harness": harness.name(), "scope": scope, "status": result,
                "next_action": if result == WorkBuddyStatus::AwaitingConfirmation { Some("请在 WorkBuddy 中完成本连接器的原生确认，然后运行 status。") } else { None }}),
            );
        }
    };
    Ok(
        json!({"harness": harness.name(), "scope": scope, "profile": args.profile, "status": status}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    fn environment(root: &Path) -> Environment {
        Environment {
            values: HashMap::from([
                ("HOME", root.join("home").into_os_string()),
                ("USERPROFILE", root.join("home").into_os_string()),
                ("PATH", root.join("bin").into_os_string()),
            ]),
            cwd: root.join("project"),
            executable: root.join("产品 bin/tokemula-image"),
        }
    }
    #[test]
    fn resolves_effective_roots_without_mutating_process_environment() {
        let root = tempfile::tempdir().unwrap();
        let mut env = environment(root.path());
        env.values.extend([
            (
                "CODEX_HOME",
                root.path().join("codex override").into_os_string(),
            ),
            (
                "CLAUDE_CONFIG_DIR",
                root.path().join("claude override").into_os_string(),
            ),
            (
                "XDG_CONFIG_HOME",
                root.path().join("xdg config").into_os_string(),
            ),
            (
                "XDG_STATE_HOME",
                root.path().join("xdg state").into_os_string(),
            ),
            (
                "OPENCODE_CONFIG_DIR",
                root.path().join("open dir").into_os_string(),
            ),
            (
                "OPENCODE_CONFIG",
                root.path().join("open.jsonc").into_os_string(),
            ),
            (
                "DSH_HOME",
                root.path().join("dsh override").into_os_string(),
            ),
            (
                "WORKBUDDY_CONFIG_DIR",
                root.path().join("wb override").into_os_string(),
            ),
        ]);
        let args = TargetArgs {
            project: Some(PathBuf::from("中文 project")),
            ..Default::default()
        };
        let p = resolve(&args, &env).unwrap();
        assert_eq!(p.codex_home, root.path().join("codex override"));
        assert_eq!(p.claude_config, Some(root.path().join("claude override")));
        assert_eq!(p.xdg_config, root.path().join("xdg config"));
        if !cfg!(windows) {
            assert_eq!(
                p.state,
                root.path()
                    .join("xdg state/tokemula-image-connector/adapters")
            );
        }
        assert_eq!(
            p.opencode_overrides.config_file,
            Some(root.path().join("open.jsonc"))
        );
        assert_eq!(
            p.opencode_overrides.config_dir,
            Some(root.path().join("open dir"))
        );
        assert_eq!(p.dsh_home, root.path().join("dsh override"));
        assert_eq!(p.workbuddy_config, root.path().join("wb override"));
        assert_eq!(p.project, env.cwd.join("中文 project"));
        assert_eq!(
            p.mcp,
            env.executable.with_file_name(if cfg!(windows) {
                "tokemula-image-mcp.exe"
            } else {
                "tokemula-image-mcp"
            })
        );
    }
    #[test]
    fn explicit_home_changes_harness_defaults_without_erasing_effective_overrides() {
        let root = tempfile::tempdir().unwrap();
        let mut env = environment(root.path());
        env.values.insert(
            "CODEX_HOME",
            root.path().join("effective codex").into_os_string(),
        );
        env.values.insert(
            "CLAUDE_CONFIG_DIR",
            root.path().join("effective claude").into_os_string(),
        );
        let args = TargetArgs {
            home: Some(root.path().join("isolated harness home")),
            ..Default::default()
        };
        let paths = resolve(&args, &env).unwrap();
        assert_eq!(paths.home, root.path().join("isolated harness home"));
        assert_eq!(paths.codex_home, root.path().join("effective codex"));
        assert_eq!(
            paths.claude_config,
            Some(root.path().join("effective claude"))
        );
        assert_eq!(
            paths.workbuddy_config,
            root.path().join("isolated harness home/.workbuddy")
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }

    #[test]
    fn default_paths_and_rejected_scopes_are_specific_and_read_only() {
        let root = tempfile::tempdir().unwrap();
        let env = environment(root.path());
        let p = resolve(&TargetArgs::default(), &env).unwrap();
        assert_eq!(p.codex_home, root.path().join("home/.codex"));
        assert!(p.claude_config.is_none());
        assert_eq!(p.workbuddy_config, root.path().join("home/.workbuddy"));
        for harness in [HarnessChoice::Deepseek, HarnessChoice::Workbuddy] {
            let args = TargetArgs {
                harness: Some(harness),
                scope: ScopeChoice::Project,
                ..Default::default()
            };
            assert_eq!(
                execute_in(TargetAction::Install, &args, &env)
                    .unwrap_err()
                    .code,
                "unsupported_scope"
            );
        }
        let args = TargetArgs {
            harness: Some(HarnessChoice::Deepseek),
            profile: Some("desktop".into()),
            ..Default::default()
        };
        assert_eq!(
            execute_in(TargetAction::Install, &args, &env)
                .unwrap_err()
                .code,
            "desktop_validation_required"
        );
        assert_eq!(std::fs::read_dir(root.path()).unwrap().count(), 0);
    }
    #[test]
    fn discovers_new_and_legacy_apps_with_explicit_application_priority() {
        let root = tempfile::tempdir().unwrap();
        let home = root.path().join("home");
        let system = root.path().join("Applications");
        assert_eq!(
            discover_workbuddy_app(&home, &system),
            system.join("WorkBuddy.app")
        );
        std::fs::create_dir_all(home.join("Applications/WorkBuddy.app")).unwrap();
        assert_eq!(
            discover_workbuddy_app(&home, &system),
            home.join("Applications/WorkBuddy.app")
        );
        std::fs::create_dir_all(system.join("WorkBuddy AI.app")).unwrap();
        assert_eq!(
            discover_workbuddy_app(&home, &system),
            system.join("WorkBuddy AI.app")
        );
        std::fs::create_dir_all(home.join("Applications/WorkBuddy AI.app")).unwrap();
        assert_eq!(
            discover_workbuddy_app(&home, &system),
            home.join("Applications/WorkBuddy AI.app")
        );
    }

    #[test]
    #[cfg(target_os = "macos")]
    fn workbuddy_manager_resolves_product_defaults_and_preserves_native_overrides_offline() {
        use std::{fs, os::unix::fs::PermissionsExt};
        for (identity, version, app, folder) in [
            (
                "com.tencent.workbuddy.mac",
                "5.11.3",
                "WorkBuddy.app",
                ".workbuddy",
            ),
            (
                "com.workbuddy.workbuddy-ai",
                "5.11.4",
                "WorkBuddy AI.app",
                ".workbuddy-ai",
            ),
        ] {
            let root = tempfile::tempdir().unwrap();
            let mut env = environment(root.path());
            let home = root.path().join("home");
            let bundle = home.join("Applications").join(app);
            fs::create_dir_all(bundle.join("Contents")).unwrap();
            fs::write(bundle.join("Contents/Info.plist"), format!(r#"<?xml version="1.0"?><plist version="1.0"><dict><key>CFBundleIdentifier</key><string>{identity}</string><key>CFBundleShortVersionString</key><string>{version}</string></dict></plist>"#)).unwrap();
            let mcp = env.executable.with_file_name("tokemula-image-mcp");
            fs::create_dir_all(mcp.parent().unwrap()).unwrap();
            fs::write(&mcp, "#!/bin/sh\nexit 0\n").unwrap();
            fs::set_permissions(&mcp, fs::Permissions::from_mode(0o700)).unwrap();
            let mut args = TargetArgs {
                harness: Some(HarnessChoice::Workbuddy),
                // The discovery test supplies an isolated system Applications directory.
                workbuddy_app: (version == "5.11.3").then(|| bundle.clone()),
                ..Default::default()
            };
            let paths = resolve(&args, &env).unwrap();
            assert_eq!(paths.workbuddy_app, bundle);
            assert_eq!(paths.workbuddy_config, home.join(folder));
            assert_eq!(
                execute_in(TargetAction::Install, &args, &env).unwrap()["status"],
                "awaiting_confirmation"
            );
            assert_eq!(
                execute_in(TargetAction::Status, &args, &env).unwrap()["status"],
                "awaiting_confirmation"
            );
            let config: Value =
                serde_json::from_slice(&fs::read(home.join(folder).join("mcp.json")).unwrap())
                    .unwrap();
            assert_eq!(
                config["mcpServers"]["tokemula-image"]["command"],
                json!(mcp)
            );
            assert_eq!(
                execute_in(TargetAction::Remove, &args, &env).unwrap()["status"],
                "removed"
            );
            env.values
                .insert("CODEBUDDY_CONFIG_DIR", "codebuddy override".into());
            assert_eq!(
                resolve(&args, &env).unwrap().workbuddy_config,
                env.cwd.join("codebuddy override")
            );
            env.values
                .insert("WORKBUDDY_CONFIG_DIR", "workbuddy override".into());
            let explicit_bundle = root.path().join("non-default/Explicit WorkBuddy.app");
            fs::create_dir_all(explicit_bundle.join("Contents")).unwrap();
            fs::copy(
                bundle.join("Contents/Info.plist"),
                explicit_bundle.join("Contents/Info.plist"),
            )
            .unwrap();
            args.workbuddy_app = Some(explicit_bundle.clone());
            assert_eq!(resolve(&args, &env).unwrap().workbuddy_app, explicit_bundle);
            assert_eq!(
                resolve(&args, &env).unwrap().workbuddy_config,
                env.cwd.join("workbuddy override")
            );
        }
    }
}
