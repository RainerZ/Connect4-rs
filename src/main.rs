mod engine;
mod game;
mod hints;
mod server;

use eframe::egui;
use engine::{Piece, COLS, ROWS};
use game::{Shared, Status, DEFAULT_BUDGET};
use std::time::Duration;
use std::sync::atomic::Ordering;
use std::sync::Arc;

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
        Ok(Box::new(App { shared }))
    }))
}

struct App {
    shared: Arc<Shared>,
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
        if changed {
            self.shared.notify();
        }

        let g = self.shared.game.lock().unwrap();
        let thinking = g.status == Status::Thinking;
        {
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
            let mut info = format!(
                "You: {}   Engine: {}   Think time: {} s (+/-)   LLM hints: {} (H)   Keys: 1-7 move, N new game, S swap starter",
                if g.human == Piece::Red { "Red" } else { "Yellow" },
                if g.engine() == Piece::Red { "Red" } else { "Yellow" },
                g.budget.as_secs_f64(),
                if g.hints { "on" } else { "off" }
            );
            if thinking {
                info += &format!(
                    "\nsearching depth {}  nodes {}",
                    self.shared.stats.depth.load(Ordering::Relaxed),
                    self.shared.stats.nodes.load(Ordering::Relaxed)
                );
            } else if let Some(ls) = &g.last_search {
                info += &format!(
                    "\nlast engine move: col {}  score {}  depth {}  nodes {}  {} ms ({:.1} Mnodes/s)",
                    ls.col, ls.score, ls.depth, ls.nodes, ls.millis,
                    ls.nodes as f64 / (ls.millis.max(1) as f64 * 1000.0)
                );
            }
            ui.label(info);
            ui.add_space(8.0);

            let avail = ui.available_size();
            let cell = (avail.x / COLS as f32).min(avail.y / ROWS as f32).min(90.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(cell * COLS as f32, cell * ROWS as f32), egui::Sense::hover());
            let painter = ui.painter();
            painter.rect_filled(rect, 6.0, egui::Color32::from_rgb(30, 60, 160));
            let win = g.board.winning_line();
            for c in 0..COLS {
                for r in 0..ROWS {
                    let center = egui::pos2(rect.min.x + (c as f32 + 0.5) * cell, rect.max.y - (r as f32 + 0.5) * cell);
                    let color = match g.board.get(c, r) {
                        Some(Piece::Red) => egui::Color32::from_rgb(220, 40, 40),
                        Some(Piece::Yellow) => egui::Color32::from_rgb(240, 210, 30),
                        None => egui::Color32::WHITE,
                    };
                    painter.circle_filled(center, cell * 0.42, color);
                    if win.map_or(false, |w| w.contains(&(c, r))) {
                        painter.circle_stroke(center, cell * 0.42, egui::Stroke::new(4.0, egui::Color32::BLACK));
                    }
                }
            }
            // last move marker
            if let Some(&c) = g.history.last() {
                let r = g.board.height(c) - 1;
                let center = egui::pos2(rect.min.x + (c as f32 + 0.5) * cell, rect.max.y - (r as f32 + 0.5) * cell);
                painter.circle_filled(center, cell * 0.08, egui::Color32::BLACK);
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
        }
        if thinking {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
    }
}
