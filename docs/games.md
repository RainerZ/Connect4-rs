# Game records: Claude (Fable 5) vs. the engine

All games played 2026-08-23/24 over the MCP interface, engine at the 2 s
default budget unless noted (game 9: Pascal Pons' perfect solver in the
engine seat via `--solver`). Columns are 1–7; moves alternate starting
with the first player. Any game can be replayed onto the running GUI:

```bash
printf '{"cmd":"replay","moves":[...]}\n' | nc 127.0.0.1 4444
```

## 1 — simple engine (fixed depth 10) · Claude red, no hints · engine wins, 34 plies

```
4,4,4,4,3,2,5,6,3,4,5,4,3,3,5,5,5,5,7,7,1,1,7,3,1,3,7,1,7,7,1,1,2,2
```

Claude built two odd-row threats; the engine stacked its diagonals onto the
same squares and won the parity/zugzwang fight.

## 2 — simple engine (fixed depth 10) · Claude yellow, no hints · engine wins, 17 plies
(replay with `"engine_starts":true` — the engine is red here)

```
5,4,4,4,4,5,4,4,5,6,6,7,6,5,6,3,6
```

Double threat (diagonal + column-6 vertical); Claude missed the vertical.

## 3 — simple engine (2 s budget) · Claude red, no hints · engine wins, 18 plies

```
4,4,4,4,3,2,5,6,5,4,5,5,3,3,3,2,5,1
```

Claude miscounted a column height (intended c3r3, landed c3r2) and handed
the engine a double diagonal — the loss that motivated the LLM hints.

## 4 — simple engine (2 s budget) · Claude red, hints on · **Claude wins**, 39 plies

```
4,4,4,4,5,6,3,2,5,4,5,5,5,4,1,5,3,1,3,3,6,6,6,7,7,1,1,1,7,1,7,7,7,6,6,3,3,2,2
```

The recorded win (screenshot in the README, move-by-move protocol there;
`RECORDED_LOSS` in engine.rs). Column-2 freeze, tempo battle, zugzwang.

## 5 — transposition-table engine · Claude red, hints on · **Claude wins**, engine resigns after 19 plies

```
4,4,4,4,5,6,3,2,5,4,5,5,5,5,1,4,3,7,3
```

The deeper engine deviated from game 4 exactly where the 10 s analysis
predicted (c5r6 at ply 14 — then transposed straight back — and c7r1
instead of the fatal c1r2 at ply 18), but the c3r3 freeze wins against
both tries; it proved its loss at depth 20 in 226 ms and resigned.

## 6 — threat-eval engine · Claude red, hints on · engine wins, 24 plies

```
4,4,4,4,5,3,6,7,3,3,2,3,1,6,7,7,2,4,2,2,3,5,5,5
```

Constructive style: c2r4 made a row-4 trio plus a diagonal trio, then
c5r2 stacked two win squares (c5r3/c5r4) — no single block answers.

## 7 — threat-eval engine · Claude red, hints on · engine wins, 22 plies

```
4,4,4,4,5,3,5,5,7,6,5,3,3,4,3,3,6,5,7,6,6,6
```

Claude avoided every game-6 mistake and still lost: a row-5 trio whose
completion squares were protected by an older diagonal underneath —
threats defending threats.

## 8 — combined engine · Claude red, hints on · engine wins, 34 plies (the showdown)

```
4,4,4,4,4,5,5,5,7,5,5,3,3,6,6,3,3,5,3,1,1,4,3,1,1,1,1,7,7,7,7,7,2,2
```

Engineered zugzwang: the engine arranged its win squares below Claude's in
both open columns, sealed the parity with the quiet c1r1 (+1000 announced
at depth 15 on that move), burned the neutral squares and won when Claude
ran out of safe moves — Claude's own game-4 strategy, played back without
hints and proven fifteen plies out.

## 9 — perfect solver (`--solver`, bookless) · Claude red, hints on · solver wins, 26 plies

```
4,4,4,4,4,7,5,6,3,2,5,5,6,2,3,3,1,3,3,7,7,7,2,2,5,5
```

Claude held the theoretical first-player win for eight plies (the solver's
score stayed at "losing as slowly as possible"), then threw the win at
ply 9 (c3r1, a tempo-grabbing forcing move — the solver proved the draw in
1 ms) and the draw at ply 11 (c5r2, enabling the c5r3 diagonal hub). The
execution ended with the signature stacked double: Claude's forced row-4
block at c5r4 directly beneath the solver's diagonal win at c5r5. Solver
think times fell from 101 s (first bookless query) to 0 ms as its
kept-alive process warmed up.

## 10 — combined engine + distilled book vs the perfect solver · **engine wins**, 41 plies

```
4,4,4,4,4,7,3,2,2,2,2,7,6,5,5,5,6,4,2,6,6,7,7,6,2,7,6,7,3,1,5,5,3,3,3,1,1,3,5,1,1
```

The finale: with a 6-move opening book distilled from the solver itself
(6 525 positions, one solver verdict each), the engine beat the perfect
solver as first player — at a 2 s budget, and again at 10 s (game:
`4,4,4,4,4,7,3,2,2,2,2,7,2,7,7,6,6,6,6,7,4,7,2,1,6,6,1,1,1,1,1,3,5,5,3,3,3,3,5,5,5`).
The solver's score after every engine move stayed at −1: the theoretical
win was never dropped. Plies 13–25 were played by the unassisted 2 s
heuristic — perfectly; from ply 25 the engine's own proofs took over and
converted on stone 41, the latest a perfect defender can be beaten.
