//! Connect4 engine: bitboards, incremental evaluation and a
//! negamax/alpha-beta search with transposition table, principal variation
//! search and MTD(f). This module is the instructive core of the repo -
//! the pieces fit together like this:
//!
//! # Board representation
//! Two `u64` bitboards (one per colour) hold the stones; 7 bits per
//! column, one of them a guard bit so shift tricks never bleed between
//! columns. Win detection is four shift/AND pairs (`has_won`), and the
//! same layout yields Pascal Pons' perfect 49-bit position key (`key`)
//! used by the transposition table and the opening books.
//!
//! # Evaluation (two layers, both incremental)
//! 1. The original Java heuristic: each of the 69 possible four-in-a-row
//!    *lines* contributes its signed stone count while only one colour
//!    occupies it (`line_value`). "How many lines am I still building?"
//! 2. Threat/parity knowledge from Victor Allis' 1988 thesis: a line with
//!    three stones and one empty square is a *threat*; whether it will
//!    ever convert depends on the zugzwang parity of its empty square and
//!    on the other threats below it in the same column (`threat_value`,
//!    `col_threat_value`).
//! Both layers are maintained *incrementally* in `make`/`unmake`: a move
//! touches at most 13 lines, so updating per-line counters beats
//! re-scanning all 69 lines by an order of magnitude. A random-playout
//! test compares against a full-scan reference to keep this honest.
//!
//! # Search (inside out)
//! * `negamax` - depth-limited alpha/beta in negamax form ("my best score
//!   is the negation of the opponent's"), with a transposition-table
//!   probe/store and principal variation search (first child full window,
//!   siblings zero window).
//! * `mtdf` - converges on the exact minimax value with a sequence of
//!   zero-window searches; zero windows make the stored TT bounds
//!   maximally reusable.
//! * `best_move` - iterative deepening driver on a wall-clock budget:
//!   depth 1, 2, 3, ... each depth solved by MTD(f), previous results
//!   seeding move ordering and the next value guess.
//!
//! No heap allocation in the per-node hot path: board state lives in
//! fixed-size arrays; the only allocation is the transposition table, made
//! once per engine thread.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

pub const COLS: usize = 7;
pub const ROWS: usize = 6;
pub const WIN_SCORE: i32 = 1000;
pub const INF: i32 = 1_000_000;

/// Threat/parity evaluation, the central insight of Victor Allis' 1988
/// thesis "A Knowledge-Based Approach of Connect-Four".
///
/// A *threat* is a line with three stones of one colour and one empty
/// square: whoever gets that square completes four. But threat squares
/// high up a column cannot be taken at will - a stone can only be played
/// on top of the current stack. So the question "who will eventually get
/// this square?" is decided by *zugzwang*: late in the game the players
/// are forced to fill the remaining columns move by move, and with 7
/// columns x 6 rows = 42 squares and strict alternation, the first player
/// (red) naturally receives the odd rows (1, 3, 5) and the second player
/// the even rows - unless somebody sacrifices tempo.
///
/// Hence two pieces of knowledge make a threat valuable:
///
/// * **Parity**: a threat whose empty square lies on the "own" parity row
///   (odd for red, even for yellow) is usually convertible via zugzwang
///   and scores +-24; a wrong-parity threat still has forcing value (the
///   opponent must respect it) but rarely converts, +-6.
/// * **The lowest threat per column dominates**: if my threat square sits
///   *below* yours in the same column, yours only becomes reachable after
///   mine resolves - typically by me winning. So only the lowest threat
///   square of each column is scored, and a square both sides threaten
///   goes to the side whose parity matches its row. (This rule is what
///   the engine's winning "frozen column" games are made of: park a
///   threat under the opponent's and wait.)
pub const THREAT_GOOD: i32 = 24;
pub const THREAT_WEAK: i32 = 6;

/// Signed value of a threat for `pi` (0 = red) on square `sq`.
#[inline(always)]
fn threat_value(pi: usize, sq: usize) -> i32 {
    let row = sq % ROWS; // 0-based: row 0 is the bottom = 1-based row 1 (odd)
    let odd = row % 2 == 0;
    let good = if pi == 0 { odd } else { !odd };
    let v = if good { THREAT_GOOD } else { THREAT_WEAK };
    if pi == 0 { v } else { -v }
}

/// Bitboard layout: bit index = col * 7 + row (row 0 = bottom). Each
/// column owns 7 bits although the board is only 6 high - the extra top
/// bit is a *guard*: it is never occupied, so when `has_won` shifts the
/// board by 1 (vertical) or the key encoding carries out of a full
/// column, nothing spills into the neighbouring column's bits.
///
/// ```text
///   bit index per cell        col:  0    1    2   ...
///   row 5 (top)                     5   12   19
///   ...                            ...
///   row 1                           1    8   15
///   row 0 (bottom)                  0    7   14
///   (bit 6, 13, 20, ... are the unused guard bits)
/// ```
const COL_BITS: usize = ROWS + 1;
const BOARD_MASK: u64 = {
    let mut m = 0u64;
    let mut c = 0;
    while c < COLS {
        m |= 0x3F << (c * COL_BITS);
        c += 1;
    }
    m
};

/// One bit at the bottom row of every column (for the position key).
const BOTTOM_MASK: u64 = {
    let mut m = 0u64;
    let mut c = 0;
    while c < COLS {
        m |= 1u64 << (c * COL_BITS);
        c += 1;
    }
    m
};

/// Column search order, centre first. Alpha/beta prunes best when the
/// strongest move is tried first, and in Connect Four central columns
/// intersect the most four-in-a-row lines (the centre cell lies on 13 of
/// the 69, an edge cell on 3), so "centre first" is a free, surprisingly
/// strong static move ordering.
pub const COL_ORDER: [usize; COLS] = [3, 4, 2, 1, 5, 0, 6];

/// Number of possible 4-in-a-row lines on a 7x6 board:
/// 24 horizontal (4 windows x 6 rows), 21 vertical (3 windows x 7
/// columns), 12 + 12 diagonals = 69. Everything the evaluation knows is
/// phrased in terms of these lines.
pub const NLINES: usize = 69;
/// Maximum number of lines passing through a single square.
const MAX_LINES_PER_SQ: usize = 13;

