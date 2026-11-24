//! M029: deterministic schema diff planner and guarded migration runner.
//! Database execution is delegated to an adapter; this module owns planning,
//! dependency ordering, checksums, compatibility classification and locking.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::sync::{Arc, Mutex};

pub type ObjectName = String;

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum SqlType {
    Integer,
    BigInt,
    Text,
    Boolean,
    Timestamp,
    Decimal,
    Blob,
    Custom(String),
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ColumnDef {
    pub name: String,
    pub sql_type: SqlType,
    pub nullable: bool,
    pub default_sql: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct ForeignKey {
    pub name: String,
    pub columns: Vec<String>,
    pub referenced_table: String,
    pub referenced_columns: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct IndexDef {
    pub name: String,
    pub columns: Vec<String>,
    pub unique: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TableDef {
    pub name: String,
    pub columns: BTreeMap<String, ColumnDef>,
    pub primary_key: Vec<String>,
    pub foreign_keys: BTreeMap<String, ForeignKey>,
    pub indexes: BTreeMap<String, IndexDef>,
}

#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct Schema {
    pub tables: BTreeMap<String, TableDef>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Step {
    CreateTable(TableDef),
    DropTable(String),
    AddColumn {
        table: String,
        column: ColumnDef,
    },
    DropColumn {
        table: String,
        column: String,
    },
    AlterColumn {
        table: String,
        before: ColumnDef,
        after: ColumnDef,
    },
    AddIndex {
        table: String,
        index: IndexDef,
    },
    DropIndex {
        table: String,
        index: String,
    },
    AddForeignKey {
        table: String,
        foreign_key: ForeignKey,
    },
    DropForeignKey {
        table: String,
        foreign_key: String,
    },
    SetPrimaryKey {
        table: String,
        columns: Vec<String>,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Compatibility {
    BackwardCompatible,
    ForwardCompatible,
    Breaking,
    Unknown,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PlannedStep {
    pub step: Step,
    pub compatibility: Compatibility,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MigrationError {
    InvalidSchema(&'static str),
    DuplicateMigrationVersion,
    InvalidMigrationName,
    ChecksumMismatch(String),
    AlreadyApplied(String),
    LockUnavailable,
    Adapter(String),
    PlanLimitExceeded,
    UnsafeSql,
}
impl fmt::Display for MigrationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for MigrationError {}

pub fn validate_schema(schema: &Schema) -> Result<(), MigrationError> {
    for (table_name, table) in &schema.tables {
        if table_name.is_empty() || table.name != *table_name {
            return Err(MigrationError::InvalidSchema("table key/name mismatch"));
        }
        if table.columns.is_empty() {
            return Err(MigrationError::InvalidSchema("table must have columns"));
        }
        for (name, col) in &table.columns {
            if name.is_empty() || col.name != *name {
                return Err(MigrationError::InvalidSchema("column key/name mismatch"));
            }
        }
        for key in &table.primary_key {
            if !table.columns.contains_key(key) {
                return Err(MigrationError::InvalidSchema("primary key column missing"));
            }
        }
        for fk in table.foreign_keys.values() {
            if fk.columns.is_empty() || fk.columns.len() != fk.referenced_columns.len() {
                return Err(MigrationError::InvalidSchema("foreign key arity mismatch"));
            }
            if fk.columns.iter().any(|c| !table.columns.contains_key(c)) {
                return Err(MigrationError::InvalidSchema(
                    "foreign key local column missing",
                ));
            }
            let referenced =
                schema
                    .tables
                    .get(&fk.referenced_table)
                    .ok_or(MigrationError::InvalidSchema(
                        "foreign key target table missing",
                    ))?;
            if fk
                .referenced_columns
                .iter()
                .any(|c| !referenced.columns.contains_key(c))
            {
                return Err(MigrationError::InvalidSchema(
                    "foreign key target column missing",
                ));
            }
        }
        for index in table.indexes.values() {
            if index.columns.is_empty()
                || index.columns.iter().any(|c| !table.columns.contains_key(c))
            {
                return Err(MigrationError::InvalidSchema("index column missing"));
            }
        }
    }
    Ok(())
}

pub fn diff_schemas(
    old: &Schema,
    new: &Schema,
    max_steps: usize,
) -> Result<Vec<PlannedStep>, MigrationError> {
    validate_schema(old)?;
    validate_schema(new)?;
    let mut steps = Vec::new();
    let mut push = |step: Step, compatibility: Compatibility| -> Result<(), MigrationError> {
        if steps.len() >= max_steps {
            return Err(MigrationError::PlanLimitExceeded);
        }
        steps.push(PlannedStep {
            step,
            compatibility,
        });
        Ok(())
    };

    for (name, before) in &old.tables {
        if !new.tables.contains_key(name) {
            push(Step::DropTable(name.clone()), Compatibility::Breaking)?;
            continue;
        }
        let after = new
            .tables
            .get(name)
            .ok_or(MigrationError::InvalidSchema("table disappeared"))?;
        for (col_name, old_col) in &before.columns {
            match after.columns.get(col_name) {
                None => push(
                    Step::DropColumn {
                        table: name.clone(),
                        column: col_name.clone(),
                    },
                    Compatibility::Breaking,
                )?,
                Some(new_col) if old_col != new_col => {
                    let compat = if old_col.sql_type == new_col.sql_type
                        && old_col.nullable
                        && !new_col.nullable
                    {
                        Compatibility::Breaking
                    } else {
                        Compatibility::Unknown
                    };
                    push(
                        Step::AlterColumn {
                            table: name.clone(),
                            before: old_col.clone(),
                            after: new_col.clone(),
                        },
                        compat,
                    )?;
                }
                _ => {}
            }
        }
        for (col_name, col) in &after.columns {
            if !before.columns.contains_key(col_name) {
                let compat = if col.nullable || col.default_sql.is_some() {
                    Compatibility::BackwardCompatible
                } else {
                    Compatibility::Breaking
                };
                push(
                    Step::AddColumn {
                        table: name.clone(),
                        column: col.clone(),
                    },
                    compat,
                )?;
            }
        }
        for idx_name in before.indexes.keys() {
            if !after.indexes.contains_key(idx_name) {
                push(
                    Step::DropIndex {
                        table: name.clone(),
                        index: idx_name.clone(),
                    },
                    Compatibility::Unknown,
                )?;
            }
        }
        for (idx_name, idx) in &after.indexes {
            if !before.indexes.contains_key(idx_name) || before.indexes.get(idx_name) != Some(idx) {
                if before.indexes.contains_key(idx_name) {
                    push(
                        Step::DropIndex {
                            table: name.clone(),
                            index: idx_name.clone(),
                        },
                        Compatibility::Unknown,
                    )?;
                }
                push(
                    Step::AddIndex {
                        table: name.clone(),
                        index: idx.clone(),
                    },
                    Compatibility::BackwardCompatible,
                )?;
            }
        }
        for (fk_name, fk) in &before.foreign_keys {
            if after.foreign_keys.get(fk_name) != Some(fk) {
                push(
                    Step::DropForeignKey {
                        table: name.clone(),
                        foreign_key: fk_name.clone(),
                    },
                    Compatibility::Unknown,
                )?;
            }
        }
        for (fk_name, fk) in &after.foreign_keys {
            if before.foreign_keys.get(fk_name) != Some(fk) {
                push(
                    Step::AddForeignKey {
                        table: name.clone(),
                        foreign_key: fk.clone(),
                    },
                    Compatibility::Unknown,
                )?;
            }
        }
        if before.primary_key != after.primary_key {
            push(
                Step::SetPrimaryKey {
                    table: name.clone(),
                    columns: after.primary_key.clone(),
                },
                Compatibility::Breaking,
            )?;
        }
    }
    for (name, table) in &new.tables {
        if !old.tables.contains_key(name) {
            push(
                Step::CreateTable(table.clone()),
                Compatibility::BackwardCompatible,
            )?;
        }
    }
    Ok(steps)
}

/// Deterministic FNV-1a fingerprint. This is a drift detector, not a cryptographic signature.
pub fn checksum(bytes: &[u8]) -> u64 {
    let mut hash = 0xcbf29ce484222325u64;
    for byte in bytes {
        hash = (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3);
    }
    hash
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Migration {
    pub version: u64,
    pub name: String,
    pub up_sql: String,
    pub down_sql: Option<String>,
    pub checksum: u64,
}
impl Migration {
    pub fn new(
        version: u64,
        name: String,
        up_sql: String,
        down_sql: Option<String>,
    ) -> Result<Self, MigrationError> {
        if version == 0
            || name.is_empty()
            || name.len() > 128
            || !name
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
        {
            return Err(MigrationError::InvalidMigrationName);
        }
        let sum = checksum(up_sql.as_bytes());
        Ok(Self {
            version,
            name,
            up_sql,
            down_sql,
            checksum: sum,
        })
    }
    pub fn verify(&self) -> Result<(), MigrationError> {
        if checksum(self.up_sql.as_bytes()) != self.checksum {
            return Err(MigrationError::ChecksumMismatch(self.name.clone()));
        }
        Ok(())
    }
}

pub trait DatabaseAdapter {
    fn begin(&mut self) -> Result<(), String>;
    fn execute(&mut self, sql: &str) -> Result<(), String>;
    fn record_applied(&mut self, version: u64, name: &str, checksum: u64) -> Result<(), String>;
    fn commit(&mut self) -> Result<(), String>;
    fn rollback(&mut self) -> Result<(), String>;
}

#[derive(Default)]
pub struct MigrationRunner {
    applied: BTreeMap<u64, (String, u64)>,
}
impl MigrationRunner {
    pub fn applied_versions(&self) -> Vec<u64> {
        self.applied.keys().copied().collect()
    }

    pub fn apply<A: DatabaseAdapter>(
        &mut self,
        adapter: &mut A,
        migrations: &[Migration],
        max_sql_bytes: usize,
    ) -> Result<Vec<u64>, MigrationError> {
        let mut ordered = migrations.to_vec();
        ordered.sort_by_key(|m| m.version);
        let mut previous = None;
        let mut pending = Vec::new();
        for migration in &ordered {
            migration.verify()?;
            if previous == Some(migration.version) {
                return Err(MigrationError::DuplicateMigrationVersion);
            }
            previous = Some(migration.version);
            if migration.up_sql.len() > max_sql_bytes {
                return Err(MigrationError::PlanLimitExceeded);
            }
            if migration.up_sql.trim().is_empty() {
                return Err(MigrationError::UnsafeSql);
            }
            if let Some((name, sum)) = self.applied.get(&migration.version) {
                if name != &migration.name || sum != &migration.checksum {
                    return Err(MigrationError::ChecksumMismatch(migration.name.clone()));
                }
                return Err(MigrationError::AlreadyApplied(migration.name.clone()));
            }
            pending.push(migration);
        }
        let mut done = Vec::new();
        for migration in pending {
            adapter.begin().map_err(MigrationError::Adapter)?;
            let operation = adapter
                .execute(&migration.up_sql)
                .and_then(|_| {
                    adapter.record_applied(migration.version, &migration.name, migration.checksum)
                })
                .and_then(|_| adapter.commit());
            if let Err(error) = operation {
                let _ = adapter.rollback();
                return Err(MigrationError::Adapter(error));
            }
            self.applied.insert(
                migration.version,
                (migration.name.clone(), migration.checksum),
            );
            done.push(migration.version);
        }
        Ok(done)
    }
}

#[derive(Clone, Default)]
pub struct MigrationLock {
    held: Arc<Mutex<BTreeSet<String>>>,
}
pub struct LockGuard {
    name: String,
    held: Arc<Mutex<BTreeSet<String>>>,
}
impl MigrationLock {
    pub fn acquire(&self, scope: &str) -> Result<LockGuard, MigrationError> {
        if scope.is_empty() || scope.len() > 256 {
            return Err(MigrationError::InvalidSchema("invalid lock scope"));
        }
        let mut held = self
            .held
            .lock()
            .map_err(|_| MigrationError::LockUnavailable)?;
        if !held.insert(scope.to_owned()) {
            return Err(MigrationError::LockUnavailable);
        }
        Ok(LockGuard {
            name: scope.to_owned(),
            held: Arc::clone(&self.held),
        })
    }
}
impl Drop for LockGuard {
    fn drop(&mut self) {
        if let Ok(mut held) = self.held.lock() {
            held.remove(&self.name);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[derive(Default)]
    struct FakeDb {
        sql: Vec<String>,
        versions: Vec<u64>,
        in_tx: bool,
    }
    impl DatabaseAdapter for FakeDb {
        fn begin(&mut self) -> Result<(), String> {
            self.in_tx = true;
            Ok(())
        }
        fn execute(&mut self, sql: &str) -> Result<(), String> {
            self.sql.push(sql.to_owned());
            Ok(())
        }
        fn record_applied(
            &mut self,
            version: u64,
            _name: &str,
            _checksum: u64,
        ) -> Result<(), String> {
            self.versions.push(version);
            Ok(())
        }
        fn commit(&mut self) -> Result<(), String> {
            self.in_tx = false;
            Ok(())
        }
        fn rollback(&mut self) -> Result<(), String> {
            self.in_tx = false;
            Ok(())
        }
    }
    fn col(name: &str, nullable: bool) -> ColumnDef {
        ColumnDef {
            name: name.into(),
            sql_type: SqlType::Text,
            nullable,
            default_sql: None,
        }
    }
    #[test]
    fn schema_diff_adds_nullable_column() {
        let mut old = Schema::default();
        old.tables.insert(
            "users".into(),
            TableDef {
                name: "users".into(),
                columns: BTreeMap::from([("id".into(), col("id", false))]),
                ..TableDef::default()
            },
        );
        let mut new = old.clone();
        if let Some(t) = new.tables.get_mut("users") {
            t.columns.insert("nickname".into(), col("nickname", true));
        }
        let diff = diff_schemas(&old, &new, 20).expect("diff");
        assert_eq!(diff.len(), 1);
        assert_eq!(diff[0].compatibility, Compatibility::BackwardCompatible);
    }
    #[test]
    fn migration_apply_is_ordered_and_tracked() {
        let mut runner = MigrationRunner::default();
        let mut db = FakeDb::default();
        let second =
            Migration::new(2, "second".into(), "SELECT 2".into(), None).expect("migration");
        let first = Migration::new(1, "first".into(), "SELECT 1".into(), None).expect("migration");
        assert_eq!(
            runner.apply(&mut db, &[second, first], 100).expect("apply"),
            vec![1, 2]
        );
        assert_eq!(db.versions, vec![1, 2]);
    }
    #[test]
    fn lock_is_exclusive_and_released() {
        let lock = MigrationLock::default();
        let guard = lock.acquire("tenant:7").expect("lock");
        assert_eq!(
            lock.acquire("tenant:7").err(),
            Some(MigrationError::LockUnavailable)
        );
        drop(guard);
        assert!(lock.acquire("tenant:7").is_ok());
    }
}
