//! Minimal control socket: one JSON object per line in, one JSON line out.
//! {"cmd":"state"} | {"cmd":"move","col":1..7} | {"cmd":"new","engine_starts":bool}
//! {"cmd":"hints","on":bool}   toggles LLM assistance (see hints.rs)
//! {"cmd":"replay","moves":[..]} replays a full game (both sides) into place
//! `move` blocks until the engine has answered (or the game ended).

use crate::game::{Game, Shared, Status, PORT};
use serde_json::{json, Value};
use std::io::{BufRead, BufReader, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::Arc;
use std::time::Duration;

pub fn run(shared: Arc<Shared>) {
    let listener = match TcpListener::bind(("127.0.0.1", PORT)) {
        Ok(l) => l,
        Err(e) => {
            eprintln!("control socket: cannot bind port {PORT}: {e}");
            return;
        }
    };
    for stream in listener.incoming().flatten() {
        let s = shared.clone();
        std::thread::spawn(move || handle(stream, s));
    }
}

fn handle(stream: TcpStream, shared: Arc<Shared>) {
    let mut reader = BufReader::new(stream.try_clone().unwrap());
    let mut out = stream;
    let mut line = String::new();
    loop {
        line.clear();
        if reader.read_line(&mut line).unwrap_or(0) == 0 {
            return;
        }
        let req: Value = match serde_json::from_str(&line) {
            Ok(v) => v,
            Err(e) => {
                let _ = writeln!(out, "{}", json!({"error": format!("bad json: {e}")}));
                continue;
            }
        };
        let resp = execute(&shared, &req);
        if writeln!(out, "{resp}").is_err() {
            return;
        }
    }
}

fn wait_engine(shared: &Shared) {
    let mut g = shared.game.lock().unwrap();
    while g.status == Status::Thinking {
        let (ng, _) = shared.changed.wait_timeout(g, Duration::from_secs(600)).unwrap();
        g = ng;
    }
}

fn execute(shared: &Shared, req: &Value) -> Value {
    match req.get("cmd").and_then(Value::as_str) {
        Some("state") => json!(shared.game.lock().unwrap().to_json()),
        Some("new") => {
            let es = req.get("engine_starts").and_then(Value::as_bool).unwrap_or(false);
            let mut g = shared.game.lock().unwrap();
            *g = Game::new(es, g.budget, g.hints, g.show_hints);
            drop(g);
            shared.notify();
            wait_engine(shared);
            json!(shared.game.lock().unwrap().to_json())
        }
        Some("move") => {
            let col = req.get("col").and_then(Value::as_u64).unwrap_or(0);
            if !(1..=7).contains(&col) {
                return json!({"error": "col must be 1..7"});
            }
            let ok = shared.game.lock().unwrap().human_move(col as usize - 1);
            if !ok {
                let g = shared.game.lock().unwrap();
                return json!({"error": format!("illegal move (status {:?}, column full or not your turn)", g.status), "state": g.to_json()});
            }
            shared.notify();
            wait_engine(shared);
            json!(shared.game.lock().unwrap().to_json())
        }
        Some("replay") => {
            let moves: Vec<usize> = match req.get("moves").and_then(Value::as_array) {
                Some(a) if a.iter().all(|m| m.as_u64().is_some_and(|m| (1..=7).contains(&m))) => {
                    a.iter().map(|m| m.as_u64().unwrap() as usize - 1).collect()
                }
                _ => return json!({"error": "replay needs moves: array of columns 1..7"}),
            };
            let es = req.get("engine_starts").and_then(Value::as_bool).unwrap_or(false);
            let mut g = shared.game.lock().unwrap();
            let mut ng = Game::new(es, g.budget, g.hints, g.show_hints);
            for (i, &c) in moves.iter().enumerate() {
                if !ng.replay_move(c) {
                    return json!({"error": format!("illegal replay move {} (col {})", i + 1, c + 1), "state": ng.to_json()});
                }
            }
            *g = ng;
            drop(g);
            shared.notify();
            wait_engine(shared);
            json!(shared.game.lock().unwrap().to_json())
        }
        Some("hints") => {
            let mut g = shared.game.lock().unwrap();
            if let Some(on) = req.get("on").and_then(Value::as_bool) {
                g.hints = on;
            }
            let j = g.to_json();
            drop(g);
            shared.notify();
            json!(j)
        }
        _ => json!({"error": "unknown cmd, use state | move | new | hints | replay"}),
    }
}