/// For every square: the line indices passing through it (terminated by 0xFF).
struct LineTables {
    sq_lines: [[u8; MAX_LINES_PER_SQ]; COLS * ROWS],
    line_squares: [[u8; 4]; NLINES],
}

/// Precompute, at compile time, (a) for every square the list of lines
/// passing through it and (b) for every line its four squares. These two
/// tables are what makes the evaluation incremental: when a stone lands
/// on a square, only the lines of that square (at most 13) can change
/// their value.
const fn build_tables() -> LineTables {
    let mut sq_lines = [[0xFFu8; MAX_LINES_PER_SQ]; COLS * ROWS];
    let mut line_squares = [[0u8; 4]; NLINES];
    let mut n = 0usize;
    // Same construction order as the Java buildLines() (not that it matters
    // for the score, but it keeps things comparable).
    let mut r = 0;
    while r < ROWS {
        let mut c = 0;
        while c < COLS {
            let dirs: [(isize, isize, bool); 4] = [
                (0, 1, r + 4 <= ROWS),
                (1, 0, c + 4 <= COLS),
                (1, 1, r + 4 <= ROWS && c + 4 <= COLS),
                (-1, 1, r + 4 <= ROWS && c >= 3),
            ];
            let mut d = 0;
            while d < 4 {
                let (dc, dr, ok) = dirs[d];
                if ok {
                    let mut i = 0;
                    while i < 4 {
                        let cc = (c as isize + dc * i as isize) as usize;
                        let rr = (r as isize + dr * i as isize) as usize;
                        let sq = cc * ROWS + rr;
                        line_squares[n][i] = sq as u8;
                        let mut k = 0;
                        while sq_lines[sq][k] != 0xFF {
                            k += 1;
                        }
                        sq_lines[sq][k] = n as u8;
                        i += 1;
                    }
                    n += 1;
                }
                d += 1;
            }
            c += 1;
        }
        r += 1;
    }
    assert!(n == NLINES);
    LineTables { sq_lines, line_squares }
}

static TABLES: LineTables = build_tables();

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Piece {
    Red,
    Yellow,
}

impl Piece {
    #[inline]
    pub fn other(self) -> Piece {
        match self {
            Piece::Red => Piece::Yellow,
            Piece::Yellow => Piece::Red,
        }
    }
    #[inline]
    pub fn sign(self) -> i32 {
        match self {
            Piece::Red => 1,
            Piece::Yellow => -1,
        }
    }
    #[inline]
    fn idx(self) -> usize {
        match self {
            Piece::Red => 0,
            Piece::Yellow => 1,
        }
    }
}

/// Line value exactly as the Java `Line.value()`: the signed stone count
/// while the line belongs to one colour alone, 0 as soon as both colours
/// appear (a "mixed" line can never become four-in-a-row, so it is dead
/// and worth nothing). This is the classic cheap Connect Four heuristic:
/// it rewards keeping many lines alive and progressing them, without any
/// notion of *whether* a line can actually be completed - that deeper
/// knowledge is layered on top by the threat evaluation.
#[inline(always)]
fn line_value(red: u8, yellow: u8) -> i32 {
    if yellow == 0 {
        red as i32
    } else if red == 0 {
        -(yellow as i32)
    } else {
        0
    }
}

#[derive(Clone, Copy)]
pub struct Board {
    /// bb[0] = red pieces, bb[1] = yellow pieces
    bb: [u64; 2],
    /// Number of pieces per column = the row the next stone lands on.
    height: [u8; COLS],
    total: u8,
    /// Per line: number of red / yellow stones. This little table is the
    /// heart of the incremental evaluation - from it, a line's value and
    /// its threat status can be read off in O(1) whenever a move touches
    /// the line.
    counts: [[u8; 2]; NLINES],
    /// Sum of all line values (red positive). Identical to the Java
    /// `getBoardScore(board, +1)` unless a line is complete.
    score: i32,
    /// Sum of the per-column threat values (red positive).
    threats: i32,
    /// Per square: number of red / yellow 3-lines whose empty square it is.
    threat_at: [[u8; 2]; COLS * ROWS],
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

impl Board {
    pub fn new() -> Board {
        Board { bb: [0, 0], height: [0; COLS], total: 0, counts: [[0; 2]; NLINES], score: 0, threats: 0, threat_at: [[0; 2]; COLS * ROWS] }
    }

    #[inline]
    pub fn get(&self, col: usize, row: usize) -> Option<Piece> {
        let bit = 1u64 << (col * COL_BITS + row);
        if self.bb[0] & bit != 0 {
            Some(Piece::Red)
        } else if self.bb[1] & bit != 0 {
            Some(Piece::Yellow)
        } else {
            None
        }
    }

    #[inline]
    pub fn height(&self, col: usize) -> usize {
        self.height[col] as usize
    }

    #[inline]
    pub fn total(&self) -> usize {
        self.total as usize
    }

    #[inline]
    pub fn can_play(&self, col: usize) -> bool {
        col < COLS && (self.height[col] as usize) < ROWS
    }

    #[inline]
    pub fn is_full(&self) -> bool {
        self.total as usize >= COLS * ROWS
    }

    /// Raw line-sum evaluation from red's point of view (no win check).
    #[allow(dead_code)]
    #[inline]
    pub fn raw_score(&self) -> i32 {
        self.score
    }

    /// Parity-weighted threat evaluation from red's point of view.
    #[allow(dead_code)]
    #[inline]
    pub fn threat_score(&self) -> i32 {
        self.threats
    }

    /// Value of a column's threats - the "lowest threat dominates" rule.
    /// Scanning bottom-up, the first square any side threatens decides the
    /// whole column: its owner collects the parity-weighted value, and
    /// everything above contributes nothing, because those squares only
    /// become playable after the lower threat resolves (usually by
    /// someone winning). A square threatened by *both* sides is awarded to
    /// the side whose zugzwang parity matches its row - that side expects
    /// to be handed the square in the endgame filling.
    fn col_threat_value(&self, col: usize) -> i32 {
        for r in 0..ROWS {
            let sq = col * ROWS + r;
            let red = self.threat_at[sq][0] > 0;
            let yellow = self.threat_at[sq][1] > 0;
            if red || yellow {
                let pi = if red && yellow {
                    if r % 2 == 0 { 0 } else { 1 }
                } else if red {
                    0
                } else {
                    1
                };
                return threat_value(pi, sq);
            }
        }
        0
    }

