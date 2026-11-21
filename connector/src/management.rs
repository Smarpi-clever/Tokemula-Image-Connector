//! User-facing management commands. Secrets are entered only in the terminal or loopback page.

#[path = "management_paths.rs"]
mod paths;

use crate::{
    configuration::SessionManager,
    image_parameters::{ImagePreference, ImagePreferences, SizeRules, quality_label, size_label},
    settings::{ModelPreferences, SettingsService, ValidationState},
};
use clap::{Parser, Subcommand, error::ErrorKind};
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    io::{self, IsTerminal, Write},
    process::Command,
};

#[derive(Parser)]
#[command(
    name = "tokemula-image",
    version,
    about = "连接 Tokemula，管理图片工具与默认模型。"
)]
struct Arguments {
    #[command(subcommand)]
    command: Action,
}

#[derive(Subcommand)]
enum Action {
    /// 为指定 Harness 安装 MCP 和共用 Skill；保留原生确认。
    Install(paths::TargetArgs),
    /// 在自己的终端或一次性本机页面绑定 Key 并选择模型。
    Configure {
        #[arg(long, conflicts_with = "terminal")]
        browser: bool,
        #[arg(long, conflicts_with = "browser")]
        terminal: bool,
        /// 只显示页面链接，不打开默认浏览器。
        #[arg(long, requires = "browser")]
        no_open: bool,
    },
    /// 查看当前凭据来源、模型偏好和接入状态，不显示 Key。
    Status(paths::TargetArgs),
    /// 删除本产品保存的 Key，保留模型偏好和 Harness 接入。
    Logout,
    /// 仅移除指定 Harness 接入，保留共享凭据和其他接入。
    Remove(paths::TargetArgs),
    /// 安装本产品的成套程序；与 install 接入管理分开。
    Setup {
        #[arg(long)]
        source: std::path::PathBuf,
        #[arg(long)]
        trust_key: std::path::PathBuf,
        #[arg(long)]
        install_root: Option<std::path::PathBuf>,
        #[arg(long)]
        adapter_state: Option<std::path::PathBuf>,
        #[arg(long)]
        update_url: Option<String>,
    },
    /// 检查可信更新或经用户确认后升级。
    Update {
        #[command(subcommand)]
        command: UpdateAction,
    },
    /// 使用上一可用程序恢复成套安装。
    Recover {
        #[arg(long)]
        install_root: Option<std::path::PathBuf>,
    },
    /// 卸载产品和受管接入；默认保留凭据、偏好与任务引用。
    Uninstall {
        #[arg(long)]
        install_root: Option<std::path::PathBuf>,
        #[arg(long)]
        purge: bool,
        #[arg(long)]
        confirm: bool,
    },
    /// 只读检查内嵌页面、Skill和插件资源。
    Health,
    #[command(hide = true)]
    ManagedRefresh {
        #[arg(long)]
        state_dir: std::path::PathBuf,
        #[arg(long)]
        mcp: std::path::PathBuf,
    },
}

#[derive(Subcommand)]
enum UpdateAction {
    /// 合法更新一个既有接入，并补齐旧归属的管理上下文。
    Integration(paths::TargetArgs),
    Check {
        #[arg(long)]
        install_root: Option<std::path::PathBuf>,
    },
    Apply {
        #[arg(long)]
        install_root: Option<std::path::PathBuf>,
        #[arg(long)]
        confirm: bool,
    },
}

pub async fn run(arguments: impl IntoIterator<Item = OsString>) -> i32 {
    run_inner(arguments, None).await
}

/// Controlled constructors provide isolated Keychain and loopback fixtures to integration tests.
#[doc(hidden)]
pub async fn run_with_service(
    arguments: impl IntoIterator<Item = OsString>,
    service: SettingsService,
) -> i32 {
    run_inner(arguments, Some(service)).await
}

