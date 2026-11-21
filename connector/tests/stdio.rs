use std::{process::Stdio, time::Duration};

use rmcp::{
    ClientLifecycleMode, ClientServiceExt,
    model::{
        CallToolRequestParams, CallToolResponse, ClientConfig, Implementation, ProtocolVersion,
        ServerJsonRpcMessage,
    },
};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    process::Command,
    time::timeout,
};

const DEADLINE: Duration = Duration::from_secs(5);
const SYNTHETIC_SECRET: &str = "synthetic-stdio-secret-do-not-echo";

fn binary_command() -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_tokemula-image-mcp"));
    command
        .env_clear()
        .env("UNRELATED_SYNTHETIC_SECRET", SYNTHETIC_SECRET)
        .env("TOKEMULA_IMAGE_KEY", "")
        .kill_on_drop(true)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    command
}

async fn check_protocol_lifecycle(version: ProtocolVersion, lifecycle: ClientLifecycleMode) {
    let directory = tempfile::tempdir().unwrap();
    let mut child = binary_command()
        .current_dir(directory.path())
        .env("HOME", directory.path())
        .spawn()
        .unwrap();
    let stdin = child.stdin.take().unwrap();
    let mut stdout = child.stdout.take().unwrap();
    let mut stderr = child.stderr.take().unwrap();
    let stderr_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        stderr.read_to_end(&mut bytes).await.unwrap();
        bytes
    });
    let (client_stdout, mut forward_stdout) = tokio::io::duplex(64 * 1024);
    let stdout_task = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let mut buffer = [0; 8192];
        loop {
            let count = stdout.read(&mut buffer).await.unwrap();
            if count == 0 {
                break;
            }
            bytes.extend_from_slice(&buffer[..count]);
            // Keep capturing shutdown output after the SDK drops its read side.
            let _ = forward_stdout.write_all(&buffer[..count]).await;
        }
        bytes
    });

    let client = timeout(
        DEADLINE,
        ClientConfig::new(
            Default::default(),
            Implementation::new(SYNTHETIC_SECRET, "0.0.test"),
        )
        .with_protocol_version(version.clone())
        .serve_with_lifecycle((client_stdout, stdin), lifecycle),
    )
    .await
    .expect("binary protocol startup timed out")
    .expect("binary protocol startup failed");
    assert_eq!(client.peer_info().unwrap().protocol_version, version);

    let tools = timeout(DEADLINE, client.list_tools(None))
        .await
        .expect("tool discovery timed out")
        .unwrap();
    let mut names: Vec<_> = tools.tools.iter().map(|tool| tool.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "cancel_image_task",
            "configure_tokemula",
            "edit_image",
            "generate_image",
            "get_image_batch",
            "get_image_billing",
            "get_image_task",
            "list_image_models",
            "submit_image_batch"
        ]
    );
    assert!(tools.next_cursor.is_none());
    let edit = tools
        .tools
        .iter()
        .find(|tool| tool.name == "edit_image")
        .unwrap();
    let schema = serde_json::to_value(&edit.input_schema).unwrap();
    for field in [
        "image_path",
        "image_paths",
        "mask_path",
        "background",
        "output_format",
        "output_compression",
        "input_fidelity",
        "size",
        "quality",
        "n",
        "mode",
    ] {
        assert!(
            schema["properties"].get(field).is_some(),
            "missing edit field: {field}"
        );
    }
    assert_eq!(schema["required"], serde_json::json!(["prompt"]));

    let response = timeout(
        DEADLINE,
        client.call_tool_once(CallToolRequestParams::new("list_image_models")),
    )
    .await
    .expect("unconfigured tool call timed out")
    .unwrap();
    let CallToolResponse::Complete(result) = response else {
        panic!("unconfigured tool must return a complete credential error");
    };
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.as_ref().unwrap()["code"],
        "credential_unavailable"
    );
    let result_json = serde_json::to_string(&result).unwrap();
    assert!(!result_json.contains(SYNTHETIC_SECRET));

    let response = timeout(
        DEADLINE,
        client.call_tool_once(
            CallToolRequestParams::new("get_image_billing").with_arguments(
                serde_json::json!({"request_ref":"synthetic-previous-request"})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        ),
    )
    .await
    .expect("unconfigured billing call timed out")
    .unwrap();
    let CallToolResponse::Complete(result) = response else {
        panic!("unconfigured billing must return a complete credential error");
    };
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.as_ref().unwrap()["code"],
        "credential_unavailable"
    );

    let response = timeout(
        DEADLINE,
        client.call_tool_once(
            CallToolRequestParams::new("generate_image").with_arguments(
                serde_json::json!({"prompt": "synthetic prompt", "n": SYNTHETIC_SECRET})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        ),
    )
    .await
    .expect("invalid-parameter tool call timed out")
    .unwrap();
    let CallToolResponse::Complete(result) = response else {
        panic!("invalid parameters must return a complete tool error");
    };
    assert_eq!(result.is_error, Some(true));
    assert_eq!(
        result.structured_content.as_ref().unwrap()["code"],
        "invalid_parameters"
    );

    let response = timeout(
        DEADLINE,
        client.call_tool_once(
            CallToolRequestParams::new("get_image_billing").with_arguments(
                serde_json::json!({"request_ref":SYNTHETIC_SECRET,"api_key":SYNTHETIC_SECRET})
                    .as_object()
                    .unwrap()
                    .clone(),
            ),
        ),
    )
    .await
    .expect("invalid billing parameters timed out")
    .unwrap();
    let CallToolResponse::Complete(result) = response else {
        panic!("invalid billing parameters must return a complete tool error");
    };
    assert_eq!(
        result.structured_content.as_ref().unwrap()["code"],
        "invalid_parameters"
    );

    timeout(DEADLINE, client.cancel())
        .await
        .expect("closing SDK transport timed out")
        .unwrap();
    let status = timeout(DEADLINE, child.wait())
        .await
        .expect("binary did not exit on stdin EOF")
        .unwrap();
    assert!(status.success(), "binary exited unsuccessfully: {status}");

    let stdout = String::from_utf8(stdout_task.await.unwrap()).unwrap();
    assert!(!stdout.is_empty());
    assert!(stdout.ends_with('\n'));
    assert!(!stdout.contains(SYNTHETIC_SECRET));
    for line in stdout.lines() {
        assert!(
            serde_json::from_str::<ServerJsonRpcMessage>(line).is_ok(),
            "stdout contains non-MCP output"
        );
    }
    let stderr = String::from_utf8(stderr_task.await.unwrap()).unwrap();
    assert!(!stderr.contains(SYNTHETIC_SECRET));
}

#[tokio::test]
async fn legacy_initialize_discovers_tools_calls_without_credentials_and_exits_on_eof() {
    check_protocol_lifecycle(
        ProtocolVersion::V_2025_11_25,
        ClientLifecycleMode::Initialize,
    )
    .await;
}

#[tokio::test]
async fn discover_2026_07_28_discovers_tools_calls_without_credentials_and_exits_on_eof() {
    check_protocol_lifecycle(
        ProtocolVersion::V_2026_07_28,
        ClientLifecycleMode::Discover {
            preferred_versions: vec![ProtocolVersion::V_2026_07_28],
        },
    )
    .await;
}

#[tokio::test]
async fn eof_before_protocol_startup_exits_successfully_without_stdout() {
    let directory = tempfile::tempdir().unwrap();
    let child = binary_command()
        .current_dir(directory.path())
        .env("HOME", directory.path())
        .stdin(Stdio::null())
        .spawn()
        .unwrap();
    let output = timeout(DEADLINE, child.wait_with_output())
        .await
        .expect("binary did not exit on initial stdin EOF")
        .unwrap();
    assert!(output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!String::from_utf8_lossy(&output.stderr).contains(SYNTHETIC_SECRET));
}
