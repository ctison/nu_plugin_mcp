use std::{sync::Arc, time::Duration};

use async_trait::async_trait;
use nu_plugin::EngineInterface;
use nu_protocol::{Signals, Value};
use rust_mcp_axum::{AxumServerOptions, create_axum_server};
use rust_mcp_sdk::{
    McpServer, StdioTransport, ToMcpServerHandler, TransportOptions,
    error::SdkResult,
    mcp_server::{McpServerOptions, ServerHandler, server_runtime},
    schema::{
        CallToolRequestParams, CallToolResult, Implementation, InitializeResult, ListToolsResult,
        PaginatedRequestParams, ProtocolVersion, RpcError, ServerCapabilities,
        ServerCapabilitiesTools, schema_utils::CallToolError,
    },
};
use tracing::level_filters::LevelFilter;

use crate::{
    convert::{json_to_nu, nu_to_json},
    descriptor::NuTool,
};

const INTERRUPT_POLL_INTERVAL: Duration = Duration::from_millis(25);
const HTTP_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

/// Transport selected for a running MCP server.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum TransportKind {
    Stdio,
    Http,
}

/// User-configurable MCP server settings parsed from command flags.
pub struct ServerConfig {
    pub transport: TransportKind,
    pub log_level: LevelFilter,
    pub host: String,
    pub port: u16,
    pub path: String,
    pub name: String,
    pub version: String,
    pub instructions: Option<String>,
}

/// MCP handler that dispatches each tool call back into the originating Nushell engine.
struct NuServerHandler {
    engine: EngineInterface,
    tools: Arc<Vec<NuTool>>,
}

