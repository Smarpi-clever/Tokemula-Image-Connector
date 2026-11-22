//! Isolated configuration and package tests; synthetic approval records are not client evidence.
#![cfg(target_os = "macos")]

use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{fs, os::unix::fs::PermissionsExt, path::PathBuf};
use tempfile::TempDir;
use tokemula_image_connector::adapters::{
    REGISTRATION_NAME, SKILL_CONTENT, Scope,
    workbuddy::{WorkBuddyAdapter, WorkBuddyPaths, WorkBuddyStatus},
};

struct Fixture {
    root: TempDir,
    paths: WorkBuddyPaths,
    adapter: WorkBuddyAdapter,
}

impl Fixture {
    fn new() -> Self {
        Self::new_product("com.tencent.workbuddy.mac", "5.11.3")
    }

    fn new_product(identity: &str, version: &str) -> Self {
        let root = tempfile::Builder::new()
            .prefix("tokemula-workbuddy 中文 ")
            .tempdir()
            .unwrap();
        let app_bundle = root.path().join("WorkBuddy fixture.app");
        fs::create_dir_all(app_bundle.join("Contents/Resources")).unwrap();
        fs::create_dir_all(app_bundle.join("Contents/MacOS")).unwrap();
        fs::write(
            app_bundle.join("Contents/Resources/app.asar"),
            b"static version fixture",
        )
        .unwrap();
        fs::write(
            app_bundle.join("Contents/MacOS/Electron"),
            b"not launched\n",
        )
        .unwrap();
        Self::write_identity(&app_bundle, identity, version);
        let executable = root.path().join("独立 MCP 程序");
        fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = WorkBuddyPaths {
            home: root.path().join("home"),
            config_dir: root.path().join("WorkBuddy config"),
            state_dir: root.path().join("private state"),
            app_bundle,
            executable,
        };
        fs::create_dir_all(&paths.home).unwrap();
        fs::create_dir_all(&paths.config_dir).unwrap();
        let adapter = WorkBuddyAdapter::new(Scope::User, paths.clone()).unwrap();
        Self {
            root,
            paths,
            adapter,
        }
    }

    fn write_version(bundle: &std::path::Path, version: &str) {
        Self::write_identity(bundle, "com.tencent.workbuddy.mac", version);
    }

    fn write_identity(bundle: &std::path::Path, identity: &str, version: &str) {
        fs::write(
            bundle.join("Contents/Info.plist"),
            format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>{identity}</string><key>CFBundleShortVersionString</key><string>{version}</string><key>CFBundleVersion</key><string>{version}</string><key>CFBundleExecutable</key><string>Electron</string></dict></plist>\n"),
        )
        .unwrap();
    }

    fn complex_config(&self) -> (Vec<u8>, Value) {
        let value = json!({
            "provider": {"name":"existing-provider", "settings":{"retain":true}},
            "model": "existing-language-model",
            "mcpServers": {"other-tool":{"command":"/bin/false", "disabled":true, "future":42}},
            "future_unknown": {"values":[1,2,3]},
            "permissions": {"requireApproval":true}
        });
        let bytes = serde_json::to_vec(&value).unwrap();
        fs::write(self.adapter.config_path(), &bytes).unwrap();
        (bytes, value)
    }

    fn config(&self) -> Value {
        serde_json::from_slice(&fs::read(self.adapter.config_path()).unwrap()).unwrap()
    }

    fn write_config(&self, value: &Value) {
        fs::write(
            self.adapter.config_path(),
            serde_json::to_vec(value).unwrap(),
        )
        .unwrap();
    }

    fn approval_path(&self) -> PathBuf {
        self.paths.config_dir.join("mcp-approvals.json")
    }

    fn simulate_native_approval_for_unit_only(&self) {
        // The native client check separately proves its approval algorithm and connection.
        let material = format!("{}||", self.paths.executable.to_str().unwrap());
        let fingerprint: String = Sha256::digest(material.as_bytes())
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let key = format!("{fingerprint}::{REGISTRATION_NAME}");
        fs::write(
            self.approval_path(),
            serde_json::to_vec(&json!({key:1700000000})).unwrap(),
        )
        .unwrap();
    }
}

