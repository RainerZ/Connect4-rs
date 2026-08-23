//! Connect4 engine: bitboards, incremental board evaluation and negamax with
//! alpha/beta pruning. Faithful port of the Java `Connect4AiPlayer` /
//! `Connect4Board` evaluation semantics, but with O(1) make/unmake and an
//! O(#lines-through-square) score update instead of a full re-scan.
//!
//! No heap allocation anywhere in this module: all state lives in fixed-size
//! arrays (the module is `no_std`-compatible apart from `AtomicU64`).

use std::sync::atomic::{AtomicU64, Ordering};

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

    pub fn bitboard(&self, p: Piece) -> u64 {
        self.bb[p.idx()] & BOARD_MASK
    }
}

/// Search statistics / live progress, readable from another thread.
#[derive(Default)]
pub struct SearchStats {
    pub nodes: AtomicU64,
    pub depth: AtomicU64,
}

pub struct SearchResult {
    pub col: Option<usize>,
    pub score: i32,
    pub depth: usize,
    pub nodes: u64,
}

pub struct Searcher<'a> {
    max_depth: usize,
    stats: &'a SearchStats,
    nodes: u64,
}

impl<'a> Searcher<'a> {
    /// Depth heuristic ported from Java `setOptimalMaxDepth`: go deeper as
    /// columns fill up.
    pub fn optimal_depth(board: &Board, initial: usize) -> usize {
        let full = (0..COLS).filter(|&c| board.height(c) >= ROWS).count();
        let mut d = initial;
        match full {
            0 | 1 => {
                if board.total() > 16 {
                    d += 1
                }
            }
            2 => d += 2,
            _ => d = 18,
        }
        let remaining = COLS * ROWS - board.total();
        if d > remaining {
            d = remaining;
        }
        d
    }

    /// Compute the best move for `p` on `board`.
    pub fn best_move(board: &Board, p: Piece, initial_depth: usize, stats: &'a SearchStats) -> SearchResult {
        let mut b = *board;
        let max_depth = Self::optimal_depth(&b, initial_depth);
        let mut s = Searcher { max_depth, stats, nodes: 0 };
        stats.depth.store(max_depth as u64, Ordering::Relaxed);
        stats.nodes.store(0, Ordering::Relaxed);
        let (mut col, mut score) = s.root(&mut b, p);
        // Java fallback: a forced loss was found - search shallow so we at
        // least do not lose immediately (opponent might blunder).
        if score == -WIN_SCORE && max_depth != 2 && col.is_some() {
            s.max_depth = 2;
            stats.depth.store(2, Ordering::Relaxed);
            let (c2, s2) = s.root(&mut b, p);
            col = c2;
            score = s2;
        }
        SearchResult { col, score, depth: s.max_depth, nodes: s.nodes }
    }

    fn root(&mut self, b: &mut Board, p: Piece) -> (Option<usize>, i32) {
        let mut alpha = -INF;
        let beta = INF;
        let mut s_max = -INF;
        let mut c_max = None;
        for &c in COL_ORDER.iter() {
            if b.can_play(c) {
                b.make(c, p);
                let s = -self.negamax(b, p.other(), 1, -beta, -alpha);
                b.unmake(c, p);
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
        }
        let s = b.score_for(p);
        if b.is_full() || depth >= self.max_depth || s == WIN_SCORE || s == -WIN_SCORE {
            return s;
        }
        let mut s_max = -INF;
        for &c in COL_ORDER.iter() {
            if b.can_play(c) {
                b.make(c, p);
                let s = -self.negamax(b, p.other(), depth + 1, -beta, -alpha);
                b.unmake(c, p);
                if s > s_max {
                    s_max = s;
                }
                if s > alpha {
                    alpha = s;
                    if alpha > beta {
                        break;
                    }
                }
            }
        }
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
        let mut b = Board::new();
        for _ in 0..3 {
            b.make(0, Piece::Red);
            b.make(1, Piece::Yellow);
        }
        let r = Searcher::best_move(&b, Piece::Red, 4, &stats);
        assert_eq!(r.col, Some(0));
        assert_eq!(r.score, WIN_SCORE);
        let r = Searcher::best_move(&b, Piece::Yellow, 4, &stats);
        assert_eq!(r.col, Some(1)); // yellow wins itself first
        b.unmake(1, Piece::Yellow);
        b.make(2, Piece::Yellow);
        let r = Searcher::best_move(&b, Piece::Yellow, 4, &stats);
        assert_eq!(r.col, Some(0)); // must block
    }
}

#[cfg(test)]
mod bench {
    use super::*;
    #[test]
    #[ignore]
    fn bench_search() {
        let stats = SearchStats::default();
        let mut b = Board::new();
        b.make(3, Piece::Red);
        b.make(3, Piece::Yellow);
        b.make(2, Piece::Red);
        for depth in [10usize, 12, 14] {
            let t = std::time::Instant::now();
            let r = Searcher::best_move(&b, Piece::Yellow, depth, &stats);
            let ms = t.elapsed().as_millis().max(1);
            eprintln!("depth {depth}: col {:?} score {} nodes {} {} ms = {:.1} Mn/s", r.col, r.score, r.nodes, ms, r.nodes as f64 / ms as f64 / 1000.0);
        }
    }
}
