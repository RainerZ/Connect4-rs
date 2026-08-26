//! Game state shared between GUI, engine thread and the control socket.

use crate::engine::{Board, Piece, SearchStats, Searcher, TransTable, COLS, ROWS};
use crate::book::Book;
use crate::solver::ExternalSolver;
use crate::hints::{self, Hints};
use serde::{Deserialize, Serialize};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

/// --log: trace to stderr what each player sees and what the engine did.
pub static LOG: AtomicBool = AtomicBool::new(false);

#[inline]
fn logging() -> bool {
    LOG.load(Ordering::Relaxed)
}

fn fmt_cols(cols: &[usize]) -> String {
    cols.iter().map(|c| (c + 1).to_string()).collect::<Vec<_>>().join(",")
}

/// Default think time per engine move.
pub const DEFAULT_BUDGET: Duration = Duration::from_secs(2);
pub const PORT: u16 = 4444;

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    HumanToMove,
    Thinking,
    Won(WinnerJs),
    Draw,
}

/// Result of an engine search from the engine's point of view, used to
/// decide between moving, announcing a forced win and resigning.
pub struct EngineMove {
    pub col: Option<usize>,
    pub score: i32,
    pub depth: usize,
    pub nodes: u64,
    pub millis: u128,
    /// The move came from the opening book (solver-distilled), not a search.
    pub book: bool,
}

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WinnerJs {
    Human,
    Engine,
}

pub struct Game {
    pub board: Board,
    pub human: Piece,
    pub to_move: Piece,
    pub status: Status,
    pub history: Vec<usize>,
    pub last_search: Option<LastSearch>,
    /// The engine resigned because its search proved a loss.
    pub resigned: bool,
    /// Think time per move (iterative deepening budget).
    pub budget: Duration,
    pub engine_starts: bool,
    /// LLM assistance (see hints.rs): include tactical hints in the JSON
    /// state served to socket/MCP clients.
    pub hints: bool,
    /// Show the hint rings in the GUI. Independent of `hints`, so a human
    /// can watch the rings while a model plays unaided - and vice versa.
    pub show_hints: bool,
}

#[derive(Clone, Serialize)]
pub struct LastSearch {
    pub col: usize,
    pub score: i32,
    pub depth: usize,
    pub nodes: u64,
    pub millis: u128,
    /// True when the move came from the solver opening book.
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub book: bool,
}

/// JSON view of the game, returned by the control socket.
#[derive(Serialize)]
pub struct StateJson {
    pub status: Status,
    pub human: &'static str,
    pub to_move: &'static str,
    /// rows[0] is the top row, 'R', 'Y' or '.'
    pub rows: Vec<String>,
    pub history: Vec<usize>,
    pub last_search: Option<LastSearch>,
    pub message: String,
    #[serde(skip_serializing_if = "std::ops::Not::not")]
    pub resigned: bool,
    /// Only present when hints are enabled (LLM assistance, see hints.rs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Hints>,
}

impl Game {
    pub fn new(engine_starts: bool, budget: Duration, hints: bool, show_hints: bool) -> Game {
        let human = if engine_starts { Piece::Yellow } else { Piece::Red };
        Game {
            board: Board::new(),
            human,
            to_move: Piece::Red,
            status: if engine_starts { Status::Thinking } else { Status::HumanToMove },
            history: Vec::new(),
            last_search: None,
            resigned: false,
            budget,
            engine_starts,
            hints,
            show_hints,
        }
    }

    pub fn engine(&self) -> Piece {
        self.human.other()
    }