    /// A 3-line's empty square appeared (`d = 1`) or disappeared (`d = -1`)
    /// for `side`; update the per-square counts and the affected column's
    /// contribution to the threat score.
    fn threat_change(&mut self, side: usize, esq: usize, d: i8) {
        let col = esq / ROWS;
        let before = self.col_threat_value(col);
        self.threat_at[esq][side] = (self.threat_at[esq][side] as i8 + d) as u8;
        self.threats += self.col_threat_value(col) - before;
    }

    /// The (single) empty square of line `l`, ignoring `exclude` (pass
    /// usize::MAX to ignore nothing). Caller guarantees it exists.
    #[inline(always)]
    fn line_empty_sq(&self, l: usize, exclude: usize) -> usize {
        let occ = self.bb[0] | self.bb[1];
        for &sq in TABLES.line_squares[l].iter() {
            let sq = sq as usize;
            if sq != exclude && occ & (1u64 << (sq / ROWS * COL_BITS + sq % ROWS)) == 0 {
                return sq;
            }
        }
        unreachable!()
    }

    /// Drop a piece and update both evaluation layers incrementally.
    /// Caller must ensure `can_play(col)`.
    ///
    /// The discipline: for each of the <= 13 lines through the landing
    /// square, compare the line's value before and after the stone count
    /// changes and add the difference to the running total - never rescan.
    /// The same loop detects *threat transitions* from the line value it
    /// already computed (see the comment inside), so the second layer
    /// costs the common path only two integer compares.
    #[inline(always)]
    pub fn make(&mut self, col: usize, p: Piece) {
        let row = self.height[col] as usize;
        let sq = col * ROWS + row;
        self.bb[p.idx()] |= 1u64 << (col * COL_BITS + row);
        self.height[col] += 1;
        self.total += 1;
        let pi = p.idx();
        let mut delta = 0;
        for &l in TABLES.sq_lines[sq].iter() {
            if l == 0xFF {
                break;
            }
            let l = l as usize;
            let c = &mut self.counts[l];
            let before = line_value(c[0], c[1]);
            c[pi] += 1;
            delta += line_value(c[0], c[1]) - before;
            // Threat transitions, detected on the signed line value already
            // in hand (rare; the common path costs two compares). Reading
            // `signed` as "the line value from the mover's point of view":
            //   signed == 2  -> the mover just made it 3-of-a-kind: a new
            //                   threat is born on the line's one remaining
            //                   empty square (found by scanning its 4 squares);
            //   signed == 3  -> the mover completed the line to four: the
            //                   threat it *was* is consumed - and its empty
            //                   square was exactly the square being played;
            //   signed == -3 -> the mover just blocked an opponent threat:
            //                   same square logic, opposite owner.
            // Every other value means no threat appeared or disappeared.
            let signed = if pi == 0 { before } else { -before };
            if signed == 2 {
                let esq = self.line_empty_sq(l, usize::MAX);
                self.threat_change(pi, esq, 1);
            } else if signed == 3 {
                self.threat_change(pi, sq, -1);
            } else if signed == -3 {
                self.threat_change(pi ^ 1, sq, -1);
            }
        }
        self.score += delta;
    }

    /// Undo the last piece dropped into `col` (must be of colour `p`) -
    /// the exact mirror of `make`, so search can explore a move and take
    /// it back in O(lines through the square). The threat transitions are
    /// detected on the *after* value here, because unmake's after-state is
    /// make's before-state.
    #[inline(always)]
    pub fn unmake(&mut self, col: usize, p: Piece) {
        self.height[col] -= 1;
        self.total -= 1;
        let row = self.height[col] as usize;
        let sq = col * ROWS + row;
        self.bb[p.idx()] &= !(1u64 << (col * COL_BITS + row));
        let pi = p.idx();
        let mut delta = 0;
        for &l in TABLES.sq_lines[sq].iter() {
            if l == 0xFF {
                break;
            }
            let l = l as usize;
            let c = &mut self.counts[l];
            let before = line_value(c[0], c[1]);
            c[pi] -= 1;
            let after = line_value(c[0], c[1]);
            delta += after - before;
            // Threat transitions, mirroring make (the after-state here is
            // the before-state there), detected on the signed line value.
            let signed = if pi == 0 { after } else { -after };
            if signed == 2 {
                // Line was a threat; its empty square was the one that is
                // not sq (sq was occupied before this unmake).
                let esq = self.line_empty_sq(l, sq);
                self.threat_change(pi, esq, -1);
            } else if signed == 3 {
                self.threat_change(pi, sq, 1);
            } else if signed == -3 {
                self.threat_change(pi ^ 1, sq, 1);
            }
        }
        self.score += delta;
    }

    /// True if `p` has four in a row - branch-free bitboard classic.
    ///
    /// For each direction d (vertical: 1 bit, horizontal: 7 = one column
    /// width, diagonals: 6 and 8), `m = b & (b >> d)` marks every stone
    /// that has a same-colour neighbour d steps away, i.e. every pair;
    /// `m & (m >> 2d)` then marks a pair whose start is 2d away from
    /// another pair's start - four in a row. The per-column guard bit
    /// guarantees the shifts cannot manufacture a "pair" across the seam
    /// between two columns.
    #[inline(always)]
    pub fn has_won(&self, p: Piece) -> bool {
        let b = self.bb[p.idx()];
        // vertical (shift 1), horizontal (7), diagonals (6, 8)
        let m = b & (b >> 1);
        if m & (m >> 2) != 0 {
            return true;
        }
        let m = b & (b >> COL_BITS);
        if m & (m >> (2 * COL_BITS)) != 0 {
            return true;
        }
        let m = b & (b >> (COL_BITS - 1));
        if m & (m >> (2 * (COL_BITS - 1))) != 0 {
            return true;
        }
        let m = b & (b >> (COL_BITS + 1));
        m & (m >> (2 * (COL_BITS + 1))) != 0
    }

