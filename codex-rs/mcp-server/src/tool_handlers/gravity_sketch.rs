use std::collections::HashMap;
use std::env;
use std::path::PathBuf;
use std::process::Stdio;
use std::sync::Arc;

use anyhow::Context;
use anyhow::Result;
use mcp_types::CallToolResult;
use mcp_types::ContentBlock;
use mcp_types::RequestId;
use mcp_types::TextContent;
use mcp_types::Tool;
use mcp_types::ToolInputSchema;
use schemars::JsonSchema;
use schemars::r#gen::SchemaSettings;
use serde::Deserialize;
use serde::Serialize;
use serde_json::json;
use tokio::io::AsyncBufReadExt;
use tokio::io::BufReader;
use tokio::process::Command;
use tokio::task;
use tracing::error;

use crate::outgoing_message::OutgoingMessageSender;
use crate::outgoing_message::OutgoingNotification;
use crate::outgoing_message::OutgoingNotificationMeta;
use crate::outgoing_message::OutgoingNotificationParams;

/// Default command used to invoke the Gravity Sketch CLI when no override is supplied.
const DEFAULT_GRAVITY_SKETCH_CLI: &str = "gravity-sketch-cli";

/// Describes the supported Gravity Sketch operations that can be triggered via the MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "kebab-case")]
pub enum GravitySketchAction {
    /// Launch or focus the Gravity Sketch application.
    Launch,
    /// Open an existing scene by id.
    OpenScene,
    /// Export the active scene to a local file.
    ExportScene,
    /// Import an asset or scene file into Gravity Sketch.
    ImportAsset,
    /// Execute a custom CLI subcommand with additional arguments.
    CustomCommand,
}

impl GravitySketchAction {
    fn as_display_name(&self) -> &'static str {
        match self {
            Self::Launch => "launch",
            Self::OpenScene => "open-scene",
            Self::ExportScene => "export-scene",
            Self::ImportAsset => "import-asset",
            Self::CustomCommand => "custom-command",
        }
    }
}

/// Parameters accepted by the `gravity-sketch` MCP tool.
#[derive(Debug, Clone, Serialize, Deserialize, JsonSchema)]
#[serde(rename_all = "camelCase")]
pub struct GravitySketchToolCallParam {
    /// Action to execute.
    pub action: GravitySketchAction,

    /// Gravity Sketch host. Defaults to `127.0.0.1` when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub host: Option<String>,

    /// Gravity Sketch API port. Defaults to the CLI default when omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,

    /// Authentication token injected into the child process via `GRAVITY_SKETCH_TOKEN`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,

    /// Optional project identifier used by some commands.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub project_id: Option<String>,

    /// Scene identifier for `open-scene`/`export-scene`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scene_id: Option<String>,

    /// Local file used when exporting a scene.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub export_path: Option<PathBuf>,

    /// Local path of an asset/scene to import.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub import_path: Option<PathBuf>,

    /// Extra CLI arguments appended after the action.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub extra_args: Vec<String>,

    /// JSON payload forwarded to the CLI via `--payload`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub payload: Option<serde_json::Value>,

    /// Override the CLI executable used to reach Gravity Sketch.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_path: Option<PathBuf>,

    /// Additional environment variables for the launched process.
    #[serde(default, skip_serializing_if = "HashMap::is_empty")]
    pub env: HashMap<String, String>,
}

impl GravitySketchToolCallParam {
    fn resolve_cli_path(&self) -> PathBuf {
        if let Some(path) = &self.cli_path {
            return path.clone();
        }
        if let Some(path) = env::var("GRAVITY_SKETCH_CLI")
            .ok()
            .filter(|path| !path.is_empty())
        {
            return PathBuf::from(path);
        }
        PathBuf::from(DEFAULT_GRAVITY_SKETCH_CLI)
    }