#[async_trait]
impl ServerHandler for NuServerHandler {
    /// Returns all tools declared when the command started.
    async fn handle_list_tools_request(
        &self,
        _params: Option<PaginatedRequestParams>,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<ListToolsResult, RpcError> {
        Ok(ListToolsResult {
            tools: self.tools.iter().map(|tool| tool.tool.clone()).collect(),
            meta: None,
            next_cursor: None,
        })
    }

    /// Validates MCP arguments, evaluates the matching closure, and maps its result.
    async fn handle_call_tool_request(
        &self,
        params: CallToolRequestParams,
        _runtime: Arc<dyn McpServer>,
    ) -> Result<CallToolResult, CallToolError> {
        let Some(tool) = self.tools.iter().find(|tool| tool.tool.name == params.name) else {
            return Err(CallToolError::unknown_tool(params.name));
        };
        let arguments = serde_json::Value::Object(params.arguments.unwrap_or_default());
        if let Err(error) = tool.input_validator.validate(&arguments) {
            return Ok(tool_error(format!("Invalid tool arguments: {error}")));
        }
        let arguments = match json_to_nu(&arguments, tool.closure.span) {
            Ok(arguments) => arguments,
            Err(error) => return Ok(tool_error(format!("Invalid tool arguments: {error}"))),
        };

        let engine = self.engine.clone();
        let closure = tool.closure.clone();
        let evaluation = tokio::task::spawn_blocking(move || {
            engine
                .eval_closure(&closure, vec![arguments], None)
                .map_err(|error| error.to_string())
        })
        .await;
        let value = match evaluation {
            Ok(Ok(value)) => value,
            Ok(Err(error)) => return Ok(tool_error(error)),
            Err(error) => return Ok(tool_error(format!("Closure evaluation failed: {error}"))),
        };

        Ok(map_tool_result(value, tool.output_validator.as_deref()))
    }
}

/// Starts the requested transport and waits until it shuts down.
pub async fn run_server(
    config: ServerConfig,
    engine: EngineInterface,
    tools: Vec<NuTool>,
) -> Result<(), String> {
    let details = server_details(&config);
    let signals = engine.signals().clone();
    let handler = NuServerHandler {
        engine,
        tools: Arc::new(tools),
    }
    .to_mcp_server_handler();

    match config.transport {
        TransportKind::Stdio => run_stdio(details, handler, &signals).await,
        TransportKind::Http => run_http(config, details, handler, &signals).await,
    }
    .map_err(|error| error.to_string())
}

/// Runs a newline-delimited MCP server on the plugin process's inherited stdio.
async fn run_stdio(
    details: InitializeResult,
    handler: Arc<dyn rust_mcp_sdk::McpServerHandler>,
    signals: &Signals,
) -> SdkResult<()> {
    let transport = StdioTransport::new(TransportOptions::default())?;
    let server = server_runtime::create_server(McpServerOptions {
        transport,
        handler,
        server_details: details,
        task_store: None,
        client_task_store: None,
        message_observer: None,
    });
    tokio::select! {
        result = server.start() => result,
        () = wait_for_interrupt(signals) => Ok(()),
    }
}

/// Runs Streamable HTTP while explicitly disabling the legacy SSE endpoints.
async fn run_http(
    config: ServerConfig,
    details: InitializeResult,
    handler: Arc<dyn rust_mcp_sdk::McpServerHandler>,
    signals: &Signals,
) -> SdkResult<()> {
    let server = create_axum_server(
        details,
        handler,
        AxumServerOptions {
            host: config.host,
            port: config.port,
            custom_streamable_http_endpoint: Some(config.path),
            sse_support: false,
            ..Default::default()
        },
    );
    let handle = server.server_handle();
    let running = server.start();
    tokio::pin!(running);

    tokio::select! {
        result = &mut running => result,
        () = wait_for_interrupt(signals) => {
            handle.graceful_shutdown(Some(HTTP_SHUTDOWN_TIMEOUT));
            running.await
        },
    }
}

/// Waits asynchronously until Nushell marks the active plugin call as interrupted.
async fn wait_for_interrupt(signals: &Signals) {
    while !signals.interrupted() {
        tokio::time::sleep(INTERRUPT_POLL_INTERVAL).await;
    }
}

/// Builds the MCP initialization response from command-level server metadata.
fn server_details(config: &ServerConfig) -> InitializeResult {
    InitializeResult {
        server_info: Implementation {
            name: config.name.clone(),
            version: config.version.clone(),
            title: None,
            description: None,
            icons: vec![],
            website_url: None,
        },
        capabilities: ServerCapabilities {
            tools: Some(ServerCapabilitiesTools {
                list_changed: Some(false),
            }),
            ..Default::default()
        },
        protocol_version: ProtocolVersion::V2025_11_25.into(),
        instructions: config.instructions.clone(),
        meta: None,
    }
}

/// Maps a successful Nushell value into MCP text and optional structured content.
fn map_tool_result(
    value: Value,
    output_validator: Option<&jsonschema::Validator>,
) -> CallToolResult {
    let json = match nu_to_json(&value) {
        Ok(json) => json,
        Err(error) => return tool_error(error),
    };
    if let Some(validator) = output_validator
        && let Err(error) = validator.validate(&json)
    {
        return tool_error(format!("Tool output does not match output_schema: {error}"));
    }

    if let serde_json::Value::Object(object) = json {
        let text = serde_json::to_string(&object).expect("JSON objects always serialize");
        return CallToolResult::text_content(vec![text.into()]).with_structured_content(object);
    }
    if let Value::String { val, .. } = value {
        return CallToolResult::text_content(vec![val.into()]);
    }
    let text = serde_json::to_string(&json).expect("JSON values always serialize");
    CallToolResult::text_content(vec![text.into()])
}

/// Creates an MCP-visible tool execution error instead of a protocol-level error.
fn tool_error(message: impl Into<String>) -> CallToolResult {
    let mut result = CallToolResult::text_content(vec![message.into().into()]);
    result.is_error = Some(true);
    result
}

#[cfg(test)]
mod tests {
    use nu_protocol::{Record, Span, Value};
    use serde_json::json;

    use super::map_tool_result;

    /// Maps records to both structured content and backward-compatible JSON text.
    #[test]
    fn maps_record_results() {
        let span = Span::test_data();
        let mut record = Record::new();
        record.push("answer", Value::int(42, span));
        let result = map_tool_result(Value::record(record, span), None);
        assert_eq!(
            result.structured_content,
            Some(json!({"answer": 42}).as_object().unwrap().clone())
        );
        assert_eq!(result.is_error, None);
    }

    /// Preserves strings as human-readable text rather than quoted JSON.
    #[test]
    fn maps_string_results() {
        let result = map_tool_result(Value::test_string("hello"), None);
        assert_eq!(result.is_error, None);
        assert!(result.structured_content.is_none());
        assert_eq!(
            serde_json::to_value(result.content).unwrap()[0]["text"],
            "hello"
        );
    }

    /// Reports output-schema failures as tool execution errors.
    #[test]
    fn validates_output_schema() {
        let validator = jsonschema::draft202012::new(&json!({
            "type": "object",
            "required": ["answer"]
        }))
        .unwrap();
        let result = map_tool_result(Value::test_string("hello"), Some(&validator));
        assert_eq!(result.is_error, Some(true));
    }
}
