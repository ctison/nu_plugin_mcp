# nu_plugin_mcp

`nu_plugin_mcp` runs Model Context Protocol servers whose tools are ordinary
Nushell closures. It supports MCP 2025-11-25 over stdio and Streamable HTTP.

## Build and register

The plugin targets Nushell 0.114.1 and Rust 1.95 or newer.

```nu
cargo build --release
plugin add target/release/nu_plugin_mcp
plugin use mcp
```

For a script or MCP client, `nu --plugins` loads the executable without editing
the user's plugin registry:

```nu
nu --plugins /absolute/path/to/nu_plugin_mcp server.nu
```

## Define tools

`mcp tool` creates the descriptor accepted by `mcp`. The closure receives the
MCP arguments as one positional record.

```nu
let prefix = "Hello"
let greet = mcp tool greet {|args|
  $"($prefix), ($args.name)!"
} --title "Greeter" --description "Greet a person" --input-schema {
  type: object
  properties: {
    name: {type: string}
  }
  required: [name]
  additionalProperties: false
} --annotations {
  read_only_hint: true
  idempotent_hint: true
  open_world_hint: false
}
```

The `input_schema` default is `{type: object, additionalProperties: true}`.
`output_schema` has no default. When supplied, it must have `type: object`, and
the closure must return a matching record.

Manual descriptor records may contain `name`, `closure`, `title`, `description`,
`input_schema`, `output_schema`, and `annotations`. Unknown fields and duplicate
tool names are rejected before the server starts.

## Stdio server

stdio is the default transport:

```nu
mcp [$greet]
```

The MCP client launches Nushell, which in turn launches the plugin. A client
configuration can therefore point at a Nushell script containing the tool
definitions and final `mcp` command:

```json
{
  "command": "nu",
  "args": [
    "--no-config-file",
    "--plugins",
    "/absolute/path/to/nu_plugin_mcp",
    "/absolute/path/to/server.nu"
  ]
}
```

Nushell must run the plugin protocol over its local socket so the inherited
stdin and stdout remain available to MCP. The command detects an incompatible
stdio plugin connection and reports an error.

## Streamable HTTP server

Descriptors can also arrive through the pipeline:

```nu
[$greet] | mcp --transport http --host 127.0.0.1 --port 8080 --path /mcp
```

The MCP URL is `http://127.0.0.1:8080/mcp`. The command stays in the foreground;
press Ctrl-C to shut down the server, or use Nushell jobs when you intentionally
want background execution.

Set `--log-level` to `trace`, `debug`, `info`, `warn`, `error`, or `off` to
control stderr logging. It defaults to `info`.

HTTP has no authentication or TLS in this version. Binding `--host` beyond
loopback emits a warning, but it still exposes every closure to network clients.
Only use a non-loopback host on a trusted, separately protected network.

## Results and errors

- Strings become MCP text content.
- Records become `structuredContent` and also JSON text for older clients.
- Other JSON-compatible Nushell values become JSON text.
- Schema failures and closure errors are returned with `isError: true`.
- Unknown tool names are returned as MCP protocol errors.

Schemas use JSON Schema 2020-12. Local references work; HTTP and filesystem
reference resolution is disabled.

## Development

Run the complete repository check through Nushell:

```nu
nu scripts/check.nu
```

Tracing is written only to stderr. The default level is `info`; use
`--log-level off` to disable it entirely.