    /// The squares of the (first) completed line, if any. Used by the GUI.
    pub fn winning_line(&self) -> Option<[(usize, usize); 4]> {
        for (l, c) in self.counts.iter().enumerate() {
            if c[0] == 4 || c[1] == 4 {
                let mut out = [(0, 0); 4];
                for (i, &sq) in TABLES.line_squares[l].iter().enumerate() {
                    out[i] = (sq as usize / ROWS, sq as usize % ROWS);
                }
                return Some(out);
            }
        }
        None
    }

    /// Board score for player `p`: +-WIN_SCORE if a line is complete,
    /// otherwise the Java line-value sum plus the parity-weighted threat
    /// values. The heuristic part is clamped strictly below WIN_SCORE - an
    /// invariant the search relies on: a score of exactly +-1000 always
    /// means a *proven* win/loss, which drives the forced-win banner, the
    /// engine's resignation, and the early exit of iterative deepening.
    /// (Negamax convention: the score is from `p`'s own point of view.)
    #[inline(always)]
    pub fn score_for(&self, p: Piece) -> i32 {
        if self.has_won(Piece::Red) {
            return p.sign() * WIN_SCORE;
        }
        if self.has_won(Piece::Yellow) {
            return -p.sign() * WIN_SCORE;
        }
        p.sign() * (self.score + self.threats).clamp(-(WIN_SCORE - 1), WIN_SCORE - 1)
    }

    /// Unique 49-bit position key for the side to move (Pascal Pons'
    /// encoding):
    ///
    /// ```text
    ///   key = stones(mover) + occupancy + bottom
    /// ```
    ///
    /// Why this is collision-free: within each 7-bit column,
    /// `occupancy + bottom_bit` is a chain of carries that leaves a single
    /// 1 exactly *on top* of the stack - so the highest set bit of a
    /// column's key value is its height, and the bits below it are the
    /// mover's stones (the opponent's are the occupied rest). Height,
    /// ownership and (via total stone count) the side to move are all
    /// recoverable, hence the mapping is invertible - `bookview` does
    /// exactly that inversion. The per-column guard bit absorbs the carry
    /// of a full column. Used by the transposition table and as the line
    /// format of the opening/corrective books.
    #[inline(always)]
    pub fn key(&self, to_move: Piece) -> u64 {
        self.bb[to_move.idx()] + (self.bb[0] | self.bb[1]) + BOTTOM_MASK
    }

    pub fn bitboard(&self, p: Piece) -> u64 {
        self.bb[p.idx()] & BOARD_MASK
    }
}

/// Transposition table entry (16 bytes): the full key (no collisions), the
/// score from the mover's point of view, the searched depth below the node
/// and what kind of alpha/beta bound the score is, plus the best column
/// for move ordering.
///
/// Why *bounds* and not just scores: alpha/beta rarely computes exact
/// values. A node that failed high only proved "score >= x" (TT_LOWER: the
/// search was cut off - the truth may be even better); a node where no
/// child beat alpha only proved "score <= x" (TT_UPPER); only a score that
/// landed strictly inside the window is exact (TT_EXACT). On probe, a
/// bound is usable when it already decides the current window - e.g. a
/// lower bound >= beta re-triggers the same cutoff without any search.
#[derive(Clone, Copy, Default)]
struct TtEntry {
    key: u64,
    score: i16,
    depth: u8,
    flag: u8,
    best: u8,
}

const TT_EXACT: u8 = 0;
const TT_LOWER: u8 = 1;
const TT_UPPER: u8 = 2;

/// Transposition table: memoizes search results so positions reached by
/// different move orders (transpositions) are searched once. The game tree
/// is really a graph - red d1/yellow b1/red e1 meets red e1/yellow b1/red
/// d1 - and without a table each such meeting point is searched from
/// scratch every time.
///
/// Sized 2^22 entries (64 MB), indexed by the low key bits, verified by
/// the full key; always-replace on store (simplest scheme, and measured
/// good enough here). An entry with key 0 is empty (a real key is never 0
/// thanks to BOTTOM_MASK). Note from this repo's own measurements: with
/// wide search windows the table alone bought only ~5 % - it is the
/// combination with zero-window search (PVS + MTD(f)) that makes the
/// stored bounds constantly reusable.
pub struct TransTable {
    entries: Vec<TtEntry>,
}

impl Default for TransTable {
    fn default() -> Self {
        Self::new()
    }
}

impl TransTable {
    pub fn new() -> TransTable {
        Self::with_log2_entries(22)
    }

    /// Table with 2^n entries (16 bytes each). n = 0 effectively disables
    /// the table (used for measurements).
    pub fn with_log2_entries(n: usize) -> TransTable {
        TransTable { entries: vec![TtEntry::default(); 1 << n] }
    }

    #[inline(always)]
    fn idx(&self, key: u64) -> usize {
        key as usize & (self.entries.len() - 1)
    }
}

/// Search statistics / live progress, readable from another thread.
#[derive(Default)]
pub struct SearchStats {
    pub nodes: AtomicU64,
    /// Depth of the iteration currently being searched.
    pub depth: AtomicU64,
}

pub struct SearchResult {
    pub col: Option<usize>,
    pub score: i32,
    /// Depth of the deepest completed iteration.
    pub depth: usize,
    pub nodes: u64,
}

pub struct Searcher<'a> {
    max_depth: usize,
    stats: &'a SearchStats,
    tt: &'a mut TransTable,
    pub probes: u64,
    pub key_hits: u64,
    pub cut_hits: u64,
    nodes: u64,
    /// Hard deadline: if an iteration is still running past it, abort it and
    /// keep the previous iteration's result.
    deadline: Instant,
    aborted: bool,
}

