use std::sync::atomic::Ordering;

use nu_plugin::{EngineInterface, EvaluatedCall, SimplePluginCommand};
use nu_protocol::{Category, Example, LabeledError, Signature, Span, SyntaxShape, Type, Value};
use tracing::level_filters::LevelFilter;

use crate::{
    descriptor::parse_tools,
    logging,
    plugin::McpPlugin,
    server::{ServerConfig, TransportKind, run_server},
};

/// Starts an MCP server backed by a list of Nushell closure descriptors.
pub struct McpCommand;

impl SimplePluginCommand for McpCommand {
    type Plugin = McpPlugin;

    /// Returns the root Nushell command name.
    fn name(&self) -> &'static str {
        "mcp"
    }

    /// Defines the supported descriptor inputs, transports, and server metadata flags.
    fn signature(&self) -> Signature {
        Signature::build(self.name())
            .optional(
                "tools",
                SyntaxShape::List(Box::new(SyntaxShape::Record(vec![].into()))),
                "list of records created by `mcp tool`",
            )
            .named(
                "transport",
                SyntaxShape::String,
                "MCP transport: stdio or http",
                None,
            )
            .named("host", SyntaxShape::String, "HTTP bind host", None)
            .named("port", SyntaxShape::Int, "HTTP bind port", None)
            .named(
                "log-level",
                SyntaxShape::String,
                "log level: trace, debug, info, warn, error, or off (default: info)",
                None,
            )
            .named(
                "path",
                SyntaxShape::String,
                "Streamable HTTP route path",
                None,
            )
            .named(
                "name",
                SyntaxShape::String,
                "MCP server implementation name",
                None,
            )
            .named(
                "server-version",
                SyntaxShape::String,
                "MCP server implementation version",
                None,
            )
            .named(
                "instructions",
                SyntaxShape::String,
                "instructions advertised to MCP clients",
                None,
            )
            .input_output_types(vec![
                (Type::Nothing, Type::Nothing),
                (Type::List(Type::Any.into()), Type::Nothing),
            ])
            .category(Category::Network)
    }

    /// Describes the foreground server lifecycle.
    fn description(&self) -> &'static str {
        "Run an MCP server whose tools are Nushell closures"
    }

    /// Explains why the command remains active while closures are available.
    fn extra_description(&self) -> &'static str {
        "The command stays in the foreground for the server lifetime. Stdio requires Nushell to communicate with this plugin over a local socket."
    }

    /// Shows positional stdio and piped HTTP invocation styles.
    fn examples(&self) -> Vec<Example<'_>> {
        vec![
            Example {
                example: "mcp [$tool]",
                description: "Run a stdio MCP server",
                result: None,
            },
            Example {
                example: "$tools | mcp --transport http --port 8080 --path /mcp",
                description: "Run a Streamable HTTP MCP server",
                result: None,
            },
        ]
    }

    /// Resolves tools and flags, then blocks until the selected MCP transport stops.
    fn run(
        &self,
        plugin: &Self::Plugin,
        engine: &EngineInterface,
        call: &EvaluatedCall,
        input: &Value,
    ) -> Result<Value, LabeledError> {
        let descriptors = resolve_descriptors(call, input)?;
        let tools = parse_tools(&descriptors)?;
        let config = parse_config(call)?;
        logging::set_level(config.log_level)
            .map_err(|error| labeled("Failed to set log level", error, call.head))?;

        if config.transport == TransportKind::Stdio && engine.is_using_stdio() {
            return Err(labeled(
                "Stdio is unavailable",
                "Nushell is using this plugin's stdio; use a local-socket-capable plugin registration or `--transport http`",
                call.head,
            ));
        }

        let _stdio_guard = if config.transport == TransportKind::Stdio {
            Some(StdioGuard::acquire(plugin, call.head)?)
        } else {
            None
        };
        warn_for_remote_http(&config);

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .enable_all()
            .build()
            .map_err(|error| labeled("Failed to start Tokio", error.to_string(), call.head))?;
        runtime
            .block_on(run_server(config, engine.clone(), tools))
            .map_err(|error| labeled("MCP server failed", error, call.head))?;
        Ok(Value::nothing(call.head))
    }
}

/// RAII guard that ensures only one server consumes process stdio at a time.
struct StdioGuard<'a> {
    plugin: &'a McpPlugin,
}

impl<'a> StdioGuard<'a> {
    /// Atomically acquires the process-wide stdio transport.
    fn acquire(plugin: &'a McpPlugin, span: Span) -> Result<Self, LabeledError> {
        plugin
            .stdio_active
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map_err(|_| {
                labeled(
                    "Stdio MCP server already running",
                    "only one stdio server can run in a plugin process",
                    span,
                )
            })?;
        Ok(Self { plugin })
    }
}

impl Drop for StdioGuard<'_> {
    /// Releases stdio when the server finishes or startup fails.
    fn drop(&mut self) {
        self.plugin.stdio_active.store(false, Ordering::Release);
    }
}

