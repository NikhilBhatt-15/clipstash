//! Plaintext clipboard history in SQLite.
//!
//! One row per distinct text. Copying something already in history moves it
//! back to the top instead of adding a duplicate. Unpinned rows are pruned to
//! the newest `keep` after every insert; pinned rows are never pruned and
//! always sort first.

use rusqlite::{params_from_iter, Connection, OptionalExtension};
use std::cell::Cell;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

/// Larger copies (log dumps, whole files) are not worth keeping as history.
pub const MAX_TEXT_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Clip {
    pub id: i64,
    pub text: String,
    pub pinned: bool,
    pub used_ms: i64,
}

pub struct Store {
    conn: Connection,
    keep: Cell<usize>,
}

pub fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis() as i64).unwrap_or(0)
}

impl Store {
    pub fn open(path: &Path, keep: usize) -> rusqlite::Result<Self> {
        Self::init(Connection::open(path)?, keep)
    }

    #[cfg(test)]
    pub fn open_in_memory(keep: usize) -> rusqlite::Result<Self> {
        Self::init(Connection::open_in_memory()?, keep)
    }

    fn init(conn: Connection, keep: usize) -> rusqlite::Result<Self> {
        conn.execute_batch(
            "PRAGMA journal_mode = WAL;
             PRAGMA synchronous = NORMAL;
             CREATE TABLE IF NOT EXISTS clips (
                 id         INTEGER PRIMARY KEY,
                 text       TEXT    NOT NULL UNIQUE,
                 created_ms INTEGER NOT NULL,
                 used_ms    INTEGER NOT NULL
             );
             CREATE INDEX IF NOT EXISTS clips_used ON clips (used_ms DESC);",
        )?;
        let version: i64 = conn.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version < 1 {
            conn.execute_batch(
                "ALTER TABLE clips ADD COLUMN pinned INTEGER NOT NULL DEFAULT 0;
                 PRAGMA user_version = 1;",
            )?;
        }
        Ok(Self { conn, keep: Cell::new(keep) })
    }

    /// Records a copy. Returns false when the text is not worth storing.
    pub fn add(&self, text: &str) -> rusqlite::Result<bool> {
        if text.trim().is_empty() || text.len() > MAX_TEXT_BYTES {
            return Ok(false);
        }
        let now = self.next_stamp()?;
        self.conn.execute(
            "INSERT INTO clips (text, created_ms, used_ms) VALUES (?1, ?2, ?2)
             ON CONFLICT (text) DO UPDATE SET used_ms = excluded.used_ms",
            (text, now),
        )?;
        self.prune()?;
        Ok(true)
    }

    pub fn set_keep(&self, keep: usize) -> rusqlite::Result<()> {
        self.keep.set(keep);
        self.prune()
    }

    fn prune(&self) -> rusqlite::Result<()> {
        self.conn.execute(
            "DELETE FROM clips WHERE pinned = 0 AND id NOT IN
                 (SELECT id FROM clips WHERE pinned = 0 ORDER BY used_ms DESC LIMIT ?1)",
            [self.keep.get() as i64],
        )?;
        Ok(())
    }

    /// Moves an entry to the top, e.g. after it was pasted from the popup.
    pub fn touch(&self, id: i64) -> rusqlite::Result<()> {
        let now = self.next_stamp()?;
        self.conn.execute("UPDATE clips SET used_ms = ?1 WHERE id = ?2", (now, id))?;
        Ok(())
    }

    pub fn set_pinned(&self, id: i64, pinned: bool) -> rusqlite::Result<()> {
        self.conn.execute("UPDATE clips SET pinned = ?1 WHERE id = ?2", (pinned, id))?;
        Ok(())
    }

    pub fn delete(&self, id: i64) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM clips WHERE id = ?1", [id])?;
        Ok(())
    }

    /// Removes everything except pinned entries.
    pub fn clear_unpinned(&self) -> rusqlite::Result<()> {
        self.conn.execute("DELETE FROM clips WHERE pinned = 0", [])?;
        Ok(())
    }

    /// Pinned first, then most recently used, matching every
    /// whitespace-separated term (case-insensitive for ASCII).
    pub fn search(&self, query: &str, limit: usize) -> rusqlite::Result<Vec<Clip>> {
        let terms: Vec<String> = query.split_whitespace().map(|t| format!("%{}%", escape_like(t))).collect();
        let mut sql = String::from("SELECT id, text, pinned, used_ms FROM clips");
        for i in 0..terms.len() {
            sql += if i == 0 { " WHERE " } else { " AND " };
            sql += &format!("text LIKE ?{} ESCAPE '\\'", i + 1);
        }
        sql += &format!(" ORDER BY pinned DESC, used_ms DESC LIMIT {limit}");

        let mut stmt = self.conn.prepare_cached(&sql)?;
        let rows = stmt.query_map(params_from_iter(terms.iter()), |r| {
            Ok(Clip { id: r.get(0)?, text: r.get(1)?, pinned: r.get(2)?, used_ms: r.get(3)? })
        })?;
        rows.collect()
    }

    pub fn count(&self) -> rusqlite::Result<usize> {
        self.conn.query_row("SELECT COUNT(*) FROM clips", [], |r| r.get::<_, i64>(0).map(|n| n as usize))
    }

    /// Wall-clock milliseconds, nudged forward so that two operations within
    /// the same millisecond still order correctly.
    fn next_stamp(&self) -> rusqlite::Result<i64> {
        let now = now_ms();
        let max: Option<i64> = self
            .conn
            .query_row("SELECT MAX(used_ms) FROM clips", [], |r| r.get(0))
            .optional()?
            .flatten();
        Ok(match max {
            Some(m) if m >= now => m + 1,
            _ => now,
        })
    }
}

