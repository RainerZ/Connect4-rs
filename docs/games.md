# Game records: Claude (Fable 5) vs. the engine

All games played 2026-08-23/24 over the MCP interface, engine at the 2 s
default budget unless noted (game 9: Pascal Pons' perfect solver in the
engine seat via `--solver`). Columns are 1–7; moves alternate starting
with the first player. Board squares in the notes use chess-like
notation: columns a–g left to right, rows 1–6 bottom up (so move "4"
lands on d1 when the column is empty). Any game can be replayed onto the running GUI:

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

Claude miscounted a column height (intended c3, landed c2) and handed
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
predicted (e6 at ply 14 — then transposed straight back — and g1
instead of the fatal a2 at ply 18), but the c3 freeze wins against
both tries; it proved its loss at depth 20 in 226 ms and resigned.

## 6 — threat-eval engine · Claude red, hints on · engine wins, 24 plies

```
4,4,4,4,5,3,6,7,3,3,2,3,1,6,7,7,2,4,2,2,3,5,5,5
```

Constructive style: b4 made a row-4 trio plus a diagonal trio, then
e2 stacked two win squares (e3/e4) — no single block answers.

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
both open columns, sealed the parity with the quiet a1 (+1000 announced
at depth 15 on that move), burned the neutral squares and won when Claude
ran out of safe moves — Claude's own game-4 strategy, played back without
hints and proven fifteen plies out.

## 9 — perfect solver (`--solver`, bookless) · Claude red, hints on · solver wins, 26 plies

```
4,4,4,4,4,7,5,6,3,2,5,5,6,2,3,3,1,3,3,7,7,7,2,2,5,5
```

Claude held the theoretical first-player win for eight plies (the solver's
score stayed at "losing as slowly as possible"), then threw the win at
ply 9 (c1, a tempo-grabbing forcing move — the solver proved the draw in
1 ms) and the draw at ply 11 (e2, enabling the e3 diagonal hub). The
execution ended with the signature stacked double: Claude's forced row-4
block at e4 directly beneath the solver's diagonal win at e5. Solver
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




## 11 — V1.3.0 **engine lost against kimi-k3**

kimi-k3 on red, llm hints on, budget 2s, · **kimi-k3 wins**, engine resigns after 23 plies
Connect4-rs v1.3.0 with 50 line corrective book.  

```
4,4,3,5,2,1,4,3,5,2,1,3,4,3,3,5,4,4,5,5,3,6,7
```
 
A zugzwang win built on a diagonal the engine could never safely contest.
Red's ply-21 c6 completed the trio c6,d5,e4 with the completion
square f3 one stone above the floor of column 6 — making column 6 taboo
for *both* sides: yellow's f2 loses instantly to f3, while red's
f2 would let yellow cap the diagonal at f3. Column 2 was equally
taboo for red since b3 hands yellow the e1,d2,c3,b4
diagonal. With 15 neutral filler cells left outside those columns and red
to move, yellow was bound to run out of safe moves first; after red's
quiet g1 the engine's search proved every line lost (-1000, depth 16)
and it resigned rather than be forced into column 6.
 
Note: 6s think time on MacBook Air is enough to break this sequence.
Since the learn feature analyzed this game, the ply-12 blunder is booked
(`corrective-book.txt`, with a provenance comment): replaying kimi's line
now deviates at stone 12 — the engine answers e from the book and holds
the win at 2 s.

Analysis — the solver trace rewrites the story of game 11 in a
fascinating way:

| ply | position after | verdict (engine view) | engine played | judgment |
|---|---|---|---|---|
| 1 | `4` | lost (−1) — kimi's center opening is perfect | 4 | best resistance ✓ |
| 3 | `4,4,3` | **engine WINS (+2)** — kimi's 3 threw the win away! | 5 | would keep the win ✓ |
| 5–9 | … | engine still winning (+2…+1) | 1, 3, 2 | all preserving ✓ |
| 11 | `4,4,3,5,2,1,4,3,5,2,1` | engine wins (+1), **only column 5** | 3 | **THREW IT** (c scores −2) |
| 13–21 | … | lost (−2) | … | best resistance to the end ✓ |

So the real drama: kimi blundered first (ply 3, 3 handed the engine a won game), the engine held that win for eight plies — through exactly the band the corrective book covers, playing correctly everywhere — and then returned the favor at stone 11, in a needle-sharp position where only the e-column wins. Everything after was kimi flawlessly converting a theoretically won game; the celebrated zugzwang construction was the execution, not the turning point.

## 12 — V1.4.0 combined engine + 672-entry corrective book · Claude red, hints on · engine wins, 30 plies

```
4,4,4,4,4,5,5,5,5,7,3,2,7,2,7,5,7,7,2,4,2,5,7,3,3,3,3,3,1,1
```

Claude held the theoretical first-player win for ten plies — the centre
stack and e2, e4 are all solver-correct — then threw it with the
"prophylactic" c1 at ply 11 (solver: b1, d6 or e5 keep the win; c1 scores
−3). The engine punished it with best-resistance-to-win moves throughout,
and finished with a triple-purpose c2: it forced the c3 block, completed
b2–c2–d2 with a2 as a stacked win square above Claude's a1, and left every
remaining column poisoned (a1 → a2, b5 → b6, f1 → f2) — total zugzwang,
row 2 completed on stone 30.

The new corrective book never fired: the audit had already certified the
engine's own replies in this line's first five stones, and Claude's mistake
came at stone 11 — beyond the corrective horizon, in the engine's own
search territory, where a fat +3 win is easy to collect. The mirror image
of game 11.

| ply | position after | verdict (red view) | best | Claude played | judgment |
|---|---|---|---|---|---|
| 1–9 | … | WIN (+1) throughout | 4, 4, 4, {2,5}, 5 | 4, 4, 4, 5, 5 | all correct ✓ |
| 11 | `4,4,4,4,4,5,5,5,5,7` | WIN (+1) | 2, 4, 5 | 3 | **THREW IT** (c1 scores −3) |
| 13–29 | … | lost (−3 … −7) | … | … | best resistance ✓ |
