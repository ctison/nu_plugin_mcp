use std::{
    io::{BufRead, BufReader, Write},
    process::{Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

/// Exercises initialization, discovery, and closure evaluation over real stdio transport.
#[test]
fn invokes_captured_nushell_closure_over_stdio() {
    let plugin = env!("CARGO_BIN_EXE_nu_plugin_mcp");
    let source = r#"
        let prefix = "Hello"
        let greet = mcp tool greet {|args| $"($prefix), ($args.name)!" } --description "Greet someone" --input-schema {type: object, properties: {name: {type: string}}, required: [name]}
        mcp [$greet]
    "#;
    let mut child = Command::new("nu")
        .args([
            "--no-config-file",
            "--no-std-lib",
            "--plugins",
            plugin,
            "-c",
            source,
        ])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Nushell should start");
    let mut input = child.stdin.take().expect("stdin should be piped");
    let output = child.stdout.take().expect("stdout should be piped");
    let mut output = BufReader::new(output);

    send(
        &mut input,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "integration-test", "version": "1.0.0"}
            }
        }),
    );
    let initialized = receive(&mut output);
    assert_eq!(initialized["id"], 1);
    assert_eq!(initialized["result"]["protocolVersion"], "2025-11-25");

    send(
        &mut input,
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    send(
        &mut input,
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let listed = receive(&mut output);
    assert_eq!(listed["result"]["tools"][0]["name"], "greet");

    send(
        &mut input,
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "greet", "arguments": {"name": "Ada"}}
        }),
    );
    let called = receive(&mut output);
    assert_eq!(called["result"]["content"][0]["text"], "Hello, Ada!");
    assert_ne!(called["result"]["isError"], true);

    #[cfg(unix)]
    interrupt(&child);
    drop(input);
    drop(output);
    #[cfg(unix)]
    wait_for_interrupted_exit(&mut child);
    #[cfg(not(unix))]
    {
        let result = child.wait_with_output().expect("Nushell should exit");
        assert!(
            result.status.success(),
            "Nushell failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );
    }
}

/// Sends SIGINT to the Nushell process that owns the active plugin call.
#[cfg(unix)]
fn interrupt(child: &std::process::Child) {
    let status = Command::new("kill")
        .args(["-INT", &child.id().to_string()])
        .status()
        .expect("kill should start");
    assert!(status.success(), "SIGINT should be delivered");
}

/// Verifies that stdio cancellation lets Nushell exit without an external kill.
#[cfg(unix)]
fn wait_for_interrupted_exit(child: &mut std::process::Child) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if child
            .try_wait()
            .expect("child status should read")
            .is_some()
        {
            return;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("Nushell did not exit after SIGINT");
        }
        thread::sleep(Duration::from_millis(25));
    }
}

/// Writes one compact newline-delimited JSON-RPC message.
fn send(writer: &mut impl Write, message: &Value) {
    serde_json::to_writer(&mut *writer, message).expect("message should serialize");
    writer.write_all(b"\n").expect("message should write");
    writer.flush().expect("message should flush");
}

/// Reads one newline-delimited JSON-RPC response and rejects stdout pollution.
fn receive(reader: &mut impl BufRead) -> Value {
    let mut line = String::new();
    reader.read_line(&mut line).expect("response should read");
    assert!(!line.is_empty(), "MCP server closed stdout unexpectedly");
    serde_json::from_str(&line)
        .unwrap_or_else(|error| panic!("stdout was not a valid MCP message ({error}): {line:?}"))
}
