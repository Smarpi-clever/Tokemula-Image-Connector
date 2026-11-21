//! Explicit D2 lifecycle fixture. It can access only this batch's synthetic system entry.
use serde::Deserialize;
use serde_json::json;
use std::{path::PathBuf, sync::Arc, time::Duration};
use tokemula_image_connector::{
    distribution::{DataPaths, Installer},
    management,
    settings::{ModelPreferences, SettingsService},
};

const SYNTHETIC_KEY: &str = "D2_SYNTHETIC_LIFECYCLE_KEEP_ONLY";

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    directory: PathBuf,
    owned_root: PathBuf,
    install_root: Option<PathBuf>,
    source: Option<PathBuf>,
    trust: Option<PathBuf>,
    update_url: Option<String>,
    adapter_state: Option<PathBuf>,
    tasks: Option<PathBuf>,
    cache: Option<PathBuf>,
    hook_phase: Option<String>,
    ready: Option<PathBuf>,
    release: Option<PathBuf>,
    namespace: String,
    loopback_base: String,
}

#[tokio::main]
async fn main() {
    let result = async {
        let path = std::env::var_os("TOKEMULA_D2_ACCEPTANCE_CONTEXT").ok_or(())?;
        let context: Context =
            serde_json::from_slice(&std::fs::read(path).map_err(|_| ())?).map_err(|_| ())?;
        if !context.directory.starts_with(&context.owned_root)
            || !context
                .namespace
                .starts_with("tokemula-image-test-d2-acceptance-")
        {
            return Err(());
        }
        let service = SettingsService::for_isolated_tests(
            context.directory,
            &context.namespace,
            &context.loopback_base,
        )
        .map_err(|_| ())?;
        let action = std::env::var("TOKEMULA_D2_ACCEPTANCE_ACTION").map_err(|_| ())?;
        match action.as_str() {
            "seed" => {
                let validated = service
                    .validate(SYNTHETIC_KEY.to_owned())
                    .await
                    .map_err(|_| ())?;
                let preferences = ModelPreferences {
                    default_model: "sample-fast".into(),
                    fallback_models: vec![],
                    auto_fallback: false,
                };
                service.save(&validated, preferences).map_err(|_| ())?;
                println!("{}", json!({"saved_synthetic_entry":true}));
                Ok(0)
            }
            "inspect" => {
                let context = service.snapshot().map_err(|_| ())?;
                println!(
                    "{}",
                    json!({
                        "key_present":context.api_key.is_some(),
                        "synthetic_key_matches":context.api_key.as_deref()==Some(SYNTHETIC_KEY),
                        "preferences":&context.preferences,
                        "source":context.source,
                    })
                );
                Ok(0)
            }
            "logout" => {
                service.logout().map_err(|_| ())?;
                let context = service.snapshot().map_err(|_| ())?;
                if context.api_key.is_some() {
                    return Err(());
                }
                println!(
                    "{}",
                    json!({"synthetic_entry_deleted":true,"preferences":&context.preferences})
                );
                Ok(0)
            }
            "manage" => Ok(management::run_with_service(std::env::args_os(), service).await),
            "hook_apply" | "hook_setup" => {
                let install_root = context.install_root.ok_or(())?;
                let adapter_state = context.adapter_state.ok_or(())?;
                let tasks = context.tasks.ok_or(())?;
                let cache = context.cache.ok_or(())?;
                let ready = context.ready.ok_or(())?;
                let release = context.release.ok_or(())?;
                for path in [
                    &install_root,
                    &adapter_state,
                    &tasks,
                    &cache,
                    &ready,
                    &release,
                ] {
                    if !path.starts_with(&context.owned_root) {
                        return Err(());
                    }
                }
                let phase = context.hook_phase.ok_or(())?;
                if !matches!(phase.as_str(), "journal_prepared" | "pointer_switched") {
                    return Err(());
                }
                let installer = Installer::new(install_root, adapter_state)
                    .map_err(|_| ())?
                    .with_data_paths(DataPaths { tasks, cache })
                    .map_err(|_| ())?
                    .with_test_hook(Arc::new(move |observed| {
                        if observed == phase {
                            std::fs::write(&ready, observed.as_bytes()).expect("owned hook ready");
                            while !release.exists() {
                                std::thread::sleep(Duration::from_millis(30));
                            }
                        }
                    }));
                let operation = if action == "hook_setup" {
                    let source = context.source.ok_or(())?;
                    let trust = context.trust.ok_or(())?;
                    if !source.starts_with(&context.owned_root)
                        || !trust.starts_with(&context.owned_root)
                    {
                        return Err(());
                    }
                    installer.setup(&source, &trust, context.update_url).await
                } else {
                    installer.apply(true).await
                };
                match operation {
                    Ok(status) => {
                        println!("{}", status);
                        Ok(0)
                    }
                    Err(error) => {
                        println!("{}", json!({"code":error.code,"message":error.message}));
                        Ok(1)
                    }
                }
            }
            _ => Err(()),
        }
    }
    .await;
    std::process::exit(match result {
        Ok(code) => code,
        Err(()) => {
            eprintln!("D2 fixture requires an explicit isolated context and allowed action.");
            2
        }
    });
}