    fn configure_command(&self, command: &mut Command) -> Result<()> {
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        command.stdin(Stdio::null());

        if let Some(host) = &self.host {
            command.arg("--host").arg(host);
        }
        if let Some(port) = self.port {
            command.arg("--port").arg(port.to_string());
        }
        if let Some(project_id) = &self.project_id {
            command.arg("--project-id").arg(project_id);
        }
        if let Some(token) = &self.token {
            command.env("GRAVITY_SKETCH_TOKEN", token);
        }
        for (key, value) in &self.env {
            command.env(key, value);
        }

        match self.action {
            GravitySketchAction::Launch => {
                command.arg("launch");
            }
            GravitySketchAction::OpenScene => {
                let scene_id = self
                    .scene_id
                    .as_ref()
                    .context("`scene_id` is required for the open-scene action")?;
                command.arg("open-scene").arg("--scene-id").arg(scene_id);
            }
            GravitySketchAction::ExportScene => {
                let scene_id = self
                    .scene_id
                    .as_ref()
                    .context("`scene_id` is required for the export-scene action")?;
                let export_path = self
                    .export_path
                    .as_ref()
                    .context("`export_path` is required for the export-scene action")?;
                command
                    .arg("export-scene")
                    .arg("--scene-id")
                    .arg(scene_id)
                    .arg("--output")
                    .arg(export_path);
            }
            GravitySketchAction::ImportAsset => {
                let import_path = self
                    .import_path
                    .as_ref()
                    .context("`import_path` is required for the import-asset action")?;
                command.arg("import-asset").arg(import_path);
            }
            GravitySketchAction::CustomCommand => {
                if self.extra_args.is_empty() {
                    anyhow::bail!(
                        "`extra_args` must contain at least one argument for the custom-command action"
                    );
                }
                command.arg("run");
                for arg in &self.extra_args {
                    command.arg(arg);
                }
            }
        }

        if let Some(payload) = &self.payload {
            let payload = serde_json::to_string(payload)?;
            command.arg("--payload").arg(payload);
        }

        Ok(())
    }
}

/// Builds the JSON schema describing the `gravity-sketch` MCP tool.
pub(crate) fn create_tool_for_gravity_sketch() -> Tool {
    let schema = SchemaSettings::draft2019_09()
        .with(|settings| {
            settings.inline_subschemas = true;
            settings.option_add_null_type = false;
        })
        .into_generator()
        .into_root_schema_for::<GravitySketchToolCallParam>();

    #[expect(clippy::expect_used)]
    let schema_value =
        serde_json::to_value(&schema).expect("Gravity Sketch tool schema should serialize to JSON");

    let tool_input_schema =
        serde_json::from_value::<ToolInputSchema>(schema_value).unwrap_or_else(|error| {
            panic!("failed to create Gravity Sketch tool schema: {error}");
        });

    Tool {
        name: "gravity-sketch".to_string(),
        title: Some("Gravity Sketch".to_string()),
        description: Some(
            "Control a local Gravity Sketch instance via its command line interface.".to_string(),
        ),
        input_schema: tool_input_schema,
        output_schema: None,
        annotations: None,
    }
}

/// Handle a `tools/call` invocation for the `gravity-sketch` tool.
pub(crate) async fn handle_tool_call_gravity_sketch(
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
    arguments: Option<serde_json::Value>,
) {
    let params = match arguments {
        Some(value) => match serde_json::from_value::<GravitySketchToolCallParam>(value) {
            Ok(params) => params,
            Err(err) => {
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_owned(),
                        text: format!(
                            "Failed to parse configuration for gravity-sketch tool: {err}"
                        ),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                outgoing.send_response(request_id, result).await;
                return;
            }
        },
        None => {
            let result = CallToolResult {
                content: vec![ContentBlock::TextContent(TextContent {
                    r#type: "text".to_owned(),
                    text: "Missing arguments for gravity-sketch tool-call.".to_owned(),
                    annotations: None,
                })],
                is_error: Some(true),
                structured_content: None,
            };
            outgoing.send_response(request_id, result).await;
            return;
        }
    };

    let outgoing_for_task = outgoing.clone();
    task::spawn(async move {
        let action = params.action.clone();
        let result =
            run_gravity_sketch(params, outgoing_for_task.clone(), request_id.clone()).await;

        match result {
            Ok(result) => {
                outgoing_for_task.send_response(request_id, result).await;
            }
            Err(err) => {
                error!("gravity-sketch action {action:?} failed: {err}");
                let result = CallToolResult {
                    content: vec![ContentBlock::TextContent(TextContent {
                        r#type: "text".to_owned(),
                        text: format!(
                            "Failed to execute gravity-sketch action {}: {err}",
                            action.as_display_name()
                        ),
                        annotations: None,
                    })],
                    is_error: Some(true),
                    structured_content: None,
                };
                outgoing_for_task.send_response(request_id, result).await;
            }
        }
    });
}