impl<'a> Searcher<'a> {
    /// Compute the best move for `p` on `board`: the top-level driver.
    ///
    /// Iterative deepening - searching depth 1, 2, 3, ... instead of one
    /// deep search - looks wasteful but is nearly free (each iteration
    /// costs ~2-4x the previous, so the shallow ones sum to a fraction of
    /// the last) and pays twice: the previous iteration's best column is
    /// tried first (dramatically better pruning), and its score seeds the
    /// next MTD(f) guess. It also gives clean time control: the result is
    /// always the deepest *completed* iteration.
    ///
    /// Budget rules: a new iteration only starts while less than a third
    /// of the budget is spent ("don't start what you can't finish" - the
    /// final iteration dominates the cost); a runaway iteration is aborted
    /// at 2x the budget and its partial result discarded. Deepening also
    /// stops on a proven win/loss (deeper search cannot change a proof)
    /// and when the remaining game is fully searched.
    pub fn best_move(board: &Board, p: Piece, budget: Duration, stats: &'a SearchStats, tt: &'a mut TransTable) -> SearchResult {
        let mut b = *board;
        let t0 = Instant::now();
        let remaining = COLS * ROWS - b.total();
        let mut s = Searcher { max_depth: 1, stats, tt, probes: 0, key_hits: 0, cut_hits: 0, nodes: 0, deadline: t0 + budget * 2, aborted: false };
        stats.nodes.store(0, Ordering::Relaxed);
        let mut best = SearchResult { col: None, score: 0, depth: 0, nodes: 0 };
        let mut first = None;
        let mut guess = 0;
        for depth in 1..=remaining.max(1) {
            s.max_depth = depth;
            stats.depth.store(depth as u64, Ordering::Relaxed);
            let (col, score) = s.mtdf(&mut b, p, guess, first);
            if s.aborted {
                break;
            }
            guess = score;
            best = SearchResult { col, score, depth, nodes: s.nodes };
            first = col;
            // Proven win/loss or remaining game fully searched: deeper is pointless.
            if score == WIN_SCORE || score == -WIN_SCORE || depth >= remaining {
                break;
            }
            if t0.elapsed() * 3 > budget {
                break;
            }
        }
        best.nodes = s.nodes;
        best
    }

    /// Deterministic variant of `best_move`: the same iterative deepening
    /// with MTD(f), but to a fixed maximum depth with no clock - so the
    /// result depends only on the position (and table state), not timing.
    /// Used by the corrective-book audit in `bookgen`.
    #[allow(dead_code)]
    pub fn best_move_to_depth(board: &Board, p: Piece, max_depth: usize, stats: &'a SearchStats, tt: &'a mut TransTable) -> SearchResult {
        let mut b = *board;
        let remaining = COLS * ROWS - b.total();
        let mut s = Searcher {
            max_depth: 1,
            stats,
            tt,
            probes: 0,
            key_hits: 0,
            cut_hits: 0,
            nodes: 0,
            deadline: Instant::now() + Duration::from_secs(3600),
            aborted: false,
        };
        let mut best = SearchResult { col: None, score: 0, depth: 0, nodes: 0 };
        let mut first = None;
        let mut guess = 0;
        for depth in 1..=max_depth.min(remaining).max(1) {
            s.max_depth = depth;
            let (col, score) = s.mtdf(&mut b, p, guess, first);
            guess = score;
            best = SearchResult { col, score, depth, nodes: s.nodes };
            first = col;
            if score == WIN_SCORE || score == -WIN_SCORE {
                break;
            }
        }
        best
    }

    /// Single full-window search to exactly `depth` (no time limit, no
    /// iterative deepening). Used by tests.
    #[cfg(test)]
    pub fn fixed_depth(board: &Board, p: Piece, depth: usize, stats: &'a SearchStats, tt: &'a mut TransTable) -> i32 {
        let mut b = *board;
        let mut s = Searcher {
            max_depth: depth,
            stats,
            tt,
            probes: 0,
            key_hits: 0,
            cut_hits: 0,
            nodes: 0,
            deadline: Instant::now() + Duration::from_secs(3600),
            aborted: false,
        };
        s.root(&mut b, p, None).1
    }

    /// Zero-window search [beta-1, beta] over the root moves. Returns the
    /// fail-soft score and the best column (the proving move on a fail
    /// high, otherwise the move with the highest bound). "Fail-soft" means
    /// returning the actual best value found even when it falls outside
    /// the window - a tighter bound for the caller than clamping would be.
    fn zero_window_root(&mut self, b: &mut Board, p: Piece, beta: i32, first: Option<usize>) -> (Option<usize>, i32) {
        let mut s_max = -INF;
        let mut c_max = None;
        let order = first.into_iter().chain(COL_ORDER.iter().copied().filter(|&c| Some(c) != first));
        for c in order {
            if b.can_play(c) {
                b.make(c, p);
                let s = -self.negamax(b, p.other(), 1, -beta, -(beta - 1));
                b.unmake(c, p);
                if self.aborted {
                    return (c_max, s_max);
                }
                if s > s_max {
                    s_max = s;
                    c_max = Some(c);
                }
                if s_max >= beta {
                    break;
                }
            }
        }
        (c_max, s_max)
    }

    /// MTD(f) (Plaat et al.): find the exact minimax value using *only*
    /// zero-window searches. A zero-window search [beta-1, beta] is a
    /// cheap yes/no question - "is the true value >= beta?" - that fails
    /// either high (answer >= beta, a lower bound) or low (an upper
    /// bound). MTD(f) plays twenty-questions: start at `guess`, ask, use
    /// the answer to tighten a [lo, hi] bracket, re-ask at the new edge,
    /// until lo meets hi - typically 2-4 passes when the guess comes from
    /// the previous iteration.
    ///
    /// The payoff is synergy with the transposition table: every stored
    /// bound comes from the same kind of narrow window, so later passes
    /// (and later iterations) constantly re-hit usable entries. This
    /// combination is what doubled this engine's effective search speed -
    /// the table alone barely helped (see TransTable docs).
    fn mtdf(&mut self, b: &mut Board, p: Piece, mut guess: i32, first: Option<usize>) -> (Option<usize>, i32) {
        let (mut lo, mut hi) = (-INF, INF);
        let mut best = first;
        let mut fallback = None;
        for _ in 0..64 {
            if lo >= hi {
                break;
            }
            let beta = if guess == lo { guess + 1 } else { guess };
            let (c, g) = self.zero_window_root(b, p, beta, best);
            if self.aborted {
                return (best.or(fallback).or(c), guess);
            }
            fallback = c.or(fallback);
            guess = g;
            if g < beta {
                hi = g;
            } else {
                lo = g;
                best = c; // proving move of the fail high
            }
        }
        (best.or(fallback), guess)
    }