fn assert_original_values(original: &Value, current: &Value) {
    for (key, value) in original.as_object().unwrap() {
        if key == "mcpServers" {
            for (name, entry) in value.as_object().unwrap() {
                assert_eq!(&current[key][name], entry, "other server {name} changed");
            }
        } else {
            assert_eq!(&current[key], value, "original field {key} changed");
        }
    }
}

#[test]
fn pending_confirmation_repeat_and_selective_uninstall_preserve_other_content() {
    for (identity, version) in [
        ("com.tencent.workbuddy.mac", "5.11.3"),
        ("com.workbuddy.workbuddy-ai", "5.11.4"),
    ] {
        let f = Fixture::new_product(identity, version);
        let (_, original) = f.complex_config();
        let other_skill = f.paths.config_dir.join("skills/other-skill/SKILL.md");
        fs::create_dir_all(other_skill.parent().unwrap()).unwrap();
        fs::write(&other_skill, b"user skill\n").unwrap();
        fs::create_dir_all(&f.paths.state_dir).unwrap();
        let legacy_snapshot = f.paths.state_dir.join("legacy.private");
        fs::write(&legacy_snapshot, b"other managed snapshot").unwrap();
        let other_owned = json!({
            "harness":"codex", "scope":"user",
            "config_path":f.root.path().join("codex/config.toml"),
            "skill_path":f.root.path().join("other-home/.agents/skills/tokemula-image/SKILL.md"),
            "entry":{"command":"/other/managed-program","args":[]},
            "skill":"other managed content", "snapshot":legacy_snapshot
        });
        fs::write(
            f.paths.state_dir.join("ownership.json"),
            serde_json::to_vec(&json!({"installations":[other_owned.clone()]})).unwrap(),
        )
        .unwrap();
        assert_eq!(f.adapter.detect().unwrap(), version);
        assert_eq!(f.adapter.plan().unwrap().status, WorkBuddyStatus::Ready);
        assert_eq!(
            f.adapter.install().unwrap(),
            WorkBuddyStatus::AwaitingConfirmation
        );
        assert_eq!(
            f.adapter.check().unwrap(),
            WorkBuddyStatus::AwaitingConfirmation
        );
        assert!(
            !f.approval_path().exists(),
            "installation must not approve itself"
        );
        let installed = f.config();
        assert_original_values(&original, &installed);
        assert_eq!(
            installed["mcpServers"][REGISTRATION_NAME],
            json!({"command":f.paths.executable})
        );
        assert_eq!(
            fs::read_to_string(f.adapter.skill_path()).unwrap(),
            SKILL_CONTENT
        );
        let config_before = fs::read(f.adapter.config_path()).unwrap();
        let state_path = f.paths.state_dir.join("ownership.json");
        let state_before = fs::read(&state_path).unwrap();
        assert_eq!(
            f.adapter.install().unwrap(),
            WorkBuddyStatus::AwaitingConfirmation
        );
        assert_eq!(fs::read(f.adapter.config_path()).unwrap(), config_before);
        assert_eq!(fs::read(&state_path).unwrap(), state_before);
        assert_eq!(
            fs::metadata(&f.paths.state_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&state_path).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let state: Value = serde_json::from_slice(&state_before).unwrap();
        assert_eq!(state["installations"][0], other_owned);
        let snapshot = PathBuf::from(state["installations"][1]["snapshot"].as_str().unwrap());
        assert_eq!(
            fs::metadata(snapshot).unwrap().permissions().mode() & 0o777,
            0o600
        );

        fs::write(
            f.approval_path(),
            br#"{"wrong-config::tokemula-image":1700000000}"#,
        )
        .unwrap();
        assert_eq!(
            f.adapter.check().unwrap(),
            WorkBuddyStatus::AwaitingConfirmation
        );
        f.simulate_native_approval_for_unit_only();
        let approval_before = fs::read(f.approval_path()).unwrap();
        assert_eq!(f.adapter.check().unwrap(), WorkBuddyStatus::Installed);
        assert_eq!(
            f.adapter.install().unwrap(),
            WorkBuddyStatus::AlreadyInstalled
        );
        let mut current = f.config();
        current["user_later"] = json!({"keep":"later value"});
        current["mcpServers"]["later-tool"] = json!({"command":"/bin/false", "disabled":true});
        f.write_config(&current);
        let extra = f
            .adapter
            .skill_path()
            .parent()
            .unwrap()
            .join("user-notes.txt");
        fs::write(&extra, b"keep user file").unwrap();
        assert_eq!(f.adapter.remove().unwrap(), WorkBuddyStatus::Removed);
        let removed = f.config();
        assert_original_values(&original, &removed);
        assert_eq!(removed["user_later"], current["user_later"]);
        assert_eq!(
            removed["mcpServers"]["later-tool"],
            current["mcpServers"]["later-tool"]
        );
        assert!(removed["mcpServers"].get(REGISTRATION_NAME).is_none());
        assert!(!f.adapter.skill_path().exists());
        assert_eq!(fs::read(extra).unwrap(), b"keep user file");
        assert_eq!(fs::read(other_skill).unwrap(), b"user skill\n");
        assert_eq!(f.adapter.check().unwrap(), WorkBuddyStatus::NotInstalled);
        assert_eq!(f.adapter.remove().unwrap(), WorkBuddyStatus::NotInstalled);
        assert_eq!(fs::read(f.approval_path()).unwrap(), approval_before);
        let remaining: Value = serde_json::from_slice(&fs::read(state_path).unwrap()).unwrap();
        assert_eq!(remaining["installations"], json!([other_owned]));
        assert_eq!(
            fs::read(legacy_snapshot).unwrap(),
            b"other managed snapshot"
        );
    }
}

#[test]
fn foreign_and_modified_owned_entries_are_preserved_and_rejected() {
    for scenario in [
        "foreign-mcp",
        "foreign-skill",
        "changed-mcp",
        "changed-skill",
        "invalid-json",
        "invalid-servers",
    ] {
        let f = Fixture::new();
        f.complex_config();
        match scenario {
            "foreign-mcp" => {
                let mut v = f.config();
                v["mcpServers"][REGISTRATION_NAME] = json!({"command":f.paths.executable});
                f.write_config(&v);
            }
            "foreign-skill" => {
                fs::create_dir_all(f.adapter.skill_path().parent().unwrap()).unwrap();
                fs::write(f.adapter.skill_path(), SKILL_CONTENT).unwrap();
            }
            "changed-mcp" | "changed-skill" => {
                f.adapter.install().unwrap();
                if scenario == "changed-mcp" {
                    let mut v = f.config();
                    v["mcpServers"][REGISTRATION_NAME]["command"] = json!("/user/changed");
                    f.write_config(&v);
                } else {
                    fs::write(
                        f.adapter.skill_path(),
                        format!("{SKILL_CONTENT}\nUser change.\n"),
                    )
                    .unwrap();
                }
            }
            "invalid-json" => fs::write(f.adapter.config_path(), b"not-json").unwrap(),
            "invalid-servers" => f.write_config(&json!({"mcpServers":[]})),
            _ => unreachable!(),
        }
        let config_before = fs::read(f.adapter.config_path()).unwrap();
        let skill_before = fs::read(f.adapter.skill_path()).ok();
        let code = if scenario.starts_with("invalid-") {
            "unrecognized_configuration"
        } else {
            "ownership_conflict"
        };
        assert_eq!(f.adapter.plan().unwrap_err().code, code, "plan: {scenario}");
        assert_eq!(
            f.adapter.install().unwrap_err().code,
            code,
            "install: {scenario}"
        );
        assert_eq!(
            f.adapter.remove().unwrap_err().code,
            code,
            "remove: {scenario}"
        );
        assert_eq!(
            fs::read(f.adapter.config_path()).unwrap(),
            config_before,
            "{scenario}"
        );
        assert_eq!(
            fs::read(f.adapter.skill_path()).ok(),
            skill_before,
            "{scenario}"
        );
    }
}

struct RestorePermissions(PathBuf);
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700)).unwrap();
    }
}