/// Chooses exactly one descriptor source from the positional argument or pipeline.
fn resolve_descriptors(call: &EvaluatedCall, input: &Value) -> Result<Value, LabeledError> {
    let positional = call.opt::<Value>(0)?;
    let piped = (!matches!(input, Value::Nothing { .. })).then(|| input.clone());
    match (positional, piped) {
        (Some(_), Some(_)) => Err(labeled(
            "Tools were provided twice",
            "pass tools either positionally or through the pipeline",
            call.head,
        )),
        (Some(value), None) | (None, Some(value)) => Ok(value),
        (None, None) => Err(labeled(
            "Missing MCP tools",
            "pass a list of descriptors or pipe one into `mcp`",
            call.head,
        )),
    }
}

/// Parses command flags and applies the documented server defaults.
fn parse_config(call: &EvaluatedCall) -> Result<ServerConfig, LabeledError> {
    let transport = match call
        .get_flag::<String>("transport")?
        .as_deref()
        .unwrap_or("stdio")
    {
        "stdio" => TransportKind::Stdio,
        "http" => TransportKind::Http,
        value => {
            return Err(labeled(
                "Unknown MCP transport",
                format!("expected `stdio` or `http`, found `{value}`"),
                call.get_flag_span("transport").unwrap_or(call.head),
            ));
        }
    };
    let port = call.get_flag::<i64>("port")?.unwrap_or(8080);
    let port = u16::try_from(port).map_err(|_| {
        labeled(
            "Invalid HTTP port",
            "port must be between 0 and 65535",
            call.get_flag_span("port").unwrap_or(call.head),
        )
    })?;
    let path = call
        .get_flag::<String>("path")?
        .unwrap_or_else(|| "/mcp".into());
    if !path.starts_with('/') || path.contains(['?', '#']) {
        return Err(labeled(
            "Invalid HTTP path",
            "path must start with `/` and cannot contain a query or fragment",
            call.get_flag_span("path").unwrap_or(call.head),
        ));
    }

    Ok(ServerConfig {
        transport,
        log_level: parse_log_level(
            call.get_flag::<String>("log-level")?.as_deref(),
            call.get_flag_span("log-level").unwrap_or(call.head),
        )?,
        host: call
            .get_flag::<String>("host")?
            .unwrap_or_else(|| "127.0.0.1".into()),
        port,
        path,
        name: call
            .get_flag::<String>("name")?
            .unwrap_or_else(|| "nu-mcp".into()),
        version: call
            .get_flag::<String>("server-version")?
            .unwrap_or_else(|| env!("CARGO_PKG_VERSION").into()),
        instructions: call.get_flag("instructions")?,
    })
}

/// Parses a tracing level while applying the command's info-level default.
fn parse_log_level(value: Option<&str>, span: Span) -> Result<LevelFilter, LabeledError> {
    let value = value.unwrap_or("info");
    value.parse().map_err(|_| {
        labeled(
            "Invalid log level",
            format!(
                "expected `trace`, `debug`, `info`, `warn`, `error`, or `off`, found `{value}`"
            ),
            span,
        )
    })
}

/// Warns when unauthenticated HTTP is intentionally bound beyond loopback.
fn warn_for_remote_http(config: &ServerConfig) {
    if config.transport == TransportKind::Http
        && !matches!(config.host.as_str(), "127.0.0.1" | "::1" | "localhost")
    {
        tracing::warn!(
            host = %config.host,
            "HTTP MCP is binding beyond loopback without authentication or TLS"
        );
    }
}

/// Creates a consistently labeled command error.
fn labeled(title: impl Into<String>, label: impl Into<String>, span: Span) -> LabeledError {
    LabeledError::new(title).with_label(label, span)
}

#[cfg(test)]
mod tests {
    use nu_plugin_test_support::PluginTest;

    use super::parse_log_level;
    use crate::plugin::McpPlugin;

    /// Evaluates root-command validation through Nushell's real parser and test engine.
    #[test]
    fn rejects_unknown_transport() {
        let error = PluginTest::new("mcp", McpPlugin::default().into())
            .expect("plugin test should initialize")
            .eval("[] | mcp --transport websocket")
            .expect_err("unknown transport should fail");
        assert!(error.to_string().contains("Unknown MCP transport"));
    }

    /// Verifies that omitting the flag selects info-level logging.
    #[test]
    fn defaults_log_level_to_info() {
        assert_eq!(
            parse_log_level(None, nu_protocol::Span::unknown()).expect("default should parse"),
            tracing::level_filters::LevelFilter::INFO
        );
    }

    /// Evaluates log-level validation through Nushell's real parser and test engine.
    #[test]
    fn rejects_unknown_log_level() {
        let error = PluginTest::new("mcp", McpPlugin::default().into())
            .expect("plugin test should initialize")
            .eval("[] | mcp --log-level verbose")
            .expect_err("unknown log level should fail");
        assert!(error.to_string().contains("Invalid log level"));
    }
}