    /// Full-window root search. Only used by tests (fixed_depth); the
    /// engine itself searches via mtdf().
    #[cfg(test)]
    fn root(&mut self, b: &mut Board, p: Piece, first: Option<usize>) -> (Option<usize>, i32) {
        let mut alpha = -INF;
        let beta = INF;
        let mut s_max = -INF;
        let mut c_max = None;
        // Best column of the previous iteration first, then the static order.
        let order = first.into_iter().chain(COL_ORDER.iter().copied().filter(|&c| Some(c) != first));
        for c in order {
            if b.can_play(c) {
                b.make(c, p);
                let s = -self.negamax(b, p.other(), 1, -beta, -alpha);
                b.unmake(c, p);
                if self.aborted {
                    return (c_max, s_max);
                }
                if s > s_max {
                    s_max = s;
                    c_max = Some(c);
                }
                if s > alpha {
                    alpha = s;
                }
            }
        }
        (c_max, s_max)
    }

    /// The heart of the search: depth-limited alpha/beta in *negamax*
    /// form. Negamax exploits the zero-sum symmetry min(a,b) = -max(-a,-b)
    /// so both players run the same maximizing code: every position is
    /// scored from the side to move's view, and a child's result comes
    /// back negated - "my best is the negation of your best reply". The
    /// [alpha, beta] window travels along as (-beta, -alpha) for the same
    /// reason: my lower bound is your upper bound, negated.
    ///
    /// alpha = best score I can already guarantee (raise it as children
    /// improve); beta = best the opponent will allow (inherited); once
    /// alpha passes beta the opponent would never steer into this node, so
    /// the remaining children are skipped - the *beta cutoff* that gives
    /// alpha/beta its power, and the reason move ordering (TT best move,
    /// centre-first) matters so much.
    #[inline]
    fn negamax(&mut self, b: &mut Board, p: Piece, depth: usize, mut alpha: i32, beta: i32) -> i32 {
        self.nodes += 1;
        if self.nodes & 0xFFFF == 0 {
            self.stats.nodes.store(self.nodes, Ordering::Relaxed);
            if Instant::now() >= self.deadline {
                self.aborted = true;
            }
        }
        if self.aborted {
            return 0;
        }
        let s = b.score_for(p);
        if b.is_full() || depth >= self.max_depth || s == WIN_SCORE || s == -WIN_SCORE {
            return s;
        }

        // Transposition table probe. A stored result is reusable when it
        // searched at least as deep below this position as we are about to
        // (in Connect Four the stone count fixes the ply, so "same
        // position" also means "same distance from the root" - entries
        // from deeper *iterations* are the ones with more depth). Then:
        // exact score -> done; lower bound >= beta -> the cutoff happens
        // again without searching; upper bound <= alpha -> hopeless here
        // too; otherwise the bound still narrows the window. Even a
        // too-shallow entry contributes its best column for move ordering.
        let remaining = (self.max_depth - depth) as u8;
        let key = b.key(p);
        let idx = self.tt.idx(key);
        let e = self.tt.entries[idx];
        let mut beta = beta;
        let mut tt_col = None;
        self.probes += 1;
        if e.key == key {
            self.key_hits += 1;
            tt_col = Some(e.best as usize);
            if e.depth >= remaining {
                let es = e.score as i32;
                match e.flag {
                    TT_EXACT => {
                        self.cut_hits += 1;
                        return es;
                    }
                    TT_LOWER => {
                        if es >= beta {
                            self.cut_hits += 1;
                            return es;
                        }
                        alpha = alpha.max(es);
                    }
                    _ => {
                        if es <= alpha {
                            self.cut_hits += 1;
                            return es;
                        }
                        beta = beta.min(es);
                    }
                }
            }
        }
        let alpha0 = alpha;

        // Principal variation search. Bet on the move ordering: the first
        // (best-ordered) child is searched with the full window and is
        // expected to be the best; every sibling then only gets the
        // zero-width question "are you better than what we have?"
        // [alpha, alpha+1] - much cheaper, because narrow windows cut
        // early everywhere below. When a sibling surprisingly answers
        // "yes" (score lands in (alpha, beta)), the bet is lost and that
        // child is re-searched with the honest window. Zero-window
        // results are pure bounds - exactly the currency the transposition
        // table trades in, which is why PVS and the TT reinforce each
        // other.
        let mut s_max = -INF;
        let mut best_c = 0;
        let order = tt_col.into_iter().chain(COL_ORDER.iter().copied().filter(|&c| Some(c) != tt_col));
        for c in order {
            if b.can_play(c) {
                b.make(c, p);
                let s = if s_max == -INF {
                    -self.negamax(b, p.other(), depth + 1, -beta, -alpha)
                } else {
                    let s = -self.negamax(b, p.other(), depth + 1, -alpha - 1, -alpha);
                    if s > alpha && s < beta && !self.aborted {
                        -self.negamax(b, p.other(), depth + 1, -beta, -s)
                    } else {
                        s
                    }
                };
                b.unmake(c, p);
                if self.aborted {
                    return 0;
                }
                if s > s_max {
                    s_max = s;
                    best_c = c;
                }
                if s > alpha {
                    alpha = s;
                    if alpha > beta {
                        break;
                    }
                }
            }
        }

        // Store the fail-soft result with its bound type (classified
        // against the window this node actually searched): score below the
        // original alpha -> we only proved an upper bound; score past beta
        // -> only a lower bound (cutoff); in between -> exact. Always
        // replace - simple, and good enough at this table size.
        let flag = if s_max <= alpha0 {
            TT_UPPER
        } else if s_max > beta {
            TT_LOWER
        } else {
            TT_EXACT
        };
        self.tt.entries[idx] = TtEntry { key, score: s_max as i16, depth: remaining, flag, best: best_c as u8 };
        s_max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Reference evaluation: straight port of the Java full scan.
    fn java_score(b: &Board, p: Piece) -> i32 {
        let mut s = 0;
        for l in 0..NLINES {
            let mut v = 0i32;
            for &sq in TABLES.line_squares[l].iter() {
                let (c, r) = (sq as usize / ROWS, sq as usize % ROWS);
                let f = b.get(c, r).map(|p| p.sign()).unwrap_or(0);
                if v * f < 0 {
                    v = 0;
                    break;
                }
                v += f;
            }
            if v == 4 || v == -4 {
                return p.sign() * v * WIN_SCORE / 4;
            }
            s += v;
        }
        p.sign() * s
    }

    #[test]
    fn tables_consistent() {
        for sq in 0..COLS * ROWS {
            let n = TABLES.sq_lines[sq].iter().take_while(|&&l| l != 0xFF).count();
            assert!(n > 0 && n <= MAX_LINES_PER_SQ);
        }
    }

    /// Reference threat evaluation: full scan over all lines, then the
    /// column rule (lowest threat square decides, shared squares go to the
    /// side whose parity matches).
    fn threat_ref(b: &Board) -> i32 {
        let mut at = [[false; 2]; COLS * ROWS];
        for l in 0..NLINES {
            let (mut red, mut yellow, mut empty, mut esq) = (0, 0, 0, 0);
            for &sq in TABLES.line_squares[l].iter() {
                let (c, r) = (sq as usize / ROWS, sq as usize % ROWS);
                match b.get(c, r) {
                    Some(Piece::Red) => red += 1,
                    Some(Piece::Yellow) => yellow += 1,
                    None => {
                        empty += 1;
                        esq = sq as usize;
                    }
                }
            }
            if empty == 1 && (red == 3 || yellow == 3) {
                at[esq][if red == 3 { 0 } else { 1 }] = true;
            }
        }
        let mut t = 0;
        for col in 0..COLS {
            for r in 0..ROWS {
                let sq = col * ROWS + r;
                let (red, yellow) = (at[sq][0], at[sq][1]);
                if red || yellow {
                    let pi = if red && yellow {
                        if r % 2 == 0 { 0 } else { 1 }
                    } else if red {
                        0
                    } else {
                        1
                    };
                    t += threat_value(pi, sq);
                    break;
                }
            }
        }
        t
    }

    #[test]
    fn incremental_matches_full_scan() {
        // pseudo random playouts
        let mut seed = 12345u64;
        for _ in 0..2000 {
            let mut b = Board::new();
            let mut p = Piece::Red;
            let mut hist = [(0usize, Piece::Red); 42];
            let mut n = 0;
            loop {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let c = (seed % COLS as u64) as usize;
                if !b.can_play(c) {
                    if b.is_full() {
                        break;
                    }
                    continue;
                }
                b.make(c, p);
                hist[n] = (c, p);
                n += 1;
                assert_eq!(b.threat_score(), threat_ref(&b));
                if !b.has_won(p) {
                    assert_eq!(b.raw_score(), java_score(&b, Piece::Red));
                    assert_eq!(b.score_for(Piece::Red), (java_score(&b, Piece::Red) + threat_ref(&b)).clamp(-999, 999));
                    assert_eq!(b.score_for(Piece::Yellow), -b.score_for(Piece::Red));
                } else {
                    assert_eq!(b.score_for(p), WIN_SCORE);
                }
                assert_eq!(b.has_won(p), b.winning_line().is_some());
                if b.has_won(p) {
                    break;
                }
                p = p.other();
            }
            while n > 0 {
                n -= 1;
                let (c, p) = hist[n];
                b.unmake(c, p);
                assert_eq!(b.threat_score(), threat_ref(&b));
            }
            assert_eq!(b.raw_score(), 0);
            assert_eq!(b.threat_score(), 0);
        }
    }

    #[test]
    fn finds_immediate_win_and_block() {
        let stats = SearchStats::default();
        let mut tt = TransTable::new();
        let mut b = Board::new();
        for _ in 0..3 {
            b.make(0, Piece::Red);
            b.make(1, Piece::Yellow);
        }
        let r = Searcher::best_move(&b, Piece::Red, Duration::from_millis(50), &stats, &mut tt);
        assert_eq!(r.col, Some(0));
        assert_eq!(r.score, WIN_SCORE);
        let r = Searcher::best_move(&b, Piece::Yellow, Duration::from_millis(50), &stats, &mut tt);
        assert_eq!(r.col, Some(1)); // yellow wins itself first
        b.unmake(1, Piece::Yellow);
        b.make(2, Piece::Yellow);
        let r = Searcher::best_move(&b, Piece::Yellow, Duration::from_millis(50), &stats, &mut tt);
        assert_eq!(r.col, Some(0)); // must block
    }

    /// Reference negamax without a transposition table (the pre-TT search).
    fn plain_negamax(b: &mut Board, p: Piece, depth: usize) -> i32 {
        let s = b.score_for(p);
        if b.is_full() || depth == 0 || s == WIN_SCORE || s == -WIN_SCORE {
            return s;
        }
        let mut s_max = -INF;
        for &c in COL_ORDER.iter() {
            if b.can_play(c) {
                b.make(c, p);
                let s = -plain_negamax(b, p.other(), depth - 1);
                b.unmake(c, p);
                s_max = s_max.max(s);
            }
        }
        s_max
    }

    /// For searches to the end of the game the table must not change the
    /// result: compare against a plain no-TT negamax on random late-game
    /// positions. (At partial depth TT reuse may legitimately substitute
    /// deeper results, so equality only holds for solved-to-the-end runs.)
    #[test]
    fn tt_matches_plain_search_to_the_end() {
        let stats = SearchStats::default();
        let mut tt = TransTable::new();
        let mut seed = 24680u64;
        let mut tested = 0;
        while tested < 20 {
            let mut b = Board::new();
            let mut p = Piece::Red;
            let mut ok = true;
            while b.total() < 31 {
                seed ^= seed << 13;
                seed ^= seed >> 7;
                seed ^= seed << 17;
                let c = (seed % COLS as u64) as usize;
                if b.can_play(c) {
                    b.make(c, p);
                    if b.has_won(p) {
                        ok = false;
                        break;
                    }
                    p = p.other();
                }
            }
            if !ok {
                continue;
            }
            tested += 1;
            let remaining = COLS * ROWS - b.total();
            let want = plain_negamax(&mut b.clone(), p, remaining);
            let got = Searcher::fixed_depth(&b, p, remaining, &stats, &mut tt);
            assert_eq!(got, want, "position {tested} (total {})", b.total());
        }
    }

    /// Odd-row threats are strong for red, even-row threats weak.
    #[test]
    fn threat_parity_weights() {
        // Red c2r1, c3r1, c4r1: two horizontal threats with empty squares on
        // row 1 (odd) - the good parity for red.
        let mut b = Board::new();
        b.make(1, Piece::Red);
        b.make(2, Piece::Red);
        b.make(3, Piece::Red);
        assert_eq!(b.threat_score(), 2 * THREAT_GOOD);
        // Red stacked c1r1-r3: one vertical threat with the empty square on
        // row 4 (even) - the wrong parity for red.
        let mut b = Board::new();
        for _ in 0..3 {
            b.make(0, Piece::Red);
        }
        assert_eq!(b.threat_score(), THREAT_WEAK);
        // Mirrored for yellow: even rows are yellow's good parity.
        let mut b = Board::new();
        for _ in 0..3 {
            b.make(0, Piece::Yellow);
        }
        assert_eq!(b.threat_score(), -THREAT_GOOD);
    }

    /// The recorded game the engine lost to Claude (red, with hints; see
    /// README "Results"). Red wins with 2-3-4-5 on row 3.
    pub const RECORDED_LOSS: [usize; 39] = [4, 4, 4, 4, 5, 6, 3, 2, 5, 4, 5, 5, 5, 4, 1, 5, 3, 1, 3, 3, 6, 6, 6, 7, 7, 1, 1, 1, 7, 1, 7, 7, 7, 6, 6, 3, 3, 2, 2];

    fn replay(moves: &[usize]) -> Board {
        let mut b = Board::new();
        let mut p = Piece::Red;
        for &c in moves {
            b.make(c - 1, p);
            p = p.other();
        }
        b
    }

    /// Would more think time have saved the engine in the recorded loss?
    /// History of this test: before the transposition table + MTD(f), a
    /// 10 s search at ply 18 still played the recorded losing move with a
    /// healthy score (+4 at depth 19) - the loss was beyond its horizon.
    /// With them it reaches depth 20 there, evaluates the position as
    /// slightly bad and (on this machine) deviates to column 7. The proven
    /// loss at ply 20 is found almost instantly either way. The assertions
    /// stick to what is machine-independent: the ply-18 horizon (deep but
    /// not proven) and the ply-20 proof.
    #[test]
    fn think_time_and_the_recorded_loss() {
        let budget = Duration::from_secs(10);
        let stats = SearchStats::default();
        let mut tt = TransTable::new();
        assert!(replay(&RECORDED_LOSS).has_won(Piece::Red));
        // Before the recorded ply 18 (yellow to move): deep, but the loss is
        // not yet provable.
        let r = Searcher::best_move(&replay(&RECORDED_LOSS[..17]), Piece::Yellow, budget, &stats, &mut tt);
        assert!(r.depth >= 19, "10 s search only reached depth {}", r.depth);
        assert!(r.score > -WIN_SCORE, "10 s search saw the loss at ply 18 (score {} depth {})", r.score, r.depth);
        // Before the recorded ply 20: the loss is proven within 10 s.
        let r = Searcher::best_move(&replay(&RECORDED_LOSS[..19]), Piece::Yellow, budget, &stats, &mut tt);
        assert_eq!(r.score, -WIN_SCORE, "loss not proven at ply 20 (score {} depth {})", r.score, r.depth);
    }

    /// Endgame from a real game (16 empties, red to move and lost by
    /// zugzwang): iterative deepening must reach the proven result in budget.
    #[test]
    fn proves_endgame_win() {
        let stats = SearchStats::default();
        let mut tt = TransTable::new();
        let mut b = Board::new();
        let mut p = Piece::Red;
        for c in [4, 4, 4, 4, 3, 2, 5, 6, 3, 4, 5, 4, 3, 3, 5, 5, 5, 5, 7, 7, 1, 1, 7, 3, 1, 3] {
            b.make(c - 1, p);
            p = p.other();
        }
        let r = Searcher::best_move(&b, Piece::Red, Duration::from_secs(2), &stats, &mut tt);
        assert_eq!(r.score, -WIN_SCORE, "depth {} nodes {}", r.depth, r.nodes);
        b.make(6, Piece::Red);
        let r = Searcher::best_move(&b, Piece::Yellow, Duration::from_secs(2), &stats, &mut tt);
        assert_eq!(r.score, WIN_SCORE, "depth {} nodes {}", r.depth, r.nodes);
    }
}

#[cfg(test)]
mod bench {
    use super::*;

