# Connect4-rs

**A Connect Four engine in Rust — play it yourself, or let your favourite
LLM try to beat it while you watch.**

Bitboard engine searching ~20 million positions per second, a clean egui
desktop GUI, and an [MCP](https://modelcontextprotocol.io) server that turns
the running game into three tools any LLM agent can play with.

<p align="center"><img src="docs/winning-board.png" alt="Claude's winning board against the engine" width="520"></p>

*The board above is a historic moment: the first game Claude won against
the engine — more on that [below](#results-so-far).*

## Highlights

* **Fast engine** — two `u64` bitboards, incrementally updated evaluation,
  negamax with alpha/beta, transposition table, principal variation search
  and MTD(f), iterative deepening on a time budget. Endgames are searched
  to the end; proven wins are announced and hopeless positions resigned.
* **Human-friendly GUI** — click to drop (with a landing ghost), gravity
  drop animation, tactical hint rings, eval bar, think-time slider.
* **LLM-friendly interface** — a JSON control socket and an MCP server with
  optional tactical hints, so language models can play full games against
  the engine and you can watch live.

## Quick start

```bash
cargo build --release
cargo run --release          # you are red and start; the engine answers
```

Requires a Rust toolchain (edition 2024, i.e. Rust ≥ 1.85). Build in
release mode — the engine is roughly 10× slower in debug.

```
src/
  engine.rs   bitboard + incremental evaluation + negamax search (no_std-style, no allocations)
  game.rs     game state shared by GUI, engine thread and control socket
  hints.rs    optional LLM assistance (tactical bookkeeping), not used by the engine
  server.rs   control socket on 127.0.0.1:4444 (newline-delimited JSON)
  main.rs     egui/eframe GUI            -> binary `connect4-rs`
  mcp.rs      MCP stdio server           -> binary `connect4-mcp`
```
(both binaries land in `target/release/`)

## What's there

### Engine (`src/engine.rs`)

* Two `u64` bitboards (7 bits per column); win detection in 4 shift/and ops.
* Evaluation identical to the Java `getBoardScore`: for each of the 69
  possible 4-in-a-row lines the value is the signed piece count if only one
  colour is present, else 0; ±1000 for a completed line. The sum is updated
  incrementally on `make`/`unmake` from per-line piece counts, so a node
  costs O(lines through the played square) (≤13) instead of a full scan.
* Negamax with alpha/beta, column order 4,5,3,2,6,1,7 (centre first),
  principal variation search (first child full window, siblings zero
  window with re-search).
* Transposition table: 2^22 entries × 16 bytes (64 MB), keyed by Pascal
  Pons' perfect 49-bit position key (`mover + occupancy + bottom row` —
  the carry marks each column's stack height), storing score, bound type
  (exact/lower/upper) and best column, always-replace.
* MTD(f) on top: each depth converges on the minimax value with a few
  zero-window searches around the previous score. Zero windows keep the
  stored bounds maximally reusable and pair naturally with PVS.
* Iterative deepening with a wall-clock budget (default 2 s, `--budget`):
  depth 1, 2, 3, … with the previous best column tried first; a new
  iteration starts only while less than a third of the budget is used, and
  an iteration running past 2× the budget is aborted (the previous result is
  kept). Stops early on a proven win/loss or when the rest of the game is
  fully searched. Replaces the Java fixed-depth heuristic (10, +1/+2 as
  columns fill).
* ~15-20 Mnodes/s single-threaded on Apple Silicon; with the table, PVS
  and MTD(f) a 2 s budget reaches depth 13 from the opening (~2× fewer
  nodes than plain alpha/beta) and proves endgames in milliseconds.

### GUI (`src/main.rs`)

When the engine's search proves a win it says so ("Engine sees a forced
win!", headline in red) instead of letting you walk into it unwarned; when
it proves its own loss it resigns ("Engine gives up") rather than playing
out the zugzwang. The socket/MCP state carries this as `resigned: true` and
in `message`.

Play with the mouse (hover shows a translucent ghost on the landing slot,
click drops the piece; pieces fall with a short gravity animation) or the
keyboard. With hints on, the landing slots of tactically decisive columns
are marked: green ring = wins now, orange ring = must block, grey ring with
an x = loses immediately.

A settings strip offers a New-game button, an "engine starts" checkbox
(applies to the next game), the hints toggle and a logarithmic think-time
slider (0.05–60 s). A small eval bar in the status area shows red's share
of the engine's last score: middle = balanced, full = proven win.

Keys: `1`–`7` drop a piece, `N`/`Space` new game, `S` swap who starts (and
start a new game), `+`/`-` double/halve the engine's think time, `H` toggle
hints (see below). The status line shows think time and hint state; while
the engine thinks it shows the iteration depth and live node count,
afterwards score, depth, nodes and time of the last search.

```bash
cargo run --release -- --budget 5     # think time per engine move in seconds (default 2)
cargo run --release -- --no-hints     # start without LLM hints (they are on by default)
```

### Control socket (`src/server.rs`)

The GUI listens on `127.0.0.1:4444`, one JSON object per line:

```
{"cmd":"state"}
{"cmd":"move","col":4}             # 1..7, blocks until the engine has answered
{"cmd":"new","engine_starts":false}
{"cmd":"hints","on":true}          # toggle LLM hints, returns the state
{"cmd":"replay","moves":[4,4,…]}   # replay a full game, both sides' columns
```

Replies contain the board (`rows`, top row first, `R`/`Y`/`.`), `status`
(`human_to_move`, `thinking`, `{"won":"human"|"engine"}`, `draw`),
`to_move`, `history` (columns played so far), `last_search`
(engine column, depth, nodes, millis, score) and, when enabled, `hints`.

### LLM hints (`src/hints.rs`) — optional, on by default

When LLMs play over the socket their losses are mostly bookkeeping errors
(miscounting a column's height, overlooking a vertical three), not strategy.
To separate the two, the state can carry one-ply tactical hints:

* `next_row` — next free row per column (1 = bottom, `null` if full)
* `winning_moves` — columns that complete four right now
* `must_block` — columns where the opponent wins next move
* `losing_moves` — columns after which the opponent has an immediate win

This is **assistance for the client only**; the engine never sees it. On by
default; toggle with `H` in the GUI, `--no-hints` at startup,
`{"cmd":"hints","on":…}` on the socket or the `connect4_hints` MCP tool, and
compare a model's results with and without.

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

## Test

```bash
cargo test --release
```

Unit tests in `engine.rs` check the line tables, that the incremental score
matches a full-scan reference port of the Java evaluation over random
playouts, that the search finds immediate wins and blocks, and that a real
zugzwang endgame is proven within the budget; `hints.rs` tests the
win/block/losing-move detection, and a search with a warm transposition
table is checked against a plain no-table negamax on positions solved to
the end of the game. A slower test replays the recorded lost game: at
ply 18 even a 10 s search cannot prove the loss (it is beyond a depth-20
horizon), while at ply 20 the loss is proven in about 100 ms. An ignored
`analyse_recorded_win` helper prints the 10 s evaluation of every engine
move of that game — with the transposition table and MTD(f) it reaches
1-4 plies deeper than before and deviates from the recorded line from
ply 14 on.

Search benchmark (ignored by default, prints depth reached and nodes/s for
a 0.5 s and a 2 s budget):

```bash
cargo test --release bench_search -- --ignored --nocapture
```

## Play

### Yourself, in the GUI

`cargo run --release` — you play red and move first; use the settings strip
or press `S` to let the engine start.

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

   Hints are on by default; ask the model to call `connect4_hints` with
   `on=false` first to test it unaided.

   The model calls `connect4_new`, then `connect4_move` repeatedly; every
   reply already includes the engine's answer, so one tool call per turn is
   enough. The board updates live in the GUI window.

To test a model without an MCP client, drive the socket from any language
and feed the JSON state into the model's prompt; the protocol is the three
commands above.

### Results so far

Claude (Fable 5) vs. the engine: **engine 3 – Claude 1**.

Without hints, Claude lost all three games — twice at fixed depth 10 (a
parity/zugzwang squeeze in 34 plies; a diagonal + vertical double threat in
17) and once against the 2 s budget (18 plies, after miscounting a column
height and handing the engine a double diagonal). Every loss traced back to
a bookkeeping error, not strategy — which is what motivated the hints.

With hints on, Claude (red) beat the engine (2 s budget, depth 12–20) —
that game's final position is the screenshot at the top of this page, with
the winning line circled.

Protocol of the win (columns 1–7; `Rc`/`Yc` = red/yellow drop in column c):

| # | moves | note |
|---|-------|------|
| 1–6 | R4 Y4, R4 Y4, R5 Y6 | centre stack, then both extend row 1 |
| 7–12 | R3 Y2, R5 Y4, R5 Y5 | red builds column 5 / row 3; yellow takes c4r4/c4r5 |
| 13–14 | R5 Y4 | **c5r5**: kills the two yellow diagonals through that hinge (they decided game 3); yellow fills column 4 |
| 15–16 | R1 Y5 | **c1r1** prophylaxis: the a1–d4 diagonal is dead for good |
| 17–20 | R3 Y1, R3 Y3 | red takes **c3r3** (row 3 trio) — engine eval drops to −1000: column 2 is frozen, c2r3 wins for both sides but red's claim sits below |
| 21–24 | R6 Y6, R6 Y7 | forced sequence in column 6: yellow must block row 3, red must block row 4 |
| 25–26 | R7 Y1 | hint-flagged forced block: c7r2 would have completed yellow's c4r5–c7r2 diagonal |
| 27–28 | R1 Y1 | red breaks yellow's column-1 vertical |
| 29–34 | R7 Y1, R7 Y7, R7 Y6 | red's **column-7 vertical threat** forces yellow's block — the tempo gain that wins the zugzwang |
| 35–38 | R6 Y3, R3 Y2 | red seals row 6, blocks c3r6; yellow is out of safe squares and must enter column 2 |
| 39 | **R2** | c2r3 completes two fours at once: 2-3-4-5 on row 3 and the c4r1–c1r4 diagonal (circled in the screenshot at the top) — red wins |

The hints did exactly what they were built for: they caught the two forced
blocks and kept the column heights straight, while the strategy (the c5r5
prophylaxis, freezing column 2, the tempo fight) was the model's own.


### Ideas for the engine

Done so far: iterative deepening on a time budget; transposition table +
PVS + MTD(f) (≈2× fewer nodes, endgame proofs in milliseconds, 1-4 plies
deeper at the same budget). Still open:

* **Threat-aware evaluation** — count open three-in-a-rows with a
  playable-eventually fourth square, and weight them by row parity (odd
  rows for the first player, even for the second). The classic Connect Four
  heuristic from Victor Allis's thesis; it gives the engine the concept
  directly instead of relying on search depth.
