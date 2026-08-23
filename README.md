# Connect4-rs

Rust port of [Connect4](https://github.com/RainerZ/Connect4) (Java/JavaFX):
a Connect Four engine (negamax with alpha/beta pruning on bitboards), an
egui desktop GUI, and an [MCP](https://modelcontextprotocol.io) server so any
LLM agent can play against the engine while you watch.

```
src/
  engine.rs   bitboard + incremental evaluation + negamax search (no_std-style, no allocations)
  game.rs     game state shared by GUI, engine thread and control socket
  hints.rs    optional LLM assistance (tactical bookkeeping), not used by the engine
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
* Negamax with alpha/beta, column order 4,5,3,2,6,1,7 (centre first).
* Iterative deepening with a wall-clock budget (default 2 s, `--budget`):
  depth 1, 2, 3, … with the previous best column tried first; a new
  iteration starts only while less than a third of the budget is used, and
  an iteration running past 2× the budget is aborted (the previous result is
  kept). Stops early on a proven win/loss or when the rest of the game is
  fully searched. Replaces the Java fixed-depth heuristic (10, +1/+2 as
  columns fill), so the engine reaches depth ~12 in the opening and
  searches endgames to the end.
* ~20 Mnodes/s single-threaded on Apple Silicon.

### GUI (`src/main.rs`)

Keys: `1`–`7` drop a piece, `N`/`Space` new game, `S` swap who starts (and
start a new game), `+`/`-` double/halve the engine's think time, `H` toggle
LLM hints (see below). The status line shows think time and hint state;
while the engine thinks it shows the
iteration depth and live node count, afterwards score, depth, nodes and time
of the last search.

```bash
cargo run --release -- --budget 5     # think time per engine move in seconds (default 2)
cargo run --release -- --hints        # start with LLM hints enabled
```

### Control socket (`src/server.rs`)

The GUI listens on `127.0.0.1:4444`, one JSON object per line:

```
{"cmd":"state"}
{"cmd":"move","col":4}             # 1..7, blocks until the engine has answered
{"cmd":"new","engine_starts":false}
{"cmd":"hints","on":true}          # toggle LLM hints, returns the state
```

Replies contain the board (`rows`, top row first, `R`/`Y`/`.`), `status`
(`human_to_move`, `thinking`, `{"won":"human"|"engine"}`, `draw`),
`to_move`, `history` (columns played so far), `last_search`
(engine column, depth, nodes, millis, score) and, when enabled, `hints`.

### LLM hints (`src/hints.rs`) — optional, off by default

When LLMs play over the socket their losses are mostly bookkeeping errors
(miscounting a column's height, overlooking a vertical three), not strategy.
To separate the two, the state can carry one-ply tactical hints:

* `next_row` — next free row per column (1 = bottom, `null` if full)
* `winning_moves` — columns that complete four right now
* `must_block` — columns where the opponent wins next move
* `losing_moves` — columns after which the opponent has an immediate win

This is **assistance for the client only**; the engine never sees it. Toggle
with `H` in the GUI, `--hints` at startup, `{"cmd":"hints","on":…}` on the
socket or the `connect4_hints` MCP tool, and compare a model's results with
and without.

### MCP server (`src/mcp.rs`)

`connect4-mcp` is a stdio MCP server (hand-rolled JSON-RPC, no framework)
that forwards four tools to the running GUI:

| tool             | arguments                        |
|------------------|----------------------------------|
| `connect4_new`   | `engine_starts: bool` (default false) |
| `connect4_move`  | `col: 1..7`                      |
| `connect4_state` | –                                |
| `connect4_hints` | `on: bool` — LLM assistance toggle |

Each result is a text rendering of the board (plus a `hints:` line when
enabled) followed by the raw JSON state.

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
playouts, that the search finds immediate wins and blocks, and that a real
zugzwang endgame is proven within the budget; `hints.rs` tests the
win/block/losing-move detection.

Search benchmark (ignored by default, prints depth reached and nodes/s for
a 0.5 s and a 2 s budget):

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

   Add "call connect4_hints with on=true first" to give the model the
   tactical bookkeeping; leave it off to test the model unaided.

   The model calls `connect4_new`, then `connect4_move` repeatedly; every
   reply already includes the engine's answer, so one tool call per turn is
   enough. The board updates live in the GUI window.

To test a model without an MCP client, drive the socket from any language
and feed the JSON state into the model's prompt; the protocol is the three
commands above.

### Result so far

Claude (Fable 5) vs. the engine, no hints: engine 3 – Claude 0.
At fixed depth 10: as red Claude lost in 34 plies to a parity/zugzwang fight
after the engine stacked its diagonals onto Claude's two row‑3 threat squares;
as yellow Claude lost in 17 plies to a double threat (diagonal + vertical).
With the 2 s budget (depth 12–14): as red Claude lost in 18 plies after
miscounting a column height and handing the engine a double diagonal.


### Notes

Ways the engine could be made to handle this knowingly, roughly in order of payoff:

Transposition table — lets the same depth search far more cheaply, so the exhaustive endgame phase starts earlier. Purely a speed win, no knowledge needed.
Threat-aware evaluation — count open three-in-a-rows with a playable-eventually fourth square, and weight them by row parity (odd rows for red, even for yellow). This is the classic Connect Four heuristic from Victor Allis's thesis and gives the engine the concept directly instead of relying on search depth.
