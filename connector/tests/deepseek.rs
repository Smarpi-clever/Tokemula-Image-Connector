//! Fixed-version native DeepSeek checks; all client roots are disposable and no model turn starts.
#![cfg(unix)]
use serde_json::{Value, json};
use std::{
    ffi::OsString,
    fs,
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
    process::Command,
};
use tempfile::TempDir;
use tokemula_image_connector::adapters::deepseek::{
    DeepSeekAdapter, DeepSeekPaths, DeepSeekRuntime,
};
use tokemula_image_connector::adapters::{InstallStatus, SKILL_CONTENT, Scope};

fn paths(root: &Path, cli: PathBuf) -> DeepSeekPaths {
    for name in ["home", "dsh", "project 中文 space", "state/tmp"] {
        fs::create_dir_all(root.join(name)).unwrap();
    }
    let exe = root.join("独立 图片 MCP");
    fs::write(&exe, b"#!/bin/sh\nexit 0\n").unwrap();
    fs::set_permissions(&exe, fs::Permissions::from_mode(0o700)).unwrap();
    DeepSeekPaths {
        home: root.join("home"),
        dsh_home: root.join("dsh"),
        project_dir: root.join("project 中文 space"),
        state_dir: root.join("state"),
        cli,
        executable: exe,
        process_path: std::env::var_os("TOKEMULA_TEST_DSH_PATH")
            .unwrap_or_else(|| OsString::from("/usr/bin:/bin")),
        profile: "web".into(),
        runtime: DeepSeekRuntime::CliWeb,
    }
}
fn command(p: &DeepSeekPaths) -> Command {
    let mut c = Command::new(&p.cli);
    c.env_clear()
        .current_dir(&p.project_dir)
        .env("PATH", &p.process_path)
        .env("HOME", &p.home)
        .env("USERPROFILE", &p.home)
        .env("DSH_HOME", &p.dsh_home)
        .env("DSH_AGENTS_HOME", p.home.join(".agents"))
        .env("XDG_CONFIG_HOME", p.home.join(".config"))
        .env("XDG_CACHE_HOME", p.home.join(".cache"))
        .env("XDG_DATA_HOME", p.home.join(".local/share"))
        .env("XDG_STATE_HOME", p.home.join(".local/state"))
        .env("TMPDIR", p.state_dir.join("tmp"))
        .env("DSH_TELEMETRY_MODE", "DISABLED")
        .env("DSH_TELEMETRY_DISABLED", "1")
        .env("HTTP_PROXY", "http://127.0.0.1:9")
        .env("HTTPS_PROXY", "http://127.0.0.1:9")
        .env("ALL_PROXY", "http://127.0.0.1:9")
        .env("NO_PROXY", "localhost,127.0.0.1,::1");
    c
}
fn initialize(p: &DeepSeekPaths) {
    assert!(
        command(p)
            .args(["--profile", &p.profile, "--dump-config"])
            .output()
            .unwrap()
            .status
            .success()
    );
}
fn doc(a: &DeepSeekAdapter) -> Value {
    serde_json::from_slice(&fs::read(a.config_path()).unwrap()).unwrap()
}
fn write_doc(a: &DeepSeekAdapter, v: &Value) {
    fs::write(a.config_path(), serde_json::to_vec_pretty(v).unwrap()).unwrap();
}
fn real_paths(root: &Path) -> DeepSeekPaths {
    paths(
        root,
        PathBuf::from(
            std::env::var_os("TOKEMULA_TEST_DSH_CLI").expect("explicit test runtime required"),
        ),
    )
}

#[test]
fn old_scope_and_uninitialized_desktop_are_not_silently_redirected() {
    let r = TempDir::new().unwrap();
    let mut p = paths(r.path(), r.path().join("missing-cli"));
    assert_eq!(
        DeepSeekAdapter::new(Scope::Project, p.clone())
            .unwrap_err()
            .code,
        "unsupported_scope"
    );
    p.profile = "desktop".into();
    assert_eq!(
        DeepSeekAdapter::new(Scope::User, p.clone())
            .unwrap_err()
            .code,
        "desktop_runtime_required"
    );
    p.profile = "web".into();
    let a = DeepSeekAdapter::new(Scope::User, p).unwrap();
    assert_eq!(a.plan().unwrap_err().code, "client_missing");
    assert!(!a.config_path().exists());
}

