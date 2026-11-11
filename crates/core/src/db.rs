//! The per-project SQLite database (`<project>/.binning/project.db`). Table and column names are
//! the Android app's schema version 5, so the two apps describe a project the same way.

use crate::analysis::{ActionEntry, AnalysisResult, ItemKind};
use crate::error::{now_ms, Error, Result};
use crate::sorting::{Assignment, CategoryInfo, NewCategoryEntity};
use parking_lot::Mutex;
use rusqlite::{params, params_from_iter, Connection, OpenFlags, OptionalExtension, Row, Transaction, TransactionBehavior};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub const PROJECT_DB_VERSION: i32 = 5;
/// Room's identity hash for schema v5, written so the Android app recognises the schema.
pub const IDENTITY_HASH_V5: &str = "aad19e007824238ddc0889e1b29274b0";

/// Room's identity hash for each schema version the Android app has shipped.
pub fn known_hash(version: i32) -> Option<&'static str> {
    match version {
        1 => Some("8a77c185aacfe349637924448fd8027c"),
        2 => Some("8ed51d8952043c923ca05ffc2c9ac4f0"),
        3 => Some("95cf2f611134324a62782362640a5820"),
        4 => Some("e2915b6307ad16155355e32e657395f2"),
        5 => Some(IDENTITY_HASH_V5),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum ItemStatus {
    Pending,
    Analyzing,
    Analyzed,
    Failed,
    Sorted,
    Binned,
    Missing,
}

impl ItemStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            ItemStatus::Pending => "Pending",
            ItemStatus::Analyzing => "Analyzing",
            ItemStatus::Analyzed => "Analyzed",
            ItemStatus::Failed => "Failed",
            ItemStatus::Sorted => "Sorted",
            ItemStatus::Binned => "Binned",
            ItemStatus::Missing => "Missing",
        }
    }
    pub fn parse(s: &str) -> Self {
        match s {
            "Analyzing" => ItemStatus::Analyzing,
            "Analyzed" => ItemStatus::Analyzed,
            "Failed" => ItemStatus::Failed,
            "Sorted" => ItemStatus::Sorted,
            "Binned" => ItemStatus::Binned,
            "Missing" => ItemStatus::Missing,
            _ => ItemStatus::Pending,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Item {
    pub id: i64,
    pub content_hash: String,
    pub original_path: String,
    pub original_name: String,
    pub current_path: String,
    pub kind: ItemKind,
    pub taken_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
    pub status: ItemStatus,
    pub category_id: Option<i64>,
    pub friendly_name: Option<String>,
    pub name_set_by_user: bool,
    pub category_set_by_user: bool,
    pub added_at: i64,
    pub failure_reason: Option<String>,
    pub attempts: i64,
}

const ITEM_COLS: &str = "id, contentHash, originalPath, originalName, currentPath, kind, takenAt, durationMs, width, height, status, categoryId, friendlyName, nameSetByUser, categorySetByUser, addedAt, failureReason, attempts";

fn item_from(r: &Row) -> rusqlite::Result<Item> {
    Ok(Item {
        id: r.get(0)?,
        content_hash: r.get(1)?,
        original_path: r.get(2)?,
        original_name: r.get(3)?,
        current_path: r.get(4)?,
        kind: ItemKind::parse(&r.get::<_, String>(5)?).unwrap_or(ItemKind::Photo),
        taken_at: r.get(6)?,
        duration_ms: r.get(7)?,
        width: r.get(8)?,
        height: r.get(9)?,
        status: ItemStatus::parse(&r.get::<_, String>(10)?),
        category_id: r.get(11)?,
        friendly_name: r.get(12)?,
        name_set_by_user: r.get(13)?,
        category_set_by_user: r.get(14)?,
        added_at: r.get(15)?,
        failure_reason: r.get(16)?,
        attempts: r.get(17)?,
    })
}

/// A file to register as a new Pending item.
#[derive(Debug, Clone)]
pub struct NewItem {
    pub content_hash: String,
    pub path: String,
    pub name: String,
    pub kind: ItemKind,
    pub taken_at: Option<i64>,
    pub duration_ms: Option<i64>,
    pub width: Option<i64>,
    pub height: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Category {
    pub id: i64,
    pub name: String,
    pub description: String,
    pub created_by: String,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Analysis {
    pub item_id: i64,
    pub description: String,
    pub actions: Vec<ActionEntry>,
    pub actions_json: String,
    pub suggested_name: String,
    pub shot_type: String,
    pub model_id: String,
    pub prompt_version: String,
    pub stop_reason: String,
    pub ttft_ms: f64,
    pub generated_tokens: i64,
    pub analyzed_at: i64,
}

/// Numbers GenieX reports for one reply.
#[derive(Debug, Clone, Default, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Profile {
    /// `eos`, `length`, `user` or `stop_sequence`.
    pub stop_reason: String,
    pub ttft_ms: f64,
    pub prompt_tokens: i64,
    pub generated_tokens: i64,
    pub decoding_speed: f64,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct SortRun {
    pub id: i64,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub model_id: String,
    pub items: i64,
    pub sorted: i64,
    pub new_categories: i64,
    pub outcome: String,
    pub message: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct UncategorizedItem {
    pub id: i64,
    pub kind: ItemKind,
    pub description: String,
    pub actions_json: String,
    pub suggested_name: String,
    pub tags: Vec<String>,
    pub name_set_by_user: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MoveRun {
    pub id: i64,
    pub kind: String,
    pub started_at: i64,
    pub finished_at: Option<i64>,
    pub state: String,
    pub message: Option<String>,
    pub removed_folders: Option<String>,
    pub in_place: i64,
    pub check_ok: Option<bool>,
    pub report_closed: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MoveStep {
    pub id: i64,
    pub run_id: i64,
    pub item_id: i64,
    pub source: String,
    pub dest: String,
    pub state: String,
    pub bytes: i64,
    pub attempts: i64,
    pub keep_source: bool,
    pub numbered: bool,
    pub error: Option<String>,
}

/// What the move planner needs of one item.
#[derive(Debug, Clone, PartialEq)]
pub struct BinItem {
    pub id: i64,
    pub status: ItemStatus,
    pub category_id: Option<i64>,
    pub friendly_name: Option<String>,
    pub current_path: String,
    pub original_name: String,
    pub failure_reason: Option<String>,
}

/// One Project Media tile.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MediaRow {
    pub id: i64,
    pub kind: ItemKind,
    pub duration_ms: Option<i64>,
    pub taken_at: Option<i64>,
    pub status: ItemStatus,
    pub category_id: Option<i64>,
    pub friendly_name: Option<String>,
    pub suggested_name: Option<String>,
    pub shot_type: Option<String>,
    pub name_set_by_user: bool,
    pub category_set_by_user: bool,
    pub current_path: String,
    pub original_name: String,
    pub failure_reason: Option<String>,
    pub has_analysis: bool,
    /// Which field produced a search hit: Name, Category, Tags, Actions or Description.
    pub matched_in: Option<&'static str>,
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS item (
  id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, contentHash TEXT NOT NULL, originalPath TEXT NOT NULL,
  originalName TEXT NOT NULL, currentPath TEXT NOT NULL, kind TEXT NOT NULL, takenAt INTEGER, durationMs INTEGER,
  width INTEGER, height INTEGER, status TEXT NOT NULL, categoryId INTEGER, friendlyName TEXT,
  nameSetByUser INTEGER NOT NULL, categorySetByUser INTEGER NOT NULL, addedAt INTEGER NOT NULL, failureReason TEXT,
  attempts INTEGER NOT NULL DEFAULT 0);
CREATE UNIQUE INDEX IF NOT EXISTS index_item_contentHash ON item (contentHash);
CREATE UNIQUE INDEX IF NOT EXISTS index_item_currentPath ON item (currentPath);
CREATE TABLE IF NOT EXISTS analysis (
  itemId INTEGER NOT NULL, description TEXT NOT NULL, actionsJson TEXT NOT NULL, suggestedName TEXT NOT NULL,
  shotType TEXT NOT NULL, rawJson TEXT NOT NULL, modelId TEXT NOT NULL, promptVersion TEXT NOT NULL,
  stopReason TEXT NOT NULL, ttftMs REAL NOT NULL, generatedTokens INTEGER NOT NULL, analyzedAt INTEGER NOT NULL,
  PRIMARY KEY(itemId), FOREIGN KEY(itemId) REFERENCES item(id) ON UPDATE NO ACTION ON DELETE CASCADE);
CREATE TABLE IF NOT EXISTS tag (id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, name TEXT NOT NULL);
CREATE UNIQUE INDEX IF NOT EXISTS index_tag_name ON tag (name);
CREATE TABLE IF NOT EXISTS item_tag (
  itemId INTEGER NOT NULL, tagId INTEGER NOT NULL, PRIMARY KEY(itemId, tagId),
  FOREIGN KEY(itemId) REFERENCES item(id) ON UPDATE NO ACTION ON DELETE CASCADE,
  FOREIGN KEY(tagId) REFERENCES tag(id) ON UPDATE NO ACTION ON DELETE CASCADE);
CREATE INDEX IF NOT EXISTS index_item_tag_tagId ON item_tag (tagId);
CREATE TABLE IF NOT EXISTS category (
  id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, name TEXT NOT NULL, description TEXT NOT NULL,
  createdBy TEXT NOT NULL, folderName TEXT);
CREATE TABLE IF NOT EXISTS move_run (
  id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, kind TEXT NOT NULL, startedAt INTEGER NOT NULL, finishedAt INTEGER,
  state TEXT NOT NULL, message TEXT, removedFolders TEXT, inPlace INTEGER NOT NULL DEFAULT 0, checkOk INTEGER,
  reportClosed INTEGER NOT NULL DEFAULT 0);
CREATE TABLE IF NOT EXISTS move_step (
  id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, runId INTEGER NOT NULL, itemId INTEGER NOT NULL, source TEXT NOT NULL,
  dest TEXT NOT NULL, state TEXT NOT NULL, bytes INTEGER NOT NULL DEFAULT 0, attempts INTEGER NOT NULL DEFAULT 0,
  keepSource INTEGER NOT NULL DEFAULT 0, numbered INTEGER NOT NULL DEFAULT 0, error TEXT);
CREATE INDEX IF NOT EXISTS index_move_step_runId ON move_step (runId);
CREATE TABLE IF NOT EXISTS project_meta (
  id INTEGER NOT NULL, name TEXT NOT NULL, createdAt INTEGER NOT NULL, schemaVersion INTEGER NOT NULL,
  stoppedByUser INTEGER NOT NULL DEFAULT 0, PRIMARY KEY(id));
CREATE TABLE IF NOT EXISTS sort_run (
  id INTEGER PRIMARY KEY AUTOINCREMENT NOT NULL, startedAt INTEGER NOT NULL, finishedAt INTEGER, modelId TEXT NOT NULL,
  promptVersion TEXT NOT NULL, items INTEGER NOT NULL, sorted INTEGER NOT NULL, newCategories INTEGER NOT NULL,
  outcome TEXT NOT NULL, message TEXT);
CREATE TABLE IF NOT EXISTS removed_file (path TEXT NOT NULL, removedAt INTEGER NOT NULL, reason TEXT NOT NULL, PRIMARY KEY(path));
CREATE TABLE IF NOT EXISTS room_master_table (id INTEGER PRIMARY KEY, identity_hash TEXT);
CREATE VIRTUAL TABLE IF NOT EXISTS item_fts USING fts5(description, actions, tags, friendly_name, category, tokenize='porter unicode61');
"#;

/// Rebuilds an item's FTS row from the tables; an item with no analysis still gets its name and category.
const FTS_REBUILD: &str = "INSERT INTO item_fts(rowid, description, actions, tags, friendly_name, category) \
SELECT item.id, COALESCE(analysis.description, ''), COALESCE(analysis.actionsJson, ''), \
COALESCE((SELECT group_concat(tag.name, ' ') FROM item_tag INNER JOIN tag ON tag.id = item_tag.tagId WHERE item_tag.itemId = item.id), ''), \
COALESCE(item.friendlyName, ''), COALESCE(category.name, '') \
FROM item LEFT JOIN analysis ON analysis.itemId = item.id LEFT JOIN category ON category.id = item.categoryId WHERE item.id = ?1";

/// Items a Re-sort makes waiting again: sorted or binned, not placed by the user, with an analysis.
const RESORTABLE: &str =
    "status IN ('Sorted', 'Binned') AND categorySetByUser = 0 AND EXISTS (SELECT 1 FROM analysis WHERE analysis.itemId = item.id)";

/// Under SQLite's bound-variable limit.
const CHUNK: usize = 500;

fn placeholders(n: usize) -> String {
    vec!["?"; n].join(",")
}

pub fn refresh_fts(c: &Connection, item_id: i64) -> Result<()> {
    c.execute("DELETE FROM item_fts WHERE rowid = ?1", [item_id])?;
    c.execute(FTS_REBUILD, [item_id])?;
    Ok(())
}

/// What a read-only look at a candidate `.binning/project.db` found.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct DatabaseFacts {
    pub exists: bool,
    pub readable: bool,
    pub user_version: Option<i32>,
    pub identity_hash: Option<String>,
    pub meta_name: Option<String>,
    pub has_meta: bool,
    pub quick_check_ok: bool,
}

/// A read-only look at a candidate project database. Never writes: a missing file is not created
/// and nothing is migrated.
pub fn probe(db_file: &Path) -> DatabaseFacts {
    if !db_file.is_file() {
        return DatabaseFacts::default();
    }
    let read = || -> rusqlite::Result<DatabaseFacts> {
        let c = Connection::open_with_flags(db_file, OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX)?;
        let has_table = |name: &str| -> rusqlite::Result<bool> {
            c.query_row("SELECT COUNT(*) FROM sqlite_master WHERE type = 'table' AND name = ?1", [name], |r| r.get::<_, i64>(0)).map(|n| n > 0)
        };
        let quick: String = c.query_row("PRAGMA quick_check", [], |r| r.get(0))?;
        let version: i32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        let hash = if has_table("room_master_table")? {
            c.query_row("SELECT identity_hash FROM room_master_table WHERE id = 42", [], |r| r.get::<_, Option<String>>(0)).optional()?.flatten()
        } else {
            None
        };
        let has_meta_table = has_table("project_meta")?;
        let meta_name = if has_meta_table {
            c.query_row("SELECT name FROM project_meta WHERE id = 1", [], |r| r.get::<_, String>(0)).optional()?
        } else {
            None
        };
        Ok(DatabaseFacts {
            exists: true,
            readable: true,
            user_version: Some(version),
            identity_hash: hash,
            has_meta: meta_name.is_some(),
            meta_name,
            quick_check_ok: quick == "ok",
        })
    };
    read().unwrap_or(DatabaseFacts { exists: true, ..Default::default() })
}

/// One open project database. Every call takes the one connection's lock, so calls from the
/// pipeline thread and the UI never interleave inside a transaction.
pub struct ProjectDb {
    conn: Mutex<Connection>,
}

impl ProjectDb {
    /// Opens (creating if needed) the database at `db_file`; `name` is written to a new project's meta row.
    pub fn open(db_file: &Path, name: &str) -> Result<Self> {
        if let Some(dir) = db_file.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let c = Connection::open(db_file)?;
        c.busy_timeout(std::time::Duration::from_secs(10))?;
        // One plain file with no -wal beside it, as on Android: safe on network and removable drives.
        c.query_row("PRAGMA journal_mode = TRUNCATE", [], |_| Ok(()))?;
        c.execute_batch("PRAGMA foreign_keys = ON;")?;
        let version: i32 = c.query_row("PRAGMA user_version", [], |r| r.get(0))?;
        if version > PROJECT_DB_VERSION {
            return Err(Error::msg("Made with a newer app version. Update to open it."));
        }
        if version != 0 && version < PROJECT_DB_VERSION {
            migrate(&c, version)?;
        }
        c.execute_batch(SCHEMA)?;
        c.execute("INSERT OR REPLACE INTO room_master_table (id, identity_hash) VALUES (42, ?1)", [IDENTITY_HASH_V5])?;
        c.execute_batch(&format!("PRAGMA user_version = {PROJECT_DB_VERSION};"))?;
        c.execute(
            "INSERT OR IGNORE INTO project_meta (id, name, createdAt, schemaVersion, stoppedByUser) VALUES (1, ?1, ?2, ?3, 0)",
            params![name, now_ms(), PROJECT_DB_VERSION],
        )?;
        Ok(Self { conn: Mutex::new(c) })
    }

    pub fn read<T>(&self, f: impl FnOnce(&Connection) -> Result<T>) -> Result<T> {
        f(&self.conn.lock())
    }

    /// Runs `f` in one immediate transaction; an error rolls everything back.
    pub fn tx<T>(&self, f: impl FnOnce(&Transaction) -> Result<T>) -> Result<T> {
        let mut c = self.conn.lock();
        let t = c.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let out = f(&t)?;
        t.commit()?;
        Ok(out)
    }

    // ---- meta ----

    pub fn meta_name(&self) -> Result<Option<String>> {
        self.read(|c| Ok(c.query_row("SELECT name FROM project_meta WHERE id = 1", [], |r| r.get(0)).optional()?))
    }

    pub fn stopped_by_user(&self) -> Result<bool> {
        self.read(|c| Ok(c.query_row("SELECT stoppedByUser FROM project_meta WHERE id = 1", [], |r| r.get(0)).optional()?.unwrap_or(false)))
    }

    pub fn set_stopped_by_user(&self, stopped: bool) -> Result<()> {
        self.read(|c| Ok(c.execute("UPDATE project_meta SET stoppedByUser = ?1 WHERE id = 1", [stopped]).map(|_| ())?))
    }

    // ---- items ----

    pub fn item(&self, id: i64) -> Result<Option<Item>> {
        self.read(|c| item(c, id))
    }

    pub fn items(&self) -> Result<Vec<Item>> {
        self.read(|c| {
            let mut s = c.prepare(&format!("SELECT {ITEM_COLS} FROM item ORDER BY addedAt, id"))?;
            let rows = s.query_map([], item_from)?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn has_path(&self, path: &str) -> Result<bool> {
        self.read(|c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM item WHERE currentPath = ?1 COLLATE NOCASE)", [path], |r| r.get(0))?))
    }

    pub fn has_hash(&self, hash: &str) -> Result<bool> {
        self.read(|c| Ok(c.query_row("SELECT EXISTS(SELECT 1 FROM item WHERE contentHash = ?1)", [hash], |r| r.get(0))?))
    }

    /// Which of `paths` this project holds (any case), as given.
    pub fn held_paths(&self, paths: &[String]) -> Result<Vec<String>> {
        self.read(|c| {
            let mut out = Vec::new();
            let mut s = c.prepare("SELECT EXISTS(SELECT 1 FROM item WHERE currentPath = ?1 COLLATE NOCASE)")?;
            for p in paths {
                if s.query_row([p], |r| r.get::<_, bool>(0))? {
                    out.push(p.clone());
                }
            }
            Ok(out)
        })
    }

    pub fn insert_item(&self, n: &NewItem) -> Result<i64> {
        self.read(|c| {
            c.execute(
                "INSERT INTO item (contentHash, originalPath, originalName, currentPath, kind, takenAt, durationMs, width, height, status, nameSetByUser, categorySetByUser, addedAt, attempts) \
                 VALUES (?1, ?2, ?3, ?2, ?4, ?5, ?6, ?7, ?8, 'Pending', 0, 0, ?9, 0)",
                params![n.content_hash, n.path, n.name, n.kind.as_str(), n.taken_at, n.duration_ms, n.width, n.height, now_ms()],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn next_pending(&self) -> Result<Option<Item>> {
        self.read(|c| Ok(c.query_row(&format!("SELECT {ITEM_COLS} FROM item WHERE status = 'Pending' ORDER BY id LIMIT 1"), [], item_from).optional()?))
    }

    pub fn set_status(&self, id: i64, status: ItemStatus, reason: Option<&str>) -> Result<()> {
        self.read(|c| Ok(c.execute("UPDATE item SET status = ?2, failureReason = ?3 WHERE id = ?1", params![id, status.as_str(), reason]).map(|_| ())?))
    }

    /// Marks the item Analyzing and counts the attempt.
    pub fn mark_analyzing(&self, id: i64) -> Result<()> {
        self.read(|c| Ok(c.execute("UPDATE item SET status = 'Analyzing', failureReason = NULL, attempts = attempts + 1 WHERE id = ?1", [id]).map(|_| ())?))
    }

    pub fn return_to_pending(&self, id: i64) -> Result<()> {
        self.read(|c| Ok(c.execute("UPDATE item SET status = 'Pending', attempts = 0 WHERE id = ?1", [id]).map(|_| ())?))
    }

    /// For items left Analyzing by a process that died: those with at least `fail_after` attempts
    /// become Failed, the rest go back to Pending. Returns how many it touched.
    pub fn recover_interrupted(&self, fail_after: i64, reason: &str) -> Result<usize> {
        self.tx(|t| {
            let failed = t.execute("UPDATE item SET status = 'Failed', failureReason = ?2 WHERE status = 'Analyzing' AND attempts >= ?1", params![fail_after, reason])?;
            let reset = t.execute("UPDATE item SET status = 'Pending' WHERE status = 'Analyzing'", [])?;
            Ok(failed + reset)
        })
    }

    /// Saves an analysis and its tags, marks the item Analyzed (or keeps it Sorted when it has a
    /// category) and refreshes its FTS row, all in one transaction.
    pub fn save_analysis(&self, item_id: i64, result: &AnalysisResult, raw: &str, model_id: &str, prompt_version: &str, profile: &Profile) -> Result<()> {
        self.tx(|t| {
            t.execute(
                "INSERT OR REPLACE INTO analysis (itemId, description, actionsJson, suggestedName, shotType, rawJson, modelId, promptVersion, stopReason, ttftMs, generatedTokens, analyzedAt) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    item_id,
                    result.description,
                    serde_json::to_string(&result.actions)?,
                    result.suggested_name,
                    result.shot_type,
                    raw,
                    model_id,
                    prompt_version,
                    profile.stop_reason,
                    profile.ttft_ms,
                    profile.generated_tokens,
                    now_ms()
                ],
            )?;
            t.execute("DELETE FROM item_tag WHERE itemId = ?1", [item_id])?;
            for tag in &result.tags {
                let tag = tag.trim().to_lowercase();
                if tag.is_empty() {
                    continue;
                }
                t.execute("INSERT OR IGNORE INTO tag (name) VALUES (?1)", [&tag])?;
                t.execute("INSERT OR IGNORE INTO item_tag (itemId, tagId) SELECT ?1, id FROM tag WHERE name = ?2", params![item_id, tag])?;
            }
            t.execute(
                "UPDATE item SET status = CASE WHEN categoryId IS NULL THEN 'Analyzed' ELSE 'Sorted' END, failureReason = NULL, attempts = 0, \
                 friendlyName = CASE WHEN nameSetByUser OR friendlyName IS NOT NULL THEN friendlyName ELSE ?2 END WHERE id = ?1",
                params![item_id, result.suggested_name],
            )?;
            refresh_fts(t, item_id)
        })
    }

    pub fn analysis(&self, item_id: i64) -> Result<Option<Analysis>> {
        self.read(|c| {
            Ok(c.query_row(
                "SELECT itemId, description, actionsJson, suggestedName, shotType, modelId, promptVersion, stopReason, ttftMs, generatedTokens, analyzedAt FROM analysis WHERE itemId = ?1",
                [item_id],
                |r| {
                    let actions_json: String = r.get(2)?;
                    Ok(Analysis {
                        item_id: r.get(0)?,
                        description: r.get(1)?,
                        actions: serde_json::from_str(&actions_json).unwrap_or_default(),
                        actions_json,
                        suggested_name: r.get(3)?,
                        shot_type: r.get(4)?,
                        model_id: r.get(5)?,
                        prompt_version: r.get(6)?,
                        stop_reason: r.get(7)?,
                        ttft_ms: r.get(8)?,
                        generated_tokens: r.get(9)?,
                        analyzed_at: r.get(10)?,
                    })
                },
            )
            .optional()?)
        })
    }

    pub fn tags_for(&self, item_id: i64) -> Result<Vec<String>> {
        self.read(|c| tags_for(c, item_id))
    }

    // ---- categories and sorting ----

    pub fn categories(&self) -> Result<Vec<Category>> {
        self.read(categories)
    }

    pub fn category_infos(&self) -> Result<Vec<CategoryInfo>> {
        self.read(|c| {
            let mut s = c.prepare(
                "SELECT category.id, category.name, category.description, category.createdBy, \
                 (SELECT COUNT(*) FROM item WHERE item.categoryId = category.id) FROM category ORDER BY category.id",
            )?;
            let rows = s
                .query_map([], |r| Ok(CategoryInfo { id: r.get(0)?, name: r.get(1)?, description: r.get(2)?, created_by: r.get(3)?, items: r.get(4)? }))?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    pub fn item_count(&self) -> Result<i64> {
        self.read(|c| Ok(c.query_row("SELECT COUNT(*) FROM item", [], |r| r.get(0))?))
    }

    /// Analyzed items with no category, oldest first. An item whose category the user set is left out.
    pub fn uncategorized(&self) -> Result<Vec<UncategorizedItem>> {
        self.read(|c| {
            let mut s = c.prepare(
                "SELECT item.id, item.kind, analysis.description, analysis.actionsJson, analysis.suggestedName, item.nameSetByUser \
                 FROM item INNER JOIN analysis ON analysis.itemId = item.id \
                 WHERE item.status = 'Analyzed' AND item.categoryId IS NULL AND item.categorySetByUser = 0 ORDER BY item.id",
            )?;
            let mut rows = s
                .query_map([], |r| {
                    Ok(UncategorizedItem {
                        id: r.get(0)?,
                        kind: ItemKind::parse(&r.get::<_, String>(1)?).unwrap_or(ItemKind::Photo),
                        description: r.get(2)?,
                        actions_json: r.get(3)?,
                        suggested_name: r.get(4)?,
                        tags: Vec::new(),
                        name_set_by_user: r.get(5)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            for row in rows.iter_mut() {
                row.tags = tags_for(c, row.id)?;
            }
            Ok(rows)
        })
    }

    /// Applies one sorted chunk all-or-nothing. Each assignment's category is found by exact name
    /// among the existing categories, else taken from `new_categories` and inserted then. Only an
    /// item that is still uncategorized and whose category the user never set is assigned.
    /// Returns how many items were assigned.
    pub fn apply_chunk(&self, new_categories: &[NewCategoryEntity], assignments: &[Assignment]) -> Result<usize> {
        self.tx(|t| {
            let mut applied = 0;
            for a in assignments {
                if !t.query_row("SELECT EXISTS(SELECT 1 FROM item WHERE id = ?1)", [a.item_id], |r| r.get::<_, bool>(0))? {
                    continue;
                }
                let existing: Option<i64> = t.query_row("SELECT id FROM category WHERE name = ?1 ORDER BY id LIMIT 1", [&a.category_name], |r| r.get(0)).optional()?;
                let offer = new_categories.iter().find(|c| c.name == a.category_name);
                if existing.is_none() && offer.is_none() {
                    return Err(Error::msg(format!("No category named '{}' for item {}", a.category_name, a.item_id)));
                }
                let assignable: bool =
                    t.query_row("SELECT EXISTS(SELECT 1 FROM item WHERE id = ?1 AND categorySetByUser = 0 AND categoryId IS NULL)", [a.item_id], |r| r.get(0))?;
                if !assignable {
                    continue;
                }
                let category_id = match existing {
                    Some(id) => id,
                    None => {
                        let o = offer.unwrap();
                        t.execute("INSERT INTO category (name, description, createdBy, folderName) VALUES (?1, ?2, 'model', NULL)", params![o.name, o.description])?;
                        t.last_insert_rowid()
                    }
                };
                t.execute(
                    "UPDATE item SET categoryId = ?2, status = 'Sorted', friendlyName = CASE WHEN nameSetByUser THEN friendlyName ELSE ?3 END \
                     WHERE id = ?1 AND categorySetByUser = 0 AND categoryId IS NULL",
                    params![a.item_id, category_id, a.friendly_name],
                )?;
                refresh_fts(t, a.item_id)?;
                applied += 1;
            }
            Ok(applied)
        })
    }

    pub fn start_sort_run(&self, model_id: &str, prompt_version: &str, items: i64) -> Result<i64> {
        self.read(|c| {
            c.execute(
                "INSERT INTO sort_run (startedAt, finishedAt, modelId, promptVersion, items, sorted, newCategories, outcome, message) VALUES (?1, NULL, ?2, ?3, ?4, 0, 0, 'partial', NULL)",
                params![now_ms(), model_id, prompt_version, items],
            )?;
            Ok(c.last_insert_rowid())
        })
    }

    pub fn finish_sort_run(&self, id: i64, sorted: i64, new_categories: i64, outcome: &str, message: Option<&str>) -> Result<()> {
        self.read(|c| {
            c.execute(
                "UPDATE sort_run SET finishedAt = ?2, sorted = ?3, newCategories = ?4, outcome = ?5, message = ?6 WHERE id = ?1",
                params![id, now_ms(), sorted, new_categories, outcome, message],
            )?;
            Ok(())
        })
    }

    /// Closes the run rows a process death left open as `partial` with `message`.
    pub fn close_open_sort_runs(&self, message: &str) -> Result<usize> {
        self.read(|c| Ok(c.execute("UPDATE sort_run SET finishedAt = ?1, outcome = 'partial', message = ?2 WHERE finishedAt IS NULL", params![now_ms(), message])?))
    }

    /// The last 20 finished runs, newest first.
    pub fn recent_sort_runs(&self) -> Result<Vec<SortRun>> {
        self.read(|c| {
            let mut s = c.prepare("SELECT id, startedAt, finishedAt, modelId, items, sorted, newCategories, outcome, message FROM sort_run WHERE finishedAt IS NOT NULL ORDER BY id DESC LIMIT 20")?;
            let rows = s
                .query_map([], |r| {
                    Ok(SortRun {
                        id: r.get(0)?,
                        started_at: r.get(1)?,
                        finished_at: r.get(2)?,
                        model_id: r.get(3)?,
                        items: r.get(4)?,
                        sorted: r.get(5)?,
                        new_categories: r.get(6)?,
                        outcome: r.get(7)?,
                        message: r.get(8)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Makes unlocked Sorted and Binned items waiting again for a Re-sort; returns how many.
    /// Without `keep_edits` every lock is cleared first. Empty model-created categories are deleted.
    pub fn prepare_resort(&self, keep_edits: bool) -> Result<usize> {
        self.tx(|t| {
            if !keep_edits {
                t.execute("UPDATE item SET nameSetByUser = 0, categorySetByUser = 0", [])?;
                t.execute("UPDATE category SET createdBy = 'model'", [])?;
            }
            let ids: Vec<i64> = {
                let mut s = t.prepare(&format!("SELECT id FROM item WHERE {RESORTABLE}"))?;
                let v = s.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
                v
            };
            if !ids.is_empty() {
                t.execute(&format!("UPDATE item SET categoryId = NULL, status = 'Analyzed' WHERE {RESORTABLE}"), [])?;
            }
            t.execute("DELETE FROM category WHERE createdBy = 'model' AND NOT EXISTS (SELECT 1 FROM item WHERE item.categoryId = category.id)", [])?;
            for id in &ids {
                refresh_fts(t, *id)?;
            }
            Ok(ids.len())
        })
    }

    // ---- Project Media ----

    pub fn grid(&self) -> Result<Vec<MediaRow>> {
        self.read(|c| {
            let mut s = c.prepare(
                "SELECT item.id, item.kind, item.durationMs, item.takenAt, item.status, item.categoryId, item.friendlyName, analysis.suggestedName, \
                 analysis.shotType, item.nameSetByUser, item.categorySetByUser, item.currentPath, item.originalName, item.failureReason, analysis.itemId IS NOT NULL \
                 FROM item LEFT JOIN analysis ON analysis.itemId = item.id ORDER BY item.takenAt ASC, item.id ASC",
            )?;
            let rows = s
                .query_map([], |r| {
                    Ok(MediaRow {
                        id: r.get(0)?,
                        kind: ItemKind::parse(&r.get::<_, String>(1)?).unwrap_or(ItemKind::Photo),
                        duration_ms: r.get(2)?,
                        taken_at: r.get(3)?,
                        status: ItemStatus::parse(&r.get::<_, String>(4)?),
                        category_id: r.get(5)?,
                        friendly_name: r.get(6)?,
                        suggested_name: r.get(7)?,
                        shot_type: r.get(8)?,
                        name_set_by_user: r.get(9)?,
                        category_set_by_user: r.get(10)?,
                        current_path: r.get(11)?,
                        original_name: r.get(12)?,
                        failure_reason: r.get(13)?,
                        has_analysis: r.get(14)?,
                        matched_in: None,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Item ids matching an FTS5 MATCH expression.
    pub fn search(&self, fts_query: &str) -> Result<Vec<i64>> {
        self.read(|c| {
            let mut s = c.prepare("SELECT rowid FROM item_fts WHERE item_fts MATCH ?1")?;
            let rows = s.query_map([fts_query], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(rows)
        })
    }

    /// Which field produced each hit, in the order Name, Category, Tags, Actions, Description.
    pub fn matched_in(&self, fts_query: &str) -> Result<Vec<(i64, &'static str)>> {
        self.read(|c| {
            let mut found: Vec<(i64, &'static str)> = Vec::new();
            for (label, col) in [("Name", "friendly_name"), ("Category", "category"), ("Tags", "tags"), ("Actions", "actions"), ("Description", "description")] {
                let Ok(mut s) = c.prepare("SELECT rowid FROM item_fts WHERE item_fts MATCH ?1") else { continue };
                let q = format!("{col} : {fts_query}");
                let Ok(rows) = s.query_map([q], |r| r.get::<_, i64>(0)) else { continue };
                for id in rows.flatten() {
                    if !found.iter().any(|(i, _)| *i == id) {
                        found.push((id, label));
                    }
                }
            }
            Ok(found)
        })
    }

    // ---- moves ----

    pub fn bin_items(&self) -> Result<Vec<BinItem>> {
        self.read(bin_items)
    }

    pub fn category_names(&self) -> Result<Vec<(i64, String)>> {
        self.read(category_names)
    }

    pub fn removed_paths(&self) -> Result<Vec<String>> {
        self.read(|c| {
            let mut s = c.prepare("SELECT path FROM removed_file ORDER BY path")?;
            let v = s.query_map([], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
    }

    pub fn drop_removed(&self, paths: &[String]) -> Result<()> {
        self.tx(|t| {
            for p in paths {
                t.execute("DELETE FROM removed_file WHERE path = ?1", [p])?;
            }
            Ok(())
        })
    }

    /// Remembers `paths` as files the folder check skips: `removed` or `left`.
    pub fn add_removed(&self, paths: &[String], reason: &str) -> Result<()> {
        self.tx(|t| add_removed(t, paths, reason))
    }

    pub fn mark_items(&self, in_place: &[i64], missing: &[i64]) -> Result<()> {
        self.tx(|t| mark_items(t, in_place, missing))
    }

    pub fn mark_sorted(&self, ids: &[i64]) -> Result<()> {
        self.tx(|t| set_status_many(t, ids, "Sorted"))
    }

    pub fn open_move_run(&self) -> Result<Option<MoveRun>> {
        self.read(|c| Ok(c.query_row(&format!("SELECT {RUN_COLS} FROM move_run WHERE state IN ('Running', 'Paused') ORDER BY id DESC LIMIT 1"), [], run_from).optional()?))
    }

    pub fn move_run(&self, id: i64) -> Result<Option<MoveRun>> {
        self.read(|c| Ok(c.query_row(&format!("SELECT {RUN_COLS} FROM move_run WHERE id = ?1"), [id], run_from).optional()?))
    }

    /// The newest finished run whose report the user hasn't closed.
    pub fn unread_move_run(&self) -> Result<Option<MoveRun>> {
        self.read(|c| {
            Ok(c.query_row(&format!("SELECT {RUN_COLS} FROM move_run WHERE finishedAt IS NOT NULL AND reportClosed = 0 ORDER BY id DESC LIMIT 1"), [], run_from).optional()?)
        })
    }

    /// The latest finished run's folder check: Some(false) when it found something.
    pub fn last_check_ok(&self) -> Result<Option<bool>> {
        self.read(|c| Ok(c.query_row("SELECT checkOk FROM move_run WHERE finishedAt IS NOT NULL ORDER BY id DESC LIMIT 1", [], |r| r.get::<_, Option<bool>>(0)).optional()?.flatten()))
    }

    /// Closes this run's report and any older one still open.
    pub fn close_report(&self, run_id: i64) -> Result<()> {
        self.read(|c| Ok(c.execute("UPDATE move_run SET reportClosed = 1 WHERE id <= ?1", [run_id]).map(|_| ())?))
    }

    pub fn finish_move_run(&self, run_id: i64, state: &str, message: Option<&str>, removed_folders: &[String], check_ok: Option<bool>) -> Result<()> {
        let folders = if removed_folders.is_empty() { None } else { Some(removed_folders.join("\n")) };
        self.read(|c| {
            c.execute(
                "UPDATE move_run SET state = ?2, finishedAt = ?3, message = ?4, removedFolders = ?5, checkOk = ?6 WHERE id = ?1",
                params![run_id, state, now_ms(), message, folders, check_ok],
            )?;
            Ok(())
        })
    }

    pub fn set_run_state(&self, run_id: i64, state: &str, message: Option<&str>) -> Result<()> {
        self.read(|c| Ok(c.execute("UPDATE move_run SET state = ?2, message = ?3 WHERE id = ?1", params![run_id, state, message]).map(|_| ())?))
    }

    pub fn steps(&self, run_id: i64) -> Result<Vec<MoveStep>> {
        self.read(|c| {
            let mut s = c.prepare("SELECT id, runId, itemId, source, dest, state, bytes, attempts, keepSource, numbered, error FROM move_step WHERE runId = ?1 ORDER BY id")?;
            let v = s
                .query_map([run_id], |r| {
                    Ok(MoveStep {
                        id: r.get(0)?,
                        run_id: r.get(1)?,
                        item_id: r.get(2)?,
                        source: r.get(3)?,
                        dest: r.get(4)?,
                        state: r.get(5)?,
                        bytes: r.get(6)?,
                        attempts: r.get(7)?,
                        keep_source: r.get(8)?,
                        numbered: r.get(9)?,
                        error: r.get(10)?,
                    })
                })?
                .collect::<rusqlite::Result<Vec<_>>>()?;
            Ok(v)
        })
    }

    /// Counts a start of the step and returns how many there have been, this one included.
    pub fn start_step(&self, step_id: i64) -> Result<i64> {
        self.tx(|t| {
            t.execute("UPDATE move_step SET attempts = attempts + 1 WHERE id = ?1", [step_id])?;
            Ok(t.query_row("SELECT attempts FROM move_step WHERE id = ?1", [step_id], |r| r.get(0))?)
        })
    }

    pub fn set_step_state(&self, step_id: i64, state: &str, error: Option<&str>) -> Result<()> {
        self.read(|c| Ok(c.execute("UPDATE move_step SET state = ?2, error = ?3 WHERE id = ?1", params![step_id, state, error]).map(|_| ())?))
    }

    pub fn set_step_dest(&self, step_id: i64, dest: &str) -> Result<()> {
        self.read(|c| Ok(c.execute("UPDATE move_step SET dest = ?2 WHERE id = ?1", params![step_id, dest]).map(|_| ())?))
    }

    /// In one transaction: the item is at `dest` and binned, its hash is `hash` (unless another
    /// item has it), and the step is `state`.
    pub fn record_moved(&self, step_id: i64, item_id: i64, dest: &str, hash: Option<&str>, state: &str) -> Result<()> {
        self.tx(|t| {
            t.execute("UPDATE item SET currentPath = ?2, status = 'Binned' WHERE id = ?1", params![item_id, dest])?;
            if let Some(h) = hash {
                let taken: bool = t.query_row("SELECT EXISTS(SELECT 1 FROM item WHERE contentHash = ?1 AND id != ?2)", params![h, item_id], |r| r.get(0))?;
                if !taken {
                    t.execute("UPDATE item SET contentHash = ?2 WHERE id = ?1", params![item_id, h])?;
                }
            }
            t.execute("UPDATE move_step SET state = ?2, error = NULL WHERE id = ?1", params![step_id, state])?;
            Ok(())
        })
    }

    pub fn record_missing(&self, step_id: i64, item_id: i64) -> Result<()> {
        self.tx(|t| {
            t.execute("UPDATE item SET status = 'Missing' WHERE id = ?1", [item_id])?;
            t.execute("UPDATE move_step SET state = 'Missing', error = NULL WHERE id = ?1", [step_id])?;
            Ok(())
        })
    }

    /// The item is back at `source` and Sorted, and the step Failed with `error`.
    pub fn record_reverted(&self, step_id: i64, item_id: i64, source: &str, error: &str) -> Result<()> {
        self.tx(|t| {
            t.execute("UPDATE item SET currentPath = ?2, status = 'Sorted' WHERE id = ?1", params![item_id, source])?;
            t.execute("UPDATE move_step SET state = 'Failed', error = ?2 WHERE id = ?1", params![step_id, error])?;
            Ok(())
        })
    }

    /// Folders a finished step put a file in: the only folders Bin Files may remove once empty.
    pub fn binned_folders(&self) -> Result<Vec<String>> {
        self.read(|c| {
            let mut s = c.prepare("SELECT DISTINCT dest FROM move_step WHERE state IN ('Done', 'Verified')")?;
            let mut out: Vec<String> = Vec::new();
            for d in s.query_map([], |r| r.get::<_, String>(0))? {
                let folder = crate::paths::parent(&d?).to_string();
                if !out.contains(&folder) {
                    out.push(folder);
                }
            }
            Ok(out)
        })
    }
}

const RUN_COLS: &str = "id, kind, startedAt, finishedAt, state, message, removedFolders, inPlace, checkOk, reportClosed";

fn run_from(r: &Row) -> rusqlite::Result<MoveRun> {
    Ok(MoveRun {
        id: r.get(0)?,
        kind: r.get(1)?,
        started_at: r.get(2)?,
        finished_at: r.get(3)?,
        state: r.get(4)?,
        message: r.get(5)?,
        removed_folders: r.get(6)?,
        in_place: r.get(7)?,
        check_ok: r.get(8)?,
        report_closed: r.get(9)?,
    })
}

pub fn item(c: &Connection, id: i64) -> Result<Option<Item>> {
    Ok(c.query_row(&format!("SELECT {ITEM_COLS} FROM item WHERE id = ?1"), [id], item_from).optional()?)
}

pub fn tags_for(c: &Connection, item_id: i64) -> Result<Vec<String>> {
    let mut s = c.prepare("SELECT tag.name FROM tag INNER JOIN item_tag ON item_tag.tagId = tag.id WHERE item_tag.itemId = ?1 ORDER BY tag.name")?;
    let v = s.query_map([item_id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

pub fn categories(c: &Connection) -> Result<Vec<Category>> {
    let mut s = c.prepare("SELECT id, name, description, createdBy FROM category ORDER BY id")?;
    let v = s.query_map([], |r| Ok(Category { id: r.get(0)?, name: r.get(1)?, description: r.get(2)?, created_by: r.get(3)? }))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

pub fn category(c: &Connection, id: i64) -> Result<Option<Category>> {
    Ok(c.query_row("SELECT id, name, description, createdBy FROM category WHERE id = ?1", [id], |r| {
        Ok(Category { id: r.get(0)?, name: r.get(1)?, description: r.get(2)?, created_by: r.get(3)? })
    })
    .optional()?)
}

pub fn category_names(c: &Connection) -> Result<Vec<(i64, String)>> {
    let mut s = c.prepare("SELECT id, name FROM category ORDER BY id")?;
    let v = s.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

pub fn bin_items(c: &Connection) -> Result<Vec<BinItem>> {
    let mut s = c.prepare("SELECT id, status, categoryId, friendlyName, currentPath, originalName, failureReason FROM item ORDER BY id")?;
    let v = s
        .query_map([], |r| {
            Ok(BinItem {
                id: r.get(0)?,
                status: ItemStatus::parse(&r.get::<_, String>(1)?),
                category_id: r.get(2)?,
                friendly_name: r.get(3)?,
                current_path: r.get(4)?,
                original_name: r.get(5)?,
                failure_reason: r.get(6)?,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

pub fn item_ids_in_category(c: &Connection, category_id: i64) -> Result<Vec<i64>> {
    let mut s = c.prepare("SELECT id FROM item WHERE categoryId = ?1")?;
    let v = s.query_map([category_id], |r| r.get(0))?.collect::<rusqlite::Result<Vec<_>>>()?;
    Ok(v)
}

pub fn add_removed(c: &Connection, paths: &[String], reason: &str) -> Result<()> {
    for p in paths {
        c.execute("INSERT OR REPLACE INTO removed_file (path, removedAt, reason) VALUES (?1, ?2, ?3)", params![p, now_ms(), reason])?;
    }
    Ok(())
}

fn set_status_many(c: &Connection, ids: &[i64], status: &str) -> Result<()> {
    for chunk in ids.chunks(CHUNK) {
        c.execute(&format!("UPDATE item SET status = '{status}' WHERE id IN ({})", placeholders(chunk.len())), params_from_iter(chunk.iter()))?;
    }
    Ok(())
}

pub fn mark_items(c: &Connection, in_place: &[i64], missing: &[i64]) -> Result<()> {
    set_status_many(c, in_place, "Binned")?;
    set_status_many(c, missing, "Missing")
}

pub fn mark_sorted(c: &Connection, ids: &[i64]) -> Result<()> {
    set_status_many(c, ids, "Sorted")
}

/// A step to write with a run.
#[derive(Debug, Clone, PartialEq)]
pub struct NewStep {
    pub item_id: i64,
    pub source: String,
    pub dest: String,
    pub bytes: i64,
    pub keep_source: bool,
    pub numbered: bool,
}

/// Writes a run and every step before anything moves, with the items found in place marked binned
/// and the ones whose file is gone marked Missing. Call inside a transaction. Returns the run's id.
pub fn write_run(c: &Connection, kind: &str, steps: &[NewStep], in_place: &[i64], missing: &[i64], count_in_place: bool) -> Result<i64> {
    c.execute(
        "INSERT INTO move_run (kind, startedAt, finishedAt, state, inPlace, reportClosed) VALUES (?1, ?2, NULL, 'Running', ?3, 0)",
        params![kind, now_ms(), if count_in_place { in_place.len() as i64 } else { 0 }],
    )?;
    let run_id = c.last_insert_rowid();
    for s in steps {
        c.execute(
            "INSERT INTO move_step (runId, itemId, source, dest, state, bytes, attempts, keepSource, numbered) VALUES (?1, ?2, ?3, ?4, 'Planned', ?5, 0, ?6, ?7)",
            params![run_id, s.item_id, s.source, s.dest, s.bytes, s.keep_source, s.numbered],
        )?;
    }
    mark_items(c, in_place, missing)?;
    Ok(run_id)
}

/// Brings an older Android-made schema up to version 5.
fn migrate(c: &Connection, from: i32) -> Result<()> {
    let has_col = |table: &str, col: &str| -> Result<bool> {
        let mut s = c.prepare(&format!("PRAGMA table_info({table})"))?;
        let names = s.query_map([], |r| r.get::<_, String>(1))?.collect::<rusqlite::Result<Vec<_>>>()?;
        Ok(names.iter().any(|n| n == col))
    };
    let add = |table: &str, col: &str, decl: &str| -> Result<()> {
        if !has_col(table, col)? {
            c.execute_batch(&format!("ALTER TABLE {table} ADD COLUMN {col} {decl};"))?;
        }
        Ok(())
    };
    if from < 2 {
        add("item", "attempts", "INTEGER NOT NULL DEFAULT 0")?;
        add("project_meta", "stoppedByUser", "INTEGER NOT NULL DEFAULT 0")?;
    }
    if from < 4 {
        add("move_step", "bytes", "INTEGER NOT NULL DEFAULT 0")?;
        add("move_step", "attempts", "INTEGER NOT NULL DEFAULT 0")?;
        add("move_step", "keepSource", "INTEGER NOT NULL DEFAULT 0")?;
        add("move_step", "numbered", "INTEGER NOT NULL DEFAULT 0")?;
        add("move_step", "error", "TEXT")?;
        add("move_run", "message", "TEXT")?;
        add("move_run", "removedFolders", "TEXT")?;
    }
    if from < 5 {
        add("move_run", "inPlace", "INTEGER NOT NULL DEFAULT 0")?;
        add("move_run", "checkOk", "INTEGER")?;
        add("move_run", "reportClosed", "INTEGER NOT NULL DEFAULT 0")?;
    }
    Ok(())
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;

    pub fn temp_db() -> (tempfile::TempDir, ProjectDb) {
        let dir = tempfile::tempdir().unwrap();
        let db = ProjectDb::open(&dir.path().join(".binning/project.db"), "Test").unwrap();
        (dir, db)
    }

    pub fn add(db: &ProjectDb, name: &str) -> i64 {
        db.insert_item(&NewItem {
            content_hash: format!("hash-{name}"),
            path: format!("C:/media/{name}"),
            name: name.to_string(),
            kind: if name.ends_with(".mp4") { ItemKind::Video } else { ItemKind::Photo },
            taken_at: Some(1),
            duration_ms: None,
            width: None,
            height: None,
        })
        .unwrap()
    }

    pub fn analyze(db: &ProjectDb, id: i64, name: &str, tags: &[&str]) {
        let r = AnalysisResult {
            description: format!("A {name} by the sea"),
            actions: vec![],
            tags: tags.iter().map(|s| s.to_string()).collect(),
            suggested_name: name.to_string(),
            shot_type: "wide".into(),
        };
        db.save_analysis(id, &r, "{}", "model", "v", &Profile { stop_reason: "eos".into(), ..Default::default() }).unwrap();
    }

    #[test]
    fn analysis_and_search() {
        let (_d, db) = temp_db();
        let id = add(&db, "a.jpg");
        assert_eq!(db.next_pending().unwrap().unwrap().id, id);
        analyze(&db, id, "Sea Turtle", &["Turtle", "ocean"]);
        let item = db.item(id).unwrap().unwrap();
        assert_eq!(item.status, ItemStatus::Analyzed);
        assert_eq!(item.friendly_name.as_deref(), Some("Sea Turtle"));
        assert_eq!(db.tags_for(id).unwrap(), vec!["ocean", "turtle"]);
        assert_eq!(db.search("\"turtl\"*").unwrap(), vec![id]);
        assert_eq!(db.matched_in("\"turtle\"").unwrap(), vec![(id, "Name")]);
        assert_eq!(db.matched_in("\"ocean\"").unwrap(), vec![(id, "Tags")]);
        assert!(db.search("\"zebra\"").unwrap().is_empty());
    }

    #[test]
    fn apply_chunk_and_resort() {
        let (_d, db) = temp_db();
        let a = add(&db, "a.jpg");
        let b = add(&db, "b.jpg");
        analyze(&db, a, "Turtle", &[]);
        analyze(&db, b, "Seal", &[]);
        assert_eq!(db.uncategorized().unwrap().len(), 2);
        let cats = vec![NewCategoryEntity { name: "Animals".into(), description: "d".into() }, NewCategoryEntity { name: "Unused".into(), description: "d".into() }];
        let n = db
            .apply_chunk(&cats, &[Assignment { item_id: a, category_name: "Animals".into(), friendly_name: "Green Turtle".into() }, Assignment { item_id: 99, category_name: "Nope".into(), friendly_name: "x".into() }])
            .unwrap();
        assert_eq!(n, 1);
        assert_eq!(db.categories().unwrap().len(), 1);
        assert_eq!(db.item(a).unwrap().unwrap().status, ItemStatus::Sorted);
        assert_eq!(db.search("\"animals\"").unwrap(), vec![a]);
        assert!(db.apply_chunk(&[], &[Assignment { item_id: b, category_name: "Nope".into(), friendly_name: "x".into() }]).is_err());
        assert_eq!(db.prepare_resort(true).unwrap(), 1);
        assert!(db.categories().unwrap().is_empty());
        assert_eq!(db.item(a).unwrap().unwrap().status, ItemStatus::Analyzed);
    }

    #[test]
    fn recovers_interrupted() {
        let (_d, db) = temp_db();
        let a = add(&db, "a.jpg");
        let b = add(&db, "b.jpg");
        db.mark_analyzing(a).unwrap();
        db.mark_analyzing(a).unwrap();
        db.mark_analyzing(b).unwrap();
        assert_eq!(db.recover_interrupted(2, "interrupted").unwrap(), 2);
        assert_eq!(db.item(a).unwrap().unwrap().status, ItemStatus::Failed);
        assert_eq!(db.item(b).unwrap().unwrap().status, ItemStatus::Pending);
    }

    #[test]
    fn probe_reads_facts() {
        let (d, db) = temp_db();
        drop(db);
        let facts = probe(&d.path().join(".binning/project.db"));
        assert!(facts.readable && facts.quick_check_ok && facts.has_meta);
        assert_eq!(facts.user_version, Some(5));
        assert_eq!(facts.identity_hash.as_deref(), Some(IDENTITY_HASH_V5));
        assert_eq!(facts.meta_name.as_deref(), Some("Test"));
        assert!(!probe(&d.path().join("nope.db")).exists);
    }
}
