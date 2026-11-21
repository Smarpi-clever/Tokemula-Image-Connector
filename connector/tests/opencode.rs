//! OpenCode configuration protection and opt-in native CLI acceptance; no model turns.
#![cfg(unix)]

use serde_json::{Value, json};
use std::{
    env,
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};
use tempfile::TempDir;
use tokemula_image_connector::adapters::{
    Adapter, AdapterPaths, Harness, InstallStatus, SKILL_CONTENT, Scope,
    opencode::{OpenCodeAdapter, OpenCodeOverrides, OpenCodePaths},
};

struct Fixture {
    root: TempDir,
    paths: OpenCodePaths,
    scope: Scope,
}
impl Fixture {
    fn new(scope: Scope, cli: PathBuf) -> Self {
        let root = tempfile::Builder::new()
            .prefix("opencode-test 中文 ")
            .tempdir()
            .unwrap();
        let paths = OpenCodePaths {
            home: root.path().join("home"),
            xdg_config_home: root.path().join("xdg config"),
            project_dir: root.path().join("project 中文 space"),
            state_dir: root.path().join("private state"),
            cli,
            executable: root.path().join("独立 MCP space/tokemula-image-mcp"),
            process_path: env::var_os("TOKEMULA_TEST_PROCESS_PATH")
                .unwrap_or_else(|| OsString::from("/usr/bin:/bin")),
        };
        for p in [
            &paths.home,
            &paths.xdg_config_home,
            &paths.project_dir,
            paths.executable.parent().unwrap(),
        ] {
            fs::create_dir_all(p).unwrap();
        }
        fs::copy(env!("CARGO_BIN_EXE_tokemula-image-mcp"), &paths.executable).unwrap();
        Self { root, paths, scope }
    }
    fn real(scope: Scope) -> Self {
        Self::new(
            scope,
            PathBuf::from(
                env::var_os("TOKEMULA_OPENCODE_CLI")
                    .expect("explicit isolated OpenCode CLI required"),
            ),
        )
    }
    fn adapter(&self, overrides: OpenCodeOverrides) -> OpenCodeAdapter {
        OpenCodeAdapter::new(self.scope, self.paths.clone(), overrides).unwrap()
    }
    fn config(&self) -> PathBuf {
        match self.scope {
            Scope::User => self.paths.xdg_config_home.join("opencode/opencode.jsonc"),
            Scope::Project => self.paths.project_dir.join(".opencode/opencode.jsonc"),
        }
    }
    fn seed(&self, path: &Path) -> Vec<u8> {
        let text = br#"{
  // keep operator comment
  "providers": {"existing-provider": {"model": "synthetic-language", "future": true}},
  "model": "existing-provider/synthetic-language",
  "policies": [{"effect": "deny", "action": "synthetic-future-action"}],
  "mcp": {"servers": {
    "other-tool": {"type": "local", "command": ["/synthetic/other-tool"], "disabled": true} // keep other tool comment
  }},
  "future_unknown": {"values": [1, 2, 3]}
}
"#;
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(path, text).unwrap();
        text.to_vec()
    }
    fn state(&self) -> Value {
        let path = self.paths.state_dir.join("ownership.json");
        if path.exists() {
            serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
        } else {
            json!({"installations":[]})
        }
    }
}
fn parsed(path: &Path) -> Value {
    jsonc_parser::parse_to_serde_value(&fs::read_to_string(path).unwrap(), &Default::default())
        .unwrap()
}
fn add_later(path: &Path) {
    let mut text = fs::read_to_string(path).unwrap();
    let end = text.rfind('}').unwrap();
    text.insert_str(
        end,
        ",\n  // later user comment\n  \"later_user\": {\"keep\":\"later-value\"}\n",
    );
    fs::write(path, text).unwrap();
}
fn assert_preserved(path: &Path) {
    let text = fs::read_to_string(path).unwrap();
    assert!(text.contains("// keep operator comment"));
    assert!(text.contains("// keep other tool comment"));
    assert!(text.contains("// later user comment"));
    let v = parsed(path);
    assert_eq!(v["model"], "existing-provider/synthetic-language");
    assert_eq!(v["providers"]["existing-provider"]["future"], true);
    assert_eq!(v["future_unknown"]["values"], json!([1, 2, 3]));
    assert_eq!(v["later_user"]["keep"], "later-value");
    assert_eq!(v["mcp"]["servers"]["other-tool"]["disabled"], true);
}
fn script(path: &Path, body: &str) {
    fs::write(path, body).unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

#[test]
#[ignore = "Requires an explicit OpenCode V2 CLI; config management only"]
fn real_user_project_lifecycle_preserves_jsonc_and_effective_file_precedence() {
    for scope in [Scope::User, Scope::Project] {
        let f = Fixture::real(scope);
        let config = f.config();
        f.seed(&config);
        let lower = config.with_extension("json");
        fs::write(
            &lower,
            b"{\"future_lower\":\"must-remain-byte-identical\"}\n",
        )
        .unwrap();
        let lower_before = fs::read(&lower).unwrap();
        let adapter = f.adapter(OpenCodeOverrides::default());
        let plan = adapter.plan().unwrap();
        assert_eq!(plan.config_path, config);
        assert!(plan.skill_path.starts_with(f.root.path()));
        assert_eq!(plan.status, InstallStatus::Ready);
        assert_eq!(adapter.install().unwrap(), InstallStatus::Installed);
        let value = parsed(&config);
        assert_eq!(
            value["mcp"]["servers"]["tokemula-image"],
            json!({"type":"local","command":[f.paths.executable]})
        );
        assert_eq!(fs::read_to_string(&plan.skill_path).unwrap(), SKILL_CONTENT);
        let installed = fs::read(&config).unwrap();
        assert_eq!(adapter.install().unwrap(), InstallStatus::AlreadyInstalled);
        assert_eq!(adapter.check().unwrap(), InstallStatus::Installed);
        assert_eq!(fs::read(&config).unwrap(), installed);
        assert_eq!(fs::read(&lower).unwrap(), lower_before);
        #[cfg(unix)]
        {
            assert_eq!(
                fs::metadata(&f.paths.state_dir)
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o700
            );
            assert_eq!(
                fs::metadata(f.paths.state_dir.join("ownership.json"))
                    .unwrap()
                    .permissions()
                    .mode()
                    & 0o777,
                0o600
            );
            let snapshot = f.state()["installations"][0]["snapshot"]
                .as_str()
                .unwrap()
                .to_owned();
            assert_eq!(
                fs::metadata(snapshot).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        add_later(&config);
        assert_eq!(adapter.remove().unwrap(), InstallStatus::Removed);
        assert_preserved(&config);
        assert!(
            parsed(&config)["mcp"]["servers"]
                .get("tokemula-image")
                .is_none()
        );
        assert!(!plan.skill_path.exists());
        assert_eq!(fs::read(&lower).unwrap(), lower_before);
        assert_eq!(adapter.remove().unwrap(), InstallStatus::NotInstalled);
    }
}

#[test]
#[ignore = "Requires an explicit OpenCode V2 CLI; config management only"]
fn real_overrides_use_the_selected_root_file_and_keep_scope_boundaries() {
    let f = Fixture::real(Scope::User);
    let ordinary = f.config();
    let ordinary_before = f.seed(&ordinary);
    let custom_dir = f.root.path().join("effective config 中文");
    let selected = custom_dir.join("opencode.jsonc");
    f.seed(&selected);
    let overrides = OpenCodeOverrides {
        config_dir: Some(custom_dir.clone()),
        ..Default::default()
    };
    let adapter = f.adapter(overrides.clone());
    let plan = adapter.plan().unwrap();
    assert_eq!(plan.config_path, selected);
    assert_eq!(
        plan.skill_path,
        custom_dir.join("skills/tokemula-image/SKILL.md")
    );
    assert_eq!(adapter.install().unwrap(), InstallStatus::Installed);
    assert_eq!(fs::read(&ordinary).unwrap(), ordinary_before);
    assert_eq!(adapter.remove().unwrap(), InstallStatus::Removed);

    let explicit = f.root.path().join("selected file.jsonc");
    f.seed(&explicit);
    let adapter = f.adapter(OpenCodeOverrides {
        config_file: Some(explicit.clone()),
        config_content: Some("{\"future_inline\":true}".into()),
        ..overrides
    });
    assert_eq!(adapter.plan().unwrap().config_path, explicit);
    assert_eq!(adapter.install().unwrap(), InstallStatus::Installed);
    assert_eq!(adapter.remove().unwrap(), InstallStatus::Removed);
    assert_eq!(fs::read(&ordinary).unwrap(), ordinary_before);
    assert!(
        !parsed(&explicit)
            .as_object()
            .unwrap()
            .contains_key("future_inline")
    );

    let readonly = f.adapter(OpenCodeOverrides {
        config_content: Some(
            r#"{"mcp":{"servers":{"tokemula-image":{"type":"local","command":["/other"]}}}}"#
                .into(),
        ),
        ..Default::default()
    });
    assert_eq!(readonly.install().unwrap_err().code, "read_only_override");
    assert_eq!(fs::read(&ordinary).unwrap(), ordinary_before);
    let project = OpenCodeAdapter::new(
        Scope::Project,
        f.paths.clone(),
        OpenCodeOverrides {
            project_disabled: true,
            ..Default::default()
        },
    )
    .unwrap();
    assert_eq!(
        project.install().unwrap_err().code,
        "project_configuration_disabled"
    );
    assert!(!f.paths.project_dir.join("opencode.json").exists());
}

#[test]
#[ignore = "Requires an explicit OpenCode V2 CLI; config management only"]
fn real_conflicts_preserve_foreign_and_user_modified_items() {
    for scope in [Scope::User, Scope::Project] {
        let f = Fixture::real(scope);
        let path = f.config();
        f.seed(&path);
        let adapter = f.adapter(OpenCodeOverrides::default());
        let planned = adapter.plan().unwrap();
        let mut value = parsed(&path);
        value["mcp"]["servers"]["tokemula-image"] = json!({"type":"local","command":["/foreign"]});
        fs::write(&path, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
        let foreign = fs::read(&path).unwrap();
        assert_eq!(adapter.install().unwrap_err().code, "ownership_conflict");
        assert_eq!(fs::read(&path).unwrap(), foreign);
        f.seed(&path);
        fs::create_dir_all(planned.skill_path.parent().unwrap()).unwrap();
        fs::write(&planned.skill_path, SKILL_CONTENT).unwrap();
        assert_eq!(adapter.install().unwrap_err().code, "ownership_conflict");
        fs::remove_file(&planned.skill_path).unwrap();
        fs::remove_dir(planned.skill_path.parent().unwrap()).unwrap();
        assert_eq!(adapter.install().unwrap(), InstallStatus::Installed);
        let config_before = fs::read(&path).unwrap();
        fs::write(
            &planned.skill_path,
            format!("{SKILL_CONTENT}\nUser change\n"),
        )
        .unwrap();
        assert_eq!(adapter.check().unwrap_err().code, "ownership_conflict");
        assert_eq!(adapter.remove().unwrap_err().code, "ownership_conflict");
        assert_eq!(fs::read(&path).unwrap(), config_before);
        fs::write(&planned.skill_path, SKILL_CONTENT).unwrap();
        let mut altered = parsed(&path);
        altered["mcp"]["servers"]["tokemula-image"]["disabled"] = json!(true);
        fs::write(&path, serde_json::to_vec_pretty(&altered).unwrap()).unwrap();
        let modified = fs::read(&path).unwrap();
        assert_eq!(adapter.install().unwrap_err().code, "ownership_conflict");
        assert_eq!(adapter.remove().unwrap_err().code, "ownership_conflict");
        assert_eq!(fs::read(&path).unwrap(), modified);
        fs::write(&path, &config_before).unwrap();
        fs::remove_file(&path).unwrap();
        assert_eq!(adapter.check().unwrap_err().code, "ownership_conflict");
        fs::write(&path, config_before).unwrap();
        assert_eq!(adapter.remove().unwrap(), InstallStatus::Removed);
    }
}

#[test]
#[ignore = "Requires an explicit OpenCode V2 CLI; config management only"]
fn real_effective_foreign_server_cannot_be_shadowed_across_scopes() {
    for scope in [Scope::Project, Scope::User] {
        let f = Fixture::real(scope);
        let foreign_path = if scope == Scope::Project {
            f.paths.xdg_config_home.join("opencode/opencode.jsonc")
        } else {
            f.paths.project_dir.join("opencode.jsonc")
        };
        fs::create_dir_all(foreign_path.parent().unwrap()).unwrap();
        let foreign = br#"{"mcp":{"servers":{"tokemula-image":{"type":"local","command":["/foreign/owned-by-user"]}}}}"#;
        fs::write(&foreign_path, foreign).unwrap();
        let adapter = f.adapter(OpenCodeOverrides::default());
        let expected = if scope == Scope::Project {
            "ownership_conflict"
        } else {
            "configuration_shadowed"
        };
        assert_eq!(adapter.plan().unwrap_err().code, expected);
        assert_eq!(adapter.check().unwrap_err().code, expected);
        assert_eq!(adapter.install().unwrap_err().code, expected);
        assert_eq!(adapter.remove().unwrap_err().code, expected);
        assert_eq!(fs::read(&foreign_path).unwrap(), foreign);
        assert!(!f.paths.state_dir.join("ownership.json").exists());
        assert!(!f.paths.state_dir.join("snapshots").exists());
        if f.paths.state_dir.exists() {
            assert!(
                fs::read_dir(&f.paths.state_dir)
                    .unwrap()
                    .all(|entry| entry.unwrap().file_name() == "tmp")
            );
        }
        assert!(!f.paths.project_dir.join("opencode.json").exists());
        assert!(
            !f.paths
                .xdg_config_home
                .join("opencode/opencode.json")
                .exists()
        );
        assert!(!f.paths.project_dir.join(".opencode").exists());
    }
}

#[test]
#[ignore = "Requires an explicit OpenCode V2 CLI; config management only"]
fn real_owned_user_and_project_registrations_survive_either_uninstall_order() {
    for user_first in [true, false] {
        let f = Fixture::real(Scope::User);
        f.seed(&f.config());
        let user = f.adapter(OpenCodeOverrides::default());
        assert_eq!(user.install().unwrap(), InstallStatus::Installed);
        let user_plan = user.plan().unwrap();
        let project = OpenCodeAdapter::new(
            Scope::Project,
            f.paths.clone(),
            OpenCodeOverrides::default(),
        )
        .unwrap();
        assert_eq!(project.install().unwrap(), InstallStatus::Installed);
        let project_plan = project.plan().unwrap();
        assert_ne!(user_plan.config_path, project_plan.config_path);
        assert_eq!(user_plan.skill_path, project_plan.skill_path);
        assert_eq!(user.check().unwrap(), InstallStatus::Installed);
        assert_eq!(project.check().unwrap(), InstallStatus::Installed);
        assert_eq!(user.install().unwrap(), InstallStatus::AlreadyInstalled);
        assert_eq!(project.install().unwrap(), InstallStatus::AlreadyInstalled);
        assert_eq!(f.state()["installations"].as_array().unwrap().len(), 2);
        let (first, last, remaining) = if user_first {
            (&user, &project, &project_plan.config_path)
        } else {
            (&project, &user, &user_plan.config_path)
        };
        assert_eq!(first.remove().unwrap(), InstallStatus::Removed);
        assert_eq!(first.check().unwrap(), InstallStatus::NotInstalled);
        assert_eq!(first.remove().unwrap(), InstallStatus::NotInstalled);
        assert_eq!(last.check().unwrap(), InstallStatus::Installed);
        assert_eq!(
            parsed(remaining)["mcp"]["servers"]["tokemula-image"]["command"],
            json!([f.paths.executable])
        );
        assert_eq!(
            fs::read_to_string(&user_plan.skill_path).unwrap(),
            SKILL_CONTENT
        );
        assert_eq!(last.remove().unwrap(), InstallStatus::Removed);
        assert!(!user_plan.skill_path.exists());
        assert_eq!(user.check().unwrap(), InstallStatus::NotInstalled);
        assert_eq!(project.check().unwrap(), InstallStatus::NotInstalled);
        assert!(f.state()["installations"].as_array().unwrap().is_empty());
    }
}

#[test]
#[ignore = "Requires explicit OpenCode and Codex CLI paths; no model turns"]
fn real_codex_shared_skill_survives_either_uninstall_order() {
    let codex_cli =
        PathBuf::from(env::var_os("TOKEMULA_TEST_CODEX_CLI").expect("explicit Codex CLI required"));
    for codex_first in [true, false] {
        let f = Fixture::real(Scope::User);
        f.seed(&f.config());
        let lower_skill = f.paths.home.join(".claude/skills/tokemula-image/SKILL.md");
        fs::create_dir_all(lower_skill.parent().unwrap()).unwrap();
        let lower_content = "---\nname: another-display-name\ndescription: Existing lower priority skill\n---\nKeep the other client's skill.\n";
        fs::write(&lower_skill, lower_content).unwrap();
        let codex_home = f.root.path().join("codex home");
        fs::create_dir_all(&codex_home).unwrap();
        let codex = Adapter::new(
            Harness::Codex,
            Scope::User,
            AdapterPaths {
                home: f.paths.home.clone(),
                codex_home,
                claude_config_dir: None,
                project_dir: f.paths.project_dir.clone(),
                state_dir: f.paths.state_dir.clone(),
                cli: codex_cli.clone(),
                executable: f.paths.executable.clone(),
                process_path: f.paths.process_path.clone(),
            },
        )
        .unwrap();
        assert_eq!(codex.install().unwrap(), InstallStatus::Installed);
        let open = f.adapter(OpenCodeOverrides::default());
        let plan = open.plan().unwrap();
        assert_eq!(plan.skill_path, codex.skill_path());
        assert_eq!(open.install().unwrap(), InstallStatus::Installed);
        assert!(
            !f.paths
                .xdg_config_home
                .join("opencode/skills/tokemula-image/SKILL.md")
                .exists()
        );
        assert_eq!(f.state()["installations"].as_array().unwrap().len(), 2);
        if codex_first {
            assert_eq!(codex.remove().unwrap(), InstallStatus::Removed);
            assert_eq!(fs::read_to_string(&plan.skill_path).unwrap(), SKILL_CONTENT);
            assert_eq!(open.check().unwrap(), InstallStatus::Installed);
            assert_eq!(open.remove().unwrap(), InstallStatus::Removed);
        } else {
            assert_eq!(open.remove().unwrap(), InstallStatus::Removed);
            assert_eq!(fs::read_to_string(&plan.skill_path).unwrap(), SKILL_CONTENT);
            assert_eq!(codex.check().unwrap(), InstallStatus::Installed);
            assert_eq!(codex.remove().unwrap(), InstallStatus::Removed);
        }
        assert!(!plan.skill_path.exists());
        assert!(f.state()["installations"].as_array().unwrap().is_empty());
        assert_eq!(fs::read_to_string(lower_skill).unwrap(), lower_content);
    }
}

#[test]
#[ignore = "Requires an explicit OpenCode V2 CLI; owned filesystem fault injection"]
fn real_post_registration_skill_failure_rolls_back_and_allows_retry() {
    struct RestorePermissions(PathBuf, fs::Permissions);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, self.1.clone());
        }
    }
    for scope in [Scope::User, Scope::Project] {
        let f = Fixture::real(scope);
        let config = f.config();
        let before = f.seed(&config);
        let adapter = f.adapter(Default::default());
        let skill = adapter.plan().unwrap().skill_path;
        let root = skill.parent().unwrap().parent().unwrap();
        fs::create_dir_all(root).unwrap();
        let guard = RestorePermissions(root.to_owned(), fs::metadata(root).unwrap().permissions());
        fs::set_permissions(root, fs::Permissions::from_mode(0o500)).unwrap();
        let result = adapter.install();
        drop(guard);
        assert_eq!(result.unwrap_err().code, "storage_error");
        assert_eq!(fs::read(&config).unwrap(), before);
        assert!(f.state()["installations"].as_array().unwrap().is_empty());
        assert!(!skill.exists());
        assert_eq!(adapter.install().unwrap(), InstallStatus::Installed);
        assert_eq!(adapter.remove().unwrap(), InstallStatus::Removed);
    }
}

#[test]
fn version_and_injected_cli_failure_never_leave_invalid_registration() {
    let f = Fixture::new(Scope::User, PathBuf::from("/synthetic/missing-client"));
    f.seed(&f.config());
    let before = fs::read(f.config()).unwrap();
    assert_eq!(
        f.adapter(Default::default()).install().unwrap_err().code,
        "client_missing"
    );
    let fake = f.root.path().join("controlled-cli");
    script(&fake, "#!/bin/sh\nprintf 'opencode v1.19.6\\n'\n");
    let mut paths = f.paths.clone();
    paths.cli = fake.clone();
    let old = OpenCodeAdapter::new(Scope::User, paths.clone(), Default::default()).unwrap();
    assert_eq!(old.install().unwrap_err().code, "unsupported_version");
    assert_eq!(fs::read(f.config()).unwrap(), before);
    assert!(!f.paths.state_dir.exists());
    script(
        &fake,
        "#!/bin/sh\nif [ \"$1\" = '--version' ]; then printf 'opencode v2.3.2\\n'; exit 0; fi\nprintf '{\"partial_write\":true}' > \"$XDG_CONFIG_HOME/opencode/opencode.jsonc\"\nexit 7\n",
    );
    let failed = OpenCodeAdapter::new(Scope::User, paths, Default::default()).unwrap();
    assert_eq!(failed.install().unwrap_err().code, "cli_failed");
    assert_eq!(fs::read(f.config()).unwrap(), before);
    assert!(f.state()["installations"].as_array().unwrap().is_empty());
    assert!(
        !f.paths
            .xdg_config_home
            .join("opencode/skills/tokemula-image/SKILL.md")
            .exists()
    );
}

#[test]
#[ignore = "Explicit product API acceptance driver; caller owns all isolated roots"]
fn opencode_isolated_acceptance_driver() {
    let input = env::var_os("TOKEMULA_OPENCODE_ACCEPTANCE_INPUT").expect("explicit input required");
    let output =
        env::var_os("TOKEMULA_OPENCODE_ACCEPTANCE_OUTPUT").expect("explicit output required");
    let phase = env::var("TOKEMULA_OPENCODE_ACCEPTANCE_PHASE")
        .expect("install, remove_codex or remove phase required");
    let fixture: Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
    let path = |v: &Value, key: &str| PathBuf::from(v[key].as_str().unwrap());
    let root = path(&fixture, "root");
    assert!(root.is_absolute() && root.is_dir());
    let mut results = vec![];
    for case in fixture["cases"].as_array().unwrap() {
        for key in ["home", "xdg_config_home", "project_dir", "state_dir"] {
            assert!(
                path(case, key).starts_with(&root),
                "fixture escapes root: {key}"
            );
        }
        for key in ["config_dir", "config_file"] {
            if let Some(value) = case[key].as_str() {
                assert!(Path::new(value).starts_with(&root));
            }
        }
        if phase == "remove_codex" && case["codex_cli"].as_str().is_none() {
            continue;
        }
        let codex = case["codex_cli"].as_str().map(|cli| {
            let codex_home = path(case, "home").join(".codex");
            assert!(codex_home.starts_with(&root));
            fs::create_dir_all(&codex_home).unwrap();
            Adapter::new(
                Harness::Codex,
                Scope::User,
                AdapterPaths {
                    home: path(case, "home"),
                    codex_home,
                    claude_config_dir: None,
                    project_dir: path(case, "project_dir"),
                    state_dir: path(case, "state_dir"),
                    cli: PathBuf::from(cli),
                    executable: path(case, "executable"),
                    process_path: case["process_path"].as_str().unwrap().into(),
                },
            )
            .unwrap()
        });
        if phase == "install"
            && let Some(codex) = &codex
        {
            assert_eq!(codex.install().unwrap(), InstallStatus::Installed);
        }
        let scope = match case["scope"].as_str().unwrap() {
            "user" => Scope::User,
            "project" => Scope::Project,
            _ => panic!("invalid scope"),
        };
        let adapter = OpenCodeAdapter::new(
            scope,
            OpenCodePaths {
                home: path(case, "home"),
                xdg_config_home: path(case, "xdg_config_home"),
                project_dir: path(case, "project_dir"),
                state_dir: path(case, "state_dir"),
                cli: path(case, "cli"),
                executable: path(case, "executable"),
                process_path: case["process_path"].as_str().unwrap().into(),
            },
            OpenCodeOverrides {
                config_dir: case["config_dir"].as_str().map(PathBuf::from),
                config_file: case["config_file"].as_str().map(PathBuf::from),
                config_content: case["config_content"].as_str().map(str::to_owned),
                project_disabled: case["project_disabled"].as_bool().unwrap_or(false),
                claude_skills_disabled: case["claude_skills_disabled"].as_bool().unwrap_or(false),
            },
        )
        .unwrap();
        let plan = adapter.plan().unwrap();
        assert!(plan.config_path.starts_with(&root));
        assert!(plan.skill_path.starts_with(&root));
        match phase.as_str() {
            "install" => {
                assert_eq!(adapter.install().unwrap(), InstallStatus::Installed);
                assert_eq!(adapter.install().unwrap(), InstallStatus::AlreadyInstalled);
                assert_eq!(adapter.check().unwrap(), InstallStatus::Installed);
            }
            "remove_codex" => {
                let codex = codex.as_ref().unwrap();
                assert_eq!(plan.skill_path, codex.skill_path());
                assert_eq!(codex.remove().unwrap(), InstallStatus::Removed);
                assert_eq!(adapter.check().unwrap(), InstallStatus::Installed);
                assert_eq!(fs::read_to_string(&plan.skill_path).unwrap(), SKILL_CONTENT);
            }
            "remove" => {
                add_later(&plan.config_path);
                assert_eq!(adapter.remove().unwrap(), InstallStatus::Removed);
                assert_eq!(adapter.check().unwrap(), InstallStatus::NotInstalled);
                assert_eq!(adapter.remove().unwrap(), InstallStatus::NotInstalled);
                assert_eq!(
                    parsed(&plan.config_path)["later_user"]["keep"],
                    "later-value"
                );
                assert!(
                    parsed(&plan.config_path)["mcp"]["servers"]
                        .get("tokemula-image")
                        .is_none()
                );
                assert!(!plan.skill_path.exists());
            }
            _ => panic!("invalid phase"),
        }
        results.push(json!({"name":case["name"],"scope":scope,"version":plan.version,"phase":phase,"config_path":plan.config_path,"skill_path":plan.skill_path,"status":adapter.check().unwrap()}));
    }
    fs::write(output, serde_json::to_vec_pretty(&results).unwrap()).unwrap();
    println!(
        "{} OpenCode product-API cases passed: {}",
        results.len(),
        phase
    );
}