    fn finish_move(&mut self, col: usize) {
        let p = self.to_move;
        self.board.make(col, p);
        self.history.push(col);
        if self.board.has_won(p) {
            self.status = Status::Won(if p == self.human { WinnerJs::Human } else { WinnerJs::Engine });
        } else if self.board.is_full() {
            self.status = Status::Draw;
        } else {
            self.to_move = p.other();
            self.status = if self.to_move == self.human { Status::HumanToMove } else { Status::Thinking };
        }
        if logging() {
            let side = if p == Piece::Red { "red" } else { "yellow" };
            let seat = if p == self.human { "player" } else { "engine" };
            eprintln!(
                "log: ply {:2} {side}({seat}) col {}  score(eval red) {:5}  | history {}",
                self.history.len(), col + 1, self.board.score_for(Piece::Red), fmt_cols(&self.history)
            );
            if matches!(self.status, Status::Won(_) | Status::Draw) {
                eprintln!("log: game over: {}", self.message());
            } else if self.status == Status::HumanToMove && self.hints {
                // Exactly the tactical hints the (LLM) player will receive.
                let h = crate::hints::compute(&self.board, self.to_move);
                let rows: Vec<String> = h.next_row.iter().map(|r| r.map(|r| r.to_string()).unwrap_or("-".into())).collect();
                let list = |v: &Vec<usize>| if v.is_empty() { "-".to_string() } else { fmt_cols(&v.iter().map(|c| c - 1).collect::<Vec<_>>()) };
                eprintln!(
                    "log: hints to player: next_row {}  win {}  must_block {}  losing {}",
                    rows.join(","), list(&h.winning_moves), list(&h.must_block), list(&h.losing_moves)
                );
            }
        }
    }

    /// Replay move (0-based column) for whichever side is to move, engine
    /// included - used by the control socket's replay command. Returns false
    /// if the column is illegal or the game is over.
    pub fn replay_move(&mut self, col: usize) -> bool {
        if matches!(self.status, Status::Won(_) | Status::Draw) || !self.board.can_play(col) {
            return false;
        }
        self.finish_move(col);
        true
    }

    /// Take back moves until one human move has been removed and the human
    /// is to move again - after the engine's reply this is the last move
    /// pair, after a lost/resigned game everything back to the human's
    /// last decision, and while the engine thinks just the human's pending
    /// move (the in-flight search result is discarded by the board-identity
    /// check in the engine loop). Returns false if there is nothing to undo.
    pub fn undo(&mut self) -> bool {
        let mut hist = self.history.clone();
        let mut popped_human = false;
        while let Some(i) = hist.len().checked_sub(1) {
            let side = if i % 2 == 0 { Piece::Red } else { Piece::Yellow };
            hist.pop();
            if side == self.human {
                popped_human = true;
                break;
            }
        }
        if !popped_human {
            return false;
        }
        if logging() {
            eprintln!("log: undo -> history {}", fmt_cols(&hist));
        }
        let was_logging = LOG.swap(false, Ordering::Relaxed);
        let mut g = Game::new(self.engine_starts, self.budget, self.hints, self.show_hints);
        for &c in &hist {
            g.replay_move(c);
        }
        LOG.store(was_logging, Ordering::Relaxed);
        *self = g;
        true
    }

    /// Human move (0-based column). Returns false if illegal now.
    pub fn human_move(&mut self, col: usize) -> bool {
        if self.status != Status::HumanToMove || !self.board.can_play(col) {
            return false;
        }
        self.finish_move(col);
        true
    }

    /// The engine's last search proved a win for it (game still running).
    pub fn engine_sees_win(&self) -> bool {
        !matches!(self.status, Status::Won(_) | Status::Draw)
            && self.last_search.as_ref().is_some_and(|ls| ls.score >= crate::engine::WIN_SCORE)
    }

    pub fn message(&self) -> String {
        match self.status {
            Status::HumanToMove if self.engine_sees_win() => "Engine sees a forced win! Your move".into(),
            Status::HumanToMove => "Your move".into(),
            Status::Thinking => "Engine is thinking ...".into(),
            Status::Won(WinnerJs::Human) if self.resigned => "Engine gives up - it is facing a forced loss. You win!".into(),
            Status::Won(WinnerJs::Human) => "You win!".into(),
            Status::Won(WinnerJs::Engine) => "Engine wins!".into(),
            Status::Draw => "Draw".into(),
        }
    }

    /// Apply a finished engine search: resign on a proven loss, otherwise
    /// play the move. Factored out of the engine thread for testability.
    pub fn apply_engine_result(&mut self, r: &EngineMove) {
        if logging()
            && let Some(col) = r.col
        {
            eprintln!(
                "log: engine reply: col {}  {}  score {:5}  depth {:2}  nodes {}  {} ms",
                col + 1,
                if r.book { "from BOOK (score/depth = own quarter-budget eval)" } else { "searched" },
                r.score, r.depth, r.nodes, r.millis
            );
        }
        if let Some(col) = r.col {
            self.last_search = Some(LastSearch { col: col + 1, score: r.score, depth: r.depth, nodes: r.nodes, millis: r.millis, book: r.book });
            if r.score <= -crate::engine::WIN_SCORE {
                // Every line loses against best play: concede instead of
                // playing on to the bitter end.
                self.resigned = true;
                self.status = Status::Won(WinnerJs::Human);
                if logging() {
                    eprintln!("log: engine resigns (proven loss)");
                }
            } else {
                self.finish_move(col);
            }
        }
    }

