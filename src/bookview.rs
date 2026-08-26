//! `bookview`: make book entries and position keys human-readable.
//!
//!   cargo run --release --bin bookview -- 40820204182 80810204086 ...
//!   cargo run --release --bin bookview -- 1,2,2
//!   cargo run --release --bin bookview -- --replay 1,2,2
//!
//! Each argument is either a position key (hex, as found in the book
//! files) or a move list (columns 1-7, optionally comma separated - only
//! digits 1-7 are treated as moves). Prints the board in the same ASCII
//! form the GUI/MCP uses, the side to move, a legal move history reaching
//! the position (reconstructed by backtracking when a key is given), the
//! book verdicts from opening-book.txt / corrective-book.txt if present,
//! and a ready-to-paste replay command for the running GUI; with --replay the
//! position is pushed onto the GUI directly (if the engine is then to
//! move, it answers - book hits instantly - and the reply is reported).

#[allow(dead_code)]
mod book;
#[allow(dead_code)]
mod engine;

use book::Book;
use engine::{Board, Piece, COLS, ROWS};
use std::io::{BufRead, BufReader, Write};
use std::net::TcpStream;

/// Invert the 49-bit position key: per column the value is
/// `mover_stones + 2^height`, so the highest set bit is the height and the
/// bits below it are the mover's stones. Returns (owner grid, heights,
/// side to move); None if the key is malformed.
fn decode(key: u64) -> Option<([[Option<Piece>; 6]; 7], [usize; 7], Piece)> {
    let mut grid = [[None; 6]; 7];
    let mut heights = [0usize; 7];
    let mut mover_bits = [[false; 6]; 7];
    let mut total = 0usize;
    for c in 0..COLS {
        let k = (key >> (7 * c)) & 0x7f;
        if k == 0 {
            return None;
        }
        let h = 63 - (k as u64).leading_zeros() as usize;
        if h > ROWS {
            return None;
        }
        let mover = k - (1 << h);
        heights[c] = h;
        total += h;
        for r in 0..h {
            mover_bits[c][r] = mover & (1 << r) != 0;
        }
        if mover >> h != 0 {
            return None;
        }
    }
    // Alternation fixes the side to move; the mover's stones are known,
    // everything else occupied belongs to the opponent.
    let to_move = if total % 2 == 0 { Piece::Red } else { Piece::Yellow };
    for c in 0..COLS {
        for r in 0..heights[c] {
            grid[c][r] = Some(if mover_bits[c][r] { to_move } else { to_move.other() });
        }
    }
    // Sanity: each side's stone count must match the move alternation.
    let red: usize = (0..COLS).map(|c| (0..heights[c]).filter(|&r| grid[c][r] == Some(Piece::Red)).count()).sum();
    if red != total.div_ceil(2) {
        return None;
    }
    Some((grid, heights, to_move))
}

/// Reconstruct a legal move sequence reaching the position by *unmoving*:
/// alternation dictates who made the last move, and that stone must be on
/// top of some column - try each candidate, recurse on the remainder,
/// backtrack on dead ends. Positions have many histories (transpositions);
/// this returns the first one found, which is fine because every consumer
/// (board display, book lookup, replay, solver) depends only on the
/// resulting position, never on the path. Deliberately does not verify the
/// "nobody had already won earlier" rule - book-derived keys are always
/// live games.
fn find_history(grid: &[[Option<Piece>; 6]; 7], heights: &mut [usize; 7], n: usize, out: &mut Vec<usize>) -> bool {
    if n == 0 {
        return true;
    }
    let last = if n % 2 == 1 { Piece::Red } else { Piece::Yellow };
    for c in 0..COLS {
        if heights[c] > 0 && grid[c][heights[c] - 1] == Some(last) {
            heights[c] -= 1;
            out.push(c);
            if find_history(grid, heights, n - 1, out) {
                return true;
            }
            out.pop();
            heights[c] += 1;
        }
    }
    false
}

