mod engine;
mod game;
mod hints;
mod server;

use eframe::egui;
use engine::{Piece, COLS, ROWS};
use game::{Shared, Status, DEFAULT_BUDGET};
use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::{Duration, Instant};

/// Usage: connect4-rs [--budget <seconds>] [--no-hints]
fn parse_args() -> (Duration, bool) {
    let mut budget = DEFAULT_BUDGET;
    let mut hints = true;
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
            "-h" | "--help" => {
                println!(
                    "usage: connect4-rs [--budget <seconds>] [--no-hints]\n  --budget    think time per engine move (default {})\n  --no-hints  start without LLM assistance hints in the socket/MCP state (toggle with H)",
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
    (budget, hints)
}

fn main() -> eframe::Result {
    let (budget, hints) = parse_args();
    let shared = Shared::new(false, budget, hints);
    {
        let s = shared.clone();
        std::thread::Builder::new().name("engine".into()).stack_size(16 << 20).spawn(move || s.engine_loop()).unwrap();
    }
    {
        let s = shared.clone();
        std::thread::Builder::new().name("server".into()).spawn(move || server::run(s)).unwrap();
    }
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default().with_inner_size([640.0, 640.0]).with_title("Connect4-rs"),
        ..Default::default()
    };
    eframe::run_native("Connect4-rs", options, Box::new(|cc| {
        *shared.repaint.lock().unwrap() = Some(cc.egui_ctx.clone());
        Ok(Box::new(App { shared, seen_moves: 0, anim: None }))
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
        // +/- double/halve the think time, H toggle LLM hints
        let mut changed = false;
        ctx.input(|i| {
            let mut g = self.shared.game.lock().unwrap();
            for (k, col) in [egui::Key::Num1, egui::Key::Num2, egui::Key::Num3, egui::Key::Num4, egui::Key::Num5, egui::Key::Num6, egui::Key::Num7].iter().zip(0..COLS) {
                if i.key_pressed(*k) && g.human_move(col) {
                    changed = true;
                }
            }
            if i.key_pressed(egui::Key::N) || i.key_pressed(egui::Key::Space) {
                let (es, bud, h) = (g.engine_starts, g.budget, g.hints);
                *g = game::Game::new(es, bud, h);
                changed = true;
            }
            if i.key_pressed(egui::Key::S) {
                let (es, bud, h) = (!g.engine_starts, g.budget, g.hints);
                *g = game::Game::new(es, bud, h);
                changed = true;
            }
            if i.key_pressed(egui::Key::Plus) || i.key_pressed(egui::Key::Equals) {
                g.budget = (g.budget * 2).min(Duration::from_secs(60));
            }
            if i.key_pressed(egui::Key::Minus) {
                g.budget = (g.budget / 2).max(Duration::from_millis(50));
            }
            if i.key_pressed(egui::Key::H) {
                g.hints = !g.hints;
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
            .min((outer.height() - 150.0) / ROWS as f32)
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
                format!(
                    "last engine move: col {}  score {}  depth {}  nodes {}  {} ms ({:.1} Mnodes/s)",
                    ls.col, ls.score, ls.depth, ls.nodes, ls.millis,
                    ls.nodes as f64 / (ls.millis.max(1) as f64 * 1000.0)
                )
            } else {
                String::new()
            };
            ui.label(line2);
            ui.add_space(4.0);

            // Settings strip: the keyboard shortcuts stay available, this is
            // the discoverable way to reach the same things.
            ui.horizontal(|ui| {
                if ui.button("New game").clicked() {
                    let (es, bud, h) = (g.engine_starts, g.budget, g.hints);
                    *g = game::Game::new(es, bud, h);
                    changed = true;
                }
                let mut es = g.engine_starts;
                if ui.checkbox(&mut es, "Engine starts").on_hover_text("Applies to the next new game").changed() {
                    g.engine_starts = es;
                }
                let mut h = g.hints;
                if ui.checkbox(&mut h, "Hints").changed() {
                    g.hints = h;
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
            if resp.clicked()
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
            if g.hints && g.status == Status::HumanToMove {
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
                    format!("{}", c + 1),
                    egui::FontId::proportional(16.0),
                    ui.visuals().text_color(),
                );
            }
        });
        drop(g);
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
