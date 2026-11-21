//! Managed release updates preserve current user data; native CLI cases never start model turns.
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
    Adapter, AdapterPaths, Harness, InstallStatus, REGISTRATION_NAME, SKILL_CONTENT, Scope,
    opencode::{OpenCodeAdapter, OpenCodeOverrides, OpenCodePaths},
};

const OLD_SKILL: &str = "---\nname: tokemula-image\ndescription: Synthetic previous release\n---\nPrevious managed release.\n";

struct Fixture {
    root: TempDir,
    harness: Harness,
    scope: Scope,
    paths: AdapterPaths,
}

impl Fixture {
    fn real(harness: Harness, scope: Scope) -> Self {
        let root = tempfile::Builder::new()
            .prefix("tokemula-managed-update 中文 ")
            .tempdir()
            .unwrap();
        let cli = PathBuf::from(
            env::var_os(match harness {
                Harness::Codex => "TOKEMULA_TEST_CODEX_CLI",
                Harness::ClaudeCode => "TOKEMULA_TEST_CLAUDE_CLI",
            })
            .expect("supply the explicit installed test CLI path"),
        );
        let executable = root.path().join("release A 中文/tokemula-image-mcp");
        executable_fixture(&executable);
        let paths = AdapterPaths {
            home: root.path().join("home"),
            codex_home: root.path().join("codex config"),
            claude_config_dir: Some(root.path().join("claude config")),
            project_dir: root.path().join("project 中文"),
            state_dir: root.path().join("private state"),
            cli,
            executable,
            process_path: env::var_os("TOKEMULA_TEST_PROCESS_PATH")
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
        let fixture = Self {
            root,
            harness,
            scope,
            paths,
        };
        let config = fixture.adapter().config_path();
        fs::create_dir_all(config.parent().unwrap()).unwrap();
        let content = match harness {
            Harness::Codex => "# operator comment\nmodel='synthetic-language-model'\nmodel_provider='synthetic-existing'\n[model_providers.synthetic-existing]\nname='Existing provider'\nbase_url='http://127.0.0.1:9/v1'\nenv_key='SYNTHETIC_NOT_CONFIGURED'\nwire_api='responses'\n[mcp_servers.other-tool]\ncommand='/bin/false' # other tool comment\nenabled=false\n[future_unknown]\nkeep=true\n".to_owned(),
            Harness::ClaudeCode => serde_json::to_string_pretty(&json!({
                "model":"synthetic-language-model", "provider":{"keep":true},
                "mcpServers":{"other-tool":{"command":"/bin/false","args":[]}},
                "future_unknown":{"keep":true}
            })).unwrap(),
        };
        fs::write(config, content).unwrap();
        fixture
    }

    fn adapter(&self) -> Adapter {
        Adapter::new(self.harness, self.scope, self.paths.clone()).unwrap()
    }

    fn next(&self) -> Adapter {
        let mut paths = self.paths.clone();
        paths.executable = self.root.path().join("release B 中文/tokemula-image-mcp");
        executable_fixture(&paths.executable);
        Adapter::new(self.harness, self.scope, paths).unwrap()
    }

    fn parsed(&self) -> Value {
        let bytes = fs::read(self.adapter().config_path()).unwrap();
        self.parse_bytes(&bytes)
    }

    fn parse_bytes(&self, bytes: &[u8]) -> Value {
        match self.harness {
            Harness::Codex => toml_edit::de::from_str(std::str::from_utf8(bytes).unwrap()).unwrap(),
            Harness::ClaudeCode => serde_json::from_slice(bytes).unwrap(),
        }
    }

    fn key(&self) -> &'static str {
        match self.harness {
            Harness::Codex => "mcp_servers",
            Harness::ClaudeCode => "mcpServers",
        }
    }

    fn add_later(&self) {
        let config = self.adapter().config_path();
        match self.harness {
            Harness::Codex => {
                use std::io::Write;
                let text = fs::read_to_string(&config).unwrap();
                assert!(text.contains("[mcp_servers.tokemula-image]"));
                fs::write(
                    &config,
                    text.replace(
                        "[mcp_servers.tokemula-image]",
                        "[mcp_servers.tokemula-image] # managed entry user note",
                    ),
                )
                .unwrap();
                let mut doc: toml_edit::DocumentMut =
                    fs::read_to_string(&config).unwrap().parse().unwrap();
                doc["mcp_servers"][REGISTRATION_NAME]["command"]
                    .as_value_mut()
                    .unwrap()
                    .decor_mut()
                    .set_suffix(" # managed command user note");
                fs::write(&config, doc.to_string()).unwrap();
                fs::OpenOptions::new()
                    .append(true)
                    .open(config)
                    .unwrap()
                    .write_all(b"\n# later user comment\n[later_user]\nkeep='later-value'\n[mcp_servers.later-tool]\ncommand='/bin/false'\nenabled=false\n")
                    .unwrap();
            }
            Harness::ClaudeCode => {
                let mut value = self.parsed();
                value["later_user"] = json!({"keep":"later-value"});
                value["mcpServers"]["later-tool"] = json!({"command":"/bin/false","args":[]});
                fs::write(config, serde_json::to_vec_pretty(&value).unwrap()).unwrap();
            }
        }
    }