/// Push the position onto the running GUI and report what happened.
fn replay_to_gui(history: &[usize]) {
    let moves: Vec<String> = history.iter().map(|c| (c + 1).to_string()).collect();
    let req = format!("{{\"cmd\":\"replay\",\"moves\":[{}]}}", moves.join(","));
    let reply = (|| -> Result<serde_json::Value, String> {
        let mut s = TcpStream::connect(("127.0.0.1", 4444)).map_err(|e| format!("GUI not reachable on port 4444 (is it running?): {e}"))?;
        writeln!(s, "{req}").map_err(|e| e.to_string())?;
        let mut line = String::new();
        BufReader::new(s).read_line(&mut line).map_err(|e| e.to_string())?;
        serde_json::from_str(&line).map_err(|e| e.to_string())
    })();
    match reply {
        Ok(v) => {
            let after: Vec<u64> = v["history"].as_array().map(|a| a.iter().filter_map(|m| m.as_u64()).collect()).unwrap_or_default();
            print!("replayed onto the GUI");
            if after.len() > history.len() {
                print!("; engine answered col {}", after.last().unwrap());
                if v["last_search"]["book"].as_bool() == Some(true) {
                    print!(" (from book)");
                }
            }
            println!(" - {}", v["message"].as_str().unwrap_or(""));
        }
        Err(e) => println!("replay failed: {e}"),
    }
}

fn show(history: &[usize], books: &[(String, Book)], replay: bool) {
    let mut b = Board::new();
    let mut p = Piece::Red;
    for &c in history {
        b.make(c, p);
        p = p.other();
    }
    let key = b.key(p);
    let hist1: Vec<String> = history.iter().map(|c| (c + 1).to_string()).collect();
    println!("position after {} stones, {} to move  (key {key:x})", history.len(), if p == Piece::Red { "red" } else { "yellow" });
    println!("history: {}", hist1.join(","));
    for r in (0..ROWS).rev() {
        let row: Vec<&str> = (0..COLS)
            .map(|c| match b.get(c, r) {
                Some(Piece::Red) => "R",
                Some(Piece::Yellow) => "Y",
                None => ".",
            })
            .collect();
        println!("  {}", row.join(" "));
    }
    println!("  1 2 3 4 5 6 7");
    let mut hit = false;
    for (name, bk) in books {
        if let Some((col, raw)) = bk.get(key) {
            println!("book: {name}: play col {} (solver verdict {raw:+} for the side to move)", col + 1);
            hit = true;
        }
    }
    if !hit && !books.is_empty() {
        println!("book: no entry for this position");
    }
    if replay {
        replay_to_gui(history);
    } else {
        println!("replay: printf '{{\"cmd\":\"replay\",\"moves\":[{}]}}\\n' | nc 127.0.0.1 4444", hist1.join(","));
    }
    println!();
}

fn main() {
    let mut args: Vec<String> = std::env::args().skip(1).collect();
    let replay = args.iter().any(|a| a == "--replay" || a == "-r");
    args.retain(|a| a != "--replay" && a != "-r");
    if args.is_empty() {
        eprintln!("usage: bookview [--replay] <key-hex | moves like 1,2,2> ...");
        std::process::exit(2);
    }
    let mut books = Vec::new();
    for name in ["opening-book.txt", "corrective-book.txt"] {
        if let Ok(b) = Book::load(std::path::Path::new(name)) {
            books.push((name.to_string(), b));
        }
    }
    for a in &args {
        let cleaned: String = a.chars().filter(|c| *c != ',').collect();
        if !cleaned.is_empty() && cleaned.chars().all(|c| ('1'..='7').contains(&c)) {
            // A move list.
            let hist: Vec<usize> = cleaned.chars().map(|c| c as usize - '1' as usize).collect();
            show(&hist, &books, replay);
        } else if let Ok(key) = u64::from_str_radix(&cleaned, 16) {
            match decode(key) {
                Some((grid, mut heights, _)) => {
                    let n = heights.iter().sum();
                    let mut hist = Vec::new();
                    if find_history(&grid, &mut heights, n, &mut hist) {
                        hist.reverse();
                        show(&hist, &books, replay);
                    } else {
                        eprintln!("{a}: no legal move sequence reaches this position");
                    }
                }
                None => eprintln!("{a}: not a valid position key"),
            }
        } else {
            eprintln!("{a}: neither a move list (digits 1-7) nor a hex key");
        }
    }
}
