//! `bookgen`: distill an opening book from a perfect solver.
//!
//!   cargo run --release --bin bookgen -- --solver /path/c4solver [--plies 6] [--out opening-book.txt]
//!
//! Covers the first-player side: starting from the empty board, the first
//! player always plays the solver's best move; every opponent reply is
//! expanded, down to `--plies` book moves. One solver query per distinct
//! first-player position (transpositions are deduplicated); results are
//! appended to the output file immediately, so an interrupted run resumes
//! where it stopped.

#[allow(dead_code)]
mod engine;
#[allow(dead_code)]
mod solver;

use engine::{Board, Piece};
use solver::ExternalSolver;
use std::collections::HashMap;
use std::io::Write;
use std::time::Instant;

struct Gen {
    sv: ExternalSolver,
    known: HashMap<u64, (usize, i32)>,
    out: std::fs::File,
    queries: usize,
    t0: Instant,
}

impl Gen {
    /// Handle a first-player-to-move position: look up or solve the best
    /// move, then expand all opponent replies while book moves remain.
    fn walk(&mut self, b: &mut Board, history: &mut Vec<usize>, moves_left: usize) {
        let key = b.key(Piece::Red);
        let (col, raw) = match self.known.get(&key) {
            Some(&e) => e,
            None => {
                let scores = self.sv.analyze(history).expect("solver failed");
                let col = ExternalSolver::pick(&scores).expect("no playable column");
                let raw = scores[col];
                assert!(raw > 0, "book invariant broken: not a winning position ({history:?} -> {scores:?})");
                writeln!(self.out, "{key:x} {} {raw}", col + 1).unwrap();
                self.out.flush().unwrap();
                self.known.insert(key, (col, raw));
                self.queries += 1;
                if self.queries % 100 == 0 {
                    eprintln!(
                        "{} queries, {} entries, ply {} ({:.0} s)",
                        self.queries, self.known.len(), history.len() + 1, self.t0.elapsed().as_secs_f32()
                    );
                }
                (col, raw)
            }
        };
        let _ = raw;
        if moves_left == 1 {
            return;
        }
        b.make(col, Piece::Red);
        history.push(col);
        if !b.has_won(Piece::Red) {
            for yc in 0..7 {
                if b.can_play(yc) {
                    b.make(yc, Piece::Yellow);
                    history.push(yc);
                    // A winning position stays winning whatever the
                    // opponent plays, so every reply gets a book answer.
                    if !b.has_won(Piece::Yellow) && !b.is_full() {
                        self.walk(b, history, moves_left - 1);
                    }
                    history.pop();
                    b.unmake(yc, Piece::Yellow);
                }
            }
        }
        history.pop();
        b.unmake(col, Piece::Red);
    }
}

fn main() {
    let mut solver_cmd = None;
    let mut plies = 6usize;
    let mut out_path = "opening-book.txt".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--solver" => solver_cmd = args.next(),
            "--plies" => plies = args.next().and_then(|v| v.parse().ok()).unwrap_or(6),
            "--out" => out_path = args.next().unwrap_or(out_path),
            _ => {
                eprintln!("usage: bookgen --solver <cmd> [--plies 6] [--out opening-book.txt]");
                std::process::exit(2);
            }
        }
    }
    let Some(cmd) = solver_cmd else {
        eprintln!("--solver <cmd> is required");
        std::process::exit(2);
    };

    // Resume: previously distilled entries are kept and not re-queried.
    let mut known = HashMap::new();
    if let Ok(text) = std::fs::read_to_string(&out_path) {
        for line in text.lines() {
            let t: Vec<&str> = line.split_whitespace().collect();
            if t.len() == 3
                && let (Ok(k), Ok(c), Ok(r)) = (u64::from_str_radix(t[0], 16), t[1].parse::<usize>(), t[2].parse::<i32>())
            {
                known.insert(k, (c - 1, r));
            }
        }
        eprintln!("resuming with {} existing entries", known.len());
    }
    let out = std::fs::OpenOptions::new().create(true).append(true).open(&out_path).unwrap();
    let sv = ExternalSolver::spawn(&cmd).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });

    let mut g = Gen { sv, known, out, queries: 0, t0: Instant::now() };
    let mut b = Board::new();
    let mut history = Vec::new();
    g.walk(&mut b, &mut history, plies);
    eprintln!(
        "done: {} new queries, {} total entries, {:.0} s",
        g.queries, g.known.len(), g.t0.elapsed().as_secs_f32()
    );
}
