# Changelog

The version tags mark the milestones of the engine lab. Each engine
improvement additionally lives in its own branch (`simple-engine`,
`transposition-table`, `threat-eval`, `combined-engine`, …) so its effect
on play stays observable — see the README's branch overview.

## [V1.4.0](https://github.com/RainerZ/Connect4-rs/tree/V1.4.0) — 2026-09-01

- learn from lost games (L key) and option --tutor
- book comments, GUI polish
- corrective book enlarged to 5 stones: 4 508 positions audited, 672
  corrections (after three arbitrary first-player moves the engine has a
  won game in 58 % of positions - and now knows how to collect) 

## [V1.3.0](https://github.com/RainerZ/Connect4-rs/tree/V1.3.0) — 2026-08-26

The corrective book, tooling and polish:

- **Corrective book** (`bookgen --corrective <stones>`): audits every
  engine-to-move position (human moving first) against the perfect solver
  and books only the engine's mistakes. The 3-stone pilot found 50 blind
  spots in 245 positions (20 %) — including two direct replies to human
  openings: when the human opens on b1, only the answer c1 keeps the
  engine's win, and after an f1 opening only e1 does. Auto-loaded from
  `corrective-book.txt` alongside the opening book; killed runs resume
  with `--skip`.
- **`bookview`**: decodes position keys back to ASCII boards (the key is
  exactly invertible), reconstructs a legal move history, shows book
  entries, and with `--replay` pushes the position onto the running GUI.
  `scripts/validate_book.py` verifies every corrective entry live.
- **`--log`**: traces every move, the engine's reply details
  (book/search, score, depth, nodes, time) and the tactical hints served
  to the (LLM) player to stderr.
- **GUI**: undo (`U` key and button, works even while the engine thinks);
  book moves are marked as book moves with the engine's own evaluation
  instead of claiming a forced win.
- `default-run` fix so bare `cargo run` starts the GUI again; README:
  narrative intro, book key/file-format spec, credits section; extensive
  instructive code comments explaining the theory and architecture.

## [V1.2.0](https://github.com/RainerZ/Connect4-rs/tree/V1.2.0) — 2026-08-24

The engine beats the perfect solver:

- **`versus`**: automated engine-vs-solver matches; the solver's raw score
  after every engine move is ground truth for when the theoretical win was
  lost. Finding: bookless, more think time made the opening *worse*.
- **`bookgen`** distills an opening book from the solver (6 525 positions
  for the first six first-player moves, transposition-deduplicated);
  `opening-book.txt` is committed and auto-loaded.
- **Result: with the book, the engine beats the perfect solver as first
  player at a 2 s budget** — wire to wire, the win never dropped
  (games 9 and 10 in `docs/games.md`).

## [V1.1.0](https://github.com/RainerZ/Connect4-rs/tree/V1.1.0) — 2026-08-24

Playing against an external solver:

- **`--solver <cmd>`** seats a Pascal-Pons-protocol solver (e.g. his
  `c4solver`) in the engine chair; GUI, hints, eval bar and MCP tools work
  unchanged against a perfect opponent. Kept-alive process, measured
  bookless timings (empty board 6 min 47 s, midgame instant).
- LLM hints and GUI hint rings split into independent toggles; command
  line options documented; `CLAUDE.md` and `docs/games.md` added to
  preserve the session context in the repo.

## [V1.0.0](https://github.com/RainerZ/Connect4-rs/tree/V1.0.0) — 2026-08-24

The combined engine — everything from the initial Rust port to the merge
of both improvement branches into `main`:

- **Engine**: bitboards with incremental evaluation (port of the original
  Java heuristic), iterative deepening on a time budget, transposition
  table + PVS + MTD(f) (`transposition-table` branch), Allis threat/parity
  evaluation (`threat-eval` branch). Announces proven wins, resigns proven
  losses.
- **GUI** (egui): mouse input with landing ghost, drop animation, hint
  rings, eval bar, settings strip, think-time slider.
- **LLM interface**: JSON control socket (port 4444) with replay support,
  MCP server (`connect4-mcp`) with optional tactical hints.
- **Games 1–8** against Claude (Fable 5) recorded in `docs/games.md`:
  engine 6 – Claude 2, every engine version winning in its own
  characteristic style.