async fn run_gravity_sketch(
    params: GravitySketchToolCallParam,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
) -> Result<CallToolResult> {
    let cli_path = params.resolve_cli_path();
    let mut command = Command::new(&cli_path);
    params.configure_command(&mut command)?;

    let mut child = command
        .spawn()
        .with_context(|| format!("failed to spawn Gravity Sketch CLI at {cli_path:?}"))?;

    let stdout = child
        .stdout
        .take()
        .context("failed to capture stdout from Gravity Sketch CLI")?;
    let stderr = child
        .stderr
        .take()
        .context("failed to capture stderr from Gravity Sketch CLI")?;

    let stdout_handle = {
        let outgoing = outgoing.clone();
        let request_id = request_id.clone();
        task::spawn(async move { drain_stream(stdout, "stdout", outgoing, request_id).await })
    };
    let stderr_handle = {
        let outgoing = outgoing.clone();
        let request_id = request_id.clone();
        task::spawn(async move { drain_stream(stderr, "stderr", outgoing, request_id).await })
    };

    let status = child
        .wait()
        .await
        .context("failed to wait for Gravity Sketch CLI")?;
    let stdout_output = stdout_handle.await??;
    let stderr_output = stderr_handle.await??;

    let summary_text = if status.success() {
        if stdout_output.trim().is_empty() {
            format!(
                "Gravity Sketch action {} completed successfully.",
                params.action.as_display_name()
            )
        } else {
            stdout_output.clone()
        }
    } else {
        let mut msg = format!(
            "Gravity Sketch action {} failed with status {}.",
            params.action.as_display_name(),
            status
        );
        if !stderr_output.trim().is_empty() {
            msg.push_str("\nstderr:\n");
            msg.push_str(&stderr_output);
        } else if !stdout_output.trim().is_empty() {
            msg.push_str("\nstdout:\n");
            msg.push_str(&stdout_output);
        }
        msg
    };

    let structured_content = json!({
        "action": params.action.as_display_name(),
        "success": status.success(),
        "status_code": status.code(),
        "stdout": stdout_output,
        "stderr": stderr_output,
    });

    Ok(CallToolResult {
        content: vec![ContentBlock::TextContent(TextContent {
            r#type: "text".to_owned(),
            text: summary_text,
            annotations: None,
        })],
        is_error: Some(!status.success()),
        structured_content: Some(structured_content),
    })
}

async fn drain_stream<R>(
    reader: R,
    stream: &'static str,
    outgoing: Arc<OutgoingMessageSender>,
    request_id: RequestId,
) -> Result<String>
where
    R: tokio::io::AsyncRead + Unpin + Send + 'static,
{
    let mut reader = BufReader::new(reader).lines();
    let mut lines = Vec::new();

    while let Some(line) = reader.next_line().await? {
        if !line.is_empty() {
            send_stream_notification(&outgoing, &request_id, stream, &line).await;
        }
        lines.push(line);
    }

    Ok(lines.join("\n"))
}

async fn send_stream_notification(
    outgoing: &Arc<OutgoingMessageSender>,
    request_id: &RequestId,
    stream: &str,
    line: &str,
) {
    let params = match serde_json::to_value(OutgoingNotificationParams {
        meta: Some(OutgoingNotificationMeta::new(Some(request_id.clone()))),
        event: json!({
            "type": "stream",
            "stream": stream,
            "line": line,
        }),
    }) {
        Ok(value) => value,
        Err(err) => {
            error!("failed to serialize gravity-sketch notification payload: {err}");
            return;
        }
    };

    outgoing
        .send_notification(OutgoingNotification {
            method: "gravity-sketch/event".to_string(),
            params: Some(params),
        })
        .await;
}