    fn preserve_assertions(&self) {
        let value = self.parsed();
        assert_eq!(value["model"], "synthetic-language-model");
        assert_eq!(value["future_unknown"]["keep"], true);
        assert_eq!(value["later_user"]["keep"], "later-value");
        assert!(value[self.key()].get("other-tool").is_some());
        assert!(value[self.key()].get("later-tool").is_some());
        if self.harness == Harness::Codex {
            let text = fs::read_to_string(self.adapter().config_path()).unwrap();
            for comment in [
                "# operator comment",
                "# other tool comment",
                "# later user comment",
            ] {
                assert!(text.contains(comment));
            }
        }
    }
}

fn executable_fixture(path: &Path) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).unwrap();
}

fn cases() -> [(Harness, Scope); 4] {
    [
        (Harness::Codex, Scope::User),
        (Harness::Codex, Scope::Project),
        (Harness::ClaudeCode, Scope::User),
        (Harness::ClaudeCode, Scope::Project),
    ]
}

fn read_state(root: &Path) -> Value {
    serde_json::from_slice(&fs::read(root.join("ownership.json")).unwrap()).unwrap()
}

fn seed_previous_skill(root: &Path, skill_path: &Path) {
    let mut state = read_state(root);
    for owned in state["installations"].as_array_mut().unwrap() {
        if owned["skill_path"] == skill_path.to_string_lossy().as_ref() {
            owned["skill"] = json!(OLD_SKILL);
            owned.as_object_mut().unwrap().remove("context");
        }
    }
    fs::write(
        root.join("ownership.json"),
        serde_json::to_vec_pretty(&state).unwrap(),
    )
    .unwrap();
    fs::write(skill_path, OLD_SKILL).unwrap();
}

#[test]
#[ignore = "explicit installed Codex/Claude CLIs; isolated config management only"]
fn real_clients_update_program_and_managed_skill_then_restore_old_program_path() {
    for (harness, scope) in cases() {
        let f = Fixture::real(harness, scope);
        let old = f.adapter();
        assert_eq!(old.install().unwrap(), InstallStatus::Installed);
        f.add_later();
        seed_previous_skill(&f.paths.state_dir, &old.skill_path());
        let next = f.next();
        assert_eq!(
            next.install().unwrap_err().code,
            "installation_update_required"
        );
        assert_eq!(next.update().unwrap(), InstallStatus::Installed);
        assert_eq!(next.check().unwrap(), InstallStatus::Installed);
        assert_eq!(next.install().unwrap(), InstallStatus::AlreadyInstalled);
        assert_eq!(
            fs::read_to_string(next.skill_path()).unwrap(),
            SKILL_CONTENT
        );
        assert_eq!(
            f.parsed()[f.key()][REGISTRATION_NAME]["command"],
            f.root
                .path()
                .join("release B 中文/tokemula-image-mcp")
                .to_string_lossy()
                .as_ref()
        );
        f.preserve_assertions();
        if harness == Harness::Codex {
            assert!(
                fs::read_to_string(next.config_path())
                    .unwrap()
                    .contains("# managed entry user note")
            );
            assert!(
                fs::read_to_string(old.config_path())
                    .unwrap()
                    .contains("# managed command user note")
            );
        }
        assert_eq!(old.update().unwrap(), InstallStatus::Installed);
        assert_eq!(old.check().unwrap(), InstallStatus::Installed);
        assert_eq!(
            f.parsed()[f.key()][REGISTRATION_NAME]["command"],
            f.paths.executable.to_string_lossy().as_ref()
        );
        f.preserve_assertions();
        if harness == Harness::Codex {
            assert!(
                fs::read_to_string(old.config_path())
                    .unwrap()
                    .contains("# managed entry user note")
            );
            assert!(
                fs::read_to_string(old.config_path())
                    .unwrap()
                    .contains("# managed command user note")
            );
        }
        assert_eq!(old.remove().unwrap(), InstallStatus::Removed);
        f.preserve_assertions();
        eprintln!("managed native update/rollback {harness:?}/{scope:?}: PASS");
    }
}

