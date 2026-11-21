//! Explicit, ignored acceptance driver for installed CLIs in caller-owned isolated roots.

use serde_json::{Value, json};
use std::{env, fs, path::PathBuf};
use tokemula_image_connector::adapters::{Adapter, AdapterPaths, Harness, InstallStatus, Scope};

#[test]
#[ignore = "Requires explicit isolated fixture and installed CLI paths; never runs a model turn"]
fn installed_harness_adapter_lifecycle() {
    let input =
        env::var_os("TOKEMULA_C1_ACCEPTANCE_INPUT").expect("explicit isolated fixture is required");
    let output = env::var_os("TOKEMULA_C1_ACCEPTANCE_OUTPUT")
        .expect("explicit evidence destination is required");
    let phase = env::var("TOKEMULA_C1_ACCEPTANCE_PHASE").expect("install or remove phase");
    let fixture: Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
    let path = |v: &Value, k: &str| PathBuf::from(v[k].as_str().unwrap());
    let root = path(&fixture, "root");
    assert!(root.is_absolute() && root.is_dir());
    let mut reports = vec![];
    for case in fixture["cases"].as_array().unwrap() {
        let harness = match case["harness"].as_str().unwrap() {
            "codex" => Harness::Codex,
            "claude_code" => Harness::ClaudeCode,
            _ => panic!("unsupported fixture harness"),
        };
        let scope = match case["scope"].as_str().unwrap() {
            "user" => Scope::User,
            "project" => Scope::Project,
            _ => panic!("unsupported fixture scope"),
        };
        for k in ["home", "codex_home", "project_dir", "state_dir"] {
            assert!(
                path(case, k).starts_with(&root),
                "fixture root escapes: {k}"
            );
        }
        if let Some(value) = case["claude_config_dir"].as_str() {
            assert!(PathBuf::from(value).starts_with(&root));
        }
        let adapter = Adapter::new(
            harness,
            scope,
            AdapterPaths {
                home: path(case, "home"),
                codex_home: path(case, "codex_home"),
                claude_config_dir: case["claude_config_dir"].as_str().map(PathBuf::from),
                project_dir: path(case, "project_dir"),
                state_dir: path(case, "state_dir"),
                cli: path(case, "cli"),
                executable: path(case, "executable"),
                process_path: case["process_path"].as_str().unwrap().into(),
            },
        )
        .unwrap();
        let version = adapter.detect().unwrap();
        let before = adapter.plan().unwrap();
        match phase.as_str() {
            "install" => {
                assert_eq!(before.status, InstallStatus::Ready);
                assert_eq!(adapter.install().unwrap(), InstallStatus::Installed);
                assert_eq!(adapter.check().unwrap(), InstallStatus::Installed);
                assert_eq!(adapter.install().unwrap(), InstallStatus::AlreadyInstalled);
            }
            "remove" => {
                assert_eq!(before.status, InstallStatus::Installed);
                let config_path = adapter.config_path();
                let old = fs::read_to_string(&config_path).unwrap();
                let later = if harness == Harness::Codex {
                    format!(
                        "{old}\n# Added by the user after installation\n[c1_later_user_content]\nkeep = \"later-value\"\n"
                    )
                } else {
                    let mut value: Value = serde_json::from_str(&old).unwrap();
                    value["c1_later_user_content"] = json!({"keep":"later-value"});
                    serde_json::to_string_pretty(&value).unwrap()
                };
                fs::write(&config_path, later).unwrap();
                assert_eq!(adapter.remove().unwrap(), InstallStatus::Removed);
                assert_eq!(adapter.check().unwrap(), InstallStatus::NotInstalled);
                let remaining = fs::read_to_string(&config_path).unwrap();
                assert!(remaining.contains("later-value"));
                assert!(remaining.contains("existing-fixture-value"));
                assert!(!adapter.skill_path().exists());
                assert_eq!(adapter.remove().unwrap(), InstallStatus::NotInstalled);
            }
            _ => panic!("unsupported fixture phase"),
        }
        reports.push(
            json!({"case":case["name"],"harness":harness,"scope":scope,"version":version,
            "phase":phase,"config_path":adapter.config_path(),"skill_path":adapter.skill_path(),
            "status":adapter.check().unwrap()}),
        );
    }
    fs::write(output, serde_json::to_vec_pretty(&reports).unwrap()).unwrap();
    println!(
        "{} actual CLI/scope adapter cases passed: {}",
        reports.len(),
        phase
    );
}
