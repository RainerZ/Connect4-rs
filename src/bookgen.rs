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
//!
//! Corrective mode (for the seat where the *human* moves first):
//!
//!   bookgen --corrective 3 --solver /path/c4solver [--depths 12,13] [--out corrective-book.txt]
//!
//! enumerates every position with up to N stones and the engine (second
//! player) to move - full width, i.e. any human play and any earlier
//! engine reply. For each position the solver gives the verdict; where the
//! engine (audited with its deterministic fixed-depth search at each of
//! the given depths) would throw away a win or draw, a book entry with the
//! solver's move is written. Positions the engine handles by itself get no
//! entry - the shipped book is exactly the map of its early blind spots.

#[allow(dead_code)]
mod engine;
#[allow(dead_code)]
mod solver;

use engine::{Board, Piece, SearchStats, Searcher, TransTable};
use solver::ExternalSolver;
use std::collections::{HashMap, HashSet};
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

/// One audited engine-to-move position in corrective mode.
struct Audit {
    history: Vec<usize>,
    verdict: i32,
    best: usize,
    failed: Vec<(usize, usize)>, // (audit depth, column the engine chose)
}

struct Corrective {
    sv: ExternalSolver,
    depths: Vec<usize>,
    /// Positions already audited - pre-loaded from the audit log
    /// (corrective-audited.txt), so a continued or deeper run only pays
    /// for positions never seen before, plus in-run transposition dedupe.
    seen: HashSet<u64>,
    /// Append-only log of every audited position key; the durable resume
    /// state (--skip remains as a coarser fallback within one run).
    audit_log: std::fs::File,
    /// Keys already present in the output book (kept, not rewritten).
    booked: HashSet<u64>,
    /// Audits to skip (deterministic DFS order) when salvaging a killed run.
    skip: usize,
    audited: usize,
    stats: SearchStats,
    tt: TransTable,
    out: std::fs::File,
    audits: Vec<Audit>,
    t0: Instant,
}

impl Corrective {
    /// Depth-first over all move sequences; audit every new engine-to-move
    /// position (odd stone counts - the human is the first player).
    fn walk(&mut self, b: &mut Board, hist: &mut Vec<usize>, stones_left: usize) {
        if hist.len() % 2 == 1 {
            let key = b.key(Piece::Yellow);
            if self.seen.insert(key) {
                self.audit(b, hist, key);
            }
        }
        if stones_left == 0 {
            return;
        }
        let side = if hist.len() % 2 == 0 { Piece::Red } else { Piece::Yellow };
        for c in 0..7 {
            if b.can_play(c) {
                b.make(c, side);
                hist.push(c);
                if !b.has_won(side) {
                    self.walk(b, hist, stones_left - 1);
                }
                hist.pop();
                b.unmake(c, side);
            }
        }
    }

    fn audit(&mut self, b: &Board, hist: &[usize], key: u64) {
        self.audited += 1;
        if self.audited <= self.skip {
            return;
        }
        writeln!(self.audit_log, "{key:x}").unwrap();
        self.audit_log.flush().unwrap();
        let scores = self.sv.analyze(hist).expect("solver failed");
        let best = ExternalSolver::pick(&scores).expect("no playable column");
        let verdict = scores[best];
        let mut failed = Vec::new();
        if verdict >= 0 {
            // The engine must preserve the win (score > 0) or draw (== 0).
            for &d in &self.depths.clone() {
                let r = Searcher::best_move_to_depth(b, Piece::Yellow, d, &self.stats, &mut self.tt);
                let col = r.col.expect("no move");
                let ok = if verdict > 0 { scores[col] > 0 } else { scores[col] >= 0 };
                if !ok {
                    failed.push((d, col));
                }
            }
            if !failed.is_empty() && self.booked.insert(key) {
                writeln!(self.out, "{key:x} {} {verdict}", best + 1).unwrap();
                self.out.flush().unwrap();
            }
        }
        self.audits.push(Audit { history: hist.to_vec(), verdict, best, failed });
        if self.audits.len() % 25 == 0 {
            eprintln!("{} positions audited ({:.0} s)", self.audits.len(), self.t0.elapsed().as_secs_f32());
        }
    }
}

fn fmt_hist(h: &[usize]) -> String {
    h.iter().map(|c| (c + 1).to_string()).collect::<Vec<_>>().join(",")
}

