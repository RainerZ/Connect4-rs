//! External solver in the engine seat (`--solver <path>`).
//!
//! Speaks Pascal Pons' Connect 4 solver line protocol: one position per
//! line (the played columns as digits 1-7), answered with the position
//! echoed back plus - in `-a` analyze mode, which we always use - one score
//! per column: positive = the side to move wins with best play (higher =
//! faster), negative = loses (lower = sooner), 0 = draw, -1000 = column
//! full. The process is spawned once and kept alive so its transposition
//! table stays warm across moves - that matters when no opening book is
//! available and the first queries are expensive.
//!
//! The child runs with its working directory set to the binary's directory,
//! so a `7x6.book` placed next to it is picked up automatically.

use crate::engine::{COL_ORDER, WIN_SCORE};
use std::io::{BufRead, BufReader, Write};
use std::path::PathBuf;
use std::process::{Child, ChildStdin, ChildStdout, Command, Stdio};

const INVALID_MOVE: i32 = -1000;

pub struct ExternalSolver {
    pub path: PathBuf,
    child: Child,
    stdin: ChildStdin,
    stdout: BufReader<ChildStdout>,
}

impl ExternalSolver {
    /// `spec` is the binary path, optionally followed by extra arguments
    /// (whitespace separated), e.g. "/path/c4solver -w" for the weak solver.
    pub fn spawn(spec: &str) -> Result<ExternalSolver, String> {
        let mut parts = spec.split_whitespace();
        let path = PathBuf::from(parts.next().ok_or("empty solver command")?);
        let mut cmd = Command::new(&path);
        cmd.args(parts).arg("-a").stdin(Stdio::piped()).stdout(Stdio::piped()).stderr(Stdio::null());
        if let Some(dir) = path.parent().filter(|d| !d.as_os_str().is_empty()) {
            cmd.current_dir(dir);
        }
        let mut child = cmd.spawn().map_err(|e| format!("cannot start solver {}: {e}", path.display()))?;
        let stdin = child.stdin.take().unwrap();
        let stdout = BufReader::new(child.stdout.take().unwrap());
        Ok(ExternalSolver { path, child, stdin, stdout })
    }

    /// Ask the solver for the best column (0-based) in the position reached
    /// by `history` (0-based columns). Returns the column, the solver's
    /// score mapped onto our scale (a proven win becomes WIN_SCORE - the
    /// GUI announces it - a proven loss a mildly negative score so the
    /// seat plays on, a draw 0) and the raw solver score (positive: the
    /// side to move wins, higher = faster; 0: draw; negative: loses).
    pub fn best_move(&mut self, history: &[usize]) -> Result<(usize, i32, i32), String> {
        let line: String = history.iter().map(|c| char::from(b'1' + *c as u8)).collect();
        writeln!(self.stdin, "{line}").map_err(|e| format!("solver stdin: {e}"))?;
        self.stdin.flush().map_err(|e| format!("solver stdin: {e}"))?;
        let mut reply = String::new();
        self.stdout.read_line(&mut reply).map_err(|e| format!("solver stdout: {e}"))?;
        if reply.is_empty() {
            return Err("solver closed its output (invalid position?)".into());
        }
        // Reply: the echoed position (absent for the empty board) followed
        // by one score per column.
        let toks: Vec<&str> = reply.split_whitespace().collect();
        let scores: Vec<i32> = toks
            .iter()
            .skip(if line.is_empty() { 0 } else { 1 })
            .map(|t| t.parse::<i32>().map_err(|_| format!("bad solver reply: {reply:?}")))
            .collect::<Result<_, _>>()?;
        if scores.len() != 7 {
            return Err(format!("bad solver reply: {reply:?}"));
        }
        // Best column: maximum score, centre-first tie break, full columns
        // (INVALID_MOVE) excluded. Negative maxima are the slowest loss.
        let col = COL_ORDER
            .iter()
            .copied()
            .filter(|&c| scores[c] != INVALID_MOVE)
            .max_by_key(|&c| scores[c])
            .ok_or_else(|| "no playable column".to_string())?;
        let s = scores[col];
        let mapped = if s > 0 {
            WIN_SCORE
        } else {
            // Spread losses/draws a little for the eval bar; stay far above
            // the resign threshold.
            s * 10
        };
        Ok((col, mapped, s))
    }
}

impl Drop for ExternalSolver {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
