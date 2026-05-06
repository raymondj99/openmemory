//! End-to-end MCP test. Spawns the real `openmemory mcp` binary as a
//! subprocess, talks to it over stdio JSON-RPC, and exercises every tool.
//!
//! This is the closest thing we have to "an OpenClaw agent calling our
//! server" — it boots through `main`, opens a real SQLite database in a
//! tempdir, registers every tool from the production registry, and
//! verifies each one round-trips.
//!
//! Runtime budget: < 10 seconds on a developer laptop.

use std::io::{BufRead, BufReader, Write};
use std::path::Path;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};
use std::time::{Duration, Instant};

use serde_json::{json, Value};

const STARTUP_DEADLINE: Duration = Duration::from_secs(10);
const READ_DEADLINE: Duration = Duration::from_secs(10);

/// Locate the `openmemory` binary cargo just built. Cargo sets
/// `CARGO_BIN_EXE_<name>` for the package's [[bin]] target during tests;
/// fall back to walking up to `target/debug/openmemory` if the env var
/// is missing (so direct `rustc` runs still work).
fn binary_path() -> std::path::PathBuf {
    if let Ok(raw) = std::env::var("CARGO_BIN_EXE_openmemory") {
        return std::path::PathBuf::from(raw);
    }
    let mut path = std::env::current_exe().expect("current_exe");
    // current_exe is target/debug/deps/<test-binary>; back up to debug/.
    path.pop();
    if path.ends_with("deps") {
        path.pop();
    }
    let exe_name = if cfg!(windows) {
        "openmemory.exe"
    } else {
        "openmemory"
    };
    path.push(exe_name);
    assert!(
        path.exists(),
        "could not locate openmemory binary (env var CARGO_BIN_EXE_openmemory \
         missing and {} doesn't exist)",
        path.display()
    );
    path
}

struct ServerHandle {
    child: Child,
    stdin: ChildStdin,
    reader: BufReader<ChildStdout>,
    next_id: i64,
    _home: tempfile::TempDir,
}

impl ServerHandle {
    fn spawn(home: &Path) -> Self {
        let home_dir = tempfile::tempdir_in(home).expect("tempdir under home");
        let mut child = Command::new(binary_path())
            .args(["--home", home_dir.path().to_str().unwrap(), "mcp"])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .env("OPENMEMORY_HOME", home_dir.path())
            .spawn()
            .expect("spawn openmemory mcp");
        let stdin = child.stdin.take().unwrap();
        let stdout = child.stdout.take().unwrap();
        let reader = BufReader::new(stdout);
        Self {
            child,
            stdin,
            reader,
            next_id: 1,
            _home: home_dir,
        }
    }

    fn next_id(&mut self) -> i64 {
        let id = self.next_id;
        self.next_id += 1;
        id
    }

    fn send(&mut self, method: &str, params: Value) -> Value {
        let id = self.next_id();
        let msg = json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        let mut s = serde_json::to_string(&msg).unwrap();
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).expect("write to stdin");
        self.stdin.flush().expect("flush stdin");