async fn run_inner(
    arguments: impl IntoIterator<Item = OsString>,
    service: Option<SettingsService>,
) -> i32 {
    let arguments = match Arguments::try_parse_from(arguments) {
        Ok(value) => value,
        Err(error)
            if matches!(
                error.kind(),
                ErrorKind::DisplayHelp | ErrorKind::DisplayVersion
            ) =>
        {
            let _ = error.print();
            return 0;
        }
        Err(_) => {
            eprintln!(
                "参数无效。运行 {} 查看用法；Key 只在配置终端或本机页面输入。",
                crate::distribution::management_command("--help")
            );
            return 2;
        }
    };
    use crate::distribution::{Installer, default_adapter_state, default_install_root};
    let installer = |root: Option<std::path::PathBuf>| -> Result<Installer, crate::distribution::DistributionError> {
        Installer::new(root.map(Ok).unwrap_or_else(default_install_root)?, default_adapter_state()?)
    };
    let lifecycle: Option<Result<Value, crate::distribution::DistributionError>> =
        match &arguments.command {
            Action::Setup {
                source,
                trust_key,
                install_root,
                adapter_state,
                update_url,
            } => Some(
                match Installer::new(
                    install_root
                        .clone()
                        .map(Ok)
                        .unwrap_or_else(default_install_root)
                        .unwrap_or_default(),
                    adapter_state
                        .clone()
                        .map(Ok)
                        .unwrap_or_else(default_adapter_state)
                        .unwrap_or_default(),
                ) {
                    Ok(i) => i.setup(source, trust_key, update_url.clone()).await,
                    Err(e) => Err(e),
                },
            ),
            Action::Update {
                command: UpdateAction::Check { install_root },
            } => Some(match installer(install_root.clone()) {
                Ok(i) => i.check().await,
                Err(e) => Err(e),
            }),
            Action::Update {
                command:
                    UpdateAction::Apply {
                        install_root,
                        confirm,
                    },
            } => Some(match installer(install_root.clone()) {
                Ok(i) => i.apply(*confirm).await,
                Err(e) => Err(e),
            }),
            Action::Recover { install_root } => Some(match installer(install_root.clone()) {
                Ok(i) => i.recover().await,
                Err(e) => Err(e),
            }),
            Action::Uninstall {
                install_root,
                purge,
                confirm,
            } => {
                let service = service
                    .clone()
                    .map(Ok)
                    .unwrap_or_else(SettingsService::for_current_user);
                Some(match (installer(install_root.clone()), service) {
                    (Ok(i), Ok(s)) => i.uninstall(&s, *purge, *confirm),
                    (Err(e), _) => Err(e),
                    (_, Err(e)) => Err(crate::distribution::DistributionError {
                        code: e.code,
                        message: e.message,
                    }),
                })
            }
            Action::Update {
                command: UpdateAction::Integration(target),
            } => {
                return target_action(paths::TargetAction::Update, target.clone());
            }
            Action::Health => Some(crate::distribution::embedded_health()),
            Action::ManagedRefresh { state_dir, mcp } => {
                return match crate::adapters::update_owned(state_dir, mcp) {
                    Ok(report) => {
                        let complete = report.completed;
                        print_json(json!(report));
                        if complete { 0 } else { 1 }
                    }
                    Err(e) => print_error(e.code, e.message),
                };
            }
            _ => None,
        };
    if let Some(result) = lifecycle {
        return match result {
            Ok(v) => {
                let incomplete = v["status"] == "uninstall_incomplete";
                print_json(v);
                if incomplete { 1 } else { 0 }
            }
            Err(e) => print_error(e.code, e.message),
        };
    }
    crate::distribution::start_automatic_check();
    if matches!(&arguments.command, Action::Configure { browser: false, .. }) && !interactive() {
        terminal_guidance();
        return 2;
    }
    match arguments.command {
        Action::Install(target) => target_action(paths::TargetAction::Install, target),
        Action::Remove(target) => target_action(paths::TargetAction::Remove, target),
        command => {
            let service = match service
                .map(Ok)
                .unwrap_or_else(SettingsService::for_current_user)
            {
                Ok(service) => service,
                Err(error) => return print_error(error.code, error.message),
            };
            match command {
                Action::Configure {
                    browser,
                    terminal,
                    no_open,
                } => {
                    if browser {
                        configure_browser(service, no_open).await
                    } else if terminal || interactive() {
                        configure_terminal(&service).await
                    } else {
                        terminal_guidance();
                        2
                    }
                }
                Action::Status(target) => {
                    let (settings, exit_code) = match service.status() {
                        Ok(status) => (json!(status), 0),
                        Err(error) => (
                            json!({"availability":"unavailable","code":error.code,"message":error.message}),
                            1,
                        ),
                    };
                    let installations = if target.harness.is_some() {
                        match paths::execute(paths::TargetAction::Status, &target) {
                            Ok(value) => value,
                            Err(error) => json!({"code":error.code,"message":error.message}),
                        }
                    } else {
                        paths::default_statuses(&target)
                    };
                    print_json(json!({"settings":settings,"installations":installations}));
                    exit_code
                }
                Action::Logout => match service.logout() {
                    Ok(status) => {
                        print_json(json!({"settings":status}));
                        0
                    }
                    Err(error) => print_error(error.code, error.message),
                },
                _ => unreachable!(),
            }
        }
    }
}