#[test]
#[ignore = "explicit installed Codex/Claude CLIs; isolated config management only"]
fn real_clients_update_rejects_user_modified_registration_or_skill() {
    for (harness, scope) in cases() {
        let f = Fixture::real(harness, scope);
        let old = f.adapter();
        old.install().unwrap();
        f.add_later();
        let next = f.next();
        let config = old.config_path();
        let before = fs::read(&config).unwrap();
        let state = fs::read(f.paths.state_dir.join("ownership.json")).unwrap();
        let changed = String::from_utf8(before.clone()).unwrap().replace(
            f.paths.executable.to_str().unwrap(),
            "/user/changed-program",
        );
        fs::write(&config, &changed).unwrap();
        assert_eq!(next.update().unwrap_err().code, "ownership_conflict");
        assert_eq!(fs::read(&config).unwrap(), changed.as_bytes());
        assert_eq!(
            fs::read(f.paths.state_dir.join("ownership.json")).unwrap(),
            state
        );
        fs::write(&config, &before).unwrap();
        fs::write(old.skill_path(), "user-modified product Skill\n").unwrap();
        assert_eq!(next.update().unwrap_err().code, "ownership_conflict");
        assert_eq!(fs::read(&config).unwrap(), before);
        assert_eq!(
            fs::read(f.paths.state_dir.join("ownership.json")).unwrap(),
            state
        );
        assert_eq!(
            fs::read_to_string(old.skill_path()).unwrap(),
            "user-modified product Skill\n"
        );
        fs::write(old.skill_path(), SKILL_CONTENT).unwrap();
        old.remove().unwrap();
        f.preserve_assertions();
    }
}

struct RestorePermissions(PathBuf);
impl Drop for RestorePermissions {
    fn drop(&mut self) {
        let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
    }
}

#[test]
#[ignore = "explicit installed Codex/Claude CLIs; failure only in temporary Skill dir"]
fn real_clients_failed_skill_update_restores_current_config_and_old_ownership() {
    for (harness, scope) in cases() {
        let f = Fixture::real(harness, scope);
        let old = f.adapter();
        old.install().unwrap();
        f.add_later();
        seed_previous_skill(&f.paths.state_dir, &old.skill_path());
        let before = fs::read(old.config_path()).unwrap();
        let state = fs::read(f.paths.state_dir.join("ownership.json")).unwrap();
        let dir = old.skill_path().parent().unwrap().to_path_buf();
        fs::set_permissions(&dir, fs::Permissions::from_mode(0o500)).unwrap();
        let restore = RestorePermissions(dir);
        let failure = f.next().update().unwrap_err();
        assert_eq!(failure.code, "storage_error", "{harness:?}/{scope:?}");
        assert_eq!(f.parsed(), f.parse_bytes(&before));
        assert_eq!(
            read_state(&f.paths.state_dir),
            serde_json::from_slice::<Value>(&state).unwrap()
        );
        f.preserve_assertions();
        if harness == Harness::Codex {
            assert!(
                fs::read_to_string(old.config_path())
                    .unwrap()
                    .contains("# managed entry user note")
            );
            assert!(
                fs::read_to_string(old.config_path())
                    .unwrap()
                    .contains("# managed command user note")
            );
        }
        assert_eq!(fs::read_to_string(old.skill_path()).unwrap(), OLD_SKILL);
        assert_eq!(old.check().unwrap(), InstallStatus::Installed);
        drop(restore);
        assert_eq!(f.next().update().unwrap(), InstallStatus::Installed);
        assert_eq!(fs::read_to_string(old.skill_path()).unwrap(), SKILL_CONTENT);
        old.remove().unwrap();
        f.preserve_assertions();
    }
}