fn escape_like(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        if matches!(c, '%' | '_' | '\\') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn texts(clips: &[Clip]) -> Vec<&str> {
        clips.iter().map(|c| c.text.as_str()).collect()
    }

    fn all(s: &Store) -> Vec<Clip> {
        s.search("", 100).unwrap()
    }

    #[test]
    fn newest_first() {
        let s = Store::open_in_memory(10).unwrap();
        for t in ["one", "two", "three"] {
            assert!(s.add(t).unwrap());
        }
        assert_eq!(texts(&all(&s)), ["three", "two", "one"]);
    }

    #[test]
    fn recopy_moves_to_top_without_duplicating() {
        let s = Store::open_in_memory(10).unwrap();
        for t in ["a", "b", "c", "a"] {
            s.add(t).unwrap();
        }
        assert_eq!(texts(&all(&s)), ["a", "c", "b"]);
    }

    #[test]
    fn skips_blank_and_oversized() {
        let s = Store::open_in_memory(10).unwrap();
        assert!(!s.add("   \r\n\t").unwrap());
        assert!(!s.add(&"x".repeat(MAX_TEXT_BYTES + 1)).unwrap());
        assert!(all(&s).is_empty());
    }

    #[test]
    fn prunes_to_keep() {
        let s = Store::open_in_memory(3).unwrap();
        for i in 0..6 {
            s.add(&format!("item {i}")).unwrap();
        }
        assert_eq!(texts(&all(&s)), ["item 5", "item 4", "item 3"]);
        s.set_keep(1).unwrap();
        assert_eq!(texts(&all(&s)), ["item 5"]);
    }

    #[test]
    fn touch_reorders() {
        let s = Store::open_in_memory(10).unwrap();
        for t in ["a", "b", "c"] {
            s.add(t).unwrap();
        }
        let a = s.search("a", 1).unwrap().remove(0);
        s.touch(a.id).unwrap();
        assert_eq!(texts(&all(&s)), ["a", "c", "b"]);
    }

    #[test]
    fn pinned_sort_first_and_survive_pruning_and_clear() {
        let s = Store::open_in_memory(2).unwrap();
        s.add("keep me").unwrap();
        let id = all(&s)[0].id;
        s.set_pinned(id, true).unwrap();
        for t in ["x", "y", "z"] {
            s.add(t).unwrap();
        }
        let list = all(&s);
        assert_eq!(texts(&list), ["keep me", "z", "y"]);
        assert!(list[0].pinned && !list[1].pinned);
        s.clear_unpinned().unwrap();
        assert_eq!(texts(&all(&s)), ["keep me"]);
        s.set_pinned(id, false).unwrap();
        assert!(!all(&s)[0].pinned);
    }

    #[test]
    fn delete_and_count() {
        let s = Store::open_in_memory(10).unwrap();
        for t in ["a", "b"] {
            s.add(t).unwrap();
        }
        assert_eq!(s.count().unwrap(), 2);
        s.delete(all(&s)[0].id).unwrap();
        assert_eq!(texts(&all(&s)), ["a"]);
    }

    #[test]
    fn search_requires_every_term_case_insensitively() {
        let s = Store::open_in_memory(10).unwrap();
        for t in ["git push origin main", "Git status", "cargo build"] {
            s.add(t).unwrap();
        }
        assert_eq!(texts(&s.search("GIT", 10).unwrap()), ["Git status", "git push origin main"]);
        assert_eq!(texts(&s.search("git main", 10).unwrap()), ["git push origin main"]);
        assert!(s.search("git cargo", 10).unwrap().is_empty());
    }

    #[test]
    fn like_wildcards_are_literal() {
        let s = Store::open_in_memory(10).unwrap();
        for t in ["100% done", "1000 done", "a_b", "axb"] {
            s.add(t).unwrap();
        }
        assert_eq!(texts(&s.search("0%", 10).unwrap()), ["100% done"]);
        assert_eq!(texts(&s.search("a_b", 10).unwrap()), ["a_b"]);
    }

    #[test]
    fn migrates_v0_database() {
        let dir = std::env::temp_dir().join(format!("clipstash-test-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("v0.db");
        let _ = std::fs::remove_file(&path);
        {
            let c = Connection::open(&path).unwrap();
            c.execute_batch(
                "CREATE TABLE clips (id INTEGER PRIMARY KEY, text TEXT NOT NULL UNIQUE,
                                     created_ms INTEGER NOT NULL, used_ms INTEGER NOT NULL);
                 INSERT INTO clips (text, created_ms, used_ms) VALUES ('old', 1, 1);",
            )
            .unwrap();
        }
        let s = Store::open(&path, 10).unwrap();
        let list = all(&s);
        assert_eq!(texts(&list), ["old"]);
        assert!(!list[0].pinned);
        drop(s);
        // Reopening an already-migrated database must not fail.
        Store::open(&path, 10).unwrap();
        let _ = std::fs::remove_dir_all(&dir);
    }
}
