//! LLM ASSISTANCE - NOT PART OF THE ENGINE.
//!
//! Purpose: when an LLM plays through the control socket / MCP server, its
//! mistakes are mostly bookkeeping (miscounting a column's height, missing a
//! vertical three), not strategy. These hints take that bookkeeping away so a
//! model's *playing strength* can be compared with and without them.
//!
//! They are optional (`Game.hints`, toggled with `H` in the GUI, `--hints` on
//! the command line, or `{"cmd":"hints","on":bool}` on the socket) and only
//! affect the JSON state returned to clients. The engine never uses them.
//!
//! The design premise, measured in this repo's games: an LLM loses mostly
//! to *bookkeeping* slips (miscounted column heights, an overlooked
//! vertical three), not to strategy. These hints remove exactly that class
//! of error and nothing more, so games with hints measure a model's
//! planning rather than its counting.
//!
//! Everything here is a one-ply tactical lookup on the bitboard:
//!   * next free row per column,
//!   * columns where the side to move wins immediately,
//!   * columns the opponent would win in next move (must be blocked),
//!   * columns that hand the opponent an immediate win (playing there lets
//!     the opponent win on the next move - including not blocking a threat).

use crate::engine::{Board, Piece, COLS, ROWS};
use serde::Serialize;

#[derive(Clone, Serialize)]
pub struct Hints {
    /// Next free row (1 = bottom) per column 1..7, null if the column is full.
    pub next_row: [Option<usize>; COLS],
    /// Columns (1..7) where the side to move completes four immediately.
    pub winning_moves: Vec<usize>,
    /// Columns (1..7) where the opponent would complete four on their next
    /// move; the side to move must play one of these (unless it can win).
    pub must_block: Vec<usize>,
    /// Columns (1..7) that lose at once: after playing there the opponent has
    /// an immediate win somewhere (typically on the square just above).
    pub losing_moves: Vec<usize>,
}

/// Columns where `p` wins immediately on `b`.
fn immediate_wins(b: &Board, p: Piece) -> Vec<usize> {
    let mut b = *b;
    (0..COLS)
        .filter(|&c| {
            if !b.can_play(c) {
                return false;
            }
            b.make(c, p);
            let w = b.has_won(p);
            b.unmake(c, p);
            w
        })
        .collect()
}

pub fn compute(board: &Board, to_move: Piece) -> Hints {
    let opp = to_move.other();
    let mut next_row = [None; COLS];
    for (c, slot) in next_row.iter_mut().enumerate() {
        let h = board.height(c);
        if h < ROWS {
            *slot = Some(h + 1);
        }
    }
    let winning_moves: Vec<usize> = immediate_wins(board, to_move).into_iter().map(|c| c + 1).collect();
    let must_block: Vec<usize> = immediate_wins(board, opp).into_iter().map(|c| c + 1).collect();
    let mut b = *board;
    let losing_moves = (0..COLS)
        .filter(|&c| {
            if !b.can_play(c) {
                return false;
            }
            b.make(c, to_move);
            let lost = !b.has_won(to_move) && !immediate_wins(&b, opp).is_empty();
            b.unmake(c, to_move);
            lost
        })
        .map(|c| c + 1)
        .collect();
    Hints { next_row, winning_moves, must_block, losing_moves }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_win_block_and_losing() {
        let mut b = Board::new();
        // red: col 1 x3, yellow: col 2 x2, yellow col 3 x1
        for _ in 0..3 {
            b.make(0, Piece::Red);
            b.make(1, Piece::Yellow);
        }
        b.unmake(1, Piece::Yellow);
        b.make(2, Piece::Yellow);
        let h = compute(&b, Piece::Yellow);
        assert_eq!(h.next_row, [Some(4), Some(3), Some(2), Some(1), Some(1), Some(1), Some(1)]);
        assert!(h.winning_moves.is_empty());
        assert_eq!(h.must_block, vec![1]);
        // every move except blocking col 1 loses
        assert_eq!(h.losing_moves, vec![2, 3, 4, 5, 6, 7]);
        let h = compute(&b, Piece::Red);
        assert_eq!(h.winning_moves, vec![1]);
    }

    #[test]
    fn losing_move_on_top_of_threat() {
        // red 2,3,4 in row 1 with 1 and 5 open: yellow must block at 1 or 5;
        // anything else (e.g. 4) loses immediately.
        let mut b = Board::new();
        b.make(1, Piece::Red);
        b.make(1, Piece::Yellow);
        b.make(2, Piece::Red);
        b.make(2, Piece::Yellow);
        b.make(3, Piece::Red);
        let h = compute(&b, Piece::Yellow);
        assert_eq!(h.must_block, vec![1, 5]);
        assert!(h.losing_moves.contains(&4));
    }
}