#[test]
#[ignore = "explicit installed Codex CLI; OpenCode fixture checks configuration ownership only"]
fn shared_codex_opencode_skill_update_keeps_all_references_and_both_removal_orders() {
    for codex_first in [true, false] {
        let f = Fixture::real(Harness::Codex, Scope::User);
        let codex = f.adapter();
        codex.install().unwrap();
        let cli = f.root.path().join("synthetic OpenCode version fixture");
        fs::write(&cli, "#!/bin/sh\nif [ \"$1\" = '--version' ]; then printf 'opencode v2.3.2\\n'; exit 0; fi\nexit 23\n").unwrap();
        fs::set_permissions(&cli, fs::Permissions::from_mode(0o700)).unwrap();
        let global = f.root.path().join("xdg config/opencode");
        fs::create_dir_all(&global).unwrap();
        fs::write(
            global.join("opencode.json"),
            "{\"lower_priority_source\":true}\n",
        )
        .unwrap();
        fs::write(
            global.join("opencode.jsonc"),
            "{\n// other client config\n\"model\":\"synthetic-language\"\n}\n",
        )
        .unwrap();
        let open = OpenCodeAdapter::new(
            Scope::User,
            OpenCodePaths {
                home: f.paths.home.clone(),
                xdg_config_home: f.root.path().join("xdg config"),
                project_dir: f.paths.project_dir.clone(),
                state_dir: f.paths.state_dir.clone(),
                cli,
                executable: f.paths.executable.clone(),
                process_path: f.paths.process_path.clone(),
            },
            OpenCodeOverrides::default(),
        )
        .unwrap();
        open.install().unwrap();
        let plan = open.plan().unwrap();
        assert_eq!(plan.skill_path, codex.skill_path());
        seed_previous_skill(&f.paths.state_dir, &plan.skill_path);
        assert_eq!(f.next().update().unwrap(), InstallStatus::Installed);
        assert_eq!(open.check().unwrap(), InstallStatus::Installed);
        let state = read_state(&f.paths.state_dir);
        assert_eq!(state["installations"].as_array().unwrap().len(), 2);
        for owned in state["installations"].as_array().unwrap() {
            assert_eq!(owned["skill"], SKILL_CONTENT);
        }
        let note = plan.skill_path.parent().unwrap().join("user-note.txt");
        fs::write(&note, "user-added, preserve").unwrap();
        if codex_first {
            f.next().remove().unwrap();
            assert_eq!(open.check().unwrap(), InstallStatus::Installed);
            assert!(plan.skill_path.exists());
            open.remove().unwrap();
        } else {
            open.remove().unwrap();
            assert_eq!(f.next().check().unwrap(), InstallStatus::Installed);
            assert!(plan.skill_path.exists());
            f.next().remove().unwrap();
        }
        assert!(!plan.skill_path.exists());
        assert_eq!(fs::read_to_string(note).unwrap(), "user-added, preserve");
        assert!(
            read_state(&f.paths.state_dir)["installations"]
                .as_array()
                .unwrap()
                .is_empty()
        );
    }
}

#[test]
#[ignore = "explicit installed Codex/Claude CLIs; public owned operations with isolated roots"]
fn owned_inventory_update_and_remove_consume_context_and_preserve_legacy_explicit_path() {
    use tokemula_image_connector::adapters::{owned_inventory, remove_owned, update_owned};
    for (harness, scope) in cases() {
        let f = Fixture::real(harness, scope);
        let old = f.adapter();
        old.install().unwrap();
        f.add_later();
        seed_previous_skill(&f.paths.state_dir, &old.skill_path());
        let inventory = owned_inventory(&f.paths.state_dir).unwrap();
        assert_eq!(inventory.len(), 1);
        assert!(!inventory[0].context_available);
        assert_eq!(inventory[0].config_path, old.config_path());
        assert_eq!(inventory[0].executable.as_ref(), Some(&f.paths.executable));
        let next_exe = f.root.path().join("release B 中文/tokemula-image-mcp");
        executable_fixture(&next_exe);
        let config_before = fs::read(old.config_path()).unwrap();
        let incomplete = update_owned(&f.paths.state_dir, &next_exe).unwrap();
        assert!(!incomplete.completed);
        assert_eq!(incomplete.items.len(), 1);
        assert_eq!(
            incomplete.items[0].error.as_ref().unwrap().code,
            "installation_context_required"
        );
        assert_eq!(fs::read(old.config_path()).unwrap(), config_before);
        assert_eq!(fs::read_to_string(old.skill_path()).unwrap(), OLD_SKILL);
        // Existing explicit callers still know their roots and can upgrade the legacy record.
        assert_eq!(f.next().update().unwrap(), InstallStatus::Installed);
        let inventory = owned_inventory(&f.paths.state_dir).unwrap();
        assert!(inventory[0].context_available);
        assert_eq!(inventory[0].executable.as_ref(), Some(&next_exe));
        let restored = update_owned(&f.paths.state_dir, &f.paths.executable).unwrap();
        assert!(restored.completed);
        assert_eq!(old.check().unwrap(), InstallStatus::Installed);
        f.preserve_assertions();
        let before_user_change = fs::read(old.config_path()).unwrap();
        let modified = String::from_utf8(before_user_change.clone())
            .unwrap()
            .replace(
                f.paths.executable.to_str().unwrap(),
                "/user/modified-owned-program",
            );
        fs::write(old.config_path(), &modified).unwrap();
        let refused = remove_owned(&f.paths.state_dir).unwrap();
        assert!(!refused.completed);
        assert_eq!(
            refused.items[0].error.as_ref().unwrap().code,
            "ownership_conflict"
        );
        assert_eq!(owned_inventory(&f.paths.state_dir).unwrap().len(), 1);
        assert_eq!(fs::read(old.config_path()).unwrap(), modified.as_bytes());
        fs::write(old.config_path(), before_user_change).unwrap();
        let removed = remove_owned(&f.paths.state_dir).unwrap();
        assert!(removed.completed);
        assert_eq!(removed.items.len(), 1);
        assert!(owned_inventory(&f.paths.state_dir).unwrap().is_empty());
        assert_eq!(old.check().unwrap(), InstallStatus::NotInstalled);
        f.preserve_assertions();
        assert!(remove_owned(&f.paths.state_dir).unwrap().completed);
        eprintln!("owned API and legacy context {harness:?}/{scope:?}: PASS");
    }
}

