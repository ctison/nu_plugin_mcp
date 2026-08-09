use std::{
    collections::HashMap,
    fmt::Write as _,
    io::{Read, Write},
    net::{TcpListener, TcpStream},
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};

use serde_json::{Value, json};

/// Exercises initialization, discovery, and closure evaluation over Streamable HTTP.
#[test]
fn invokes_captured_nushell_closure_over_http() {
    let port = unused_loopback_port();
    let plugin = env!("CARGO_BIN_EXE_nu_plugin_mcp");
    let source = format!(
        r"
            let factor = 3
            let multiply = mcp tool multiply {{|args| {{product: ($args.value * $factor)}} }} --input-schema {{type: object, properties: {{value: {{type: integer}}}}, required: [value]}} --output-schema {{type: object, properties: {{product: {{type: integer}}}}, required: [product]}}
            mcp [$multiply] --transport http --port {port} --path /mcp
        "
    );
    let child = Command::new("nu")
        .args([
            "--no-config-file",
            "--no-std-lib",
            "--plugins",
            plugin,
            "-c",
            &source,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Nushell should start");
    let mut child = ChildGuard::new(child);
    wait_for_http(&mut child, port);

    let initialized = post_json(
        port,
        None,
        &json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "http-integration-test", "version": "1.0.0"}
            }
        }),
    );
    assert_eq!(initialized.status, 200, "{initialized:?}");
    let session = initialized
        .headers
        .get("mcp-session-id")
        .expect("initialize should create a session")
        .clone();
    let body = response_json(&initialized);
    assert_eq!(body["result"]["protocolVersion"], "2025-11-25");

    let notification = post_json(
        port,
        Some(&session),
        &json!({"jsonrpc": "2.0", "method": "notifications/initialized"}),
    );
    assert_eq!(notification.status, 202, "{notification:?}");

    let listed = post_json(
        port,
        Some(&session),
        &json!({"jsonrpc": "2.0", "id": 2, "method": "tools/list"}),
    );
    let body = response_json(&listed);
    assert_eq!(body["result"]["tools"][0]["name"], "multiply");

    let called = post_json(
        port,
        Some(&session),
        &json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {"name": "multiply", "arguments": {"value": 14}}
        }),
    );
    let body = response_json(&called);
    assert_eq!(body["result"]["structuredContent"], json!({"product": 42}));
    assert_eq!(body["result"]["content"][0]["text"], r#"{"product":42}"#);

    #[cfg(unix)]
    interrupt_and_wait(&mut child);
    #[cfg(not(unix))]
    child.terminate();
}

/// Sends SIGINT to Nushell and verifies that the plugin call exits promptly.
#[cfg(unix)]
fn interrupt_and_wait(child: &mut ChildGuard) {
    let process = child.child.as_mut().expect("child is active");
    let status = Command::new("kill")
        .args(["-INT", &process.id().to_string()])
        .status()
        .expect("kill should start");
    assert!(status.success(), "SIGINT should be delivered");

    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if process
            .try_wait()
            .expect("child status should read")
            .is_some()
        {
            child.child.take();
            return;
        }
        assert!(
            Instant::now() < deadline,
            "Nushell did not exit after SIGINT"
        );
        thread::sleep(Duration::from_millis(25));
    }
}

/// Minimal parsed HTTP response used by the transport integration test.
#[derive(Debug)]
struct HttpResponse {
    status: u16,
    headers: HashMap<String, String>,
    body: Vec<u8>,
}

/// Ensures a spawned Nushell process is terminated even when an assertion panics.
struct ChildGuard {
    child: Option<Child>,
}

