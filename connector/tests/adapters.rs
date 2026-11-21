//! Configuration protection tests. Real-client cases are opt-in and never start model turns.

#![cfg(unix)]

use serde_json::{Value, json};
use std::{ffi::OsString, fs, os::unix::fs::PermissionsExt, path::PathBuf};
use tempfile::TempDir;
use tokemula_image_connector::adapters::{
    Adapter, AdapterPaths, Harness, InstallStatus, REGISTRATION_NAME, SKILL_CONTENT, Scope,
};

struct Fixture {
    _root: TempDir,
    harness: Harness,
    paths: AdapterPaths,
    adapter: Adapter,
}

impl Fixture {
    fn new(harness: Harness, scope: Scope, real_cli: bool) -> Self {
        let root = tempfile::Builder::new()
            .prefix("tokemula-adapters 中文 ")
            .tempdir()
            .unwrap();
        let cli = if real_cli {
            PathBuf::from(
                std::env::var_os(match harness {
                    Harness::Codex => "TOKEMULA_TEST_CODEX_CLI",
                    Harness::ClaudeCode => "TOKEMULA_TEST_CLAUDE_CLI",
                })
                .expect("supply an explicit installed CLI path for this ignored test"),
            )
        } else {
            root.path().join("absent-client")
        };
        let executable = root.path().join("独立 MCP 程序");
        fs::write(&executable, b"#!/bin/sh\nexit 0\n").unwrap();
        fs::set_permissions(&executable, fs::Permissions::from_mode(0o700)).unwrap();
        let paths = AdapterPaths {
            home: root.path().join("home"),
            codex_home: root.path().join("codex config"),
            claude_config_dir: Some(root.path().join("claude config")),
            project_dir: root.path().join("项目 root"),
            state_dir: root.path().join("private state"),
            cli,
            executable,
            process_path: std::env::var_os("TOKEMULA_TEST_PROCESS_PATH")
                .unwrap_or_else(|| OsString::from("/usr/bin:/bin")),
        };
        for path in [
            &paths.home,
            &paths.codex_home,
            paths.claude_config_dir.as_ref().unwrap(),
            &paths.project_dir,
        ] {
            fs::create_dir_all(path).unwrap();
        }
        let adapter = Adapter::new(harness, scope, paths.clone()).unwrap();
        Self {
            _root: root,
            harness,
            paths,
            adapter,
        }
    }

    fn write_complex(&self) -> Vec<u8> {
        let content = match self.harness {
            Harness::Codex => {
                let project = serde_json::to_string(self.paths.project_dir.to_str().unwrap()).unwrap();
                format!(
                    "# keep document comment\nmodel = 'synthetic-language-model' # keep model comment\nmodel_provider = 'synthetic-existing'\n\n[model_providers.synthetic-existing]\nname = 'Existing provider'\nbase_url = 'http://127.0.0.1:9/v1'\nenv_key = 'SYNTHETIC_NOT_CONFIGURED'\nwire_api = 'responses'\n\n[mcp_servers.other-tool]\ncommand = '/bin/false' # keep other MCP comment\nenabled = false\n\n[projects.{project}]\ntrust_level = 'untrusted'\n\n[future_unknown]\nvalues = [1, 2, 3]\n"
                ).into_bytes()
            }
            Harness::ClaudeCode => serde_json::to_vec_pretty(&json!({
                "model": "synthetic-language-model",
                "provider": {"name":"existing-provider", "future_option":true},
                "mcpServers": {"other-tool":{"type":"stdio","command":"/bin/false","args":[],"env":{}}},
                "projects": {self.paths.project_dir.to_string_lossy().as_ref(): {"hasTrustDialogAccepted":false}},
                "future_unknown":{"values":[1,2,3]}
            })).unwrap(),
        };
        let config = self.adapter.config_path();
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        fs::write(config, &content).unwrap();
        let other_skill = self
            .adapter
            .skill_path()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("other-skill/SKILL.md");
        fs::create_dir_all(other_skill.parent().unwrap()).unwrap();
        fs::write(other_skill, b"User's other skill\n").unwrap();
        content
    }