fn target_action(action: paths::TargetAction, mut target: paths::TargetArgs) -> i32 {
    if target.harness.is_none() {
        if !interactive() {
            eprintln!(
                "请选择接入目标，例如：{}",
                crate::distribution::management_command("install --harness codex --scope user")
            );
            return 2;
        }
        match choose_harness() {
            Ok(Some(harness)) => target.harness = Some(harness),
            Ok(None) => return cancelled(),
            Err(_) => return terminal_error(),
        }
    }
    if matches!(target.harness, Some(paths::HarnessChoice::Deepseek))
        && target.profile.is_none()
        && interactive()
    {
        match input("DeepSeek profile 名称（q 取消）：") {
            Ok(Some(profile)) if !profile.is_empty() => target.profile = Some(profile),
            Ok(_) => return cancelled(),
            Err(_) => return terminal_error(),
        }
    }
    print_adapter_result(paths::execute(action, &target))
}

fn print_adapter_result(result: Result<Value, crate::adapters::AdapterError>) -> i32 {
    match result {
        Ok(value) => {
            print_json(value);
            0
        }
        Err(error) => print_error(error.code, error.message),
    }
}

fn print_error(code: &str, message: &str) -> i32 {
    eprintln!("{message} [{code}]");
    1
}
fn print_json(value: Value) {
    println!(
        "{}",
        serde_json::to_string_pretty(&value).expect("safe status JSON")
    );
}
fn interactive() -> bool {
    io::stdin().is_terminal() && io::stdout().is_terminal()
}
fn terminal_guidance() {
    println!(
        "请在你自己的交互终端运行：\n  {}\n或打开本机连接页：\n  {}",
        crate::distribution::management_command("configure --terminal"),
        crate::distribution::management_command("configure --browser")
    );
}
fn cancelled() -> i32 {
    println!("已取消，原配置保留。");
    0
}
fn terminal_error() -> i32 {
    print_error("terminal_unavailable", "终端输入已结束，原配置保留。")
}
fn input(prompt: &str) -> io::Result<Option<String>> {
    print!("{prompt}");
    io::stdout().flush()?;
    let mut value = String::new();
    if io::stdin().read_line(&mut value)? == 0 {
        return Ok(None);
    }
    let value = value.trim().to_owned();
    Ok((!value.eq_ignore_ascii_case("q")).then_some(value))
}
fn choose_harness() -> io::Result<Option<paths::HarnessChoice>> {
    use paths::HarnessChoice::*;
    println!("接入目标：1 Codex  2 Claude Code  3 OpenCode  4 DeepSeek  5 WorkBuddy");
    loop {
        let Some(value) = input("选择编号（q 取消）：")? else {
            return Ok(None);
        };
        match value.as_str() {
            "1" => return Ok(Some(Codex)),
            "2" => return Ok(Some(Claude)),
            "3" => return Ok(Some(Opencode)),
            "4" => return Ok(Some(Deepseek)),
            "5" => return Ok(Some(Workbuddy)),
            _ => println!("请输入 1–5。"),
        }
    }
}

