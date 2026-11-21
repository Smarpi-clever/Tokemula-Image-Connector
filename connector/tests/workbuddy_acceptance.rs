//! Explicitly supplied isolated WorkBuddy fixture; no app, login, or model is started here.

use serde_json::{Value, json};
use std::{fs, path::PathBuf};
use tokemula_image_connector::adapters::{
    Scope,
    workbuddy::{WorkBuddyAdapter, WorkBuddyPaths, WorkBuddyStatus},
};

#[test]
#[ignore = "requires a separately provisioned isolated WorkBuddy 5.11.3 fixture"]
fn isolated_workbuddy_product_lifecycle() {
    let input = PathBuf::from(std::env::var("TOKEMULA_WORKBUDDY_ACCEPTANCE_INPUT").unwrap());
    let output = PathBuf::from(std::env::var("TOKEMULA_WORKBUDDY_ACCEPTANCE_OUTPUT").unwrap());
    let phase = std::env::var("TOKEMULA_WORKBUDDY_ACCEPTANCE_PHASE").unwrap();
    let data: Value = serde_json::from_slice(&fs::read(input).unwrap()).unwrap();
    let root = PathBuf::from(data["root"].as_str().unwrap());
    assert!(root.is_absolute() && root.is_dir());
    let adapter = WorkBuddyAdapter::new(
        Scope::User,
        WorkBuddyPaths {
            home: root.join("home"),
            config_dir: root.join("config"),
            state_dir: root.join("product-state"),
            app_bundle: PathBuf::from(data["app_bundle"].as_str().unwrap()),
            executable: PathBuf::from(data["executable"].as_str().unwrap()),
        },
    )
    .unwrap();
    let result = match phase.as_str() {
        "install" => {
            let original = json!({
                "provider": {"name":"synthetic-existing"}, "model":"preserved-model",
                "unknown": {"keep":true},
                "mcpServers":{"other-tool":{"command":"/synthetic/missing","disabled":true}}
            });
            fs::create_dir_all(root.join("config")).unwrap();
            fs::write(
                adapter.config_path(),
                serde_json::to_vec_pretty(&original).unwrap(),
            )
            .unwrap();
            assert_eq!(adapter.plan().unwrap().status, WorkBuddyStatus::Ready);
            assert_eq!(
                adapter.install().unwrap(),
                WorkBuddyStatus::AwaitingConfirmation
            );
            assert_eq!(
                adapter.check().unwrap(),
                WorkBuddyStatus::AwaitingConfirmation
            );
            let before = fs::read(adapter.config_path()).unwrap();
            assert_eq!(
                adapter.install().unwrap(),
                WorkBuddyStatus::AwaitingConfirmation
            );
            assert_eq!(before, fs::read(adapter.config_path()).unwrap());
            assert!(!root.join("config/mcp-approvals.json").exists());
            let config: Value = serde_json::from_slice(&before).unwrap();
            assert_eq!(config["provider"], original["provider"]);
            assert_eq!(config["unknown"], original["unknown"]);
            assert_eq!(
                config["mcpServers"]["other-tool"],
                original["mcpServers"]["other-tool"]
            );
            let package = adapter
                .export_package(&root.join("official-package"))
                .unwrap();
            assert_eq!(
                fs::read(package.join("skills/tokemula-image/SKILL.md")).unwrap(),
                fs::read(adapter.skill_path()).unwrap()
            );
            json!({"phase":phase,"version":adapter.detect().unwrap(),"status":"awaiting_confirmation","registered":true,"native_approval_written":false,"config_path":adapter.config_path(),"skill_path":adapter.skill_path(),"package":package,"existing_fields_preserved":true})
        }
        "approved" => {
            assert_eq!(adapter.check().unwrap(), WorkBuddyStatus::Installed);
            let approval = fs::read(root.join("config/mcp-approvals.json")).unwrap();
            assert_eq!(
                adapter.install().unwrap(),
                WorkBuddyStatus::AlreadyInstalled
            );
            assert_eq!(
                approval,
                fs::read(root.join("config/mcp-approvals.json")).unwrap()
            );
            json!({"phase":phase,"status":"installed","native_approval_read_only":true})
        }
        "remove" => {
            let mut config: Value =
                serde_json::from_slice(&fs::read(adapter.config_path()).unwrap()).unwrap();
            config["later_user_content"] = json!({"preserved":true});
            fs::write(
                adapter.config_path(),
                serde_json::to_vec_pretty(&config).unwrap(),
            )
            .unwrap();
            let extra = adapter.skill_path().parent().unwrap().join("user-note.txt");
            fs::write(&extra, "later user content").unwrap();
            let approval = fs::read(root.join("config/mcp-approvals.json")).unwrap();
            assert_eq!(adapter.remove().unwrap(), WorkBuddyStatus::Removed);
            let current: Value =
                serde_json::from_slice(&fs::read(adapter.config_path()).unwrap()).unwrap();
            assert!(current["mcpServers"].get("tokemula-image").is_none());
            assert_eq!(
                current["mcpServers"]["other-tool"],
                config["mcpServers"]["other-tool"]
            );
            assert_eq!(current["later_user_content"], config["later_user_content"]);
            assert_eq!(current["provider"], config["provider"]);
            assert_eq!(
                approval,
                fs::read(root.join("config/mcp-approvals.json")).unwrap()
            );
            assert!(!adapter.skill_path().exists());
            assert_eq!(fs::read_to_string(extra).unwrap(), "later user content");
            json!({"phase":phase,"status":"removed","later_user_content_preserved":true,"other_mcp_preserved":true,"native_approval_preserved":true,"skill_extra_preserved":true})
        }
        _ => panic!("unsupported acceptance phase"),
    };
    fs::write(output, serde_json::to_vec_pretty(&result).unwrap()).unwrap();
}