#[test]
fn failed_skill_write_rolls_back_configuration_and_allows_retry() {
    let f = Fixture::new();
    let (original_bytes, _) = f.complex_config();
    let skills = f.paths.config_dir.join("skills");
    fs::create_dir(&skills).unwrap();
    let guard = RestorePermissions(skills.clone());
    fs::set_permissions(&skills, fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(f.adapter.plan().unwrap().status, WorkBuddyStatus::Ready);
    assert!(f.adapter.install().is_err());
    assert_eq!(fs::read(f.adapter.config_path()).unwrap(), original_bytes);
    assert!(!f.adapter.skill_path().exists());
    assert!(!f.paths.state_dir.join("ownership.json").exists());
    assert!(!f.approval_path().exists());
    drop(guard);
    assert_eq!(
        f.adapter.install().unwrap(),
        WorkBuddyStatus::AwaitingConfirmation
    );
    assert_eq!(f.adapter.remove().unwrap(), WorkBuddyStatus::Removed);
    assert_eq!(f.adapter.remove().unwrap(), WorkBuddyStatus::NotInstalled);
}

#[test]
fn boundaries_and_official_package_use_the_single_skill_without_secrets() {
    assert_eq!(
        SKILL_CONTENT,
        include_str!("../../skills/tokemula-image/SKILL.md")
    );
    let delivery = SKILL_CONTENT
        .split("## 交付图片")
        .nth(1)
        .unwrap()
        .split("## 使用示例")
        .next()
        .unwrap();
    for instruction in [
        "宿主正常保存或附件入口",
        "请用户保存并上传",
        "此时停止展示",
        "会话数据库、内部日志或进程缓存",
        "不做全盘扫描",
        "不重新生成",
        "直接在正文交付原图，默认不先调用 `present_files`",
        "用户明确要求打开原图或侧栏时，再使用原生 `present_files`",
        "不为展示而重读整张图片",
        "用户需要视觉分析时再按需读图",
    ] {
        assert!(
            delivery.contains(instruction),
            "missing delivery guidance: {instruction}"
        );
    }
    for instruction in [
        "macOS 桌面持久输出适用时",
        "工具 schema 明确支持 `result_mode=files`",
        "旧 schema、其他宿主和 `single_async` 沿用原行为",
        "不得擅自转用内置 `ImageGen` 或其他服务",
    ] {
        assert!(SKILL_CONTENT.contains(instruction));
    }
    let frontmatter = SKILL_CONTENT
        .strip_prefix("---\n")
        .unwrap()
        .split_once("\n---")
        .unwrap()
        .0;
    let metadata: serde_yaml_ng::Value = serde_yaml_ng::from_str(frontmatter).unwrap();
    assert_eq!(metadata["name"].as_str(), Some(REGISTRATION_NAME));
    let description = metadata["description"].as_str().unwrap();
    for rule in [
        "只描述画面、未写“生成”或服务名的图片需求同样使用",
        "生图默认必须通过 Tokemula MCP",
        "仅用户明确排除 Tokemula 或指定其他图片服务时例外",
        "提交批量图片任务、恢复任务结果",
    ] {
        assert!(description.contains(rule));
    }
    let f = Fixture::new();
    assert_eq!(
        WorkBuddyAdapter::new(Scope::Project, f.paths.clone())
            .unwrap_err()
            .code,
        "unsupported_scope"
    );
    assert!(!f.adapter.config_path().exists());
    assert!(!f.paths.state_dir.exists());
    let mut invalid_paths = f.paths.clone();
    invalid_paths.executable = PathBuf::from("relative-program");
    assert_eq!(
        WorkBuddyAdapter::new(Scope::User, invalid_paths)
            .unwrap_err()
            .code,
        "invalid_path"
    );
    Fixture::write_version(&f.paths.app_bundle, "5.10.0");
    assert_eq!(f.adapter.plan().unwrap_err().code, "unsupported_version");
    assert!(!f.adapter.config_path().exists());
    Fixture::write_version(&f.paths.app_bundle, "5.11.3");
    fs::remove_file(f.paths.app_bundle.join("Contents/Info.plist")).unwrap();
    assert_eq!(f.adapter.install().unwrap_err().code, "client_missing");
    assert!(!f.adapter.config_path().exists());
    Fixture::write_version(&f.paths.app_bundle, "5.11.3");

    let destination = f.root.path().join("交付 package");
    assert_eq!(f.adapter.export_package(&destination).unwrap(), destination);
    let metadata: Value =
        serde_json::from_slice(&fs::read(destination.join("connector-meta.json")).unwrap())
            .unwrap();
    for key in [
        "name",
        "name_en",
        "description",
        "description_zh",
        "description_en",
        "source",
        "version",
    ] {
        assert!(
            metadata[key].as_str().is_some_and(|s| !s.is_empty()),
            "metadata missing {key}"
        );
    }
    for key in ["examples_zh", "examples_en"] {
        assert!((2..=5).contains(&metadata[key].as_array().unwrap().len()));
    }
    for key in ["name", "name_en"] {
        assert!((2..=20).contains(&metadata[key].as_str().unwrap().chars().count()));
    }
    for key in ["description_zh", "description_en"] {
        assert!((20..=100).contains(&metadata[key].as_str().unwrap().chars().count()));
    }
    assert_eq!(metadata["source"], REGISTRATION_NAME);
    assert!(metadata.get("auth_mode").is_none());
    let mcp_bytes = fs::read(destination.join("mcp.json")).unwrap();
    let mcp: Value = serde_json::from_slice(&mcp_bytes).unwrap();
    assert_eq!(mcp["mcpServers"].as_object().unwrap().len(), 1);
    assert_eq!(
        mcp["mcpServers"][REGISTRATION_NAME]["command"],
        json!(f.paths.executable)
    );
    assert!(mcp["mcpServers"][REGISTRATION_NAME].get("env").is_none());
    assert!(!destination.join("token-schema.json").exists());
    assert!(
        fs::read_to_string(destination.join("icon.svg"))
            .unwrap()
            .contains("<svg")
    );
    assert_eq!(
        fs::read_to_string(destination.join("skills/tokemula-image/SKILL.md")).unwrap(),
        SKILL_CONTENT
    );
    assert_eq!(
        f.adapter.export_package(&destination).unwrap_err().code,
        "package_destination_conflict"
    );
    assert_eq!(fs::read(destination.join("mcp.json")).unwrap(), mcp_bytes);
    assert!(
        !f.adapter.config_path().exists(),
        "export must not register the product"
    );
    assert!(
        !f.paths.state_dir.exists(),
        "package generation must not claim installation"
    );
}

#[test]
fn identity_and_version_pairs_are_checked_before_writing_offline() {
    let f = Fixture::new();
    for (identity, version, code) in [
        (
            "com.workbuddy.workbuddy-ai",
            "5.11.5",
            "unsupported_version",
        ),
        ("com.tencent.workbuddy.mac", "5.11.4", "unsupported_version"),
        (
            "com.workbuddy.workbuddy-ai",
            "5.11.3",
            "unsupported_version",
        ),
        ("com.unrelated.app", "5.11.4", "unrecognized_client"),
    ] {
        Fixture::write_identity(&f.paths.app_bundle, identity, version);
        assert_eq!(f.adapter.detect().unwrap_err().code, code);
        assert_eq!(f.adapter.plan().unwrap_err().code, code);
        assert_eq!(f.adapter.check().unwrap_err().code, code);
        assert_eq!(f.adapter.install().unwrap_err().code, code);
        assert_eq!(f.adapter.remove().unwrap_err().code, code);
        assert!(!f.adapter.config_path().exists());
        assert!(!f.paths.state_dir.exists());
    }
}
