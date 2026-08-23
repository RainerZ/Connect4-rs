//! Connect4 engine: bitboards, incremental board evaluation and negamax with
//! alpha/beta pruning. Faithful port of the Java `Connect4AiPlayer` /
//! `Connect4Board` evaluation semantics, but with O(1) make/unmake and an
//! O(#lines-through-square) score update instead of a full re-scan.
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

/// Bitboard layout: bit index = col * 7 + row  (7 bits per column, top bit is
/// a guard so shifts never bleed into the next column).
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

/// Column search order (centre first) - helps alpha/beta a lot.
pub const COL_ORDER: [usize; COLS] = [3, 4, 2, 1, 5, 0, 6];

/// Number of possible 4-in-a-row lines on a 7x6 board.
pub const NLINES: usize = 69;
/// Maximum number of lines passing through a single square.
const MAX_LINES_PER_SQ: usize = 13;

/// For every square: the line indices passing through it (terminated by 0xFF).
struct LineTables {
    sq_lines: [[u8; MAX_LINES_PER_SQ]; COLS * ROWS],
    line_squares: [[u8; 4]; NLINES],
}

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

/// Line value exactly as the Java `Line.value()`: sum of field values if only
/// one colour is present in the line, otherwise 0.
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
    /// number of pieces per column
    height: [u8; COLS],
    total: u8,
    /// per line: number of red / yellow pieces
    counts: [[u8; 2]; NLINES],
    /// Sum of all line values (red positive). Identical to the Java
    /// `getBoardScore(board, +1)` unless a line is complete.
    score: i32,
}

impl Default for Board {
    fn default() -> Self {
        Self::new()
    }
}

impl Board {
    pub fn new() -> Board {
        Board { bb: [0, 0], height: [0; COLS], total: 0, counts: [[0; 2]; NLINES], score: 0 }
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

    /// Raw evaluation from red's point of view (no win check).
    #[allow(dead_code)]
    #[inline]
    pub fn raw_score(&self) -> i32 {
        self.score
    }

    /// Drop a piece. Caller must ensure `can_play(col)`.
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
            let c = &mut self.counts[l as usize];
            let before = line_value(c[0], c[1]);
            c[pi] += 1;
            delta += line_value(c[0], c[1]) - before;
        }
        self.score += delta;
    }

    /// Undo the last piece dropped into `col` (must be of colour `p`).
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
            let c = &mut self.counts[l as usize];
            let before = line_value(c[0], c[1]);
            c[pi] -= 1;
            delta += line_value(c[0], c[1]) - before;
        }
        self.score += delta;
    }

    /// True if `p` has four in a row.
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

    /// Board score for player `p` exactly as the Java `getBoardScore`.
    /// +-WIN_SCORE if a line is complete, otherwise the signed line-value sum.
    #[inline(always)]
    pub fn score_for(&self, p: Piece) -> i32 {
        if self.has_won(Piece::Red) {
            return p.sign() * WIN_SCORE;
        }
        if self.has_won(Piece::Yellow) {
            return -p.sign() * WIN_SCORE;
        }
        p.sign() * self.score
    }

    /// Unique 49-bit position key for the side to move (Pascal Pons'
    /// encoding): stones of the mover + occupancy + one bit per column. The
    /// carry of `mask + bottom` marks each column's stack top, which encodes
    /// the heights; the mover's stones encode ownership. The per-column
    /// guard bit absorbs the carry of a full column.
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
/// and what kind of alpha/beta bound the score is, plus the best column for
/// move ordering.
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
/// different move orders (transpositions) are searched once. Sized 2^22
/// entries (64 MB); always-replace on store. An entry with key 0 is empty
/// (a real key is never 0 thanks to BOTTOM_MASK).
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
    /// Compute the best move for `p` on `board` using iterative deepening
    /// within roughly `budget` wall-clock time. A new iteration is only
    /// started while less than a third of the budget is used (the last
    /// iteration dominates the cost); an iteration running past 2x the
    /// budget is aborted.
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
    /// high, otherwise the move with the highest bound).
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

    /// MTD(f): converge on the minimax value with a sequence of zero-window
    /// searches around `guess`. Zero windows keep the transposition table
    /// bounds maximally reusable; with a good guess (previous iteration's
    /// score) it converges in a few passes.
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

        // Transposition table probe: reuse the score of an at least as deep
        // search of this position if its bound allows, and remember the best
        // column for move ordering either way.
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

        // Principal variation search: the first (best-ordered) child gets
        // the full window, the rest a zero-width window - almost always a
        // cheap refutation, re-searched with the full window when it
        // unexpectedly improves alpha. Null-window results are pure bounds,
        // which is also what makes the transposition table effective.
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

        // Store fail-soft result with its bound type; always replace.
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
                assert_eq!(b.score_for(Piece::Red), java_score(&b, Piece::Red));
                assert_eq!(b.score_for(Piece::Yellow), java_score(&b, Piece::Yellow));
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
                assert_eq!(b.score_for(Piece::Red), java_score(&b, Piece::Red));
            }
            assert_eq!(b.raw_score(), 0);
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
