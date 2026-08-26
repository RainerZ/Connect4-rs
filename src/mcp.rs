//! MCP (Model Context Protocol) stdio server exposing the running Connect4-rs
//! GUI as tools. Talks to the GUI over the local control socket (port 4444).
//! Hand-rolled JSON-RPC 2.0 - small enough not to need a framework.
//!
//! Architecture note: this binary is a thin *forwarder*, not a second
//! engine host. The GUI owns the game, the engine and the socket; the MCP
//! client (e.g. Claude Code) spawns this process per session, and every
//! tool call becomes one socket round trip - so a human watching the GUI
//! and an LLM playing through MCP always see the same live game.

use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

const PORT: u16 = 4444;

fn gui(req: Value) -> Result<Value, String> {
    let mut s = TcpStream::connect(("127.0.0.1", PORT)).map_err(|e| format!("Connect4-rs GUI not reachable on port {PORT} (is it running?): {e}"))?;
    writeln!(s, "{req}").map_err(|e| e.to_string())?;
    let mut line = String::new();
    BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
    serde_json::from_str(&line).map_err(|e| e.to_string())
}

fn tools() -> Value {
    json!([
        {
            "name": "connect4_state",
            "description": "Current Connect4 board. rows[0] is the top row; R=red, Y=yellow, .=empty. Columns are numbered 1..7 left to right.",
            "inputSchema": {"type": "object", "properties": {}}
        },
        {
            "name": "connect4_move",
            "description": "Drop your piece into column 1..7 (only when status is human_to_move). Blocks until the engine has replied and returns the new board including the engine's answer (last_search).",
            "inputSchema": {"type": "object", "properties": {"col": {"type": "integer", "minimum": 1, "maximum": 7}}, "required": ["col"]}
        },
        {
            "name": "connect4_hints",
            "description": "LLM assistance toggle (not used by the engine): with on=true every state includes 'hints' - next free row per column, columns that win immediately, columns that must be blocked, and columns that lose at once. Use to compare play with and without bookkeeping help. Returns the current state.",
            "inputSchema": {"type": "object", "properties": {"on": {"type": "boolean"}}, "required": ["on"]}
        },
        {
            "name": "connect4_new",
            "description": "Start a new game. engine_starts=true lets the engine play red and move first (you are yellow).",
            "inputSchema": {"type": "object", "properties": {"engine_starts": {"type": "boolean", "default": false}}}
        }
    ])
}

fn render(state: &Value) -> String {
    // Pretty text rendering plus the raw JSON for the model.
    let mut s = String::new();
    if let Some(rows) = state.get("rows").and_then(Value::as_array) {
        for r in rows {
            if let Some(r) = r.as_str() {
                s.push_str(&r.chars().map(|c| c.to_string()).collect::<Vec<_>>().join(" "));
                s.push('\n');
            }
        }
        s.push_str("1 2 3 4 5 6 7\n");
    }
    // LLM assistance hints (optional, see hints.rs) as a readable line.
    if let Some(h) = state.get("hints") {
        let list = |k: &str| {
            let v: Vec<String> = h.get(k).and_then(Value::as_array).map(|a| a.iter().filter_map(Value::as_u64).map(|c| c.to_string()).collect()).unwrap_or_default();
            if v.is_empty() { "none".to_string() } else { v.join(",") }
        };
        let rows: Vec<String> = h.get("next_row").and_then(Value::as_array).map(|a| a.iter().map(|r| r.as_u64().map(|r| r.to_string()).unwrap_or_else(|| "-".into())).collect()).unwrap_or_default();
        s.push_str(&format!(
            "hints: next free row per column {}  |  you win now: {}  |  must block: {}  |  losing moves: {}\n",
            rows.join(" "), list("winning_moves"), list("must_block"), list("losing_moves")
        ));
    }
    s.push_str(&serde_json::to_string(state).unwrap());
    s
}

fn call(name: &str, args: &Value) -> Result<Value, String> {
    let req = match name {
        "connect4_state" => json!({"cmd": "state"}),
        "connect4_move" => json!({"cmd": "move", "col": args.get("col").cloned().unwrap_or(Value::Null)}),
        "connect4_new" => json!({"cmd": "new", "engine_starts": args.get("engine_starts").and_then(Value::as_bool).unwrap_or(false)}),
        "connect4_hints" => json!({"cmd": "hints", "on": args.get("on").and_then(Value::as_bool).unwrap_or(false)}),
        _ => return Err(format!("unknown tool {name}")),
    };
    let resp = gui(req)?;
    if let Some(err) = resp.get("error").and_then(Value::as_str) {
        let extra = resp.get("state").map(render).unwrap_or_default();
        return Err(format!("{err}\n{extra}"));
    }
    Ok(resp)
}

fn main() {
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let line = match line { Ok(l) => l, Err(_) => break };
        if line.trim().is_empty() { continue; }
        let msg: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or(json!({}));
        let result: Result<Value, (i64, String)> = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params.get("protocolVersion").and_then(Value::as_str).unwrap_or("2025-06-18"),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "connect4-rs", "version": env!("CARGO_PKG_VERSION")}
            })),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = params.get("name").and_then(Value::as_str).unwrap_or("");
                let args = params.get("arguments").cloned().unwrap_or(json!({}));
                match call(name, &args) {
                    Ok(state) => Ok(json!({"content": [{"type": "text", "text": render(&state)}], "isError": false})),
                    Err(e) => Ok(json!({"content": [{"type": "text", "text": e}], "isError": true})),
                }
            }
            m if m.starts_with("notifications/") => continue, // no response to notifications
            _ => Err((-32601, format!("method not found: {method}"))),
        };
        if id.is_none() { continue; }
        let resp = match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err((code, m)) => json!({"jsonrpc": "2.0", "id": id, "error": {"code": code, "message": m}}),
        };
        let _ = writeln!(stdout, "{resp}");
        let _ = stdout.flush();
    }
}