async fn configure_terminal(service: &SettingsService) -> i32 {
    if !interactive() {
        terminal_guidance();
        return 2;
    }
    let saved = match service.status() {
        Ok(value) => value,
        Err(error) => return print_error(error.code, error.message),
    };
    let previous_images = match service.read_image_preferences() {
        Ok(value) => value.unwrap_or_default(),
        Err(error) => return print_error(error.code, error.message),
    };
    let replace_key = if saved.source == crate::settings::CredentialSource::SystemStore
        && saved.availability == "available"
    {
        println!("已连接 Tokemula，修改设置可继续使用已存 Key。");
        match input("更换 API Key？[y/N]（q 取消）：") {
            Ok(Some(value)) => matches!(value.to_ascii_lowercase().as_str(), "y" | "yes"),
            Ok(None) => return cancelled(),
            Err(_) => return terminal_error(),
        }
    } else {
        true
    };
    let validated = if replace_key {
        println!("Key 输入不会显示；直接回车取消。");
        let key = match rpassword::prompt_password("API Key：") {
            Ok(key) if !key.trim().is_empty() => key,
            Ok(_) => return cancelled(),
            Err(_) => return terminal_error(),
        };
        match service.validate(key).await {
            Ok(value) => value,
            Err(error) => return print_error(error.code, error.message),
        }
    } else {
        match service.validate_saved().await {
            Ok(Some(value)) => value,
            Ok(None) => return print_error("credential_missing", "已存 Key 已移除，请重新配置。"),
            Err(error) => return print_error(error.code, error.message),
        }
    };
    let summary = validated.summary();
    if summary.models.is_empty() {
        return match summary.status {
            ValidationState::NoImageAccess => print_error(
                "image_access_denied",
                "当前 Key 没有图片访问权限，原配置保留。",
            ),
            _ => print_error(
                "no_available_models",
                "当前目录没有可选择的图片模型，原配置保留。",
            ),
        };
    }
    if !summary.can_submit {
        println!("Key 已验证；当前暂不能新提交，仍可保存模型设置。");
    }
    println!("可用模型：");
    for (index, model) in summary.models.iter().enumerate() {
        let name: String = model
            .display_name
            .chars()
            .filter(|c| !c.is_control())
            .collect();
        println!("  {}. {} ({})", index + 1, name, model.id);
    }
    if let Some(preferences) = &saved.preferences {
        println!(
            "当前默认模型：{}；回车保留当前模型、备用顺序和降级设置。",
            preferences.default_model
        );
    }
    let default_model = loop {
        let value = match input("默认模型编号（回车保留，q 取消）：") {
            Ok(Some(v)) => v,
            Ok(None) => return cancelled(),
            Err(_) => return terminal_error(),
        };
        if value.is_empty()
            && let Some(previous) = &saved.preferences
            && summary
                .models
                .iter()
                .any(|model| model.id == previous.default_model)
        {
            break previous.default_model.clone();
        }
        if let Ok(index) = value.parse::<usize>()
            && let Some(model) = index.checked_sub(1).and_then(|i| summary.models.get(i))
        {
            break model.id.clone();
        }
        println!("请选择列表中的编号。");
    };
    let fallback_models = loop {
        let value = match input("备用模型编号，按顺序用逗号分隔（回车保留，0 无备用，q 取消）：")
        {
            Ok(Some(v)) => v,
            Ok(None) => return cancelled(),
            Err(_) => return terminal_error(),
        };
        if value.is_empty() {
            break saved
                .preferences
                .as_ref()
                .map(|p| {
                    p.fallback_models
                        .iter()
                        .filter(|id| {
                            id.as_str() != default_model
                                && summary.models.iter().any(|m| &m.id == *id)
                        })
                        .cloned()
                        .collect()
                })
                .unwrap_or_default();
        }
        if value == "0" {
            break vec![];
        }
        match parse_fallbacks(
            &value,
            &summary
                .models
                .iter()
                .map(|m| m.id.as_str())
                .collect::<Vec<_>>(),
            &default_model,
        ) {
            Some(models) => break models,
            None => println!("编号须来自列表、不能重复，也不能包含默认模型。"),
        }
    };
    let auto_fallback = loop {
        let value = match input("首选不适用时自动使用上述备用顺序？[y/n，回车保留]（q 取消）：")
        {
            Ok(Some(v)) => v,
            Ok(None) => return cancelled(),
            Err(_) => return terminal_error(),
        };
        match value.to_ascii_lowercase().as_str() {
            "y" | "yes" => break true,
            "" => break saved.preferences.as_ref().is_some_and(|p| p.auto_fallback),
            "n" | "no" => break false,
            _ => println!("请输入 y 或 n。"),
        }
    };
    let preferences = ModelPreferences {
        default_model,
        fallback_models,
        auto_fallback,
    };
    let selected_model = summary
        .models
        .iter()
        .find(|m| m.id == preferences.default_model)
        .expect("selected model");
    let image_preferences =
        match configure_image_preferences(&selected_model.capabilities, previous_images) {
            Ok(Some(value)) => value,
            Ok(None) => return cancelled(),
            Err(_) => return terminal_error(),
        };
    println!(
        "默认模型：{}\n备用顺序：{}\n自动降级：{}",
        preferences.default_model,
        if preferences.fallback_models.is_empty() {
            "无".into()
        } else {
            preferences.fallback_models.join(" → ")
        },
        if auto_fallback { "开启" } else { "关闭" }
    );
    for profile in &image_preferences.profiles {
        println!(
            "{} · {}：尺寸 {}；画质 {}",
            operation_label(
                serde_json::to_value(profile.operation)
                    .expect("operation")
                    .as_str()
                    .expect("operation label")
            ),
            mode_label(
                serde_json::to_value(profile.mode)
                    .expect("mode")
                    .as_str()
                    .expect("mode label")
            ),
            profile
                .size
                .as_deref()
                .map(size_label)
                .unwrap_or_else(|| "自动／模型默认".into()),
            profile
                .quality
                .as_deref()
                .map(quality_label)
                .unwrap_or_else(|| "自动／模型默认".into())
        );
    }
    match input("保存以上配置？[y/N]：") {
        Ok(Some(value)) if matches!(value.to_ascii_lowercase().as_str(), "y" | "yes") => {}
        Ok(_) => return cancelled(),
        Err(_) => return terminal_error(),
    }
    match service.save_configuration(&validated, preferences, Some(image_preferences)) {
        Ok(outcome) => {
            if outcome.active_source == crate::settings::CredentialSource::Environment {
                println!("配置已保存；当前环境 Key 覆盖仍生效，已存模型偏好暂不应用。");
            }
            print_json(json!({"configuration":outcome}));
            0
        }
        Err(error) => print_error(error.code, error.message),
    }
}