fn run_corrective(cmd: &str, stones: usize, depths: Vec<usize>, out_path: &str, skip: usize) {
    let sv = ExternalSolver::spawn(cmd).unwrap_or_else(|e| {
        eprintln!("{e}");
        std::process::exit(2);
    });
    // Keep existing entries (a killed run's corrections stay valid); only
    // new corrections are appended, duplicates suppressed.
    let mut booked = HashSet::new();
    if let Ok(text) = std::fs::read_to_string(out_path) {
        for line in text.lines() {
            if let Some(k) = line.split_whitespace().next()
                && let Ok(key) = u64::from_str_radix(k, 16)
            {
                booked.insert(key);
            }
        }
        eprintln!("keeping {} existing corrections", booked.len());
    }
    let out = std::fs::OpenOptions::new().create(true).append(true).open(out_path).unwrap();
    // The audit log is the durable resume state: every position ever
    // audited (in any earlier or shallower run) is skipped up front.
    let mut seen = HashSet::new();
    if let Ok(text) = std::fs::read_to_string("corrective-audited.txt") {
        for line in text.lines() {
            if let Ok(key) = u64::from_str_radix(line.trim(), 16) {
                seen.insert(key);
            }
        }
        eprintln!("skipping {} already audited positions", seen.len());
    }
    let audit_log = std::fs::OpenOptions::new().create(true).append(true).open("corrective-audited.txt").unwrap();
    let mut g = Corrective {
        sv,
        depths,
        seen,
        audit_log,
        booked,
        skip,
        audited: 0,
        stats: SearchStats::default(),
        tt: TransTable::new(),
        out,
        audits: Vec::new(),
        t0: Instant::now(),
    };
    let mut b = Board::new();
    let mut hist = Vec::new();
    g.walk(&mut b, &mut hist, stones);

    // Report. Verdicts are from the engine's (second player's) view:
    // positive = the human's play so far has thrown the game away.
    for n in (1..=stones).step_by(2) {
        let at: Vec<&Audit> = g.audits.iter().filter(|a| a.history.len() == n).collect();
        let wins = at.iter().filter(|a| a.verdict > 0).count();
        let draws = at.iter().filter(|a| a.verdict == 0).count();
        let corrections = at.iter().filter(|a| !a.failed.is_empty()).count();
        eprintln!(
            "
=== {n} stone(s): {} positions | engine wins {wins}, draws {draws}, human still winning {} | corrections {corrections} ===",
            at.len(), at.len() - wins - draws
        );
        if n == 1 {
            for a in &at {
                let (what, speed) = match a.verdict {
                    v if v > 0 => ("ENGINE WINS", format!(" (mate at stone {})", 43 - 2 * v)),
                    0 => ("draw", String::new()),
                    _ => ("human keeps the win", String::new()),
                };
                eprintln!(
                    "  human opens {} -> {what}{speed}, solver reply col {}{}",
                    fmt_hist(&a.history), a.best + 1,
                    if a.failed.is_empty() { "" } else { "  [ENGINE WOULD MISS IT]" }
                );
            }
        }
        for a in at.iter().filter(|a| !a.failed.is_empty()) {
            let f: Vec<String> = a.failed.iter().map(|(d, c)| format!("d{d}->col{}", c + 1)).collect();
            eprintln!(
                "  correction: history {} verdict {:+} best col {} | engine fails: {}",
                fmt_hist(&a.history), a.verdict, a.best + 1, f.join(" ")
            );
        }
    }
    let total_corr = g.audits.iter().filter(|a| !a.failed.is_empty()).count();
    eprintln!(
        "
done: {} positions, {} corrections -> {out_path} ({:.0} s)",
        g.audits.len(), total_corr, g.t0.elapsed().as_secs_f32()
    );
}

fn main() {
    let mut solver_cmd = None;
    let mut plies = 6usize;
    let mut out_path = None;
    let mut corrective = None;
    let mut skip = 0usize;
    let mut depths = vec![12usize, 13];
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--solver" => solver_cmd = args.next(),
            "--plies" => plies = args.next().and_then(|v| v.parse().ok()).unwrap_or(6),
            "--corrective" => corrective = args.next().and_then(|v| v.parse::<usize>().ok()),
            "--skip" => skip = args.next().and_then(|v| v.parse().ok()).unwrap_or(0),
            "--depths" => {
                depths = args.next().unwrap_or_default().split(',').filter_map(|d| d.parse().ok()).collect();
            }
            "--out" => out_path = args.next(),
            _ => {
                eprintln!(
                    "usage: bookgen --solver <cmd> [--plies 6] [--out opening-book.txt]
       bookgen --solver <cmd> --corrective <stones> [--depths 12,13] [--out corrective-book.txt]"
                );
                std::process::exit(2);
            }
        }
    }
    let Some(cmd) = solver_cmd else {
        eprintln!("--solver <cmd> is required");
        std::process::exit(2);
    };
    if let Some(stones) = corrective {
        let out = out_path.unwrap_or_else(|| "corrective-book.txt".to_string());
        run_corrective(&cmd, stones, depths, &out, skip);
        return;
    }
    let out_path = out_path.unwrap_or_else(|| "opening-book.txt".to_string());

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