#[test]
#[ignore = "explicit isolated installed DeepSeek CLI required"]
fn native_profile_lifecycle_preserves_other_content_and_ownership() {
    let r = TempDir::new().unwrap();
    let p = real_paths(r.path());
    let a = DeepSeekAdapter::new(Scope::User, p.clone()).unwrap();
    assert_eq!(
        a.plan().unwrap_err().code,
        "profile_initialization_required"
    );
    assert!(!a.config_path().exists());
    initialize(&p);
    let other = r.path().join("other-plugin");
    fs::create_dir_all(&other).unwrap();
    fs::write(other.join("package.json"), serde_json::to_vec(&json!({"name":"synthetic-other-plugin","version":"0.0.1","type":"module","main":"index.js","engines":{"dsh":"=0.2.2-rc.5"},"dsh":{"bundle":{"patch":"./cordis.patch.yml"}}})).unwrap()).unwrap();
    fs::write(other.join("index.js"), b"export function apply() {}\n").unwrap();
    fs::write(
        other.join("cordis.patch.yml"),
        b"- insert: [{id: synthetic-other, name: synthetic-other-plugin}]\n",
    )
    .unwrap();
    assert!(
        command(&p)
            .args(["plugin", "--profile", "web", "add"])
            .arg(&other)
            .args(["--workspace-root", "--offline", "--ignore-scripts"])
            .output()
            .unwrap()
            .status
            .success()
    );
    let other_dependency = doc(&a)["dependencies"]["synthetic-other-plugin"].clone();
    let mut d = doc(&a);
    d["user_future"] = json!({"model":"synthetic-model","provider":"unchanged"});
    write_doc(&a, &d);
    let patch = p.dsh_home.join("profiles/web/cordis.patch.yml");
    fs::write(&patch,b"# user comment\n- id: mcp-manager\n  config:\n    servers:\n      other:\n        transport: stdio\n        command: /bin/false\n        enabled: false\n").unwrap();
    let before_patch = fs::read(&patch).unwrap();
    assert_eq!(a.install().unwrap(), InstallStatus::Installed);
    assert_eq!(a.check().unwrap(), InstallStatus::Installed);
    assert_eq!(a.install().unwrap(), InstallStatus::AlreadyInstalled);
    assert_eq!(fs::read(a.skill_path()).unwrap(), SKILL_CONTENT.as_bytes());
    assert_eq!(doc(&a)["user_future"], d["user_future"]);
    assert_eq!(fs::read(&patch).unwrap(), before_patch);
    let mut now = doc(&a);
    now["later_user_content"] = json!(["kept", 42]);
    write_doc(&a, &now);
    fs::write(
        a.skill_path().parent().unwrap().join("user-note.txt"),
        b"keep me",
    )
    .unwrap();
    let private = fs::metadata(p.state_dir.join("ownership.json"))
        .unwrap()
        .permissions()
        .mode()
        & 0o777;
    assert_eq!(private, 0o600);
    assert_eq!(a.remove().unwrap(), InstallStatus::Removed);
    assert_eq!(a.remove().unwrap(), InstallStatus::NotInstalled);
    assert_eq!(doc(&a)["user_future"], d["user_future"]);
    assert_eq!(doc(&a)["later_user_content"], json!(["kept", 42]));
    assert_eq!(
        doc(&a)["dependencies"]["synthetic-other-plugin"],
        other_dependency
    );
    assert!(
        doc(&a)["dsh"]["profile"]["bundles"]
            .as_array()
            .unwrap()
            .iter()
            .any(|v| v == "synthetic-other-plugin")
    );
    assert_eq!(fs::read(&patch).unwrap(), before_patch);
    assert_eq!(
        fs::read(a.skill_path().parent().unwrap().join("user-note.txt")).unwrap(),
        b"keep me"
    );
}