fn configure_image_preferences(
    capabilities: &[Value],
    mut preferences: ImagePreferences,
) -> io::Result<Option<ImagePreferences>> {
    let unsupported = preferences
        .profiles
        .iter()
        .filter(|profile| {
            !capabilities.iter().any(|capability| {
                capability["operation"] == serde_json::json!(profile.operation)
                    && capability["mode"] == serde_json::json!(profile.mode)
            })
        })
        .collect::<Vec<_>>();
    if !unsupported.is_empty() {
        println!("新默认模型不支持以下已存偏好的操作和执行模式：");
        for profile in &unsupported {
            println!(
                "  {} · {}：尺寸 {}；画质 {}",
                operation_label(
                    serde_json::to_value(profile.operation)
                        .expect("operation")
                        .as_str()
                        .expect("operation label")
                ),
                mode_label(
                    serde_json::to_value(profile.mode)
                        .expect("mode")
                        .as_str()
                        .expect("mode label")
                ),
                profile
                    .size
                    .as_deref()
                    .map(size_label)
                    .unwrap_or_else(|| "自动／模型默认".into()),
                profile
                    .quality
                    .as_deref()
                    .map(quality_label)
                    .unwrap_or_else(|| "自动／模型默认".into())
            );
        }
        match input("清除这些不兼容偏好并继续？[y/N]（q 取消）：")? {
            Some(value) if matches!(value.to_ascii_lowercase().as_str(), "y" | "yes") => {
                preferences.profiles.retain(|profile| {
                    capabilities.iter().any(|capability| {
                        capability["operation"] == serde_json::json!(profile.operation)
                            && capability["mode"] == serde_json::json!(profile.mode)
                    })
                });
            }
            _ => return Ok(None),
        }
    }
    loop {
        println!("图片参数（按操作和执行模式保存）：");
        for (index, capability) in capabilities.iter().enumerate() {
            println!(
                "  {}. {} · {}",
                index + 1,
                operation_label(capability["operation"].as_str().unwrap_or("")),
                mode_label(capability["mode"].as_str().unwrap_or(""))
            );
        }
        let Some(choice) = input("选择要设置的编号（回车完成，q 取消）：")?
        else {
            return Ok(None);
        };
        if choice.is_empty() {
            return Ok(Some(preferences));
        }
        let Some(capability) = choice
            .parse::<usize>()
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| capabilities.get(i))
        else {
            println!("请选择列表中的编号。");
            continue;
        };
        let operation =
            serde_json::from_value(capability["operation"].clone()).expect("validated operation");
        let mode = serde_json::from_value(capability["mode"].clone()).expect("validated mode");
        let old = preferences
            .profiles
            .iter()
            .find(|p| p.operation == operation && p.mode == mode);
        let parameters = &capability["parameters"];
        let sizes = parameters["sizes"].as_array().expect("validated sizes");
        let qualities = parameters["qualities"]
            .as_array()
            .expect("validated qualities");
        let rules = parameters
            .get("size_rules")
            .filter(|v| !v.is_null())
            .map(|v| serde_json::from_value::<SizeRules>(v.clone()).expect("validated size rules"));
        let Some(size) = choose_image_parameter(
            "尺寸",
            sizes,
            old.and_then(|p| p.size.as_deref()),
            rules.as_ref(),
            true,
        )?
        else {
            return Ok(None);
        };
        let Some(quality) = choose_image_parameter(
            "画质",
            qualities,
            old.and_then(|p| p.quality.as_deref()),
            None,
            false,
        )?
        else {
            return Ok(None);
        };
        if parameters["complexities"]
            .as_array()
            .is_some_and(|values| values.iter().any(|v| v == "medium"))
        {
            let choices = parameters["complexities"]
                .as_array()
                .expect("declared complexities")
                .iter()
                .filter_map(Value::as_str)
                .collect::<Vec<_>>()
                .join(" / ");
            println!("图片思考复杂度：默认 medium，单次请求可指定 {choices}。");
        } else {
            println!("图片思考复杂度：此路径不支持，保持省略。");
        }
        preferences
            .profiles
            .retain(|p| p.operation != operation || p.mode != mode);
        if size.is_some() || quality.is_some() {
            preferences.profiles.push(ImagePreference {
                operation,
                mode,
                size,
                quality,
            });
        }
    }
}

