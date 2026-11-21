//! Explicit loopback/Keychain fixture for D1's real cross-process and PTY acceptance.
use serde::Deserialize;
use std::path::PathBuf;
use tokemula_image_connector::{management, settings::SettingsService};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Context {
    directory: PathBuf,
    namespace: String,
    loopback_base: String,
}

#[tokio::main]
async fn main() {
    let result = async {
        let path = std::env::var_os("TOKEMULA_D1_CLI_TEST_CONTEXT").ok_or(())?;
        let context: Context =
            serde_json::from_slice(&std::fs::read(path).map_err(|_| ())?).map_err(|_| ())?;
        if !context.namespace.starts_with("tokemula-image-test-d1-cli-") {
            return Err(());
        }
        let service = SettingsService::for_isolated_tests(
            context.directory,
            &context.namespace,
            &context.loopback_base,
        )
        .map_err(|_| ())?;
        Ok(management::run_with_service(std::env::args_os(), service).await)
    }
    .await;
    std::process::exit(match result {
        Ok(code) => code,
        Err(()) => {
            eprintln!("D1 CLI fixture requires an explicit isolated context.");
            2
        }
    });
}