#[test]
#[ignore = "explicit isolated installed DeepSeek CLI required"]
fn native_namespace_package_and_user_edits_are_conflicts() {
    let r = TempDir::new().unwrap();
    let p = real_paths(r.path());
    initialize(&p);
    let a = DeepSeekAdapter::new(Scope::User, p.clone()).unwrap();
    let patch = p.dsh_home.join("cordis.patch.yml");
    fs::write(&patch,b"- id: mcp-manager\n  config:\n    servers:\n      tokemula-image:\n        transport: stdio\n        command: /bin/false\n        enabled: false\n").unwrap();
    assert_eq!(a.install().unwrap_err().code, "ownership_conflict");
    assert!(!a.package_path().exists());
    fs::remove_file(patch).unwrap();
    let mut d = doc(&a);
    d["dependencies"]["tokemula-image-connector"] = json!("file:/unowned-package");
    write_doc(&a, &d);
    assert_eq!(a.install().unwrap_err().code, "ownership_conflict");
    d["dependencies"]
        .as_object_mut()
        .unwrap()
        .remove("tokemula-image-connector");
    write_doc(&a, &d);
    a.install().unwrap();
    let patch = p.dsh_home.join("cordis.patch.yml");
    fs::write(
        &patch,
        b"- id: tokemula-image\n  config:\n    command: /bin/false\n",
    )
    .unwrap();
    assert_eq!(a.remove().unwrap_err().code, "ownership_conflict");
    assert!(
        doc(&a)["dependencies"]
            .get("tokemula-image-connector")
            .is_some()
    );
    fs::remove_file(patch).unwrap();
    let original = fs::read(a.skill_path()).unwrap();
    fs::write(a.skill_path(), b"user changed skill").unwrap();
    assert_eq!(a.remove().unwrap_err().code, "ownership_conflict");
    fs::write(a.skill_path(), original).unwrap();
    let code = a.package_path().join("index.js");
    let original = fs::read(&code).unwrap();
    fs::write(&code, b"user changed plugin").unwrap();
    assert_eq!(a.remove().unwrap_err().code, "ownership_conflict");
    fs::write(code, original).unwrap();
    a.remove().unwrap();
}

#[test]
#[ignore = "explicit isolated installed DeepSeek CLI required"]
fn native_install_failure_uses_package_manager_to_undo_only_this_package() {
    let r = TempDir::new().unwrap();
    let p = real_paths(r.path());
    initialize(&p);
    let a = DeepSeekAdapter::new(Scope::User, p.clone()).unwrap();
    let mut original = doc(&a);
    original["user_future"] = json!({"after":"keep"});
    write_doc(&a, &original);
    struct RestorePermissions(PathBuf);
    impl Drop for RestorePermissions {
        fn drop(&mut self) {
            let _ = fs::set_permissions(&self.0, fs::Permissions::from_mode(0o700));
        }
    }
    let skills = p.dsh_home.join("skills");
    fs::create_dir_all(&skills).unwrap();
    let permissions = RestorePermissions(skills.clone());
    fs::set_permissions(&skills, fs::Permissions::from_mode(0o500)).unwrap();
    assert_eq!(a.install().unwrap_err().code, "storage_error");
    assert!(
        doc(&a)["dependencies"]
            .get("tokemula-image-connector")
            .is_none()
    );
    assert_eq!(doc(&a)["user_future"], original["user_future"]);
    assert!(!a.package_path().exists());
    assert!(a.profile_path().join("pnpm-lock.yaml").exists());
    drop(permissions);
    assert_eq!(a.install().unwrap(), InstallStatus::Installed);
    a.remove().unwrap();
}

#[test]
#[ignore = "owned prepared roots for real Web diagnostics"]
fn real_adapter_phase_for_native_web_diagnostics() {
    let input = PathBuf::from(std::env::var_os("TOKEMULA_DSH_ACCEPTANCE_INPUT").unwrap());
    let v: Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
    let p = DeepSeekPaths {
        home: PathBuf::from(v["home"].as_str().unwrap()),
        dsh_home: PathBuf::from(v["dsh_home"].as_str().unwrap()),
        project_dir: PathBuf::from(v["project_dir"].as_str().unwrap()),
        state_dir: PathBuf::from(v["state_dir"].as_str().unwrap()),
        cli: PathBuf::from(v["cli"].as_str().unwrap()),
        executable: PathBuf::from(v["executable"].as_str().unwrap()),
        process_path: OsString::from(v["process_path"].as_str().unwrap()),
        profile: "web".into(),
        runtime: DeepSeekRuntime::CliWeb,
    };
    let a = DeepSeekAdapter::new(Scope::User, p).unwrap();
    let phase = std::env::var("TOKEMULA_DSH_ACCEPTANCE_PHASE").unwrap();
    let result = if phase == "install" {
        assert_eq!(a.install().unwrap(), InstallStatus::Installed);
        assert_eq!(a.install().unwrap(), InstallStatus::AlreadyInstalled);
        assert_eq!(a.check().unwrap(), InstallStatus::Installed);
        json!({"phase":phase,"plan":a.plan().unwrap(),"repeated_install":"already_installed"})
    } else {
        let mut d = doc(&a);
        d["after_install_user_content"] = json!({"preserved":true});
        write_doc(&a, &d);
        assert_eq!(a.remove().unwrap(), InstallStatus::Removed);
        assert_eq!(
            doc(&a)["after_install_user_content"],
            json!({"preserved":true})
        );
        assert_eq!(a.check().unwrap(), InstallStatus::NotInstalled);
        json!({"phase":phase,"status":"removed","later_user_content_preserved":true})
    };
    fs::write(
        PathBuf::from(std::env::var_os("TOKEMULA_DSH_ACCEPTANCE_OUTPUT").unwrap()),
        serde_json::to_vec_pretty(&result).unwrap(),
    )
    .unwrap();
}