        let started = Instant::now();
        let mut line = String::new();
        loop {
            line.clear();
            let n = self.reader.read_line(&mut line).expect("read line");
            if n > 0 {
                break;
            }
            assert!(
                started.elapsed() <= READ_DEADLINE,
                "timed out waiting for response to {method}",
            );
        }
        let response: Value = serde_json::from_str(line.trim())
            .unwrap_or_else(|e| panic!("parse response {line:?}: {e}"));
        response
    }

    fn notify(&mut self, method: &str, params: Value) {
        let msg = json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        let mut s = serde_json::to_string(&msg).unwrap();
        s.push('\n');
        self.stdin.write_all(s.as_bytes()).expect("write notify");
        self.stdin.flush().expect("flush notify");
    }

    fn call_tool(&mut self, name: &str, arguments: Value) -> Value {
        self.send("tools/call", json!({"name": name, "arguments": arguments}))
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn tool_text(response: &Value) -> String {
    let content = response
        .pointer("/result/content/0/text")
        .unwrap_or(&Value::Null);
    content.as_str().unwrap_or_default().to_string()
}

#[test]
fn e2e_initialize_tools_list_and_each_tool() {
    let started = Instant::now();
    let outer_home = tempfile::tempdir().unwrap();
    let mut server = ServerHandle::spawn(outer_home.path());

    // 1. initialize
    let resp = server.send("initialize", json!({}));
    assert!(
        resp.pointer("/result/serverInfo/name").unwrap().as_str() == Some("openmemory"),
        "initialize: {resp}",
    );
    server.notify("notifications/initialized", json!({}));

    // 2. tools/list — verify all 11 tools present
    let resp = server.send("tools/list", json!({}));
    let tools = resp
        .pointer("/result/tools")
        .and_then(|v| v.as_array())
        .unwrap_or_else(|| panic!("no /result/tools: {resp}"));
    let names: Vec<&str> = tools
        .iter()
        .filter_map(|t| t.get("name").and_then(|v| v.as_str()))
        .collect();
    for expected in [
        "openmemory_remember",
        "openmemory_recall",
        "openmemory_list_entities",
        "openmemory_get_entity",
        "openmemory_forget",
        "openmemory_forget_entity",
        "openmemory_status",
        "openmemory_index_text",
        "openmemory_search",
        "openmemory_delete",
        "openmemory_consolidate",
    ] {
        assert!(names.contains(&expected), "missing {expected}: {names:?}");
    }
    assert_eq!(names.len(), 11);

    // 3. openmemory_status (read) — zero counts on a fresh store
    let resp = server.call_tool("openmemory_status", json!({}));
    let body = tool_text(&resp);
    assert!(body.contains("\"total_entities\": 0"));

    // 4. openmemory_remember — write a real entity
    let resp = server.call_tool(
        "openmemory_remember",
        json!({
            "entity": "Raymond",
            "entity_type": "person",
            "observations": ["prefers Rust"],
        }),
    );
    let body = tool_text(&resp);
    assert!(body.contains("\"entity_existed\": false"));
    let observation_id = serde_json::from_str::<Value>(&body)
        .unwrap()
        .pointer("/observation_ids/0")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .expect("observation_id");

    // 5. openmemory_recall (keyword) — should find it
    let resp = server.call_tool(
        "openmemory_recall",
        json!({"query": "Rust", "mode": "keyword", "limit": 5}),
    );
    let body = tool_text(&resp);
    assert!(body.contains("Raymond"));
    assert!(body.contains("Rust"));

    // 6. openmemory_get_entity
    let resp = server.call_tool("openmemory_get_entity", json!({"entity": "Raymond"}));
    let body = tool_text(&resp);
    assert!(body.contains("\"found\": true"));
    assert!(body.contains("prefers Rust"));

    // 7. openmemory_list_entities
    let resp = server.call_tool("openmemory_list_entities", json!({}));
    let body = tool_text(&resp);
    assert!(body.contains("\"name\": \"Raymond\""));

    // 8. openmemory_index_text + openmemory_search + openmemory_delete
    let _ = server.call_tool(
        "openmemory_index_text",
        json!({"uri": "note://hello", "text": "hello world from e2e"}),
    );
    let resp = server.call_tool(
        "openmemory_search",
        json!({"query": "hello", "mode": "keyword", "limit": 5}),
    );
    let body = tool_text(&resp);
    assert!(body.contains("note://hello"));

    let resp = server.call_tool("openmemory_delete", json!({"uri": "note://hello"}));
    let body = tool_text(&resp);
    assert!(body.contains("\"removed\": 1"));

    // 9. openmemory_consolidate (idempotent on this size)
    let resp = server.call_tool("openmemory_consolidate", json!({}));
    let body = tool_text(&resp);
    assert!(body.contains("\"duplicates_merged\""));

    // 10. openmemory_forget — soft-delete the observation
    let resp = server.call_tool(
        "openmemory_forget",
        json!({"observation_id": observation_id}),
    );
    let body = tool_text(&resp);
    assert!(body.contains("\"modified\": true"));

    // 11. openmemory_forget_entity — hard-delete Raymond
    let resp = server.call_tool("openmemory_forget_entity", json!({"entity": "Raymond"}));
    let body = tool_text(&resp);
    assert!(body.contains("\"observations_removed\""));

    let elapsed = started.elapsed();
    assert!(
        elapsed < STARTUP_DEADLINE * 2,
        "e2e exceeded budget: {elapsed:?}"
    );
}

#[test]
fn e2e_invalid_method_returns_minus_32601() {
    let outer_home = tempfile::tempdir().unwrap();
    let mut server = ServerHandle::spawn(outer_home.path());
    server.send("initialize", json!({}));
    let resp = server.send("nonexistent/method", json!({}));
    assert_eq!(
        resp.pointer("/error/code").and_then(|v| v.as_i64()),
        Some(-32601)
    );
}

#[test]
fn e2e_invalid_tool_input_returns_invalid_params() {
    let outer_home = tempfile::tempdir().unwrap();
    let mut server = ServerHandle::spawn(outer_home.path());
    server.send("initialize", json!({}));
    let resp = server.call_tool(
        "openmemory_remember",
        json!({"entity": "", "observations": ["x"]}),
    );
    assert_eq!(
        resp.pointer("/error/code").and_then(|v| v.as_i64()),
        Some(-32602)
    );
}