fn choose_image_parameter(
    label: &str,
    values: &[Value],
    previous: Option<&str>,
    rules: Option<&SizeRules>,
    size: bool,
) -> io::Result<Option<Option<String>>> {
    println!(
        "{label}：当前 {}",
        previous
            .map(|value| if size {
                size_label(value)
            } else {
                quality_label(value)
            })
            .unwrap_or_else(|| "自动／模型默认".into())
    );
    println!("  0. 自动／模型默认（省略参数）");
    for (index, value) in values.iter().enumerate() {
        let value = value.as_str().expect("validated option");
        println!(
            "  {}. {}",
            index + 1,
            if size {
                size_label(value)
            } else {
                quality_label(value)
            }
        );
    }
    if rules.is_some() {
        println!("  c. 自定义宽高");
    }
    loop {
        let Some(value) = input("选择编号（回车保留，q 取消）：")? else {
            return Ok(None);
        };
        if value.is_empty() {
            return Ok(Some(previous.map(str::to_owned)));
        }
        if value == "0" {
            return Ok(Some(None));
        }
        if value.eq_ignore_ascii_case("c")
            && let Some(rules) = rules
        {
            println!(
                "宽高步长 {}；边长 {}–{}；像素 {}–{}；比例 1:{}–{}:1。",
                rules.multiple_of,
                rules.min_edge,
                rules.max_edge,
                rules.min_pixels,
                rules.max_pixels,
                rules.max_aspect_ratio,
                rules.max_aspect_ratio
            );
            let Some(width) = input("宽度（像素，q 取消）：")? else {
                return Ok(None);
            };
            let Some(height) = input("高度（像素，q 取消）：")? else {
                return Ok(None);
            };
            let custom = format!("{width}x{height}");
            if rules.allows(&custom) {
                return Ok(Some(Some(custom)));
            }
            println!("宽高不符合当前模型尺寸规则，请重新选择。");
            continue;
        }
        if let Some(value) = value
            .parse::<usize>()
            .ok()
            .and_then(|i| i.checked_sub(1))
            .and_then(|i| values.get(i))
            .and_then(Value::as_str)
        {
            return Ok(Some(Some(value.into())));
        }
        println!("请选择列表中的编号。");
    }
}

