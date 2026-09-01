//! Opening book distilled from a perfect solver (see the `bookgen` binary).
//!
//! Text format, one entry per line: `<position key hex> <col 1-7> <raw>`,
//! where the key is `Board::key` for the side to move, the column is the
//! solver's best move and `raw` its solver score (positive = the mover
//! wins). Everything after a `#` is a comment - used by learned entries to
//! record their provenance. Looked up before every engine search.

use std::collections::HashMap;
use std::path::Path;

pub struct Book {
    entries: HashMap<u64, (u8, i8)>,
}

impl Book {
    pub fn empty() -> Book {
        Book { entries: HashMap::new() }
    }

    pub fn load(path: &Path) -> Result<Book, String> {
        let mut b = Book { entries: HashMap::new() };
        b.merge(path)?;
        Ok(b)
    }

    /// Load a further book file into this one (later entries win on
    /// duplicate keys - which cannot happen between the engine-start book
    /// and the corrective book, as keys include the side to move).
    pub fn merge(&mut self, path: &Path) -> Result<(), String> {
        let text = std::fs::read_to_string(path).map_err(|e| format!("cannot read book {}: {e}", path.display()))?;
        let entries = &mut self.entries;
        for (n, line) in text.lines().enumerate() {
            // Strip comments: full-line and trailing (provenance notes).
            let line = line.split('#').next().unwrap_or("");
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
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// True if the position is already covered.
    pub fn contains(&self, key: u64) -> bool {
        self.entries.contains_key(&key)
    }

    /// Add an entry at runtime (learned corrections become active without
    /// a restart; persisting to the file is the caller's job).
    pub fn insert(&mut self, key: u64, col: usize, raw: i32) {
        self.entries.insert(key, (col as u8, raw as i8));
    }

    /// Book move (0-based column) and raw solver score for the position
    /// with `key` (side to move included in the key).
    pub fn get(&self, key: u64) -> Option<(usize, i32)> {
        self.entries.get(&key).map(|&(c, r)| (c as usize, r as i32))
    }
}
