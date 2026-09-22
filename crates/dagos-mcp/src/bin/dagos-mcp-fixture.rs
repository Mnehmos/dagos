//! A minimal stdio MCP server used by `dagos-mcp`'s tests.
//!
//! It answers `initialize` and serves two tools over two `tools/list` pages. The first argument
//! selects a misbehaviour instead: `--exit` quits at once, `--silent` never answers, and
//! `--garbage` writes a non-JSON line before answering.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

fn main() {
    let mode = std::env::args().nth(1).unwrap_or_default();
    let mut stdout = std::io::stdout();
    match mode.as_str() {
        "--exit" => return,
        "--garbage" => {
            let _ = writeln!(stdout, "this is not JSON-RPC");
        }
        _ => {}
    }
    for line in std::io::stdin().lock().lines() {
        let Ok(line) = line else { break };
        if mode == "--silent" {
            continue;
        }
        let Ok(message) = serde_json::from_str::<Value>(&line) else { continue };
        let Some(id) = message.get("id").cloned() else { continue };
        let reply = match message["method"].as_str() {
            Some("initialize") => json!({"jsonrpc": "2.0", "id": id, "result": {
                "protocolVersion": "2025-06-18",
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "dagos-mcp-fixture", "version": "0"}
            }}),
            Some("tools/list") if message["params"]["cursor"].is_null() => {
                json!({"jsonrpc": "2.0", "id": id, "result": {
                    "tools": [{
                        "name": "read_file",
                        "description": "Read a file from the workspace.",
                        "inputSchema": {"type": "object", "properties": {"path": {"type": "string"}}}
                    }],
                    "nextCursor": "page-2"
                }})
            }
            Some("tools/list") => json!({"jsonrpc": "2.0", "id": id, "result": {
                "tools": [{"name": "search", "description": "Search the workspace.",
                           "inputSchema": {"type": "object"}}]
            }}),
            _ => json!({"jsonrpc": "2.0", "id": id,
                        "error": {"code": -32601, "message": "method not found"}}),
        };
        let _ = writeln!(stdout, "{reply}");
        let _ = stdout.flush();
    }
}
