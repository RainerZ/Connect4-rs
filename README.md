# Connect4-rs

Rust port of [Connect4](https://github.com/RainerZ/Connect4) (Java/JavaFX):
a Connect Four engine (negamax with alpha/beta pruning on bitboards), an
egui desktop GUI, and an [MCP](https://modelcontextprotocol.io) server so any
LLM agent can play against the engine while you watch.

```
src/
  engine.rs   bitboard + incremental evaluation + negamax search (no_std-style, no allocations)
  game.rs     game state shared by GUI, engine thread and control socket
  server.rs   control socket on 127.0.0.1:4444 (newline-delimited JSON)
  main.rs     egui/eframe GUI            -> binary `connect4-rs`
  mcp.rs      MCP stdio server           -> binary `connect4-mcp`
```

## What's there

### Engine (`src/engine.rs`)

* Two `u64` bitboards (7 bits per column); win detection in 4 shift/and ops.
* Evaluation identical to the Java `getBoardScore`: for each of the 69
  possible 4-in-a-row lines the value is the signed piece count if only one
  colour is present, else 0; ±1000 for a completed line. The sum is updated
  incrementally on `make`/`unmake` from per-line piece counts, so a node
  costs O(lines through the played square) (≤13) instead of a full scan.
* Negamax with alpha/beta, column order 4,5,3,2,6,1,7 (centre first), the
  Java depth heuristic (10, +1 after 16 pieces, +2 with two full columns, 18
  with more), and the Java "forced loss → re-search at depth 2" fallback.
* ~20 Mnodes/s single-threaded on Apple Silicon; a typical move takes
  well under half a second.

### GUI (`src/main.rs`)

Keys: `1`–`7` drop a piece, `N`/`Space` new game, `S` swap who starts (and
start a new game). While the engine thinks the window shows depth and live
node count; afterwards score, nodes and time of the last search.

### Control socket (`src/server.rs`)

The GUI listens on `127.0.0.1:4444`, one JSON object per line:

```
{"cmd":"state"}
{"cmd":"move","col":4}             # 1..7, blocks until the engine has answered
{"cmd":"new","engine_starts":false}
```

Replies contain the board (`rows`, top row first, `R`/`Y`/`.`), `status`
(`human_to_move`, `thinking`, `{"won":"human"|"engine"}`, `draw`),
`to_move`, `history` (columns played so far) and `last_search`
(engine column, depth, nodes, millis, score).

### MCP server (`src/mcp.rs`)

`connect4-mcp` is a stdio MCP server (hand-rolled JSON-RPC, no framework)
that forwards three tools to the running GUI:

| tool             | arguments                        |
|------------------|----------------------------------|
| `connect4_new`   | `engine_starts: bool` (default false) |
| `connect4_move`  | `col: 1..7`                      |
| `connect4_state` | –                                |

Each result is a text rendering of the board plus the raw JSON state.

## Build

Requires a Rust toolchain (edition 2024, i.e. Rust ≥ 1.85).

```bash
cargo build --release
```

This produces `target/release/connect4-rs` (GUI) and
`target/release/connect4-mcp` (MCP server). Build in release mode — the
engine is roughly 10× slower in debug.

## Test

```bash
cargo test --release
```

Unit tests in `engine.rs` check the line tables, that the incremental score
matches a full-scan reference port of the Java evaluation over random
playouts, and that the search finds immediate wins and blocks.

Search benchmark (ignored by default, prints nodes/s at depth 10/12/14):

```bash
cargo test --release bench_search -- --ignored --nocapture
```

## Play

### Yourself, in the GUI

```bash
cargo run --release
```

You play red and move first; press `S` to let the engine start.

### From a script or shell

With the GUI running, talk to the control socket directly:

```bash
printf '{"cmd":"move","col":4}\n' | nc 127.0.0.1 4444
```

### Letting an LLM play against it

Any MCP-capable model/agent can play through `connect4-mcp`. The flow is
always: start the GUI, register the MCP server, then ask the model to play.

1. Build and start the GUI (it owns the engine and the socket):

   ```bash
   cargo run --release
   ```

2. Register the MCP server with your client. For **Claude Code** this repo
   already ships a project-level [`.mcp.json`](.mcp.json):

   ```json
   {
     "mcpServers": {
       "connect4": { "command": "/Users/rainer/git/Connect4-rs/target/release/connect4-mcp" }
     }
   }
   ```

   Adjust the path to your checkout. Start `claude` in this directory and the
   `connect4_*` tools are available. Alternatively, register it globally:

   ```bash
   claude mcp add connect4 -- /path/to/Connect4-rs/target/release/connect4-mcp
   ```

   The same stdio command works for other clients — e.g. Claude Desktop
   (`claude_desktop_config.json`), Cursor, Codex, Gemini CLI, or any MCP SDK —
   because the server only needs a `command` with no arguments or env.

3. Ask the model to play, e.g.

   > Play a game against Connect4-rs. Start a new game, then alternate
   > connect4_move calls until the game is over. Think about threats and
   > parity before each move.

   The model calls `connect4_new`, then `connect4_move` repeatedly; every
   reply already includes the engine's answer, so one tool call per turn is
   enough. The board updates live in the GUI window.

To test a model without an MCP client, drive the socket from any language
and feed the JSON state into the model's prompt; the protocol is the three
commands above.

### Result so far

Claude (Fable 5) vs. the engine at depth 10, Claude as red moving first:
the engine won in 34 plies by stacking its diagonals onto Claude's two
row‑3 threat squares and winning the resulting parity/zugzwang fight.