    pub fn to_json(&self) -> StateJson {
        let name = |p: Piece| if p == Piece::Red { "red" } else { "yellow" };
        let rows = (0..ROWS)
            .rev()
            .map(|r| {
                (0..COLS)
                    .map(|c| match self.board.get(c, r) {
                        Some(Piece::Red) => 'R',
                        Some(Piece::Yellow) => 'Y',
                        None => '.',
                    })
                    .collect()
            })
            .collect();
        StateJson {
            status: self.status,
            human: name(self.human),
            to_move: name(self.to_move),
            rows,
            history: self.history.iter().map(|c| c + 1).collect(),
            last_search: self.last_search.clone(),
            message: self.message(),
            resigned: self.resigned,
            hints: if self.hints && self.status == Status::HumanToMove { Some(hints::compute(&self.board, self.to_move)) } else { None },
        }
    }
}

/// Shared handle: game + condvar so waiters (socket clients) can block until
/// the engine has moved.
pub struct Shared {
    pub game: Mutex<Game>,
    pub changed: Condvar,
    pub stats: SearchStats,
    pub repaint: Mutex<Option<eframe::egui::Context>>,
    /// External solver command playing the engine seat (--solver).
    pub solver: Option<String>,
    /// Opening book consulted before searching (--book).
    pub book: Option<Book>,
}

impl Shared {
    pub fn new(engine_starts: bool, budget: Duration, hints: bool, solver: Option<String>, book: Option<Book>) -> Arc<Shared> {
        Arc::new(Shared {
            game: Mutex::new(Game::new(engine_starts, budget, hints, hints)),
            changed: Condvar::new(),
            stats: SearchStats::default(),
            repaint: Mutex::new(None),
            solver,
            book,
        })
    }

    pub fn notify(&self) {
        self.changed.notify_all();
        if let Some(ctx) = self.repaint.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
    }

    /// Engine thread body: whenever it is the engine's turn, search and play
    /// - with the built-in engine, or an external solver in the engine seat.
    pub fn engine_loop(self: Arc<Self>) {
        raise_thread_priority();
        let mut tt = TransTable::new();
        let mut solver = self.solver.clone().map(|spec| match ExternalSolver::spawn(&spec) {
            Ok(s) => {
                eprintln!("external solver in the engine seat: {}", s.path.display());
                s
            }
            Err(e) => {
                eprintln!("{e}");
                std::process::exit(2);
            }
        });
        loop {
            let (board, p, budget, history) = {
                let mut g = self.game.lock().unwrap();
                while g.status != Status::Thinking {
                    g = self.changed.wait(g).unwrap();
                }
                (g.board, g.engine(), g.budget, g.history.clone())
            };
            let t0 = Instant::now();
            let book_move = self.book.as_ref().and_then(|bk| bk.get(board.key(p)));
            let is_book = book_move.is_some();
            let r = if let Some((col, _raw)) = book_move {
                // Book hit: the move is played from the book regardless,
                // but deliberately NOT reported as a proven win - instead a
                // short search (a quarter of the budget) computes the
                // engine's own evaluation for the score display, and the
                // book flag records where the move came from.
                let r = Searcher::best_move(&board, p, budget / 4, &self.stats, &mut tt);
                crate::engine::SearchResult { col: Some(col), score: r.score, depth: r.depth, nodes: r.nodes }
            } else if let Some(sv) = solver.as_mut() {
                match sv.best_move(&history) {
                    Ok((col, score, _raw)) => {
                        // Depth reported as "solved to the end of the game".
                        let remaining = COLS * ROWS - board.total();
                        crate::engine::SearchResult { col: Some(col), score, depth: remaining, nodes: 0 }
                    }
                    Err(e) => {
                        eprintln!("solver failed ({e}), falling back to the built-in engine for this move");
                        Searcher::best_move(&board, p, budget, &self.stats, &mut tt)
                    }
                }
            } else {
                Searcher::best_move(&board, p, budget, &self.stats, &mut tt)
            };
            let millis = t0.elapsed().as_millis();
            let mut g = self.game.lock().unwrap();
            // Make sure the game was not restarted meanwhile.
            if g.status == Status::Thinking && g.board.bitboard(Piece::Red) == board.bitboard(Piece::Red)
                && g.board.bitboard(Piece::Yellow) == board.bitboard(Piece::Yellow)
            {
                g.apply_engine_result(&EngineMove { col: r.col, score: r.score, depth: r.depth, nodes: r.nodes, millis, book: is_book });
            }
            drop(g);
            self.notify();
        }
    }
}

