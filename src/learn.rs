//! Learn from a finished game: find the plies where the engine threw away
//! a win or a draw (the solver is ground truth), and turn each one into a
//! corrective-book entry - written to `corrective-book.txt` with a
//! provenance comment and inserted into the live book, so the fix is
//! active immediately. Triggered from the GUI (`L` key, with confirmation)
//! or the socket (`{"cmd":"learn"}`); the solver command comes from
//! `--tutor`, the `C4_SOLVER` env var or the `--solver` seat command.

use crate::book::Book;
use crate::engine::{Board, Piece};
use crate::solver::ExternalSolver;
use std::io::Write;
use std::sync::Mutex;

/// Analyze `history` (engine playing `engine_side`) and book every ply
/// where the engine's move lost the theoretical win or draw. `progress` is
/// called with status lines for a live display; the returned string is the
/// final report.
pub fn learn_from(
    history: &[usize],
    engine_side: Piece,
    solver_cmd: &str,
    book: &Mutex<Option<Book>>,
    progress: &dyn Fn(String),
    on_spawn: &dyn Fn(u32),
    cancelled: &dyn Fn() -> bool,
) -> String {
    let mut sv = match ExternalSolver::spawn(solver_cmd) {
        Ok(sv) => sv,
        Err(e) => return format!("learn failed: {e}"),
    };
    on_spawn(sv.pid());
    let mut report = Vec::new();
    let mut b = Board::new();
    let mut side = Piece::Red;
    for (n, &col) in history.iter().enumerate() {
        // Audit positions where the engine was to move and made a
        // non-final move (a move that ended the game won or was forced).
        if side == engine_side && !b.is_full() {
            if cancelled() {
                return "learning canceled".into();
            }
            progress(format!("analyzing ply {} of {} ...", n + 1, history.len()));
            let scores = match sv.analyze(&history[..n]) {
                Ok(s) => s,
                // A killed solver (cancel) surfaces as a read error.
                Err(e) if cancelled() => {
                    let _ = e;
                    return "learning canceled".into();
                }
                Err(e) => return format!("learn failed at ply {}: {e}", n + 1),
            };
            let best = match ExternalSolver::pick(&scores) {
                Ok(c) => c,
                Err(e) => return format!("learn failed at ply {}: {e}", n + 1),
            };
            let (verdict, played) = (scores[best], scores[col]);
            let threw = if verdict > 0 && played <= 0 {
                Some(if played == 0 { "win (to a draw)" } else { "win" })
            } else if verdict == 0 && played < 0 {
                Some("draw")
            } else {
                None
            };
            if let Some(what) = threw {
                let key = b.key(engine_side);
                let mut bk = book.lock().unwrap();
                let known = bk.as_ref().is_some_and(|b| b.contains(key));
                if known {
                    report.push(format!("ply {}: engine threw the {what}, but the book already covers it", n + 1));
                } else {
                    let hist1: Vec<String> = history.iter().map(|c| (c + 1).to_string()).collect();
                    let line = format!(
                        "{key:x} {} {verdict}   # learned from {} (threw {what} at ply {})",
                        best + 1, hist1.join(","), n + 1
                    );
                    match std::fs::OpenOptions::new().create(true).append(true).open("corrective-book.txt") {
                        Ok(mut f) => {
                            let _ = writeln!(f, "{line}");
                        }
                        Err(e) => return format!("learn failed: cannot write corrective-book.txt: {e}"),
                    }
                    match bk.as_mut() {
                        Some(b) => b.insert(key, best, verdict),
                        None => {
                            let mut nb = Book::load(std::path::Path::new("corrective-book.txt")).unwrap_or_else(|_| Book::empty());
                            nb.insert(key, best, verdict);
                            *bk = Some(nb);
                        }
                    }
                    report.push(format!(
                        "ply {}: engine threw the {what} (played {}, only {} keeps it) - booked",
                        n + 1, (b'a' + col as u8) as char, (b'a' + best as u8) as char
                    ));
                }
            }
        }
        b.make(col, side);
        if b.has_won(side) {
            break;
        }
        side = side.other();
    }
    if report.is_empty() {
        "no engine mistake found - the opponent won on merit (or the engine was lost from the start)".to_string()
    } else {
        report.join("\n")
    }
}