#[test]
#[ignore = "explicit isolated installed DeepSeek CLI required"]
fn two_profiles_share_the_skill_without_sharing_mcp_registration() {
    let r = TempDir::new().unwrap();
    let p = real_paths(r.path());
    initialize(&p);
    let a = DeepSeekAdapter::new(Scope::User, p.clone()).unwrap();
    a.install().unwrap();
    let mut second = p.clone();
    second.profile = "secondary".into();
    assert!(
        command(&second)
            .args([
                "--profile",
                "secondary",
                "--from-default-profile",
                "web",
                "--dump-config"
            ])
            .output()
            .unwrap()
            .status
            .success()
    );
    let b = DeepSeekAdapter::new(Scope::User, second).unwrap();
    assert_eq!(b.install().unwrap(), InstallStatus::Installed);
    a.remove().unwrap();
    assert_eq!(b.check().unwrap(), InstallStatus::Installed);
    assert_eq!(fs::read(b.skill_path()).unwrap(), SKILL_CONTENT.as_bytes());
    b.remove().unwrap();
    assert!(!b.skill_path().exists());
}

#[test]
#[ignore = "explicit isolated installed DeepSeek CLI required"]
fn native_remove_failure_restores_local_ownership_without_reenabling_disabled_bundle() {
    let r = TempDir::new().unwrap();
    let p = real_paths(r.path());
    initialize(&p);
    let a = DeepSeekAdapter::new(Scope::User, p.clone()).unwrap();
    a.install().unwrap();
    // Fixture for the same selection that the separately tested official setBundleEnabled produces.
    let mut d = doc(&a);
    d["dsh"]["profile"]["bundles"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v != "tokemula-image-connector");
    write_doc(&a, &d);
    let wrapper = r.path().join("native failure injection");
    let quoted = format!("'{}'", p.cli.to_str().unwrap().replace('\'', "'\\''"));
    fs::write(&wrapper,format!("#!/bin/sh\nif [ \"$1\" = plugin ] && [ \"$4\" = remove ]; then exit 23; fi\nexec {quoted} \"$@\"\n")).unwrap();
    fs::set_permissions(&wrapper, fs::Permissions::from_mode(0o700)).unwrap();
    let mut failed_paths = p.clone();
    failed_paths.cli = wrapper;
    let failed = DeepSeekAdapter::new(Scope::User, failed_paths).unwrap();
    assert_eq!(failed.remove().unwrap_err().code, "cli_failed");
    assert_eq!(doc(&a), d);
    assert_eq!(a.check().unwrap(), InstallStatus::Installed);
    assert_eq!(fs::read(a.skill_path()).unwrap(), SKILL_CONTENT.as_bytes());
    assert!(!a.plan().unwrap().bundle_enabled);
    assert_eq!(a.remove().unwrap(), InstallStatus::Removed);
}

