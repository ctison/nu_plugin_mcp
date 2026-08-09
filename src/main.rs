mod commands;
mod convert;
mod descriptor;
mod logging;
mod plugin;
mod server;

use nu_plugin::{MsgPackSerializer, serve_plugin};
use plugin::McpPlugin;

/// Starts tracing and serves the Nushell plugin protocol.
fn main() {
    logging::initialize();
    serve_plugin(&McpPlugin::default(), MsgPackSerializer);
}