impl ChildGuard {
    /// Wraps a child process for deterministic cleanup.
    fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    /// Stops the child and waits for it to release the test port.
    fn terminate(&mut self) {
        if let Some(mut child) = self.child.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

impl Drop for ChildGuard {
    /// Cleans up the child when the test exits early.
    fn drop(&mut self) {
        self.terminate();
    }
}

/// Asks the operating system for a currently unused loopback TCP port.
fn unused_loopback_port() -> u16 {
    let listener = TcpListener::bind(("127.0.0.1", 0)).expect("ephemeral port should bind");
    listener
        .local_addr()
        .expect("listener has an address")
        .port()
}

/// Waits for the HTTP listener while surfacing an early Nushell failure.
fn wait_for_http(child: &mut ChildGuard, port: u16) {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if TcpStream::connect(("127.0.0.1", port)).is_ok() {
            return;
        }
        let process = child.child.as_mut().expect("child is active");
        if let Some(status) = process.try_wait().expect("child status should read") {
            let mut stderr = String::new();
            if let Some(mut pipe) = process.stderr.take() {
                pipe.read_to_string(&mut stderr)
                    .expect("stderr should read");
            }
            panic!("HTTP server exited with {status}: {stderr}");
        }
        assert!(
            Instant::now() < deadline,
            "HTTP server did not bind in time"
        );
        thread::sleep(Duration::from_millis(50));
    }
}

/// Sends one MCP JSON-RPC message as a Streamable HTTP POST request.
fn post_json(port: u16, session: Option<&str>, message: &Value) -> HttpResponse {
    let body = serde_json::to_vec(message).expect("request JSON should serialize");
    let mut request = format!(
        "POST /mcp HTTP/1.1\r\nHost: 127.0.0.1:{port}\r\nAccept: application/json, text/event-stream\r\nContent-Type: application/json\r\nMCP-Protocol-Version: 2025-11-25\r\nContent-Length: {}\r\nConnection: close\r\n",
        body.len()
    );
    if let Some(session) = session {
        write!(request, "Mcp-Session-Id: {session}\r\n").expect("session header should format");
    }
    request.push_str("\r\n");

    let mut stream = TcpStream::connect(("127.0.0.1", port)).expect("HTTP server should accept");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("timeout should set");
    stream
        .write_all(request.as_bytes())
        .expect("headers should write");
    stream.write_all(&body).expect("body should write");
    stream.flush().expect("request should flush");
    let mut response = Vec::new();
    stream
        .read_to_end(&mut response)
        .expect("response should read");
    parse_http_response(&response)
}

/// Parses status, headers, and either fixed-length or chunked response bodies.
fn parse_http_response(response: &[u8]) -> HttpResponse {
    let boundary = response
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("HTTP response should contain headers");
    let header_text = std::str::from_utf8(&response[..boundary]).expect("headers should be UTF-8");
    let mut lines = header_text.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse().ok())
        .expect("HTTP status should parse");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(name, value)| (name.to_ascii_lowercase(), value.trim().to_owned()))
        .collect::<HashMap<_, _>>();
    let raw_body = &response[boundary + 4..];
    let body = if headers
        .get("transfer-encoding")
        .is_some_and(|value| value.eq_ignore_ascii_case("chunked"))
    {
        decode_chunked(raw_body)
    } else {
        raw_body.to_vec()
    };
    HttpResponse {
        status,
        headers,
        body,
    }
}

/// Extracts one JSON-RPC response from either JSON or the SDK's SSE response form.
fn response_json(response: &HttpResponse) -> Value {
    if response
        .headers
        .get("content-type")
        .is_some_and(|value| value.starts_with("text/event-stream"))
    {
        let body = std::str::from_utf8(&response.body).expect("SSE body should be UTF-8");
        let data = body
            .lines()
            .find_map(|line| line.strip_prefix("data: "))
            .expect("SSE response should contain data");
        serde_json::from_str(data).expect("SSE data should contain JSON-RPC")
    } else {
        serde_json::from_slice(&response.body)
            .unwrap_or_else(|error| panic!("JSON response failed ({error}): {response:?}"))
    }
}

/// Decodes the simple HTTP/1.1 chunked bodies emitted by the Axum server.
fn decode_chunked(mut input: &[u8]) -> Vec<u8> {
    let mut output = Vec::new();
    loop {
        let boundary = input
            .windows(2)
            .position(|window| window == b"\r\n")
            .expect("chunk size should terminate");
        let size_text =
            std::str::from_utf8(&input[..boundary]).expect("chunk size should be UTF-8");
        let size = usize::from_str_radix(
            size_text.split(';').next().expect("chunk size is present"),
            16,
        )
        .expect("chunk size should parse");
        input = &input[boundary + 2..];
        if size == 0 {
            return output;
        }
        output.extend_from_slice(&input[..size]);
        input = &input[size + 2..];
    }
}