#[test]
#[cfg(target_os = "macos")]
fn workbuddy_update_keeps_native_approval_separate_and_rollback_reuses_old_approval() {
    use sha2::{Digest, Sha256};
    use tokemula_image_connector::adapters::workbuddy::{
        WorkBuddyAdapter, WorkBuddyPaths, WorkBuddyStatus,
    };
    let root = TempDir::new().unwrap();
    let app = root.path().join("WorkBuddy fixture.app");
    fs::create_dir_all(app.join("Contents")).unwrap();
    fs::write(app.join("Contents/Info.plist"),
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?><plist version=\"1.0\"><dict><key>CFBundleIdentifier</key><string>com.tencent.workbuddy.mac</string><key>CFBundleShortVersionString</key><string>5.11.3</string></dict></plist>").unwrap();
    let paths = WorkBuddyPaths {
        home: root.path().join("home"),
        config_dir: root.path().join("workbuddy config"),
        state_dir: root.path().join("state"),
        app_bundle: app,
        executable: root.path().join("release A/tokemula-image-mcp"),
    };
    fs::create_dir_all(&paths.home).unwrap();
    fs::create_dir_all(&paths.config_dir).unwrap();
    executable_fixture(&paths.executable);
    let old = WorkBuddyAdapter::new(Scope::User, paths.clone()).unwrap();
    fs::write(
        old.config_path(),
        "{\"other_future\":true,\"mcpServers\":{\"other-tool\":{\"command\":\"/bin/false\"}}}",
    )
    .unwrap();
    assert_eq!(
        old.install().unwrap(),
        WorkBuddyStatus::AwaitingConfirmation
    );
    // Synthetic unit fixture for the already-proven native fingerprint; no GUI/approval RPC runs.
    let fingerprint: String = Sha256::digest(format!("{}||", paths.executable.to_str().unwrap()))
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let approval_path = paths.config_dir.join("mcp-approvals.json");
    let approval =
        serde_json::to_vec(&json!({format!("{fingerprint}::{REGISTRATION_NAME}"):1700000000}))
            .unwrap();
    fs::write(&approval_path, &approval).unwrap();
    assert_eq!(old.check().unwrap(), WorkBuddyStatus::Installed);
    seed_previous_skill(&paths.state_dir, &old.skill_path());
    let mut next_paths = paths.clone();
    next_paths.executable = root.path().join("release B 中文/tokemula-image-mcp");
    executable_fixture(&next_paths.executable);
    let next = WorkBuddyAdapter::new(Scope::User, next_paths.clone()).unwrap();
    assert_eq!(
        next.install().unwrap_err().code,
        "installation_update_required"
    );
    assert_eq!(
        next.update().unwrap(),
        WorkBuddyStatus::AwaitingConfirmation
    );
    assert_eq!(next.check().unwrap(), WorkBuddyStatus::AwaitingConfirmation);
    assert_eq!(fs::read(&approval_path).unwrap(), approval);
    assert_eq!(
        fs::read_to_string(next.skill_path()).unwrap(),
        SKILL_CONTENT
    );
    let config: Value = serde_json::from_slice(&fs::read(next.config_path()).unwrap()).unwrap();
    assert_eq!(
        config["mcpServers"][REGISTRATION_NAME]["command"],
        json!(next_paths.executable)
    );
    assert_eq!(config["other_future"], true);
    assert!(config["mcpServers"].get("other-tool").is_some());
    assert_eq!(old.update().unwrap(), WorkBuddyStatus::Installed);
    assert_eq!(fs::read(&approval_path).unwrap(), approval);
    assert_eq!(old.remove().unwrap(), WorkBuddyStatus::Removed);
    assert_eq!(fs::read(approval_path).unwrap(), approval);
}