    /// Analysis helper: replay the recorded winning game and search every
    /// engine-to-move position with a 10 s budget.
    #[test]
    #[ignore]
    fn analyse_recorded_win() {
        let stats = SearchStats::default();
        let mut tt = TransTable::new();
        let mut b = Board::new();
        let mut p = Piece::Red;
        for (i, &c) in tests::RECORDED_LOSS.iter().enumerate() {
            if p == Piece::Yellow {
                let t = std::time::Instant::now();
                let r = Searcher::best_move(&b, Piece::Yellow, std::time::Duration::from_secs(10), &stats, &mut tt);
                eprintln!(
                    "ply {:2} yellow to move: 10s search depth {:2} score {:5} col {:?} (recorded {})  [{} ms]",
                    i + 1, r.depth, r.score, r.col.map(|c| c + 1), c, t.elapsed().as_millis()
                );
            }
            b.make(c - 1, p);
            p = p.other();
        }
        assert!(b.has_won(Piece::Red));
    }
    #[test]
    #[ignore]
    fn bench_search() {
        let stats = SearchStats::default();
        let mut b = Board::new();
        b.make(3, Piece::Red);
        b.make(3, Piece::Yellow);
        b.make(2, Piece::Red);
        let mut tt = TransTable::new();
        for (label, ms) in [("cold  500", 500u64), ("cold 2000", 2000), ("warm 2000", 2000)] {
            if label.starts_with("cold") {
                tt = TransTable::new();
            }
            let t = std::time::Instant::now();
            let r = Searcher::best_move(&b, Piece::Yellow, Duration::from_millis(ms), &stats, &mut tt);
            let el = t.elapsed().as_millis().max(1);
            eprintln!(
                "{label} ms: depth {} col {:?} score {} nodes {} {} ms = {:.1} Mn/s",
                r.depth, r.col, r.score, r.nodes, el, r.nodes as f64 / el as f64 / 1000.0
            );
        }
    }
}
