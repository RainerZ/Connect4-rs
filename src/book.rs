//! Opening book distilled from a perfect solver (see the `bookgen` binary).
//!
//! Text format, one entry per line: `<position key hex> <col 1-7> <raw>`,
//! where the key is `Board::key` for the side to move, the column is the
//! solver's best move and `raw` its solver score (positive = the mover
//! wins). Looked up before every engine search; a hit is played instantly
//! and, since all distilled entries are winning moves, reported as a
//! proven win.

use std::collections::HashMap;
use std::path::Path;

pub struct Book {
    entries: HashMap<u64, (u8, i8)>,
}

impl Book {
    pub fn load(path: &Path) -> Result<Book, String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read book {}: {e}", path.display()))?;
        let mut entries = HashMap::new();
        for (n, line) in text.lines().enumerate() {
            let mut t = line.split_whitespace();
            let (Some(k), Some(c), Some(r)) = (t.next(), t.next(), t.next()) else {
                continue;
            };
            let key = u64::from_str_radix(k, 16).map_err(|_| format!("{}:{}: bad key", path.display(), n + 1))?;
            let col: u8 = c.parse().map_err(|_| format!("{}:{}: bad col", path.display(), n + 1))?;
            let raw: i8 = r.parse().map_err(|_| format!("{}:{}: bad score", path.display(), n + 1))?;
            if !(1..=7).contains(&col) {
                return Err(format!("{}:{}: bad col", path.display(), n + 1));
            }
            entries.insert(key, (col - 1, raw));
        }
        Ok(Book { entries })
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Book move (0-based column) and raw solver score for the position
    /// with `key` (side to move included in the key).
    pub fn get(&self, key: u64) -> Option<(usize, i32)> {
        self.entries.get(&key).map(|&(c, r)| (c as usize, r as i32))
    }
}
