//! Game state shared between GUI, engine thread and the control socket.

use crate::engine::{Board, Piece, SearchStats, Searcher, COLS, ROWS};
use crate::hints::{self, Hints};
use serde::{Deserialize, Serialize};
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

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
    /// Think time per move (iterative deepening budget).
    pub budget: Duration,
    pub engine_starts: bool,
    /// LLM assistance (see hints.rs): include tactical hints in the JSON state.
    pub hints: bool,
}

#[derive(Clone, Serialize)]
pub struct LastSearch {
    pub col: usize,
    pub score: i32,
    pub depth: usize,
    pub nodes: u64,
    pub millis: u128,
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
    /// Only present when hints are enabled (LLM assistance, see hints.rs).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub hints: Option<Hints>,
}

impl Game {
    pub fn new(engine_starts: bool, budget: Duration, hints: bool) -> Game {
        let human = if engine_starts { Piece::Yellow } else { Piece::Red };
        Game {
            board: Board::new(),
            human,
            to_move: Piece::Red,
            status: if engine_starts { Status::Thinking } else { Status::HumanToMove },
            history: Vec::new(),
            last_search: None,
            budget,
            engine_starts,
            hints,
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
    }

    /// Human move (0-based column). Returns false if illegal now.
    pub fn human_move(&mut self, col: usize) -> bool {
        if self.status != Status::HumanToMove || !self.board.can_play(col) {
            return false;
        }
        self.finish_move(col);
        true
    }

    pub fn message(&self) -> String {
        match self.status {
            Status::HumanToMove => "Your move (keys 1-7)".into(),
            Status::Thinking => "Engine is thinking ...".into(),
            Status::Won(WinnerJs::Human) => "You win!".into(),
            Status::Won(WinnerJs::Engine) => "Engine wins!".into(),
            Status::Draw => "Draw".into(),
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
}

impl Shared {
    pub fn new(engine_starts: bool, budget: Duration, hints: bool) -> Arc<Shared> {
        Arc::new(Shared {
            game: Mutex::new(Game::new(engine_starts, budget, hints)),
            changed: Condvar::new(),
            stats: SearchStats::default(),
            repaint: Mutex::new(None),
        })
    }

    pub fn notify(&self) {
        self.changed.notify_all();
        if let Some(ctx) = self.repaint.lock().unwrap().as_ref() {
            ctx.request_repaint();
        }
    }

    /// Engine thread body: whenever it is the engine's turn, search and play.
    pub fn engine_loop(self: Arc<Self>) {
        raise_thread_priority();
        loop {
            let (board, p, budget) = {
                let mut g = self.game.lock().unwrap();
                while g.status != Status::Thinking {
                    g = self.changed.wait(g).unwrap();
                }
                (g.board, g.engine(), g.budget)
            };
            let t0 = Instant::now();
            let r = Searcher::best_move(&board, p, budget, &self.stats);
            let millis = t0.elapsed().as_millis();
            let mut g = self.game.lock().unwrap();
            // Make sure the game was not restarted meanwhile.
            if g.status == Status::Thinking && g.board.bitboard(Piece::Red) == board.bitboard(Piece::Red)
                && g.board.bitboard(Piece::Yellow) == board.bitboard(Piece::Yellow)
            {
                if let Some(col) = r.col {
                    g.last_search = Some(LastSearch { col: col + 1, score: r.score, depth: r.depth, nodes: r.nodes, millis });
                    g.finish_move(col);
                }
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