fn partial_remove_package_manager(
    p: &DeepSeekPaths,
    root: &Path,
    fail_recovery: bool,
) -> DeepSeekPaths {
    let pnpm = std::env::split_paths(&p.process_path)
        .map(|dir| dir.join("pnpm"))
        .find(|path| path.is_file())
        .expect("explicit installed package manager path");
    let bin = root.join("fault bin");
    fs::create_dir_all(&bin).unwrap();
    let marker = root.join("recovery-unavailable");
    if fail_recovery {
        fs::write(&marker, b"native recovery failure injection").unwrap();
    }
    let script = format!(
        r#"#!/usr/bin/python3
import json,os,subprocess,sys
from pathlib import Path
real={real}
profile=Path({profile})
marker=Path({marker})
events=Path({events})
args=sys.argv[1:]
if args[0]=='remove':
 result=subprocess.run([real,*args])
 if result.returncode: sys.exit(result.returncode)
 d=json.loads(profile.read_text())
 d.get('dependencies',{{}}).pop('tokemula-image-connector',None)
 d['dsh']['profile']['bundles']=[v for v in d['dsh']['profile']['bundles'] if v!='tokemula-image-connector']
 d['written_after_failed_remove']={{'keep':True}}
 profile.write_text(json.dumps(d))
 sys.exit(23)
if args[0]=='add':
 d=json.loads(profile.read_text())
 before='tokemula-image-connector' in d['dsh']['profile']['bundles']
 if marker.exists(): sys.exit(24)
 result=subprocess.run([real,*args])
 d=json.loads(profile.read_text())
 after='tokemula-image-connector' in d['dsh']['profile']['bundles']
 with events.open('a') as f:f.write(json.dumps({{'before_add_enabled':before,'after_add_enabled':after}})+'\n')
 sys.exit(result.returncode)
os.execv(real,[real,*args])
"#,
        real = serde_json::to_string(pnpm.to_str().unwrap()).unwrap(),
        profile = serde_json::to_string(
            p.dsh_home
                .join("profiles/web/package.json")
                .to_str()
                .unwrap()
        )
        .unwrap(),
        marker = serde_json::to_string(marker.to_str().unwrap()).unwrap(),
        events = serde_json::to_string(root.join("native-recovery-events.jsonl").to_str().unwrap())
            .unwrap(),
    );
    fs::write(bin.join("pnpm"), script).unwrap();
    fs::set_permissions(bin.join("pnpm"), fs::Permissions::from_mode(0o700)).unwrap();
    let mut changed = p.clone();
    changed.process_path =
        std::env::join_paths(std::iter::once(bin).chain(std::env::split_paths(&p.process_path)))
            .unwrap();
    changed
}

#[test]
#[ignore = "explicit isolated installed DeepSeek CLI required"]
fn native_partial_remove_failure_restores_enabled_and_disabled_registration() {
    for enabled in [true, false] {
        let r = TempDir::new().unwrap();
        let p = real_paths(r.path());
        initialize(&p);
        let a = DeepSeekAdapter::new(Scope::User, p.clone()).unwrap();
        a.install().unwrap();
        let mut before = doc(&a);
        before["existing_user_fields"] = json!({"model":"preserved"});
        if !enabled {
            before["dsh"]["profile"]["bundles"]
                .as_array_mut()
                .unwrap()
                .retain(|v| v != "tokemula-image-connector");
        }
        write_doc(&a, &before);
        let failed = DeepSeekAdapter::new(
            Scope::User,
            partial_remove_package_manager(&p, r.path(), false),
        )
        .unwrap();
        assert_eq!(failed.remove().unwrap_err().code, "cli_failed");
        let after = doc(&a);
        assert_eq!(
            after["dependencies"]["tokemula-image-connector"],
            before["dependencies"]["tokemula-image-connector"]
        );
        assert_eq!(
            after["dsh"]["profile"]["bundles"],
            before["dsh"]["profile"]["bundles"]
        );
        assert_eq!(
            after["existing_user_fields"],
            before["existing_user_fields"]
        );
        assert_eq!(after["written_after_failed_remove"], json!({"keep":true}));
        assert!(
            a.profile_path()
                .join("node_modules/tokemula-image-connector/package.json")
                .is_file()
        );
        assert_eq!(a.check().unwrap(), InstallStatus::Installed);
        assert_eq!(a.plan().unwrap().bundle_enabled, enabled);
        assert!(!a.recovery_path().exists());
        let events = fs::read_to_string(r.path().join("native-recovery-events.jsonl")).unwrap();
        if !enabled {
            for line in events.lines() {
                let event: Value = serde_json::from_str(line).unwrap();
                assert_eq!(event["before_add_enabled"], false);
                assert_eq!(event["after_add_enabled"], false);
            }
        }
        assert_eq!(a.remove().unwrap(), InstallStatus::Removed);
    }
}