/// On macOS, secondary threads of a GUI app may be scheduled on efficiency
/// cores unless they carry a high QoS class. Ask for user-interactive QoS.
#[cfg(target_os = "macos")]
fn raise_thread_priority() {
    unsafe extern "C" {
        fn pthread_set_qos_class_self_np(qos_class: u32, relative_priority: i32) -> i32;
    }
    const QOS_CLASS_USER_INTERACTIVE: u32 = 0x21;
    unsafe {
        pthread_set_qos_class_self_np(QOS_CLASS_USER_INTERACTIVE, 0);
    }
}

#[cfg(not(target_os = "macos"))]
fn raise_thread_priority() {}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::WIN_SCORE;

    fn replay(cols: &[usize]) -> Game {
        let mut g = Game::new(false, Duration::from_secs(2), false, false);
        for &c in cols {
            let p = g.to_move;
            g.board.make(c - 1, p);
            g.to_move = p.other();
        }
        g
    }

    /// The engine resigns on a proven loss and the human wins.
    #[test]
    fn engine_resigns_on_forced_loss() {
        // Position after ply 19 of the first Claude win: yellow (engine) is
        // proven lost.
        let mut g = replay(&[4, 4, 4, 4, 5, 6, 3, 2, 5, 4, 5, 5, 5, 4, 1, 5, 3, 1, 3]);
        g.status = Status::Thinking;
        let stats = crate::engine::SearchStats::default();
        let mut tt = crate::engine::TransTable::new();
        let r = crate::engine::Searcher::best_move(&g.board, Piece::Yellow, Duration::from_secs(5), &stats, &mut tt);
        assert_eq!(r.score, -WIN_SCORE);
        g.apply_engine_result(&EngineMove { col: r.col, score: r.score, depth: r.depth, nodes: r.nodes, millis: 0, book: false });
        assert!(g.resigned);
        assert_eq!(g.status, Status::Won(WinnerJs::Human));
        assert!(g.message().contains("gives up"));
    }

    /// Undo removes the last move pair, or just the pending human move
    /// while the engine is thinking.
    #[test]
    fn undo_restores_human_turn() {
        let mut g = Game::new(false, Duration::from_secs(2), false, false);
        assert!(!g.undo());
        g.human_move(3);
        g.apply_engine_result(&EngineMove { col: Some(3), score: 0, depth: 1, nodes: 0, millis: 0, book: false });
        assert_eq!(g.history.len(), 2);
        assert!(g.undo());
        assert!(g.history.is_empty());
        assert_eq!(g.status, Status::HumanToMove);
        g.human_move(2);
        assert_eq!(g.status, Status::Thinking);
        assert!(g.undo());
        assert!(g.history.is_empty());
        assert_eq!(g.status, Status::HumanToMove);
    }

    /// A proven engine win is announced while the game continues.
    #[test]
    fn forced_win_is_announced() {
        // Game 1 endgame: red (human) to move is lost, i.e. the engine
        // (yellow) has a forced win.
        let mut g = replay(&[4, 4, 4, 4, 3, 2, 5, 6, 3, 4, 5, 4, 3, 3, 5, 5, 5, 5, 7, 7, 1, 1, 7, 3, 1, 3, 7]);
        g.status = Status::Thinking;
        let stats = crate::engine::SearchStats::default();
        let mut tt = crate::engine::TransTable::new();
        let r = crate::engine::Searcher::best_move(&g.board, Piece::Yellow, Duration::from_secs(5), &stats, &mut tt);
        assert_eq!(r.score, WIN_SCORE);
        g.apply_engine_result(&EngineMove { col: r.col, score: r.score, depth: r.depth, nodes: r.nodes, millis: 0, book: false });
        assert!(!g.resigned);
        assert_eq!(g.status, Status::HumanToMove);
        assert!(g.engine_sees_win());
        assert!(g.message().contains("forced win"));
    }
}