    fn parsed(&self) -> Value {
        let bytes = fs::read(self.adapter.config_path()).unwrap();
        match self.harness {
            Harness::Codex => {
                toml_edit::de::from_str(std::str::from_utf8(&bytes).unwrap()).unwrap()
            }
            Harness::ClaudeCode => serde_json::from_slice(&bytes).unwrap(),
        }
    }

    fn mutate_config(&self, f: impl FnOnce(&mut Value)) {
        if self.harness == Harness::Codex {
            panic!("use TOML mutation helper for preserving fixture comments");
        }
        let mut value = self.parsed();
        f(&mut value);
        fs::write(
            self.adapter.config_path(),
            serde_json::to_vec_pretty(&value).unwrap(),
        )
        .unwrap();
    }

    fn servers_key(&self) -> &'static str {
        match self.harness {
            Harness::Codex => "mcp_servers",
            Harness::ClaudeCode => "mcpServers",
        }
    }

    fn add_later_user_fields(&self) {
        match self.harness {
            Harness::Codex => {
                use std::io::Write;
                let mut file = fs::OpenOptions::new()
                    .append(true)
                    .open(self.adapter.config_path())
                    .unwrap();
                file.write_all(b"\n# later user comment\n[user_later]\nkeep = 'later-value'\n\n[mcp_servers.later-tool]\ncommand = '/bin/false'\nenabled = false\n").unwrap();
            }
            Harness::ClaudeCode => self.mutate_config(|v| {
                v["user_later"] = json!({"keep":"later-value"});
                v["mcpServers"]["later-tool"] =
                    json!({"type":"stdio","command":"/bin/false","args":[],"env":{}});
            }),
        }
    }

    fn alter_owned_registration(&self) {
        match self.harness {
            Harness::Codex => {
                let mut doc: toml_edit::DocumentMut =
                    fs::read_to_string(self.adapter.config_path())
                        .unwrap()
                        .parse()
                        .unwrap();
                doc["mcp_servers"][REGISTRATION_NAME]["command"] =
                    toml_edit::value("/user/changed-program");
                fs::write(self.adapter.config_path(), doc.to_string()).unwrap();
            }
            Harness::ClaudeCode => self.mutate_config(|v| {
                v["mcpServers"][REGISTRATION_NAME]["command"] = json!("/user/changed-program")
            }),
        }
    }
}

fn cases() -> [(Harness, Scope); 4] {
    [
        (Harness::Codex, Scope::User),
        (Harness::Codex, Scope::Project),
        (Harness::ClaudeCode, Scope::User),
        (Harness::ClaudeCode, Scope::Project),
    ]
}

fn assert_original_values(original: &Value, current: &Value, key: &str) {
    for (name, value) in original.as_object().unwrap() {
        if name == key {
            for (entry_name, entry) in value.as_object().unwrap() {
                assert!(
                    current[key][entry_name] == *entry,
                    "existing MCP entry changed"
                );
            }
        } else {
            assert!(
                current[name] == *value,
                "existing configuration field changed: {name}"
            );
        }
    }
}

