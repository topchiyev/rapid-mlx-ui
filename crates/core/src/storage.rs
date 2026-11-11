//! Synchronous SQLite API. Own a Store on one background worker; never call it on render.
use crate::{
    domain::{Record, RecordStatus, Session, validate_update},
    paths::{AppPaths, private_file},
};
use anyhow::{Context, Result, ensure};
use rusqlite::{Connection, OptionalExtension, Transaction, params};
use serde::{Serialize, de::DeserializeOwned};
use std::{fs, time::Duration};

pub const SCHEMA_VERSION: i64 = 1;
/// No async runtime, requests, process launches, or side-effect replay occur in this layer.
pub struct Store {
    connection: Connection,
    paths: AppPaths,
}
impl Store {
    pub fn open(paths: AppPaths) -> Result<Self> {
        paths.ensure_layout()?;
        private_file(&paths.database)?;
        for suffix in ["-wal", "-shm", "-journal"] {
            let path = paths.root.join(format!("state.sqlite3{suffix}"));
            if fs::symlink_metadata(&path).is_ok() {
                private_file(&path)?;
            }
        }
        let mut connection =
            Connection::open(&paths.database).context("cannot open application database")?;
        connection.busy_timeout(Duration::from_secs(5))?;
        let version: i64 = connection.pragma_query_value(None, "user_version", |r| r.get(0))?;
        ensure!(
            version <= SCHEMA_VERSION,
            "database schema {version} is newer than supported schema {SCHEMA_VERSION}; history was not reset"
        );
        ensure!(version >= 0, "invalid database schema version");
        if version < SCHEMA_VERSION {
            let tables: i64 = connection.query_row("SELECT count(*) FROM sqlite_master WHERE type='table' AND name NOT LIKE 'sqlite_%'", [], |r| r.get(0))?;
            if tables > 0 {
                backup(&connection, &paths, version)?;
            }
            let tx = connection.transaction()?;
            tx.execute_batch("CREATE TABLE IF NOT EXISTS sessions (id TEXT PRIMARY KEY NOT NULL, updated_at TEXT NOT NULL, data TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS settings (key TEXT PRIMARY KEY NOT NULL, data TEXT NOT NULL);
                CREATE TABLE IF NOT EXISTS records (namespace TEXT NOT NULL, id TEXT NOT NULL, session_id TEXT REFERENCES sessions(id) ON DELETE CASCADE, data TEXT NOT NULL, PRIMARY KEY(namespace,id));
                CREATE INDEX IF NOT EXISTS records_session ON records(session_id);
                PRAGMA user_version=1;")?;
            tx.commit()?;
        }
        connection.pragma_update(None, "foreign_keys", "ON")?;
        // DELETE journaling avoids persistent WAL/SHM files; SQLite derives journal permissions
        // from the private database. FULL sync commits pending intent before dispatch.
        connection.pragma_update(None, "journal_mode", "DELETE")?;
        connection.pragma_update(None, "synchronous", "FULL")?;
        connection.pragma_update(None, "temp_store", "MEMORY")?;
        let mut store = Self { connection, paths };
        store.recover()?;
        Ok(store)
    }
    pub fn paths(&self) -> &AppPaths {
        &self.paths
    }
    pub fn load_sessions(&self) -> Result<Vec<Session>> {
        let mut stmt = self
            .connection
            .prepare("SELECT data FROM sessions ORDER BY updated_at DESC, id")?;
        let rows = stmt.query_map([], |r| r.get::<_, String>(0))?;
        rows.map(|r| {
            let s: Session = serde_json::from_str(&r?)?;
            s.validate()?;
            Ok(s)
        })
        .collect()
    }
    pub fn load_session(&self, id: &str) -> Result<Option<Session>> {
        let data: Option<String> = self
            .connection
            .query_row("SELECT data FROM sessions WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        data.map(|json| {
            let s: Session = serde_json::from_str(&json)?;
            s.validate()?;
            Ok(s)
        })
        .transpose()
    }
    /// An atomic full checkpoint. The caller retains its in-memory Session on failure.
    pub fn save_session(&mut self, session: &Session) -> Result<()> {
        session.validate()?;
        let tx = self.connection.transaction()?;
        let old: Option<String> = tx
            .query_row(
                "SELECT data FROM sessions WHERE id=?1",
                [&session.id],
                |r| r.get(0),
            )
            .optional()?;
        if let Some(old) = old {
            validate_update(&serde_json::from_str(&old)?, session)?;
        }
        write_session(&tx, session)?;
        tx.commit()?;
        Ok(())
    }
    /// Commit prompt and pending attempt together. No in-memory mutation on storage failure.
    pub fn begin_turn(
        &mut self,
        session: &mut Session,
        prompt: impl Into<String>,
    ) -> Result<String> {
        let mut pending = session.clone();
        let id = pending.begin_turn(prompt)?;
        self.save_session(&pending)?;
        *session = pending;
        Ok(id)
    }
    pub fn retry(&mut self, session: &mut Session, assistant_id: &str) -> Result<String> {
        let mut pending = session.clone();
        let id = pending.retry(assistant_id)?;
        self.save_session(&pending)?;
        *session = pending;
        Ok(id)
    }
    pub fn edit_prompt(
        &mut self,
        session: &mut Session,
        prompt_id: &str,
        content: impl Into<String>,
    ) -> Result<String> {
        let mut pending = session.clone();
        let id = pending.edit_prompt(prompt_id, content)?;
        self.save_session(&pending)?;
        *session = pending;
        Ok(id)
    }
    /// Call only after explicit UI confirmation. Cascades only session-linked records.
    pub fn delete_session(&mut self, id: &str) -> Result<()> {
        let tx = self.connection.transaction()?;
        let data: Option<String> = tx
            .query_row("SELECT data FROM sessions WHERE id=?1", [id], |r| r.get(0))
            .optional()?;
        if let Some(data) = data {
            let session: Session = serde_json::from_str(&data)?;
            ensure!(
                !session.has_active_generation(),
                "cannot delete a session with an active generation"
            );
            ensure!(
                !session
                    .messages
                    .iter()
                    .flat_map(|m| &m.tools)
                    .any(|t| t.status.is_active()),
                "cannot delete a session with an active tool"
            );
            let mut stmt = tx.prepare("SELECT data FROM records WHERE session_id=?1")?;
            for row in stmt.query_map([id], |r| r.get::<_, String>(0))? {
                let record: Record = serde_json::from_str(&row?)?;
                ensure!(
                    !matches!(
                        record.status,
                        Some(RecordStatus::Pending | RecordStatus::Running)
                    ),
                    "cannot delete a session with an active job"
                );
            }
            drop(stmt);
            tx.execute("DELETE FROM sessions WHERE id=?1", [id])?;
        }
        tx.commit()?;
        Ok(())
    }
    /// Values must contain non-secret configuration only. Store credential references, not values.
    pub fn set_setting<T: Serialize>(&mut self, key: &str, value: &T) -> Result<()> {
        ensure!(!key.is_empty(), "empty setting key");
        self.connection.execute("INSERT INTO settings(key,data) VALUES(?1,?2) ON CONFLICT(key) DO UPDATE SET data=excluded.data", params![key, serde_json::to_string(value)?])?;
        Ok(())
    }
    pub fn get_setting<T: DeserializeOwned>(&self, key: &str) -> Result<Option<T>> {
        let data: Option<String> = self
            .connection
            .query_row("SELECT data FROM settings WHERE key=?1", [key], |r| {
                r.get(0)
            })
            .optional()?;
        data.map(|s| serde_json::from_str(&s).map_err(Into::into))
            .transpose()
    }
    pub fn delete_setting(&mut self, key: &str) -> Result<()> {
        self.connection
            .execute("DELETE FROM settings WHERE key=?1", [key])?;
        Ok(())
    }
    /// Provider-independent MCP registrations, transcripts and operations. Set session_id for
    /// deletion ownership; leave None for global definitions. Persist intent before executing.
    pub fn save_record(&mut self, record: &Record) -> Result<()> {
        ensure!(
            !record.namespace.is_empty() && !record.id.is_empty(),
            "record namespace and ID are required"
        );
        self.connection.execute("INSERT INTO records(namespace,id,session_id,data) VALUES(?1,?2,?3,?4) ON CONFLICT(namespace,id) DO UPDATE SET session_id=excluded.session_id,data=excluded.data", params![record.namespace, record.id, record.session_id, serde_json::to_string(record)?])?;
        Ok(())
    }
    pub fn load_records(&self, namespace: &str) -> Result<Vec<Record>> {
        let mut stmt = self
            .connection
            .prepare("SELECT data FROM records WHERE namespace=?1 ORDER BY id")?;
        stmt.query_map([namespace], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect()
    }
    pub fn delete_record(&mut self, namespace: &str, id: &str) -> Result<()> {
        self.connection.execute(
            "DELETE FROM records WHERE namespace=?1 AND id=?2",
            params![namespace, id],
        )?;
        Ok(())
    }
    fn recover(&mut self) -> Result<()> {
        let mut sessions = self.load_sessions()?;
        let mut stmt = self.connection.prepare("SELECT data FROM records")?;
        let records: Vec<Record> = stmt
            .query_map([], |r| r.get::<_, String>(0))?
            .map(|r| Ok(serde_json::from_str(&r?)?))
            .collect::<Result<_>>()?;
        drop(stmt);
        let tx = self.connection.transaction()?;
        for session in &mut sessions {
            if session.recover_interrupted() {
                write_session(&tx, session)?;
            }
        }
        for mut record in records {
            if matches!(
                record.status,
                Some(RecordStatus::Pending | RecordStatus::Running)
            ) {
                record.status = Some(
                    if matches!(
                        record.namespace.as_str(),
                        "tool" | "tools" | "tool_call" | "tool_calls" | "mcp_call"
                    ) {
                        RecordStatus::OutcomeUnknown
                    } else {
                        RecordStatus::Interrupted
                    },
                );
                tx.execute(
                    "UPDATE records SET data=?1 WHERE namespace=?2 AND id=?3",
                    params![serde_json::to_string(&record)?, record.namespace, record.id],
                )?;
            }
        }
        tx.commit()?;
        Ok(())
    }
}
fn write_session(tx: &Transaction<'_>, session: &Session) -> Result<()> {
    tx.execute("INSERT INTO sessions(id,updated_at,data) VALUES(?1,?2,?3) ON CONFLICT(id) DO UPDATE SET updated_at=excluded.updated_at,data=excluded.data", params![session.id, session.updated_at.to_rfc3339(), serde_json::to_string(session)?])?;
    Ok(())
}
fn backup(connection: &Connection, paths: &AppPaths, version: i64) -> Result<()> {
    let target = paths
        .backups
        .join(format!("state-v{version}-{}.sqlite3", uuid::Uuid::new_v4()));
    private_file(&target)?;
    let mut destination = Connection::open(&target)?;
    let backup = rusqlite::backup::Backup::new(connection, &mut destination)?;
    backup.run_to_completion(128, Duration::from_millis(5), None)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::domain::{MessageStatus, ToolRecord, ToolStatus};
    use serde_json::json;
    fn fixture() -> Result<(tempfile::TempDir, AppPaths, Store)> {
        let home = tempfile::tempdir()?;
        let paths = AppPaths::from_home(home.path())?;
        let store = Store::open(paths.clone())?;
        Ok((home, paths, store))
    }
    #[test]
    fn crash_roundtrip_keeps_checkpoint_and_never_replays_tools() -> Result<()> {
        let (_home, paths, mut store) = fixture()?;
        let mut s = Session::new("title", "missing-model@revision");
        s.draft = "unsent".into();
        let id = store.begin_turn(&mut s, "question")?;
        let a = s.message_mut(&id)?;
        a.status = MessageStatus::Streaming;
        a.content = "partial".into();
        a.reasoning = "private reasoning".into();
        let mut tool = ToolRecord::new("call1", "os", "write", json!({"path":"file"}));
        tool.status = ToolStatus::Running;
        a.tools.push(tool);
        store.save_session(&s)?;
        let mut transcript = Record::new(
            "transcript",
            "t1",
            json!({"model":"asr@rev", "text":"edited", "audio":null}),
        );
        transcript.session_id = Some(s.id.clone());
        store.save_record(&transcript)?;
        let mut job = Record::new("tool", "call2", json!({"argv":["command"]}));
        job.status = Some(RecordStatus::Running);
        store.save_record(&job)?;
        store.set_setting("mcp", &json!({"env_ref":"TOKEN"}))?;
        drop(store);
        let store = Store::open(paths)?;
        let saved = store.load_session(&s.id)?.unwrap();
        let a = saved.message(&id)?;
        assert_eq!(a.content, "partial");
        assert_eq!(a.status, MessageStatus::Interrupted);
        assert_eq!(a.model_requests[0].status, MessageStatus::Interrupted);
        assert_eq!(a.tools[0].status, ToolStatus::OutcomeUnknown);
        assert_eq!(saved.model, "missing-model@revision");
        assert!(saved.eligible_history().is_err());
        assert_eq!(
            store.load_records("tool")?[0].status,
            Some(RecordStatus::OutcomeUnknown)
        );
        assert_eq!(store.load_records("transcript")?[0], transcript);
        assert_eq!(
            store.get_setting::<serde_json::Value>("mcp")?,
            Some(json!({"env_ref":"TOKEN"}))
        );
        Ok(())
    }
    #[test]
    fn scoped_delete_preserves_other_sessions_global_records_and_model_files() -> Result<()> {
        let (home, _paths, mut store) = fixture()?;
        let model = home.path().join("weights");
        fs::write(&model, "weights")?;
        let mut a = Session::new("a", "model");
        let b = Session::new("b", "model");
        let attempt = store.begin_turn(&mut a, "hello")?;
        store.save_session(&b)?;
        assert!(store.delete_session(&a.id).is_err());
        a.message_mut(&attempt)?.status = MessageStatus::Cancelled;
        store.save_session(&a)?;
        let mut linked = Record::new("transcript", "owned", json!("text"));
        linked.session_id = Some(a.id.clone());
        store.save_record(&linked)?;
        store.save_record(&Record::new("mcp", "global", json!({"name":"example"})))?;
        store.delete_session(&a.id)?;
        assert!(store.load_session(&a.id)?.is_none());
        assert!(store.load_session(&b.id)?.is_some());
        assert!(store.load_records("transcript")?.is_empty());
        assert_eq!(store.load_records("mcp")?.len(), 1);
        assert_eq!(fs::read_to_string(model)?, "weights");
        Ok(())
    }
    #[test]
    fn pending_transaction_failure_keeps_memory_unchanged() -> Result<()> {
        let (_home, _paths, mut store) = fixture()?;
        let mut s = Session::new("unsaved", "model");
        s.draft = "keep me".into();
        store.connection.pragma_update(None, "query_only", "ON")?;
        let original = s.clone();
        assert!(store.begin_turn(&mut s, "prompt").is_err());
        assert_eq!(s, original);
        assert!(store.load_sessions()?.is_empty());
        Ok(())
    }
    #[test]
    fn newer_schema_is_rejected_without_modifying_database() -> Result<()> {
        let (_home, paths, store) = fixture()?;
        store.connection.pragma_update(None, "user_version", 999)?;
        drop(store);
        let before = fs::read(&paths.database)?;
        assert!(Store::open(paths.clone()).is_err());
        assert_eq!(fs::read(&paths.database)?, before);
        assert_eq!(fs::read_dir(paths.backups)?.count(), 0);
        Ok(())
    }
    #[test]
    fn upgrade_backs_up_existing_database_privately() -> Result<()> {
        let (_home, paths, mut store) = fixture()?;
        let s = Session::new("preserve", "model");
        store.save_session(&s)?;
        store.connection.pragma_update(None, "user_version", 0)?;
        drop(store);
        let store = Store::open(paths.clone())?;
        assert_eq!(store.load_session(&s.id)?, Some(s));
        let backups: Vec<_> = fs::read_dir(&paths.backups)?.collect::<std::io::Result<_>>()?;
        assert_eq!(backups.len(), 1);
        let backup = Connection::open(backups[0].path())?;
        let version: i64 = backup.pragma_query_value(None, "user_version", |r| r.get(0))?;
        assert_eq!(version, 0);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                fs::metadata(backups[0].path())?.permissions().mode() & 0o777,
                0o600
            );
            assert_eq!(
                fs::metadata(paths.database)?.permissions().mode() & 0o777,
                0o600
            );
        }
        Ok(())
    }
    #[test]
    fn terminal_history_is_immutable_and_missing_records_are_rejected() -> Result<()> {
        let (_home, _paths, mut store) = fixture()?;
        let mut s = Session::new("s", "m");
        let id = store.begin_turn(&mut s, "original")?;
        s.message_mut(&id)?.content = "done".into();
        s.message_mut(&id)?.status = MessageStatus::Completed;
        store.save_session(&s)?;
        let original = s.clone();
        s.message_mut(&id)?.content = "rewrite".into();
        assert!(store.save_session(&s).is_err());
        assert_eq!(store.load_session(&s.id)?, Some(original));
        let mut record = Record::new("transcript", "orphan", json!({}));
        record.session_id = Some("absent".into());
        assert!(store.save_record(&record).is_err());
        Ok(())
    }
    #[test]
    fn checkpoints_preserve_request_usage_and_do_not_allow_removing_steps() -> Result<()> {
        let (_home, paths, mut store) = fixture()?;
        let mut s = Session::new("s", "m");
        let id = store.begin_turn(&mut s, "q")?;
        let a = s.message_mut(&id)?;
        a.model_requests[0].usage.prompt_tokens = Some(7);
        a.model_requests[0].usage.completion_tokens = Some(3);
        a.model_requests[0].usage.total_tokens = Some(10);
        a.status = MessageStatus::Streaming;
        store.save_session(&s)?;
        let mut invalid = s.clone();
        invalid.message_mut(&id)?.model_requests.clear();
        assert!(store.save_session(&invalid).is_err());
        a_terminal(&mut s, &id)?;
        store.save_session(&s)?;
        let retry = store.retry(&mut s, &id)?;
        s.message_mut(&retry)?.status = MessageStatus::Cancelled;
        store.save_session(&s)?;
        let totals = s.usage_totals();
        drop(store);
        let reopened = Store::open(paths)?.load_session(&s.id)?.unwrap();
        assert_eq!(reopened.usage_totals(), totals);
        assert_eq!(totals.total_tokens, 10);
        assert_eq!(totals.unknown_requests, 1);
        Ok(())
    }
    fn a_terminal(s: &mut Session, id: &str) -> Result<()> {
        let a = s.message_mut(id)?;
        a.status = MessageStatus::Completed;
        a.model_requests[0].status = MessageStatus::Completed;
        Ok(())
    }
}
