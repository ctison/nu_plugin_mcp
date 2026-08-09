use std::sync::atomic::AtomicBool;

use nu_plugin::{Plugin, PluginCommand};

use crate::commands::{McpCommand, McpToolCommand};

/// Shared plugin state used to guard the process-wide stdio transport.
#[derive(Default)]
pub struct McpPlugin {
  pub(crate) stdio_active: AtomicBool,
}

impl Plugin for McpPlugin {
  /// Reports the Cargo package version to Nushell.
  fn version(&self) -> String {
    env!("CARGO_PKG_VERSION").into()
  }

  /// Registers the MCP server and tool-constructor commands.
  fn commands(&self) -> Vec<Box<dyn PluginCommand<Plugin = Self>>> {
    vec![Box::new(McpCommand), Box::new(McpToolCommand)]
  }
}