#[test]
#[ignore = "explicit installed CLI paths; isolated configuration operations only"]
fn installed_clients_preserve_complex_configuration_reinstall_and_uninstall() {
    for (harness, scope) in cases() {
        let fixture = Fixture::new(harness, scope, true);
        let original_bytes = fixture.write_complex();
        let original = fixture.parsed();
        assert_eq!(fixture.adapter.plan().unwrap().status, InstallStatus::Ready);
        assert_eq!(
            fixture.adapter.install().unwrap(),
            InstallStatus::Installed,
            "{harness:?}/{scope:?}"
        );
        assert_eq!(fixture.adapter.check().unwrap(), InstallStatus::Installed);
        assert_eq!(
            fs::read_to_string(fixture.adapter.skill_path()).unwrap(),
            SKILL_CONTENT
        );
        let current = fixture.parsed();
        assert_original_values(&original, &current, fixture.servers_key());
        assert_eq!(
            current[fixture.servers_key()][REGISTRATION_NAME]["command"],
            fixture.paths.executable.to_string_lossy().as_ref()
        );
        let before_repeat = fs::read(fixture.adapter.config_path()).unwrap();
        assert_eq!(
            fixture.adapter.install().unwrap(),
            InstallStatus::AlreadyInstalled
        );
        assert!(
            fs::read(fixture.adapter.config_path()).unwrap() == before_repeat,
            "repeat installation rewrote configuration"
        );
        let state = fixture.paths.state_dir.join("ownership.json");
        assert_eq!(
            fs::metadata(&fixture.paths.state_dir)
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o700
        );
        assert_eq!(
            fs::metadata(&state).unwrap().permissions().mode() & 0o777,
            0o600
        );
        let state_doc: Value = serde_json::from_slice(&fs::read(&state).unwrap()).unwrap();
        let snapshot = PathBuf::from(state_doc["installations"][0]["snapshot"].as_str().unwrap());
        assert!(snapshot.starts_with(&fixture.paths.state_dir));
        assert!(
            fs::read(&snapshot).unwrap() == original_bytes,
            "private snapshot differs from original"
        );
        assert_eq!(
            fs::metadata(&snapshot).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fixture.add_later_user_fields();
        let own_extra = fixture
            .adapter
            .skill_path()
            .parent()
            .unwrap()
            .join("user-note.txt");
        fs::write(&own_extra, b"keep this user note\n").unwrap();
        let mut next_paths = fixture.paths.clone();
        next_paths.executable = fixture.paths.home.join("新版 MCP 程序");
        fs::copy(&fixture.paths.executable, &next_paths.executable).unwrap();
        let next_adapter = Adapter::new(harness, scope, next_paths).unwrap();
        assert_eq!(next_adapter.check().unwrap(), InstallStatus::Installed);
        let before_update = fs::read(fixture.adapter.config_path()).unwrap();
        assert_eq!(
            next_adapter.plan().unwrap_err().code,
            "installation_update_required"
        );
        assert_eq!(
            next_adapter.install().unwrap_err().code,
            "installation_update_required"
        );
        assert!(fs::read(fixture.adapter.config_path()).unwrap() == before_update);
        assert_eq!(
            next_adapter.remove().unwrap(),
            InstallStatus::Removed,
            "{harness:?}/{scope:?}"
        );
        let current = fixture.parsed();
        assert_original_values(&original, &current, fixture.servers_key());
        assert!(
            current[fixture.servers_key()]
                .get(REGISTRATION_NAME)
                .is_none()
        );
        assert_eq!(current["user_later"]["keep"], "later-value");
        assert!(current[fixture.servers_key()].get("later-tool").is_some());
        assert!(!fixture.adapter.skill_path().exists());
        assert!(own_extra.exists());
        let other_skill = fixture
            .adapter
            .skill_path()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .join("other-skill/SKILL.md");
        assert_eq!(
            fs::read_to_string(other_skill).unwrap(),
            "User's other skill\n"
        );
        assert!(!snapshot.exists());
        if harness == Harness::Codex {
            let text = fs::read_to_string(fixture.adapter.config_path()).unwrap();
            for comment in [
                "# keep document comment",
                "# keep model comment",
                "# keep other MCP comment",
                "# later user comment",
            ] {
                assert!(text.contains(comment), "TOML comment lost: {comment}");
            }
        }
        eprintln!(
            "protection {harness:?}/{scope:?}: install, repeat, private snapshot, later fields and new-request-path owned uninstall PASS"
        );
    }
}

#[test]
#[ignore = "explicit installed CLI paths; isolated configuration operations only"]
fn installed_clients_reject_foreign_modified_and_malformed_configuration() {
    for (harness, scope) in cases() {
        let fixture = Fixture::new(harness, scope, true);
        fixture.write_complex();
        // An unowned same-name registration is a conflict even if it resembles our executable.
        match harness {
            Harness::Codex => {
                let mut doc: toml_edit::DocumentMut =
                    fs::read_to_string(fixture.adapter.config_path())
                        .unwrap()
                        .parse()
                        .unwrap();
                doc["mcp_servers"][REGISTRATION_NAME]["command"] =
                    toml_edit::value("/foreign/program");
                fs::write(fixture.adapter.config_path(), doc.to_string()).unwrap();
            }
            Harness::ClaudeCode => fixture.mutate_config(|v| {
                v["mcpServers"][REGISTRATION_NAME] = json!({"command":"/foreign/program"})
            }),
        }
        let foreign = fs::read(fixture.adapter.config_path()).unwrap();
        assert_eq!(
            fixture.adapter.install().unwrap_err().code,
            "ownership_conflict"
        );
        assert!(fs::read(fixture.adapter.config_path()).unwrap() == foreign);
        assert!(!fixture.paths.state_dir.join("ownership.json").exists());
        fixture.write_complex();
        let skill = fixture.adapter.skill_path();
        fs::create_dir_all(skill.parent().unwrap()).unwrap();
        fs::write(&skill, b"foreign skill\n").unwrap();
        assert_eq!(
            fixture.adapter.install().unwrap_err().code,
            "ownership_conflict"
        );
        assert_eq!(fs::read_to_string(&skill).unwrap(), "foreign skill\n");
        fs::remove_file(&skill).unwrap();
        fs::remove_dir(skill.parent().unwrap()).unwrap();
        assert_eq!(fixture.adapter.install().unwrap(), InstallStatus::Installed);
        fs::write(&skill, b"user-modified product skill\n").unwrap();
        assert_eq!(
            fixture.adapter.install().unwrap_err().code,
            "ownership_conflict"
        );
        assert_eq!(
            fixture.adapter.remove().unwrap_err().code,
            "ownership_conflict"
        );
        assert_eq!(
            fs::read_to_string(&skill).unwrap(),
            "user-modified product skill\n"
        );
        fs::write(&skill, SKILL_CONTENT).unwrap();
        fixture.alter_owned_registration();
        let changed = fs::read(fixture.adapter.config_path()).unwrap();
        assert_eq!(
            fixture.adapter.install().unwrap_err().code,
            "ownership_conflict"
        );
        assert_eq!(
            fixture.adapter.remove().unwrap_err().code,
            "ownership_conflict"
        );
        assert!(fs::read(fixture.adapter.config_path()).unwrap() == changed);
        assert!(skill.exists());
        // Independent malformed fixture must fail before any product state is written.
        let malformed = Fixture::new(harness, scope, true);
        let config = malformed.adapter.config_path();
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        let original = b"not a valid configuration [ { ";
        fs::write(&config, original).unwrap();
        assert_eq!(
            malformed.adapter.install().unwrap_err().code,
            "unrecognized_configuration"
        );
        assert!(fs::read(config).unwrap() == original);
        assert!(!malformed.paths.state_dir.join("ownership.json").exists());
        eprintln!(
            "conflicts {harness:?}/{scope:?}: foreign MCP/Skill, user-modified registration and malformed config PASS"
        );
    }
}

struct RestorePermissions(PathBuf);
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

#[test]
#[ignore = "explicit installed CLI paths; failure injected into temporary Skill directory permissions"]
fn installed_clients_roll_back_when_skill_write_fails_after_registration() {
    for (harness, scope) in cases() {
        let fixture = Fixture::new(harness, scope, true);
        let before = fixture.write_complex();
        let skills_root = fixture
            .adapter
            .skill_path()
            .parent()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        fs::create_dir_all(&skills_root).unwrap();
        fs::set_permissions(&skills_root, fs::Permissions::from_mode(0o500)).unwrap();
        let restore = RestorePermissions(skills_root.clone());
        let failure = fixture
            .adapter
            .install()
            .expect_err("permission failure must stop the install");
        assert_eq!(failure.code, "storage_error", "{harness:?}/{scope:?}");
        assert!(
            fs::read(fixture.adapter.config_path()).unwrap() == before,
            "failed installation must restore exact original configuration"
        );
        assert!(!fixture.adapter.skill_path().exists());
        assert!(!fixture.paths.state_dir.join("ownership.json").exists());
        assert_eq!(
            fs::read_dir(fixture.paths.state_dir.join("snapshots"))
                .unwrap()
                .count(),
            0
        );
        drop(restore);
        assert_eq!(fixture.adapter.install().unwrap(), InstallStatus::Installed);
        assert_eq!(fixture.adapter.remove().unwrap(), InstallStatus::Removed);
        eprintln!(
            "rollback {harness:?}/{scope:?}: real registration followed by Skill write failure restored original bytes; retry PASS"
        );
    }
}

#[test]
fn failing_native_command_restores_configuration_it_partially_wrote() {
    // This CLI is deliberately synthetic: it supports version detection, overwrites the target,
    // and fails. It does not stand in for actual-client connection evidence.
    for (harness, scope) in cases()
        .into_iter()
        .filter(|case| *case != (Harness::Codex, Scope::Project))
    {
        let mut fixture = Fixture::new(harness, scope, false);
        let before = fixture.write_complex();
        let cli = fixture.paths.home.join("fake CLI 写后失败");
        let version = match harness {
            Harness::Codex => "codex-cli 0.170.0",
            Harness::ClaudeCode => "2.1.329 (Claude Code)",
        };
        let target = match (harness, scope) {
            (Harness::Codex, _) => "$CODEX_HOME/config.toml",
            (Harness::ClaudeCode, Scope::User) => "$CLAUDE_CONFIG_DIR/.claude.json",
            (Harness::ClaudeCode, Scope::Project) => "$PWD/.mcp.json",
        };
        let script = format!(
            "#!/bin/sh\ncase \"$*\" in *--version*) printf '%s\\n' '{version}'; exit 0;; esac\nprintf 'partial invalid output' > \"{target}\"\nprintf 'reached failed command' > \"$HOME/command-called\"\nexit 7\n"
        );
        fs::write(&cli, script).unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        fixture.paths.cli = cli;
        fixture.adapter = Adapter::new(harness, scope, fixture.paths.clone()).unwrap();
        let failure = fixture.adapter.install().unwrap_err();
        assert_eq!(failure.code, "cli_failed");
        assert!(fixture.paths.home.join("command-called").exists());
        assert!(fs::read(fixture.adapter.config_path()).unwrap() == before);
        assert!(!fixture.adapter.skill_path().exists());
        assert!(!fixture.paths.state_dir.join("ownership.json").exists());
    }
}

#[test]
fn missing_client_and_relative_paths_write_no_configuration() {
    for (harness, scope) in cases() {
        let fixture = Fixture::new(harness, scope, false);
        assert_eq!(
            fixture.adapter.install().unwrap_err().code,
            "client_missing"
        );
        assert!(!fixture.adapter.config_path().exists());
        assert!(!fixture.adapter.skill_path().exists());
        assert!(!fixture.paths.state_dir.exists());
        let mut paths = fixture.paths;
        paths.executable = PathBuf::from("relative/program");
        assert_eq!(
            Adapter::new(harness, scope, paths).unwrap_err().code,
            "invalid_path"
        );
    }
}

#[test]
fn claude_without_config_override_uses_home_state_and_default_skill_root() {
    let fixture = Fixture::new(Harness::ClaudeCode, Scope::User, false);
    let override_dir = fixture.paths.claude_config_dir.clone().unwrap();
    let mut paths = fixture.paths.clone();
    paths.claude_config_dir = None;
    let adapter = Adapter::new(Harness::ClaudeCode, Scope::User, paths).unwrap();
    assert_eq!(
        adapter.config_path(),
        fixture.paths.home.join(".claude.json")
    );
    assert_eq!(
        adapter.skill_path(),
        fixture
            .paths
            .home
            .join(".claude/skills/tokemula-image/SKILL.md")
    );
    assert_ne!(adapter.config_path(), override_dir.join(".claude.json"));
    assert!(!adapter.config_path().exists());
    assert!(!adapter.skill_path().exists());
    assert!(!fixture.paths.state_dir.exists());
}
