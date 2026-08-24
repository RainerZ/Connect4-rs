//! `versus`: automated match of the built-in engine against an external
//! solver (Pascal Pons protocol), used to measure how long the engine
//! holds the theoretical result at a given think time. The solver's raw
//! score after every engine move is ground truth: negative = the engine
//! (as first player) is still winning, 0 = it only holds a draw, positive
//! = it is lost.
//!
//!   cargo run --release --bin versus -- --solver /path/c4solver [--budgets 2,10] [--solver-starts]
//!
//! One game per budget; the solver process is kept alive for the whole
//! series so its transposition table stays warm.

#[allow(dead_code)]
mod book;
#[allow(dead_code)]
mod engine;
#[allow(dead_code)]
mod solver;

use book::Book;
use engine::{Board, Piece, SearchStats, Searcher, TransTable};
use solver::ExternalSolver;
use std::time::{Duration, Instant};

fn main() {
    let mut solver_cmd = None;
    let mut budgets = vec![2.0f64];
    let mut solver_starts = false;
    let mut book_path = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--solver" => solver_cmd = args.next(),
            "--budgets" => {
                budgets = args
                    .next()
                    .unwrap_or_default()
                    .split(',')
                    .filter_map(|b| b.parse::<f64>().ok())
                    .collect();
            }
            "--solver-starts" => solver_starts = true,
            "--book" => book_path = args.next(),
            _ => {
                eprintln!("usage: versus --solver <cmd> [--budgets 2,5,10] [--solver-starts] [--book <file>]");
                std::process::exit(2);
            }
        }
    }
    let Some(cmd) = solver_cmd else {
        eprintln!("--solver <cmd> is required");
        std::process::exit(2);
    };
    let mut sv = ExternalSolver::spawn(&cmd).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });
    let stats = SearchStats::default();
    let book = book_path.map(|p| {
        let b = Book::load(std::path::Path::new(&p)).unwrap_or_else(|e| {
            eprintln!("{e}");
            std::process::exit(2);
        });
        eprintln!("opening book: {} positions", b.len());
        b
    });

    for &budget in &budgets {
        let budget_d = Duration::from_secs_f64(budget);
        let mut b = Board::new();
        let mut tt = TransTable::new();
        let mut history: Vec<usize> = Vec::new();
        let engine_p = if solver_starts { Piece::Yellow } else { Piece::Red };
        let mut to_move = Piece::Red;
        // Plies during which the engine still held the win / at least a draw
        // (judged by the solver's raw score after the engine's move).
        let (mut held_win, mut held_draw) = (0usize, 0usize);
        let mut still_winning = !solver_starts;
        let mut still_drawing = true;
        println!("=== budget {budget} s, engine plays {:?} ===", engine_p);
        let verdict = loop {
            if b.is_full() {
                break "draw (board full)";
            }
            let ply = b.total() + 1;
            if to_move == engine_p {
                let t = Instant::now();
                let bm = book.as_ref().and_then(|bk| bk.get(b.key(engine_p)));
                let (col, note) = match bm {
                    Some((col, raw)) => (col, format!("book raw {raw:3}")),
                    None => {
                        let r = Searcher::best_move(&b, engine_p, budget_d, &stats, &mut tt);
                        (r.col.expect("no move"), format!("depth {:2} score {:5}", r.depth, r.score))
                    }
                };
                b.make(col, engine_p);
                history.push(col);
                println!("ply {ply:2}  engine col {} {note}  {:5} ms", col + 1, t.elapsed().as_millis());
                if b.has_won(engine_p) {
                    break "ENGINE WINS";
                }
            } else {
                let t = Instant::now();
                let (col, _, raw) = sv.best_move(&history).expect("solver failed");
                b.make(col, to_move);
                history.push(col);
                // raw is from the solver's view before its move: negative =
                // solver losing = engine still winning.
                println!(
                    "ply {ply:2}  solver col {} raw {raw:3}            {:5} ms",
                    col + 1, t.elapsed().as_millis()
                );
                if still_winning && raw < 0 {
                    held_win = ply;
                }
                if raw > 0 {
                    if still_drawing {
                        held_draw = ply;
                    }
                    still_winning = false;
                    still_drawing = false;
                } else if raw == 0 {
                    still_winning = false;
                }
                if b.has_won(to_move) {
                    break "solver wins";
                }
            }
            to_move = to_move.other();
        };
        println!(
            "result: {verdict} after {} plies; theoretical win held to ply {}, draw to ply {}",
            b.total(), held_win, if still_drawing { b.total() } else { held_draw }
        );
        println!("game: {}", history.iter().map(|c| (c + 1).to_string()).collect::<Vec<_>>().join(","));
    }
}
