#![cfg(unix)]

use std::collections::HashMap;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::path::PathBuf;

use codex_mcp_server::GravitySketchAction;
use codex_mcp_server::GravitySketchToolCallParam;
use mcp_test_support::McpProcess;
use mcp_test_support::to_response;
use mcp_types::CallToolResult;
use mcp_types::ContentBlock;
use mcp_types::ListToolsResult;
use mcp_types::RequestId;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn tools_list_includes_gravity_sketch() {
    let codex_home = TempDir::new().expect("create tempdir");
    write_basic_config(codex_home.path()).expect("write config");

    let mut mcp = McpProcess::new(codex_home.path())
        .await
        .expect("spawn mcp process");
    timeout(DEFAULT_TIMEOUT, mcp.initialize())
        .await
        .expect("initialize timeout")
        .expect("initialize failed");

    let request_id = mcp
        .send_list_tools_request()
        .await
        .expect("send tools/list");

    let response = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await
    .expect("tools/list timeout")
    .expect("tools/list response");

    let result: ListToolsResult = to_response(response).expect("deserialize ListToolsResult");
    assert!(
        result
            .tools
            .iter()
            .any(|tool| tool.name == "gravity-sketch"),
        "tools/list should advertise gravity-sketch"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gravity_sketch_tool_success() {
    let codex_home = TempDir::new().expect("create tempdir");
    write_basic_config(codex_home.path()).expect("write config");
    let cli_path = write_mock_gravity_sketch_cli(codex_home.path()).expect("write cli");

    let cli_path_str = cli_path.to_string_lossy().to_string();
    let mut mcp = McpProcess::new_with_env(
        codex_home.path(),
        &[
            ("GRAVITY_SKETCH_CLI", Some(&cli_path_str)),
            ("GRAVITY_SKETCH_TOKEN", Some("env-token")),
        ],
    )
    .await
    .expect("spawn mcp process");
    timeout(DEFAULT_TIMEOUT, mcp.initialize())
        .await
        .expect("initialize timeout")
        .expect("initialize failed");

    let params = GravitySketchToolCallParam {
        action: GravitySketchAction::Launch,
        host: Some("127.0.0.1".to_string()),
        port: Some(7000),
        token: Some("override-token".to_string()),
        project_id: None,
        scene_id: None,
        export_path: None,
        import_path: None,
        extra_args: Vec::new(),
        payload: Some(json!({ "sync": true })),
        cli_path: None,
        env: HashMap::new(),
    };

    let request_id = mcp
        .send_gravity_sketch_tool_call(params)
        .await
        .expect("send gravity-sketch call");

    let notification = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_notification_message("gravity-sketch/event"),
    )
    .await
    .expect("gravity-sketch/event timeout")
    .expect("gravity-sketch/event notification");
    assert_eq!(notification.method, "gravity-sketch/event");
    let params = notification.params.expect("gravity-sketch/event params");
    assert_eq!(
        params.get("type").and_then(|value| value.as_str()),
        Some("stream"),
        "stream notification should set type=stream"
    );

    let response = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await
    .expect("gravity-sketch response timeout")
    .expect("gravity-sketch response");

    let result: CallToolResult = to_response(response).expect("deserialize CallToolResult");
    assert_eq!(result.is_error, Some(false));
    let text = match &result.content[..] {
        [ContentBlock::TextContent(content)] => content.text.clone(),
        other => panic!("unexpected content: {other:?}"),
    };
    assert!(
        text.contains("\"status\":\"ok\""),
        "stdout summary should include success payload"
    );

    let structured = result
        .structured_content
        .expect("structured_content should be populated");
    assert_eq!(structured.get("success"), Some(&json!(true)));
    assert!(
        structured
            .get("stdout")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .contains("launch:starting"),
        "stdout should capture CLI progress"
    );
    assert!(
        structured
            .get("stderr")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .contains("connecting to Gravity Sketch"),
        "stderr should report connection progress"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn gravity_sketch_tool_failure() {
    let codex_home = TempDir::new().expect("create tempdir");
    write_basic_config(codex_home.path()).expect("write config");
    let cli_path = write_mock_gravity_sketch_cli(codex_home.path()).expect("write cli");

    let cli_path_str = cli_path.to_string_lossy().to_string();
    let mut mcp = McpProcess::new_with_env(
        codex_home.path(),
        &[("GRAVITY_SKETCH_CLI", Some(&cli_path_str))],
    )
    .await
    .expect("spawn mcp process");
    timeout(DEFAULT_TIMEOUT, mcp.initialize())
        .await
        .expect("initialize timeout")
        .expect("initialize failed");

    let mut env = HashMap::new();
    env.insert("EXTRA_MODE".to_string(), "test".to_string());

    let params = GravitySketchToolCallParam {
        action: GravitySketchAction::ExportScene,
        host: None,
        port: None,
        token: None,
        project_id: Some("studio".to_string()),
        scene_id: Some("scene-42".to_string()),
        export_path: Some(codex_home.path().join("scene.usdz")),
        import_path: None,
        extra_args: Vec::new(),
        payload: None,
        cli_path: None,
        env,
    };

    let request_id = mcp
        .send_gravity_sketch_tool_call(params)
        .await
        .expect("send gravity-sketch call");

    let notification = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_notification_message("gravity-sketch/event"),
    )
    .await
    .expect("gravity-sketch/event timeout")
    .expect("gravity-sketch/event notification");
    assert_eq!(notification.method, "gravity-sketch/event");

    let response = timeout(
        DEFAULT_TIMEOUT,
        mcp.read_stream_until_response_message(RequestId::Integer(request_id)),
    )
    .await
    .expect("gravity-sketch response timeout")
    .expect("gravity-sketch response");

    let result: CallToolResult = to_response(response).expect("deserialize CallToolResult");
    assert_eq!(result.is_error, Some(true));
    let message = match &result.content[..] {
        [ContentBlock::TextContent(content)] => content.text.clone(),
        other => panic!("unexpected content: {other:?}"),
    };
    assert!(message.contains("failed with status"));

    let structured = result
        .structured_content
        .expect("structured_content should be populated");
    assert_eq!(structured.get("success"), Some(&json!(false)));
    assert_eq!(structured.get("status_code"), Some(&json!(2)));
    assert!(
        structured
            .get("stderr")
            .and_then(|value| value.as_str())
            .unwrap_or_default()
            .contains("missing license (test)"),
        "stderr should include the CLI failure message"
    );
}

fn write_basic_config(codex_home: &Path) -> std::io::Result<()> {
    let config_toml = codex_home.join("config.toml");
    fs::write(
        config_toml,
        r#"model = "mock-model"
approval_policy = "never"
sandbox_mode = "danger-full-access"
"#,
    )
}

fn write_mock_gravity_sketch_cli(dir: &Path) -> anyhow::Result<PathBuf> {
    let script_path = dir.join("mock_gravity_sketch.sh");
    fs::write(
        &script_path,
        r#"#!/bin/sh
HOST=""
PORT=""
PROJECT_ID=""
SCENE_ID=""
EXPORT_PATH=""
PAYLOAD=""
ACTION=""

while [ $# -gt 0 ]; do
  case "$1" in
    --host)
      HOST="$2"
      shift 2
      ;;
    --port)
      PORT="$2"
      shift 2
      ;;
    --project-id)
      PROJECT_ID="$2"
      shift 2
      ;;
    --scene-id)
      SCENE_ID="$2"
      shift 2
      ;;
    --output)
      EXPORT_PATH="$2"
      shift 2
      ;;
    --payload)
      PAYLOAD="$2"
      shift 2
      ;;
    launch|export-scene|import-asset|open-scene|run)
      ACTION="$1"
      shift
      break
      ;;
    *)
      echo "unsupported flag: $1" >&2
      exit 1
      ;;
  esac
done

case "$ACTION" in
  launch)
    echo "connecting to Gravity Sketch on ${HOST:-127.0.0.1}:${PORT:-7000}..." >&2
    echo "launch:starting"
    printf '{"status":"ok","action":"launch","token":"%s"}\n' "${GRAVITY_SKETCH_TOKEN:-unset}"
    exit 0
    ;;
  export-scene)
    echo "export failed: missing license (${EXTRA_MODE:-unset}) for ${SCENE_ID:-unknown} -> ${EXPORT_PATH:-unset}" >&2
    exit 2
    ;;
  *)
    echo "unsupported action: $ACTION" >&2
    exit 1
    ;;
esac
"#,
    )?;

    #[cfg(unix)]
    {
        let mut perms = fs::metadata(&script_path)?.permissions();
        perms.set_mode(0o755);
        fs::set_permissions(&script_path, perms)?;
    }

    Ok(script_path)
}
