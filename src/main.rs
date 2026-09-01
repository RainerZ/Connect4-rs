mod engine;
mod game;
mod book;
mod hints;
mod learn;
mod server;
mod solver;

use eframe::egui;
use engine::{Piece, COLS, ROWS};
use game::{Shared, Status, DEFAULT_BUDGET};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Usage: connect4-rs [--budget <seconds>] [--no-hints] [--solver <path>] [--book <file>]
fn parse_args() -> (Duration, bool, Option<String>, Option<String>, Option<String>) {
    let mut budget = DEFAULT_BUDGET;
    let mut hints = true;
    let mut solver = None;
    let mut book = None;
    let mut tutor = None;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--budget" | "-b" => {
                let v = args.next().and_then(|v| v.parse::<f64>().ok()).filter(|v| *v > 0.0);
                match v {
                    Some(secs) => budget = Duration::from_secs_f64(secs),
                    None => {
                        eprintln!("--budget needs a positive number of seconds");
                        std::process::exit(2);
                    }
                }
            }
            "--hints" => hints = true,
            "--no-hints" => hints = false,
            "--log" => game::LOG.store(true, std::sync::atomic::Ordering::Relaxed),
            "--solver" => match args.next() {
                Some(p) => solver = Some(p),
                None => {
                    eprintln!("--solver needs a solver command, e.g. /path/to/c4solver");
                    std::process::exit(2);
                }
            },
            "--tutor" => tutor = args.next(),
            "--book" => match args.next() {
                Some(p) => book = Some(p),
                None => {
                    eprintln!("--book needs a book file (see the bookgen binary)");
                    std::process::exit(2);
                }
            },
            "-h" | "--help" => {
                println!(
                    "usage: connect4-rs [--budget <seconds>] [--no-hints] [--solver <path>]\n  --budget, -b  think time per engine move in seconds (default {})\n  --no-hints    start with LLM hints (socket/MCP state) and the GUI hint rings off;\n                both can be re-enabled at runtime (checkboxes/H, hints command, MCP tool)\n  --solver      an external solver plays the engine seat (Pascal Pons' line\n                protocol); extra args allowed, e.g. --solver 'path/c4solver -w'.\n                A 7x6.book next to the binary is picked up automatically\n  --book        opening book for the engine seat (default: opening-book.txt\n                in the working directory if present; see the bookgen binary)\n  --log         trace every move to stderr: history, engine reply details\n                (book/search, score, depth, nodes, time) and the tactical\n                hints served to the (LLM) player\n  --tutor       solver command for the learn feature (L key): analyze a lost\n                game and book the plies where the engine threw win/draw.\n                Falls back to $C4_SOLVER, then to the --solver command",
                    DEFAULT_BUDGET.as_secs_f64()
                );
                std::process::exit(0);
            }
            _ => {
                eprintln!("unknown argument {a}");
                std::process::exit(2);
            }
        }
    }
    (budget, hints, solver, book, tutor)
}

fn main() -> eframe::Result {
    let (budget, hints, solver, book, tutor) = parse_args();
    let solver_active = solver.is_some();
    let tutor = tutor.or_else(|| std::env::var("C4_SOLVER").ok()).or_else(|| solver.clone());
    // An explicitly named book must load; the default ones are best-effort:
    // the engine-start book plus the corrective book, merged (their keys
    // cannot collide - they cover different sides to move).
    let mut book_info = "no opening book loaded".to_string();
    let book = match &book {
        Some(p) => {
            let b = book::Book::load(std::path::Path::new(p)).unwrap_or_else(|e| {
                eprintln!("{e}");
                std::process::exit(2);
            });
            book_info = format!("book {}: {} positions", p, b.len());
            Some(b)
        }
        None => {
            let mut b = book::Book::load(std::path::Path::new("opening-book.txt")).ok();
            let opening = b.as_ref().map_or(0, |b| b.len());
            let mut corrective = 0;
            if let Ok(cb) = book::Book::load(std::path::Path::new("corrective-book.txt")) {
                corrective = cb.len();
                match &mut b {
                    Some(b) => {
                        let _ = b.merge(std::path::Path::new("corrective-book.txt"));
                    }
                    None => b = Some(cb),
                }
            }
            if b.is_some() {
                book_info = format!("opening book: {opening} positions, corrective book: {corrective} entries");
            }
            b
        }
    };
    if let Some(b) = &book {
        eprintln!("opening book loaded: {} positions ({book_info})", b.len());
    }
    let shared = Shared::new(false, budget, hints, solver, book, tutor);
    {
        let s = shared.clone();
        std::thread::Builder::new().name("engine".into()).stack_size(16 << 20).spawn(move || s.engine_loop()).unwrap();
    }
    {
        let s = shared.clone();
        std::thread::Builder::new().name("server".into()).spawn(move || server::run(s)).unwrap();
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([680.0, 740.0])
            .with_title(if solver_active { "Connect4-rs vs external solver" } else { "Connect4-rs" }),
        ..Default::default()
    };
    eframe::run_native("Connect4-rs", options, Box::new(|cc| {
        *shared.repaint.lock().unwrap() = Some(cc.egui_ctx.clone());
        Ok(Box::new(App { shared, seen_moves: 0, anim: None, show_about: false, learn_confirm: false, book_info }))
    }))
}

