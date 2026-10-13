use crate::model::Account;
use rusqlite::{params, Connection, OptionalExtension};
use std::path::Path;

#[derive(Debug, thiserror::Error)]
pub enum StoreError {
    #[error("database: {0}")]
    Sqlite(#[from] rusqlite::Error),
    #[error("stored account is not valid: {0}")]
    Json(#[from] serde_json::Error),
}

/// Account metadata. Secrets are never stored here; see `vault`.
pub struct Store {
    db: Connection,
}

impl Store {
    pub fn open(path: &Path) -> Result<Self, StoreError> {
        Self::init(Connection::open(path)?)
    }

    pub fn memory() -> Result<Self, StoreError> {
        Self::init(Connection::open_in_memory()?)
    }

    fn init(db: Connection) -> Result<Self, StoreError> {
        db.execute(
            "CREATE TABLE IF NOT EXISTS accounts (id TEXT PRIMARY KEY, data TEXT NOT NULL)",
            [],
        )?;
        Ok(Self { db })
    }

    pub fn put(&self, account: &Account) -> Result<(), StoreError> {
        self.db.execute(
            "INSERT INTO accounts (id, data) VALUES (?1, ?2)
             ON CONFLICT(id) DO UPDATE SET data = excluded.data",
            params![account.id, serde_json::to_string(account)?],
        )?;
        Ok(())
    }

    pub fn get(&self, id: &str) -> Result<Option<Account>, StoreError> {
        let data: Option<String> = self
            .db
            .query_row("SELECT data FROM accounts WHERE id = ?1", params![id], |row| row.get(0))
            .optional()?;
        Ok(data.map(|d| serde_json::from_str(&d)).transpose()?)
    }

    pub fn list(&self) -> Result<Vec<Account>, StoreError> {
        let mut statement = self.db.prepare("SELECT data FROM accounts ORDER BY id")?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut accounts = Vec::new();
        for data in rows {
            accounts.push(serde_json::from_str(&data?)?);
        }
        Ok(accounts)
    }

    /// Returns whether an account was removed.
    pub fn remove(&self, id: &str) -> Result<bool, StoreError> {
        Ok(self.db.execute("DELETE FROM accounts WHERE id = ?1", params![id])? > 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixtures::account;

    #[test]
    fn put_get_list_remove() {
        let store = Store::memory().unwrap();
        assert!(store.list().unwrap().is_empty());
        assert_eq!(store.get("a").unwrap(), None);

        let b = account("b", &["work"], Some(12.5));
        let a = account("a", &[], None);
        store.put(&b).unwrap();
        store.put(&a).unwrap();
        assert_eq!(store.get("b").unwrap(), Some(b.clone()));
        assert_eq!(store.list().unwrap(), vec![a, b]);

        assert!(store.remove("a").unwrap());
        assert!(!store.remove("a").unwrap());
        assert_eq!(store.list().unwrap().len(), 1);
    }

    #[test]
    fn put_replaces_an_existing_account() {
        let store = Store::memory().unwrap();
        let mut a = account("a", &[], None);
        store.put(&a).unwrap();
        a.tags.insert("work".into());
        a.enabled = false;
        store.put(&a).unwrap();
        assert_eq!(store.list().unwrap(), vec![a]);
    }

    #[test]
    fn data_survives_reopening_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("llmpool.db");
        Store::open(&path).unwrap().put(&account("a", &["x"], None)).unwrap();
        assert_eq!(Store::open(&path).unwrap().get("a").unwrap(), Some(account("a", &["x"], None)));
    }
}
