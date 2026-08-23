# Connect4-rs — project notes for Claude

## Branch policy (deliberate — keep it)

The branches are a lab notebook: each engine improvement lives in its own
branch so its effect on play stays observable. Never collapse or delete
them; land new work on `main` (or a new branch merged into `main`).

- `simple-engine` — original Java-port evaluation, plain alpha/beta
- `transposition-table` — + TT, PVS, MTD(f)
- `threat-eval` — + Allis threat/parity evaluation
- `combined-engine` — both merged; `main` carries this

The README's "Results so far" section is the running record of
Claude-vs-engine games — update it after notable games. Score at the time
of writing: engine 6 – Claude 2.

## Workflow

- Build: `cargo build --release` (fat LTO, slow full builds; debug is ~10×
  slower at runtime — always use release).
- Test: `cargo test --release` (11 tests, ~6 s). Benchmarks/analysis are
  ignored tests: `bench_search` and `analyse_recorded_win`
  (`-- --ignored --nocapture`); the latter replays the recorded lost game
  (`RECORDED_LOSS` in engine.rs) with 10 s searches per engine move — the
  standard way to measure an engine change's effect.
- The GUI (`target/release/connect4-rs`) owns the engine and the control
  socket on 127.0.0.1:4444; the MCP server (`connect4-mcp`, registered in
  `.mcp.json`) forwards to it. After a rebuild, kill and restart the GUI
  binary or the socket keeps serving the old engine.
- Full games (both sides' moves) can be pushed onto the board with
  `{"cmd":"replay","moves":[...]}` on the socket — used to restore
  positions for screenshots or analysis. Historic games: `docs/games.md`.
- Engine think time: `--budget <seconds>` (default 2), LLM hints default
  on (`--no-hints` to disable).
- Rainer pushes to GitHub himself; `git push` is not authenticated in
  non-interactive shells.

## Code conventions

- The engine (`src/engine.rs`) keeps evaluation *incremental*: any new
  evaluation term must be updated in `make`/`unmake` (cheaply — gate work
  off the already-computed line values) and verified against an
  independent full-scan reference in a random-playout test.
- Heuristic scores must stay below `WIN_SCORE` (clamped in `score_for`) so
  they can never be mistaken for proven wins.
- `src/hints.rs` is LLM assistance only — the engine must never read it.