/// A piece falling into its slot.
struct Anim {
    col: usize,
    row: usize,
    piece: Piece,
    start: Instant,
    duration: Duration,
}

struct App {
    shared: Arc<Shared>,
    /// Number of moves already shown (to detect new moves to animate).
    seen_moves: usize,
    anim: Option<Anim>,
    /// The About popup (version, books, shortcuts) is open.
    show_about: bool,
    /// The learn confirmation dialog is open (L key).
    learn_confirm: bool,
    /// Book summary computed at startup, shown in the About popup.
    book_info: String,
}

const RED: egui::Color32 = egui::Color32::from_rgb(220, 40, 40);
const YELLOW: egui::Color32 = egui::Color32::from_rgb(240, 210, 30);
const HOLE: egui::Color32 = egui::Color32::WHITE;

fn piece_color(p: Piece) -> egui::Color32 {
    match p {
        Piece::Red => RED,
        Piece::Yellow => YELLOW,
    }
}

impl eframe::App for App {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = ui.ctx().clone();
        let ctx = &ctx;
        // Keyboard: 1-7 drop piece, N / Space new game, S swap who starts + new game,
        // U undo the last move pair, +/- double/halve the think time,
        // H toggle the hint rings (GUI only)
        let mut changed = false;
        // While the learn analysis runs, the game is read-only: no moves,
        // undo or new game (the analysis works on a snapshot, but changing
        // the board mid-learn would only confuse the user).
        let learning = self.shared.learn.lock().unwrap().running;
        ctx.input(|i| {
            if learning {
                return;
            }
            let mut g = self.shared.game.lock().unwrap();
            // Columns respond to both digits 1-7 and letters a-g (the
            // board labels use the letters; H upward stays free for the
            // other shortcuts).
            let digits = [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5, egui::Key::Num6, egui::Key::Num7];
            let letters = [egui::Key::A, egui::Key::B, egui::Key::C, egui::Key::D, egui::Key::E, egui::Key::F, egui::Key::G];
            for col in 0..COLS {
                if (i.key_pressed(digits[col]) || i.key_pressed(letters[col])) && g.human_move(col) {
                    changed = true;
                }
            }
            if i.key_pressed(egui::Key::N) || i.key_pressed(egui::Key::Space) {
                let (es, bud, h, sh) = (g.engine_starts, g.budget, g.hints, g.show_hints);
                *g = game::Game::new(es, bud, h, sh);
                changed = true;
            }
            if i.key_pressed(egui::Key::S) {
                let (es, bud, h, sh) = (!g.engine_starts, g.budget, g.hints, g.show_hints);
                *g = game::Game::new(es, bud, h, sh);
                changed = true;
            }
            if i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals) {
                g.budget = (g.budget * 2).min(Duration::from_secs(60));
            }
            if i.key_pressed(egui::Key::Minus) {
                g.budget = (g.budget / 2).max(Duration::from_millis(50));
            }
            if i.key_pressed(egui::Key::H) {
                g.show_hints = !g.show_hints;
            }
            if i.key_pressed(egui::Key::U) && g.undo() {
                changed = true;
            }
            if i.key_pressed(egui::Key::L) && !g.history.is_empty() {
                self.learn_confirm = true;
            }
        });

        let mut g = self.shared.game.lock().unwrap();
        let thinking = g.status == Status::Thinking;
        // Content column exactly as wide as the board, centred so the window
        // frame keeps the same small border left and right; heading, status
        // text and settings strip align with the board edges.
        let margin = 12.0;
        let outer = ui.available_rect_before_wrap();
        let cell_est = ((outer.width() - 2.0 * margin) / COLS as f32)
            .min((outer.height() - 175.0) / ROWS as f32)
            .clamp(20.0, 90.0);
        let content_w = cell_est * COLS as f32;
        let x0 = outer.min.x + ((outer.width() - content_w) * 0.5).max(margin);
        let content = egui::Rect::from_min_max(egui::pos2(x0, outer.min.y + 8.0), egui::pos2(x0 + content_w, outer.max.y));
        ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
            // Headline, coloured when something special is going on: red for
            // an announced forced engine win, green when the human has won.
            let heading = egui::RichText::new(g.message());
            let heading = if g.engine_sees_win() {
                heading.color(egui::Color32::from_rgb(230, 70, 70))
            } else if matches!(g.status, Status::Won(game::WinnerJs::Human)) {
                heading.color(egui::Color32::from_rgb(60, 190, 90))
            } else {
                heading
            };
            ui.heading(heading);
            ui.horizontal(|ui| {
                ui.label(format!(
                    "You: {}   Engine: {}",
                    if g.human == Piece::Red { "Red" } else { "Yellow" },
                    if g.engine() == Piece::Red { "Red" } else { "Yellow" },
                ));
                // Small eval bar, right-aligned on this short line so it
                // never truncates the search-stats line below: red's share
                // of the last engine score (tanh-scaled, a proven win fills
                // the bar).
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (bar, _) = ui.allocate_exact_size(egui::vec2(140.0, 12.0), egui::Sense::hover());
                    let red_score = g.last_search.as_ref().map_or(0.0, |ls| {
                        let s = if g.engine() == Piece::Red { ls.score } else { -ls.score } as f32;
                        if s.abs() >= engine::WIN_SCORE as f32 { s.signum() } else { (s / 20.0).tanh() }
                    });
                    let p = ui.painter();
                    p.rect_filled(bar, 3.0, YELLOW);
                    let w = (0.5 + 0.5 * red_score) * bar.width();
                    p.rect_filled(
                        egui::Rect::from_min_size(bar.min, egui::vec2(w, bar.height())),
                        3.0,
                        RED,
                    );
                    let mid = bar.min.x + bar.width() * 0.5;
                    p.line_segment(
                        [egui::pos2(mid, bar.min.y), egui::pos2(mid, bar.max.y)],
                        egui::Stroke::new(1.0, egui::Color32::from_black_alpha(120)),
                    );
                    p.rect_stroke(bar, 3.0, egui::Stroke::new(1.0, egui::Color32::from_gray(90)), egui::StrokeKind::Outside);
                });
            });
            let line2 = if thinking {
                format!(
                    "searching depth {}  nodes {}",
                    self.shared.stats.depth.load(Ordering::Relaxed),
                    self.shared.stats.nodes.load(Ordering::Relaxed)
                )
            } else if let Some(ls) = &g.last_search {
                if ls.book {
                    format!(
                        "last engine move: col {}  from the solver book  (own eval: score {} depth {})",
                        ls.col, ls.score, ls.depth
                    )
                } else {
                    format!(
                        "last engine move: col {}  score {}  depth {}  nodes {}  {} ms ({:.1} Mnodes/s)",
                        ls.col, ls.score, ls.depth, ls.nodes, ls.millis,
                        ls.nodes as f64 / (ls.millis.max(1) as f64 * 1000.0)
                    )
                }
            } else {
                String::new()
            };
            ui.label(line2);
            ui.add_space(4.0);

            // Settings strip: the keyboard shortcuts stay available, this is
            // the discoverable way to reach the same things.
            ui.horizontal(|ui| {
                ui.add_enabled_ui(!learning, |ui| {
                if ui.button("New game").clicked() {
                    let (es, bud, h, sh) = (g.engine_starts, g.budget, g.hints, g.show_hints);
                    *g = game::Game::new(es, bud, h, sh);
                    changed = true;
                }
                if ui.button("Undo").on_hover_text("Take back your last move (U)").clicked() && g.undo() {
                    changed = true;
                }
                });
                let mut es = g.engine_starts;
                if ui.checkbox(&mut es, "Engine starts").on_hover_text("Applies to the next new game").changed() {
                    g.engine_starts = es;
                }
                let mut sh = g.show_hints;
                if ui.checkbox(&mut sh, "Hint rings").on_hover_text("GUI overlay only (H)").changed() {
                    g.show_hints = sh;
                }
                if ui.button("About").clicked() {
                    self.show_about = !self.show_about;
                }
                let mut secs = g.budget.as_secs_f64();
                if ui
                    .add(egui::Slider::new(&mut secs, 0.05..=60.0).logarithmic(true).text("think time").suffix(" s"))
                    .changed()
                {
                    g.budget = Duration::from_secs_f64(secs);
                }
            });
            ui.add_space(4.0);

            let avail = ui.available_size();
            let cell = (avail.x / COLS as f32).min(avail.y / ROWS as f32).min(90.0);
            let (rect, resp) =
                ui.allocate_exact_size(egui::vec2(cell * COLS as f32, cell * ROWS as f32), egui::Sense::click());
            let center_of = |c: usize, r: usize| {
                egui::pos2(rect.min.x + (c as f32 + 0.5) * cell, rect.max.y - (r as f32 + 0.5) * cell)
            };

            // Mouse: hovered column, click to drop.
            let hover_col = resp
                .hover_pos()
                .filter(|p| rect.contains(*p))
                .map(|p| (((p.x - rect.min.x) / cell) as usize).min(COLS - 1));
            if !learning
                && resp.clicked()
                && let Some(c) = hover_col
                && g.human_move(c)
            {
                changed = true;
            }

            // Detect new moves and start the falling animation for the latest.
            if g.history.len() < self.seen_moves {
                self.seen_moves = g.history.len(); // new game
                self.anim = None;
            } else if g.history.len() > self.seen_moves {
                self.seen_moves = g.history.len();
                if let Some(&col) = g.history.last() {
                    let row = g.board.height(col) - 1;
                    let piece = g.board.get(col, row).unwrap();
                    let fall_rows = (ROWS - row) as f32;
                    self.anim = Some(Anim {
                        col,
                        row,
                        piece,
                        start: Instant::now(),
                        duration: Duration::from_secs_f32(0.10 * fall_rows.sqrt().max(1.0)),
                    });
                }
            }
            let anim_cell = match &self.anim {
                Some(a) if a.start.elapsed() < a.duration => Some((a.col, a.row)),
                Some(_) => {
                    self.anim = None;
                    None
                }
                None => None,
            };

            let painter = ui.painter();
            painter.rect_filled(rect, 6.0, egui::Color32::from_rgb(30, 60, 160));
            let win = g.board.winning_line();
            for c in 0..COLS {
                for r in 0..ROWS {
                    let color = match g.board.get(c, r) {
                        _ if anim_cell == Some((c, r)) => HOLE, // still falling
                        Some(p) => piece_color(p),
                        None => HOLE,
                    };
                    painter.circle_filled(center_of(c, r), cell * 0.42, color);
                    if anim_cell != Some((c, r)) && win.map_or(false, |w| w.contains(&(c, r))) {
                        painter.circle_stroke(center_of(c, r), cell * 0.42, egui::Stroke::new(4.0, egui::Color32::BLACK));
                    }
                }
            }

            // Falling piece: accelerate from the top row to the target slot.
            if let Some(a) = &self.anim {
                let p = (a.start.elapsed().as_secs_f32() / a.duration.as_secs_f32()).min(1.0);
                let y0 = center_of(a.col, ROWS - 1).y;
                let y1 = center_of(a.col, a.row).y;
                let y = y0 + (y1 - y0) * p * p; // gravity
                painter.circle_filled(egui::pos2(center_of(a.col, 0).x, y), cell * 0.42, piece_color(a.piece));
            }

            // Hover ghost: translucent piece on the landing slot.
            if g.status == Status::HumanToMove
                && self.anim.is_none()
                && let Some(c) = hover_col
                && g.board.can_play(c)
            {
                let ghost = piece_color(g.human).gamma_multiply(0.45);
                painter.circle_filled(center_of(c, g.board.height(c)), cell * 0.42, ghost);
            }

            // Hint overlay (H): ring on the landing slot of tactically
            // decisive columns - green: wins now, orange: must block,
            // grey + x: loses at once.
            if g.show_hints && g.status == Status::HumanToMove {
                let h = hints::compute(&g.board, g.to_move);
                let ring = |col1: usize, color: egui::Color32, cross: bool| {
                    let c = col1 - 1;
                    let center = center_of(c, g.board.height(c));
                    painter.circle_stroke(center, cell * 0.34, egui::Stroke::new(4.0, color));
                    if cross {
                        let d = cell * 0.16;
                        for s in [-1.0f32, 1.0] {
                            painter.line_segment(
                                [egui::pos2(center.x - d, center.y - s * d), egui::pos2(center.x + d, center.y + s * d)],
                                egui::Stroke::new(4.0, color),
                            );
                        }
                    }
                };
                for &c in &h.losing_moves {
                    ring(c, egui::Color32::from_rgb(130, 130, 130), true);
                }
                for &c in &h.must_block {
                    ring(c, egui::Color32::from_rgb(250, 150, 30), false);
                }
                for &c in &h.winning_moves {
                    ring(c, egui::Color32::from_rgb(40, 200, 70), false);
                }
            }

            // last move marker
            if self.anim.is_none()
                && let Some(&c) = g.history.last()
            {
                let r = g.board.height(c) - 1;
                painter.circle_filled(center_of(c, r), cell * 0.08, egui::Color32::BLACK);
            }
            // column numbers
            for c in 0..COLS {
                painter.text(
                    egui::pos2(rect.min.x + (c as f32 + 0.5) * cell, rect.max.y + 12.0),
                    egui::Align2::CENTER_CENTER,
                    format!("{}", (b'a' + c as u8) as char),
                    egui::FontId::proportional(16.0),
                    ui.visuals().text_color(),
                );
            }
        });
        drop(g);
        if self.learn_confirm {
            egui::Window::new("Learn from this game?")
                .collapsible(false)
                .resizable(false)
                .default_width(440.0)
                .show(ctx, |ui| {
                    ui.label(
                        "Analyze the game on the board with the solver and book every \
                         position where the engine threw away a win or a draw. \
                         Early positions can take the solver a few minutes.",
                    );
                    ui.horizontal(|ui| {
                        if ui.button("Analyze").clicked() {
                            self.learn_confirm = false;
                            self.shared.start_learn();
                        }
                        if ui.button("Cancel").clicked() {
                            self.learn_confirm = false;
                        }
                    });
                });
        }
        {
            let mut l = self.shared.learn.lock().unwrap();
            if l.running || !l.report.is_empty() {
                let mut open = true;
                egui::Window::new("Learning from the game")
                    .collapsible(false)
                    .resizable(false)
                    .default_width(440.0)
                    .open(&mut open)
                    .show(ctx, |ui| {
                        ui.label(&l.report);
                        if l.running {
                            ui.horizontal(|ui| {
                                ui.spinner();
                                if ui.button("Cancel").clicked() {
                                    l.cancel = true;
                                    // Kill the solver so even a long query
                                    // aborts immediately.
                                    if let Some(pid) = l.solver_pid {
                                        let _ = std::process::Command::new("kill").arg(pid.to_string()).status();
                                    }
                                }
                            });
                        }
                    });
                if !open && !l.running {
                    l.report.clear();
                }
            }
        }
        if self.show_about {
            egui::Window::new("About Connect4-rs")
                .collapsible(false)
                .resizable(false)
                .open(&mut self.show_about)
                .show(ctx, |ui| {
                    ui.label(format!(
                        "Version {} ({}), built {}",
                        env!("CARGO_PKG_VERSION"), env!("GIT_HASH"), env!("BUILD_DATE")
                    ));
                    ui.label(&self.book_info);
                    ui.separator();
                    ui.label("Keyboard shortcuts:");
                    ui.monospace("1-7 / a-g   drop a piece in that column");
                    ui.monospace("N, Space    new game");
                    ui.monospace("S           swap who starts (new game)");
                    ui.monospace("U           undo your last move");
                    ui.monospace("+ / -       double / halve think time");
                    ui.monospace("H           toggle the hint rings");
                    ui.monospace("L           learn: book the engine's mistakes in this game");
                });
        }
        if changed {
            self.shared.notify();
        }
        if self.anim.is_some() {
            ctx.request_repaint();
        } else if thinking {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }
}
