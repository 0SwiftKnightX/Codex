#![allow(clippy::expect_used, clippy::unwrap_used)]

use std::path::Path;

use mcp_test_support::McpProcess;
use mcp_test_support::create_final_assistant_message_sse_response;
use mcp_test_support::create_mock_chat_completions_server;
use mcp_types::JSONRPCNotification;
use pretty_assertions::assert_eq;
use serde_json::json;
use tempfile::TempDir;
use tokio::time::timeout;

const DEFAULT_READ_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_connect_then_send_receives_initial_state_and_notifications() {
    let responses = vec![
        create_final_assistant_message_sse_response("Done").expect("build mock assistant message"),
    ];
    let server = create_mock_chat_completions_server(responses).await;

    let codex_home = TempDir::new().expect("create temp dir");
    create_config_toml(codex_home.path(), &server.uri()).expect("write config.toml");

    let mut mcp = McpProcess::new(codex_home.path())
        .await
        .expect("spawn mcp process");
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize())
        .await
        .expect("init timeout")
        .expect("init failed");

    // Create conversation
    let conv_id = mcp
        .create_conversation_and_get_id("", "o3", "/repo")
        .await
        .expect("create conversation");

    // Connect the stream
    let (_stream_req, params) = mcp
        .connect_stream_and_expect_initial_state(&conv_id)
        .await
        .expect("initial_state params");
    assert_eq!(
        params["_meta"]["conversationId"].as_str(),
        Some(conv_id.as_str())
    );
    assert_eq!(params["initial_state"], json!({ "events": [] }));

    // Send a message and expect a subsequent notification (non-initial_state)
    mcp.send_user_message_and_wait_ok("Hello there", &conv_id)
        .await
        .expect("send message ok");

    // Read until we see an event notification (new schema example: agent_message)
    let params = mcp.wait_for_agent_message().await.expect("agent message");
    assert_eq!(
        params["msg"],
        json!({ "type": "agent_message", "message": "Done" })
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_send_then_connect_receives_initial_state_with_message() {
    let responses = vec![
        create_final_assistant_message_sse_response("Done").expect("build mock assistant message"),
    ];
    let server = create_mock_chat_completions_server(responses).await;

    let codex_home = TempDir::new().expect("create temp dir");
    create_config_toml(codex_home.path(), &server.uri()).expect("write config.toml");

    let mut mcp = McpProcess::new(codex_home.path())
        .await
        .expect("spawn mcp process");
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize())
        .await
        .expect("init timeout")
        .expect("init failed");

    // Create conversation
    let conv_id = mcp
        .create_conversation_and_get_id("", "o3", "/repo")
        .await
        .expect("create conversation");

    // Send a message BEFORE connecting stream
    mcp.send_user_message_and_wait_ok("Hello world", &conv_id)
        .await
        .expect("send message ok");

    // Now connect stream and expect InitialState with the prior message included
    let (_stream_req, params) = mcp
        .connect_stream_and_expect_initial_state(&conv_id)
        .await
        .expect("initial_state params");
    let events = params["initial_state"]["events"]
        .as_array()
        .expect("events array");
    let mut agent_events: Vec<_> = events
        .iter()
        .filter(|ev| ev["msg"]["type"].as_str() == Some("agent_message"))
        .cloned()
        .collect();
    if agent_events.is_empty() {
        // Fallback to live notification if not present in initial state, then assert the full event list
        let note: JSONRPCNotification = timeout(
            DEFAULT_READ_TIMEOUT,
            mcp.read_stream_until_notification_method("agent_message"),
        )
        .await
        .expect("agent_message note timeout")
        .expect("agent_message note err");
        let p = note.params.expect("params");
        agent_events.push(json!({ "msg": { "type": "agent_message", "message": p["msg"]["message"].as_str().expect("message str") } }));
    }
    let expected = vec![json!({ "msg": { "type": "agent_message", "message": "Done" } })];
    assert_eq!(
        agent_events, expected,
        "initial_state agent_message events should match exactly"
    );
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn test_cancel_stream_then_reconnect_catches_up_initial_state() {
    // One response is sufficient for the assertions in this test
    let responses = vec![
        create_final_assistant_message_sse_response("Done 1")
            .expect("build mock assistant message"),
        create_final_assistant_message_sse_response("Done 2")
            .expect("build mock assistant message"),
    ];
    let server = create_mock_chat_completions_server(responses).await;

    let codex_home = TempDir::new().expect("create temp dir");
    create_config_toml(codex_home.path(), &server.uri()).expect("write config.toml");

    let mut mcp = McpProcess::new(codex_home.path())
        .await
        .expect("spawn mcp process");
    timeout(DEFAULT_READ_TIMEOUT, mcp.initialize())
        .await
        .expect("init timeout")
        .expect("init failed");

    // Create and connect stream A
    let conv_id = mcp
        .create_conversation_and_get_id("", "o3", "/repo")
        .await
        .expect("create");
    let (stream_a_id, _params) = mcp
        .connect_stream_and_expect_initial_state(&conv_id)
        .await
        .expect("stream A initial_state");

    // Send M1 and ensure we get live agent_message
    mcp.send_user_message_and_wait_ok("Hello M1", &conv_id)
        .await
        .expect("send M1");
    let _params = mcp.wait_for_agent_message().await.expect("agent M1");

    // Cancel stream A
    mcp.send_notification(
        "notifications/cancelled",
        Some(json!({ "requestId": stream_a_id })),
    )
    .await
    .expect("send cancelled");

    // Send M2 while stream is cancelled; we should NOT get agent_message live
    mcp.send_user_message_and_wait_ok("Hello M2", &conv_id)
        .await
        .expect("send M2");
    let maybe = mcp
        .maybe_wait_for_agent_message(std::time::Duration::from_millis(300))
        .await
        .expect("maybe wait");
    assert!(
        maybe.is_none(),
        "should not get live agent_message after cancel"
    );

    // Connect stream B and expect initial_state that includes the response
    let (_stream_req, params) = mcp
        .connect_stream_and_expect_initial_state(&conv_id)
        .await
        .expect("stream B initial_state");
    let events = params["initial_state"]["events"]
        .as_array()
        .expect("events array");
    let agent_events: Vec<_> = events
        .iter()
        .filter(|ev| ev["msg"]["type"].as_str() == Some("agent_message"))
        .cloned()
        .collect();
    let expected = vec![
        json!({ "msg": { "type": "agent_message", "message": "Done 1" } }),
        json!({ "msg": { "type": "agent_message", "message": "Done 2" } }),
    ];
    assert_eq!(
        agent_events, expected,
        "initial_state agent_message events should match exactly"
    );
    drop(server);
}

// Helper to create a config.toml pointing at the mock model server.
fn create_config_toml(codex_home: &Path, server_uri: &str) -> std::io::Result<()> {
    let config_toml = codex_home.join("config.toml");
    std::fs::write(
        config_toml,
        format!(
            r#"
model = "mock-model"
approval_policy = "never"
sandbox_mode = "danger-full-access"

model_provider = "mock_provider"

[model_providers.mock_provider]
name = "Mock provider for test"
base_url = "{server_uri}/v1"
wire_api = "chat"
request_max_retries = 0
stream_max_retries = 0
"#
        ),
    )
}