#[test]
#[ignore = "explicit isolated installed DeepSeek CLI required"]
fn native_recovery_failure_preserves_private_evidence_and_explicit_retry() {
    let r = TempDir::new().unwrap();
    let p = real_paths(r.path());
    initialize(&p);
    let a = DeepSeekAdapter::new(Scope::User, p.clone()).unwrap();
    a.install().unwrap();
    let mut before = doc(&a);
    before["dsh"]["profile"]["bundles"]
        .as_array_mut()
        .unwrap()
        .retain(|v| v != "tokemula-image-connector");
    write_doc(&a, &before);
    let failed = DeepSeekAdapter::new(
        Scope::User,
        partial_remove_package_manager(&p, r.path(), true),
    )
    .unwrap();
    assert_eq!(failed.remove().unwrap_err().code, "rollback_required");
    let record: Value = serde_json::from_slice(&fs::read(a.recovery_path()).unwrap()).unwrap();
    assert!(record["before"]["bundle_index"].is_null());
    assert_eq!(
        record["before"]["dependency"],
        before["dependencies"]["tokemula-image-connector"]
    );
    assert!(Path::new(record["snapshot"].as_str().unwrap()).is_file());
    assert_eq!(
        fs::metadata(a.recovery_path())
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    assert_eq!(doc(&a)["written_after_failed_remove"], json!({"keep":true}));
    assert_eq!(fs::read(a.skill_path()).unwrap(), SKILL_CONTENT.as_bytes());
    fs::remove_file(r.path().join("recovery-unavailable")).unwrap();
    assert_eq!(failed.recover().unwrap(), InstallStatus::Installed);
    assert!(!a.plan().unwrap().bundle_enabled);
    assert!(!a.recovery_path().exists());
    assert_eq!(a.remove().unwrap(), InstallStatus::Removed);
}

#[test]
#[ignore = "explicit isolated installed DeepSeek CLI required"]
fn native_managed_update_refreshes_only_this_package_and_keeps_disabled_state() {
    for enabled in [true, false] {
        let root = TempDir::new().unwrap();
        let old_paths = real_paths(root.path());
        initialize(&old_paths);
        let old = DeepSeekAdapter::new(Scope::User, old_paths.clone()).unwrap();
        assert_eq!(old.install().unwrap(), InstallStatus::Installed);
        let mut profile = doc(&old);
        profile["later_user_field"] = json!({"keep":true});
        if !enabled {
            profile["dsh"]["profile"]["bundles"]
                .as_array_mut()
                .unwrap()
                .retain(|name| name != "tokemula-image-connector");
        }
        write_doc(&old, &profile);
        // Represent an installed previous release; the explicit adapter supplies its known context.
        let state_path = old_paths.state_dir.join("ownership.json");
        let mut state: Value = serde_json::from_slice(&fs::read(&state_path).unwrap()).unwrap();
        state["installations"][0]["skill"] = json!("previous native release skill");
        state["installations"][0]
            .as_object_mut()
            .unwrap()
            .remove("context");
        fs::write(&state_path, serde_json::to_vec(&state).unwrap()).unwrap();
        fs::write(old.skill_path(), b"previous native release skill").unwrap();
        let next_program = root.path().join("新版 独立 MCP");
        fs::copy(&old_paths.executable, &next_program).unwrap();
        let mut next_paths = old_paths.clone();
        next_paths.executable = next_program.clone();
        let next = DeepSeekAdapter::new(Scope::User, next_paths.clone()).unwrap();
        assert_eq!(next.update().unwrap(), InstallStatus::Installed);
        assert_eq!(next.check().unwrap(), InstallStatus::Installed);
        assert_eq!(
            fs::read(next.skill_path()).unwrap(),
            SKILL_CONTENT.as_bytes()
        );
        assert_eq!(doc(&next)["later_user_field"], json!({"keep":true}));
        assert_eq!(
            doc(&next)["dsh"]["profile"]["bundles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == "tokemula-image-connector"),
            enabled
        );
        let output = command(&next_paths)
            .args(["--profile", "web", "--dump-config"])
            .output()
            .unwrap();
        assert!(output.status.success());
        let effective: serde_yaml_ng::Value = serde_yaml_ng::from_slice(&output.stdout).unwrap();
        let registered = effective
            .as_sequence()
            .unwrap()
            .iter()
            .find(|row| row["id"].as_str() == Some("tokemula-image"));
        if enabled {
            assert_eq!(
                registered.unwrap()["config"]["command"].as_str(),
                next_program.to_str()
            );
        } else {
            assert!(registered.is_none());
        }
        // Reverting the managed executable uses the same native API; bundle selection is unchanged.
        assert_eq!(old.update().unwrap(), InstallStatus::Installed);
        assert_eq!(old.check().unwrap(), InstallStatus::Installed);
        assert_eq!(doc(&old)["later_user_field"], json!({"keep":true}));
        assert_eq!(
            doc(&old)["dsh"]["profile"]["bundles"]
                .as_array()
                .unwrap()
                .iter()
                .any(|name| name == "tokemula-image-connector"),
            enabled
        );
        assert_eq!(old.remove().unwrap(), InstallStatus::Removed);
        assert_eq!(old.check().unwrap(), InstallStatus::NotInstalled);
    }
}