fn operation_label(operation: &str) -> &str {
    match operation {
        "generation" => "生成",
        "edit" => "编辑",
        _ => operation,
    }
}
fn mode_label(mode: &str) -> &str {
    match mode {
        "sync" => "同步",
        "single_async" => "单次异步",
        "batch" => "批量",
        _ => mode,
    }
}

fn parse_fallbacks(input: &str, ids: &[&str], default: &str) -> Option<Vec<String>> {
    if input.trim().is_empty() {
        return Some(vec![]);
    }
    let mut selected = vec![];
    for item in input.split(',') {
        let index = item.trim().parse::<usize>().ok()?.checked_sub(1)?;
        let id = *ids.get(index)?;
        if id == default || selected.iter().any(|v| v == id) {
            return None;
        }
        selected.push(id.to_owned());
    }
    Some(selected)
}

async fn configure_browser(service: SettingsService, no_open: bool) -> i32 {
    let manager = SessionManager::new(service);
    let session = match manager.start().await {
        Ok(s) => s,
        Err(e) => return print_error(e.code, e.message),
    };
    println!(
        "在本机连接页完成设置：\n{}\n页面关闭或取消前，此命令保持运行。",
        session.url
    );
    if !no_open && !open_browser(&session.url) {
        eprintln!("未能打开默认浏览器，请使用上面的本机链接。");
    }
    let result = tokio::select! {
        result=manager.wait()=>result,
        _=tokio::signal::ctrl_c()=>{
            match manager.cancel().await {
                Ok(_) => manager.wait().await,
                Err(error) => Err(error),
            }
        }
    };
    match result {
        Ok(outcome) => {
            print_json(json!({"configuration":outcome}));
            0
        }
        Err(error) => print_error(error.code, error.message),
    }
}
fn open_browser(url: &str) -> bool {
    #[cfg(target_os = "macos")]
    let result = Command::new("/usr/bin/open").arg(url).status();
    #[cfg(target_os = "windows")]
    let result = Command::new("rundll32.exe")
        .args(["url.dll,FileProtocolHandler", url])
        .status();
    #[cfg(not(any(target_os = "macos", target_os = "windows")))]
    let result = Command::new("xdg-open").arg(url).status();
    result.is_ok_and(|status| status.success())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fallback_input_keeps_order_and_rejects_ambiguous_choices() {
        assert_eq!(
            parse_fallbacks("3,2", &["a", "b", "c"], "a"),
            Some(vec!["c".into(), "b".into()])
        );
        for value in ["0", "4", "1", "2,2", "two"] {
            assert!(parse_fallbacks(value, &["a", "b", "c"], "a").is_none());
        }
    }
}
