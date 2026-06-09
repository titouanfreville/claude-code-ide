//! Data-source drivers for the DB explorer panel.
//!
//! A small abstraction over the databases the [`DbObserverPanel`](super::db_observer)
//! browses. Today: **SQLite** (read-only, via `rusqlite`) and **Postgres** (read-only,
//! filled in by the Postgres task). Every operation is read-only and opens a fresh
//! connection — the panel holds no live handle — so the driver functions are plain,
//! side-effect-free queries the view runs off the UI thread.

use std::path::{Path, PathBuf};

use rusqlite::types::ValueRef;
use rusqlite::{Connection, OpenFlags};

/// Rows fetched per page in the data grid (read-only browse, not a full export).
pub const PAGE_LIMIT: usize = 200;

/// A database the panel can browse.
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum DataSource {
    /// A local SQLite file, opened read-only.
    Sqlite(PathBuf),
    /// A Postgres server addressed by a libpq-style DSN.
    Postgres(PgConfig),
}

/// A saved Postgres connection (DSN + display label).
#[derive(Clone, Debug, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct PgConfig {
    pub dsn: String,
    pub label: String,
}

impl DataSource {
    /// A short human label for tabs / the tree root.
    pub fn label(&self) -> String {
        match self {
            DataSource::Sqlite(p) => p
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_owned)
                .unwrap_or_else(|| "database".into()),
            DataSource::Postgres(c) => c.label.clone(),
        }
    }

    /// A stable de-dup key for open tabs. The SQLite form matches
    /// `OpenRequest::Db(path).key()` so a file-tree open and a restored tab collapse
    /// onto the same tab instead of duplicating.
    pub fn key(&self) -> String {
        match self {
            DataSource::Sqlite(p) => format!("db:{}", p.display()),
            DataSource::Postgres(c) => format!("db:pg:{}", c.dsn),
        }
    }
}

// ── Driver-agnostic data model ───────────────────────────────────────────────

/// A column's metadata (name, type, and key role).
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct ColumnMeta {
    pub name: String,
    pub ty: String,
    pub not_null: bool,
    pub pk: bool,
    /// `Some("other_table")` when this column references another table.
    pub fk: Option<String>,
}

/// A table or view with its columns + (optional) row count + index names.
#[derive(Clone, Debug, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TableMeta {
    pub name: String,
    /// Schema namespace (Postgres); `None` for SQLite (single namespace).
    pub schema: Option<String>,
    pub row_count: Option<i64>,
    pub columns: Vec<ColumnMeta>,
    pub indexes: Vec<String>,
}

/// The browsable structure of a data source.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Schema {
    pub tables: Vec<TableMeta>,
    pub views: Vec<TableMeta>,
}

/// A single typed cell value, kept typed so the grid can align/colour it.
#[derive(Clone, Debug, PartialEq)]
pub enum Cell {
    Null,
    Int(i64),
    Real(f64),
    Text(String),
    Bool(bool),
    /// A binary blob, summarised by byte length (never rendered raw).
    Blob(usize),
}

impl Cell {
    /// `true` for numeric cells (right-aligned, numeric colour in the grid).
    pub fn is_numeric(&self) -> bool {
        matches!(self, Cell::Int(_) | Cell::Real(_))
    }

    /// `true` for `NULL` (rendered muted + italic).
    pub fn is_null(&self) -> bool {
        matches!(self, Cell::Null)
    }

    /// The display string (the grid decides colour/alignment from the variant).
    pub fn display(&self) -> String {
        match self {
            Cell::Null => "NULL".into(),
            Cell::Int(n) => n.to_string(),
            Cell::Real(f) => f.to_string(),
            Cell::Text(t) => t.clone(),
            Cell::Bool(b) => if *b { "true" } else { "false" }.into(),
            Cell::Blob(n) => format!("‹{n} bytes›"),
        }
    }
}

/// One page of a result set: the columns, the rows, and the full row count if known.
#[derive(Clone, Debug, Default)]
pub struct Page {
    pub columns: Vec<ColumnMeta>,
    pub rows: Vec<Vec<Cell>>,
    /// Total rows in the underlying table (for pagination); `None` for ad-hoc queries.
    pub total: Option<i64>,
}

/// Sort direction for a grid column.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SortDir {
    Asc,
    Desc,
}

impl SortDir {
    pub fn toggled(self) -> Self {
        match self {
            SortDir::Asc => SortDir::Desc,
            SortDir::Desc => SortDir::Asc,
        }
    }
    pub fn sql(self) -> &'static str {
        match self {
            SortDir::Asc => "ASC",
            SortDir::Desc => "DESC",
        }
    }
    /// The header marker glyph for the active sort.
    pub fn marker(self) -> &'static str {
        match self {
            SortDir::Asc => "▲",
            SortDir::Desc => "▼",
        }
    }
}

// ── Dispatch ─────────────────────────────────────────────────────────────────

/// Load the full schema (tables + views with columns/keys/indexes).
pub fn load_schema(src: &DataSource) -> Result<Schema, String> {
    match src {
        DataSource::Sqlite(path) => sqlite::load_schema(path),
        DataSource::Postgres(cfg) => postgres_driver::load_schema(cfg),
    }
}

/// Load one page of a table, optionally sorted by a column index.
pub fn load_page(
    src: &DataSource,
    table: &TableMeta,
    sort: Option<(usize, SortDir)>,
    offset: usize,
    limit: usize,
) -> Result<Page, String> {
    match src {
        DataSource::Sqlite(path) => sqlite::load_page(path, table, sort, offset, limit),
        DataSource::Postgres(cfg) => postgres_driver::load_page(cfg, table, sort, offset, limit),
    }
}

/// Run a read-only `SELECT`/`WITH` query and return its rows.
pub fn run_query(src: &DataSource, sql: &str) -> Result<Page, String> {
    let trimmed = sql.trim();
    if !is_read_only_query(trimmed) {
        return Err("only a single read-only SELECT/WITH query is allowed".into());
    }
    match src {
        DataSource::Sqlite(path) => sqlite::run_query(path, trimmed),
        DataSource::Postgres(cfg) => postgres_driver::run_query(cfg, trimmed),
    }
}

/// A cheap guard: a single statement beginning with `SELECT`/`WITH` and carrying no
/// inner statement separator. The read-only connection/transaction is the real
/// enforcement; this just keeps obvious writes out of the console.
pub fn is_read_only_query(sql: &str) -> bool {
    let sql = sql.trim().trim_end_matches(';').trim();
    if sql.is_empty() || sql.contains(';') {
        return false;
    }
    let head = sql
        .split_whitespace()
        .next()
        .unwrap_or("")
        .to_ascii_uppercase();
    head == "SELECT" || head == "WITH"
}

/// Reject anything but a simple identifier so it can be safely interpolated into SQL
/// (table/column names can't be bound as parameters).
fn safe_ident(name: &str) -> bool {
    !name.is_empty() && name.chars().all(|c| c.is_alphanumeric() || c == '_')
}

// ── Saved data sources ───────────────────────────────────────────────────────
//
// The operator's data sources (SQLite files + Postgres connections) persist in
// `<root>/.moonlight-local/db_sources.json` — the **gitignored** local dir — so DSNs
// (which may carry a password) never land in a tracked config file.

fn sources_path(root: &Path) -> PathBuf {
    root.join(".moonlight-local").join("db_sources.json")
}

/// Load the saved data sources (missing/malformed → none).
pub fn load_sources(root: &Path) -> Vec<DataSource> {
    std::fs::read(sources_path(root))
        .ok()
        .and_then(|b| serde_json::from_slice::<Vec<DataSource>>(&b).ok())
        .unwrap_or_default()
}

/// Write the full source list, best-effort (creates the local dir).
fn write_sources(root: &Path, sources: &[DataSource]) {
    let path = sources_path(root);
    if let Some(dir) = path.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if let Ok(json) = serde_json::to_vec_pretty(sources) {
        let _ = std::fs::write(path, json);
    }
}

/// Add a data source (dedup by [`DataSource::key`]); returns the updated list.
pub fn add_source(root: &Path, source: &DataSource) -> Vec<DataSource> {
    let mut sources = load_sources(root);
    if !sources.iter().any(|s| s.key() == source.key()) {
        sources.push(source.clone());
        write_sources(root, &sources);
    }
    sources
}

/// Remove a data source by key; returns the updated list.
pub fn remove_source(root: &Path, source: &DataSource) -> Vec<DataSource> {
    let mut sources = load_sources(root);
    sources.retain(|s| s.key() != source.key());
    write_sources(root, &sources);
    sources
}

/// A short label for a DSN — `dbname@host` when parseable, else the raw DSN.
pub fn pg_label(dsn: &str) -> String {
    // postgres://user:pass@host:port/dbname?params
    let after_scheme = dsn.split("://").nth(1).unwrap_or(dsn);
    let host = after_scheme.rsplit('@').next().unwrap_or(after_scheme);
    let host_only = host.split(['/', '?', ':']).next().unwrap_or(host);
    let db = after_scheme
        .split('/')
        .nth(1)
        .map(|s| s.split('?').next().unwrap_or(s))
        .filter(|s| !s.is_empty());
    match db {
        Some(d) => format!("{d}@{host_only}"),
        None => host_only.to_string(),
    }
}

// ── SQLite ───────────────────────────────────────────────────────────────────

mod sqlite {
    use super::*;

    /// Open `path` read-only.
    fn open_ro(path: &Path) -> Result<Connection, String> {
        Connection::open_with_flags(path, OpenFlags::SQLITE_OPEN_READ_ONLY)
            .map_err(|e| e.to_string())
    }

    pub fn load_schema(path: &Path) -> Result<Schema, String> {
        let conn = open_ro(path)?;
        Ok(Schema {
            tables: load_objects(&conn, "table")?,
            views: load_objects(&conn, "view")?,
        })
    }

    /// List user objects of `kind` ("table"/"view") with full metadata, alphabetically.
    fn load_objects(conn: &Connection, kind: &str) -> Result<Vec<TableMeta>, String> {
        let mut stmt = conn
            .prepare(
                "SELECT name FROM sqlite_master \
                 WHERE type = ?1 AND name NOT LIKE 'sqlite_%' ORDER BY name",
            )
            .map_err(|e| e.to_string())?;
        let names: Vec<String> = stmt
            .query_map([kind], |row| row.get::<_, String>(0))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())?;
        names.into_iter().map(|name| table_meta(conn, &name)).collect()
    }

    /// Build a [`TableMeta`] from PRAGMAs (columns, FKs, indexes) + a row count.
    fn table_meta(conn: &Connection, name: &str) -> Result<TableMeta, String> {
        if !safe_ident(name) {
            return Err("unsupported object name".into());
        }
        // Columns: PRAGMA table_info → (cid, name, type, notnull, dflt, pk).
        let mut columns: Vec<ColumnMeta> = {
            let mut stmt = conn
                .prepare(&format!("PRAGMA table_info(\"{name}\")"))
                .map_err(|e| e.to_string())?;
            let cols = stmt
                .query_map([], |row| {
                    Ok(ColumnMeta {
                        name: row.get::<_, String>(1)?,
                        ty: row.get::<_, String>(2).unwrap_or_default(),
                        not_null: row.get::<_, i64>(3).unwrap_or(0) != 0,
                        pk: row.get::<_, i64>(5).unwrap_or(0) != 0,
                        fk: None,
                    })
                })
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            cols
        };
        // Foreign keys: PRAGMA foreign_key_list → (id, seq, table, from, to, …).
        {
            let mut stmt = conn
                .prepare(&format!("PRAGMA foreign_key_list(\"{name}\")"))
                .map_err(|e| e.to_string())?;
            let fks: Vec<(String, String)> = stmt
                .query_map([], |row| Ok((row.get::<_, String>(2)?, row.get::<_, String>(3)?)))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            for (reftable, fromcol) in fks {
                if let Some(col) = columns.iter_mut().find(|c| c.name == fromcol) {
                    col.fk = Some(reftable);
                }
            }
        }
        // Indexes: PRAGMA index_list → (seq, name, unique, …).
        let indexes: Vec<String> = {
            let mut stmt = conn
                .prepare(&format!("PRAGMA index_list(\"{name}\")"))
                .map_err(|e| e.to_string())?;
            let idx = stmt
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(|e| e.to_string())?
                .collect::<Result<_, _>>()
                .map_err(|e| e.to_string())?;
            idx
        };
        // Row count (best-effort; a plain COUNT(*) is fine for a browse).
        let row_count = conn
            .query_row(&format!("SELECT COUNT(*) FROM \"{name}\""), [], |r| r.get::<_, i64>(0))
            .ok();

        Ok(TableMeta {
            name: name.to_string(),
            schema: None,
            row_count,
            columns,
            indexes,
        })
    }

    pub fn load_page(
        path: &Path,
        table: &TableMeta,
        sort: Option<(usize, SortDir)>,
        offset: usize,
        limit: usize,
    ) -> Result<Page, String> {
        if !safe_ident(&table.name) {
            return Err("unsupported table name".into());
        }
        let conn = open_ro(path)?;
        let order = match sort {
            Some((idx, dir)) => {
                let col = table
                    .columns
                    .get(idx)
                    .ok_or_else(|| "sort column out of range".to_string())?;
                if !safe_ident(&col.name) {
                    return Err("unsupported column name".into());
                }
                format!(" ORDER BY \"{}\" {}", col.name, dir.sql())
            }
            None => String::new(),
        };
        let sql = format!(
            "SELECT * FROM \"{}\"{order} LIMIT {limit} OFFSET {offset}",
            table.name
        );
        let mut stmt = conn.prepare(&sql).map_err(|e| e.to_string())?;
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = collect_rows(&mut stmt, col_names.len())?;
        // Prefer the known column metadata; fall back to bare names if shape differs.
        let columns = if table.columns.len() == col_names.len() {
            table.columns.clone()
        } else {
            col_names.into_iter().map(named_col).collect()
        };
        Ok(Page { columns, rows, total: table.row_count })
    }

    pub fn run_query(path: &Path, sql: &str) -> Result<Page, String> {
        let conn = open_ro(path)?;
        let mut stmt = conn.prepare(sql).map_err(|e| e.to_string())?;
        let col_names: Vec<String> = stmt.column_names().iter().map(|s| s.to_string()).collect();
        let rows = collect_rows(&mut stmt, col_names.len())?;
        let columns = col_names.into_iter().map(named_col).collect();
        Ok(Page { columns, rows, total: None })
    }

    /// Run a prepared statement to typed rows.
    fn collect_rows(stmt: &mut rusqlite::Statement, ncol: usize) -> Result<Vec<Vec<Cell>>, String> {
        stmt.query_map([], |row| Ok((0..ncol).map(|i| cell_of(row, i)).collect::<Vec<_>>()))
            .map_err(|e| e.to_string())?
            .collect::<Result<_, _>>()
            .map_err(|e| e.to_string())
    }

    /// A bare column (name only) for ad-hoc query results.
    fn named_col(name: String) -> ColumnMeta {
        ColumnMeta { name, ..Default::default() }
    }

    /// Convert a SQLite cell to a typed [`Cell`].
    fn cell_of(row: &rusqlite::Row, i: usize) -> Cell {
        match row.get_ref(i) {
            Ok(ValueRef::Null) => Cell::Null,
            Ok(ValueRef::Integer(n)) => Cell::Int(n),
            Ok(ValueRef::Real(f)) => Cell::Real(f),
            Ok(ValueRef::Text(t)) => Cell::Text(String::from_utf8_lossy(t).into_owned()),
            Ok(ValueRef::Blob(b)) => Cell::Blob(b.len()),
            Err(_) => Cell::Null,
        }
    }
}

// ── Postgres (wired by the Postgres task) ────────────────────────────────────

mod postgres_driver {
    use super::*;
    use postgres::types::Type;
    use postgres::{Client, NoTls};

    /// Connect from the DSN and pin the session read-only (writes are refused server-side).
    fn connect(cfg: &PgConfig) -> Result<Client, String> {
        let mut client = Client::connect(&cfg.dsn, NoTls).map_err(|e| e.to_string())?;
        client
            .batch_execute("SET default_transaction_read_only = on")
            .map_err(|e| e.to_string())?;
        Ok(client)
    }

    pub fn load_schema(cfg: &PgConfig) -> Result<Schema, String> {
        let mut client = connect(cfg)?;
        let rows = client
            .query(
                "SELECT table_schema, table_name, table_type \
                 FROM information_schema.tables \
                 WHERE table_schema NOT IN ('pg_catalog', 'information_schema') \
                 ORDER BY table_schema, table_name",
                &[],
            )
            .map_err(|e| e.to_string())?;
        let mut tables = Vec::new();
        let mut views = Vec::new();
        for row in &rows {
            let schema: String = row.get(0);
            let name: String = row.get(1);
            let ttype: String = row.get(2);
            let meta = table_meta(&mut client, &schema, &name)?;
            if ttype == "VIEW" {
                views.push(meta);
            } else {
                tables.push(meta);
            }
        }
        Ok(Schema { tables, views })
    }

    /// Columns (+ PK/FK), indexes, and an estimated row count for one table.
    fn table_meta(client: &mut Client, schema: &str, name: &str) -> Result<TableMeta, String> {
        let mut columns: Vec<ColumnMeta> = client
            .query(
                "SELECT column_name, data_type, is_nullable \
                 FROM information_schema.columns \
                 WHERE table_schema = $1 AND table_name = $2 ORDER BY ordinal_position",
                &[&schema, &name],
            )
            .map_err(|e| e.to_string())?
            .iter()
            .map(|r| {
                let nullable: String = r.get(2);
                ColumnMeta {
                    name: r.get(0),
                    ty: r.get(1),
                    not_null: nullable == "NO",
                    pk: false,
                    fk: None,
                }
            })
            .collect();

        // Primary-key columns.
        for r in client
            .query(
                "SELECT kcu.column_name FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                   AND tc.table_schema = kcu.table_schema \
                 WHERE tc.constraint_type = 'PRIMARY KEY' \
                   AND tc.table_schema = $1 AND tc.table_name = $2",
                &[&schema, &name],
            )
            .map_err(|e| e.to_string())?
        {
            let col: String = r.get(0);
            if let Some(c) = columns.iter_mut().find(|c| c.name == col) {
                c.pk = true;
            }
        }

        // Foreign keys (column → referenced table).
        for r in client
            .query(
                "SELECT kcu.column_name, ccu.table_name FROM information_schema.table_constraints tc \
                 JOIN information_schema.key_column_usage kcu \
                   ON tc.constraint_name = kcu.constraint_name \
                   AND tc.table_schema = kcu.table_schema \
                 JOIN information_schema.constraint_column_usage ccu \
                   ON ccu.constraint_name = tc.constraint_name \
                   AND ccu.table_schema = tc.table_schema \
                 WHERE tc.constraint_type = 'FOREIGN KEY' \
                   AND tc.table_schema = $1 AND tc.table_name = $2",
                &[&schema, &name],
            )
            .map_err(|e| e.to_string())?
        {
            let col: String = r.get(0);
            let target: String = r.get(1);
            if let Some(c) = columns.iter_mut().find(|c| c.name == col) {
                c.fk = Some(target);
            }
        }

        // Indexes.
        let indexes: Vec<String> = client
            .query(
                "SELECT indexname FROM pg_indexes \
                 WHERE schemaname = $1 AND tablename = $2 ORDER BY indexname",
                &[&schema, &name],
            )
            .map_err(|e| e.to_string())?
            .iter()
            .map(|r| r.get(0))
            .collect();

        // Estimated row count (planner statistic — cheap, unlike COUNT(*) on a big table).
        let row_count = client
            .query_one(
                "SELECT reltuples::bigint FROM pg_class c \
                 JOIN pg_namespace n ON n.oid = c.relnamespace \
                 WHERE n.nspname = $1 AND c.relname = $2",
                &[&schema, &name],
            )
            .ok()
            .map(|r| r.get::<_, i64>(0))
            .filter(|&n| n >= 0);

        Ok(TableMeta {
            name: name.to_string(),
            schema: Some(schema.to_string()),
            row_count,
            columns,
            indexes,
        })
    }

    pub fn load_page(
        cfg: &PgConfig,
        table: &TableMeta,
        sort: Option<(usize, SortDir)>,
        offset: usize,
        limit: usize,
    ) -> Result<Page, String> {
        let schema = table.schema.as_deref().unwrap_or("public");
        if !safe_ident(schema) || !safe_ident(&table.name) {
            return Err("unsupported identifier".into());
        }
        if table.columns.iter().any(|c| !safe_ident(&c.name)) {
            return Err("unsupported column name".into());
        }
        let order = match sort {
            Some((idx, dir)) => {
                let col = table
                    .columns
                    .get(idx)
                    .ok_or_else(|| "sort column out of range".to_string())?;
                format!(" ORDER BY \"{}\" {}", col.name, dir.sql())
            }
            None => String::new(),
        };
        // Cast every column to text so retrieval never fails on an exotic type; the typed
        // cell is rebuilt from the column's declared type below (keeps numeric alignment).
        let select = if table.columns.is_empty() {
            "*".to_string()
        } else {
            table
                .columns
                .iter()
                .map(|c| format!("\"{0}\"::text AS \"{0}\"", c.name))
                .collect::<Vec<_>>()
                .join(", ")
        };
        let sql = format!(
            "SELECT {select} FROM \"{schema}\".\"{}\"{order} LIMIT {limit} OFFSET {offset}",
            table.name
        );
        let mut client = connect(cfg)?;
        let rows = client.query(&sql, &[]).map_err(|e| e.to_string())?;
        let columns = table.columns.clone();
        let data = rows
            .iter()
            .map(|row| {
                columns
                    .iter()
                    .enumerate()
                    .map(|(i, c)| cell_from_text(row.get::<_, Option<String>>(i), &c.ty))
                    .collect()
            })
            .collect();
        Ok(Page { columns, rows: data, total: table.row_count })
    }

    pub fn run_query(cfg: &PgConfig, sql: &str) -> Result<Page, String> {
        let mut client = connect(cfg)?;
        let rows = client.query(sql, &[]).map_err(|e| e.to_string())?;
        let columns: Vec<ColumnMeta> = match rows.first() {
            Some(first) => first
                .columns()
                .iter()
                .map(|c| ColumnMeta {
                    name: c.name().to_string(),
                    ty: c.type_().name().to_string(),
                    ..Default::default()
                })
                .collect(),
            None => Vec::new(),
        };
        let data = rows
            .iter()
            .map(|row| (0..columns.len()).map(|i| pg_cell(row, i)).collect())
            .collect();
        Ok(Page { columns, rows: data, total: None })
    }

    /// Re-type a `::text`-cast value using the column's declared Postgres type, so numbers
    /// still right-align and bools render as chips on the main browse path.
    fn cell_from_text(text: Option<String>, ty: &str) -> Cell {
        let Some(s) = text else {
            return Cell::Null;
        };
        let t = ty.to_ascii_lowercase();
        if t.contains("int") || t == "smallint" || t == "bigint" {
            if let Ok(n) = s.parse::<i64>() {
                return Cell::Int(n);
            }
        }
        if t.contains("numeric")
            || t.contains("decimal")
            || t.contains("real")
            || t.contains("double")
            || t.contains("float")
        {
            if let Ok(f) = s.parse::<f64>() {
                return Cell::Real(f);
            }
        }
        if t == "boolean" || t == "bool" {
            match s.as_str() {
                "t" | "true" => return Cell::Bool(true),
                "f" | "false" => return Cell::Bool(false),
                _ => {}
            }
        }
        Cell::Text(s)
    }

    /// Best-effort typed read for an arbitrary console result column. Exotic types the
    /// driver can't map fall back to a `‹typename›` hint (cast to `::text` to view them).
    fn pg_cell(row: &postgres::Row, i: usize) -> Cell {
        let ty = row.columns()[i].type_().clone();
        if ty == Type::BOOL {
            return row.get::<_, Option<bool>>(i).map_or(Cell::Null, Cell::Bool);
        }
        if ty == Type::INT2 {
            return row.get::<_, Option<i16>>(i).map_or(Cell::Null, |n| Cell::Int(n as i64));
        }
        if ty == Type::INT4 {
            return row.get::<_, Option<i32>>(i).map_or(Cell::Null, |n| Cell::Int(n as i64));
        }
        if ty == Type::INT8 {
            return row.get::<_, Option<i64>>(i).map_or(Cell::Null, Cell::Int);
        }
        if ty == Type::FLOAT4 {
            return row.get::<_, Option<f32>>(i).map_or(Cell::Null, |f| Cell::Real(f as f64));
        }
        if ty == Type::FLOAT8 {
            return row.get::<_, Option<f64>>(i).map_or(Cell::Null, Cell::Real);
        }
        if ty == Type::BYTEA {
            return row.get::<_, Option<Vec<u8>>>(i).map_or(Cell::Null, |b| Cell::Blob(b.len()));
        }
        match row.try_get::<_, Option<String>>(i) {
            Ok(Some(s)) => Cell::Text(s),
            Ok(None) => Cell::Null,
            Err(_) => Cell::Text(format!("‹{}›", ty.name())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn tmp() -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static N: AtomicU32 = AtomicU32::new(0);
        let dir = std::env::temp_dir().join(format!(
            "mlc-dbsrc-{}-{}",
            std::process::id(),
            N.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    fn make_db(dir: &Path) -> PathBuf {
        let path = dir.join("test.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE teams (id INTEGER PRIMARY KEY, label TEXT);\
             CREATE TABLE users (id INTEGER PRIMARY KEY, name TEXT NOT NULL, \
                 team INTEGER REFERENCES teams(id));\
             CREATE INDEX idx_users_name ON users(name);\
             INSERT INTO teams VALUES (1, 'red'), (2, 'blue');\
             INSERT INTO users VALUES (1, 'ada', 1), (2, 'lin', 2), (3, 'mae', 1);\
             CREATE VIEW v_users AS SELECT id, name FROM users;",
        )
        .unwrap();
        path
    }

    #[test]
    fn schema_reports_columns_keys_and_indexes() {
        let dir = tmp();
        let src = DataSource::Sqlite(make_db(&dir));
        let schema = load_schema(&src).unwrap();

        let tables: Vec<_> = schema.tables.iter().map(|t| t.name.as_str()).collect();
        assert_eq!(tables, vec!["teams", "users"]);
        let views: Vec<_> = schema.views.iter().map(|v| v.name.as_str()).collect();
        assert_eq!(views, vec!["v_users"]);

        let users = schema.tables.iter().find(|t| t.name == "users").unwrap();
        assert_eq!(users.row_count, Some(3));
        assert!(users.columns.iter().find(|c| c.name == "id").unwrap().pk);
        assert!(users.columns.iter().find(|c| c.name == "name").unwrap().not_null);
        assert_eq!(
            users.columns.iter().find(|c| c.name == "team").unwrap().fk.as_deref(),
            Some("teams")
        );
        assert!(users.indexes.iter().any(|i| i.contains("idx_users_name")));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn page_sorts_and_paginates_with_total() {
        let dir = tmp();
        let src = DataSource::Sqlite(make_db(&dir));
        let schema = load_schema(&src).unwrap();
        let users = schema.tables.iter().find(|t| t.name == "users").unwrap();

        // name DESC → mae, lin, ada. First page of 2:
        let page = load_page(&src, users, Some((1, SortDir::Desc)), 0, 2).unwrap();
        assert_eq!(page.total, Some(3));
        assert_eq!(page.rows.len(), 2);
        assert_eq!(page.rows[0][1], Cell::Text("mae".into()));
        assert_eq!(page.rows[1][1], Cell::Text("lin".into()));

        // Offset to the last row.
        let page2 = load_page(&src, users, Some((1, SortDir::Desc)), 2, 2).unwrap();
        assert_eq!(page2.rows.len(), 1);
        assert_eq!(page2.rows[0][1], Cell::Text("ada".into()));

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn run_query_returns_typed_rows() {
        let dir = tmp();
        let src = DataSource::Sqlite(make_db(&dir));
        let page = run_query(&src, "SELECT id, name FROM users ORDER BY id LIMIT 1").unwrap();
        assert_eq!(page.rows[0][0], Cell::Int(1));
        assert_eq!(page.rows[0][1], Cell::Text("ada".into()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn query_guard_rejects_writes_and_multi_statement() {
        assert!(is_read_only_query("SELECT 1"));
        assert!(is_read_only_query("  with x as (select 1) select * from x  "));
        assert!(is_read_only_query("SELECT 1;"));
        assert!(!is_read_only_query("DELETE FROM users"));
        assert!(!is_read_only_query("SELECT 1; DROP TABLE users"));
        assert!(!is_read_only_query("UPDATE users SET name='x'"));
        assert!(!is_read_only_query(""));
    }

    #[test]
    fn rejects_bad_table_name() {
        let bad = TableMeta { name: "users; DROP".into(), ..Default::default() };
        let src = DataSource::Sqlite("/tmp/none.sqlite".into());
        assert!(load_page(&src, &bad, None, 0, 10).is_err());
    }

    #[test]
    fn pg_label_extracts_db_and_host() {
        assert_eq!(pg_label("postgres://u:p@db.example.com:5432/shop?sslmode=require"), "shop@db.example.com");
        assert_eq!(pg_label("postgres://localhost/app"), "app@localhost");
        assert_eq!(pg_label("not-a-dsn"), "not-a-dsn");
    }

    #[test]
    fn saved_sources_round_trip_dedup_and_remove() {
        let dir = tmp();
        let pg = DataSource::Postgres(PgConfig {
            dsn: "postgres://localhost/app".into(),
            label: "app@localhost".into(),
        });
        let lite = DataSource::Sqlite("/tmp/x.sqlite".into());
        add_source(&dir, &pg);
        add_source(&dir, &pg); // dedup by key
        let after_add = add_source(&dir, &lite);
        assert_eq!(after_add, vec![pg.clone(), lite.clone()]);
        assert_eq!(load_sources(&dir), vec![pg.clone(), lite.clone()]);

        let after_remove = remove_source(&dir, &pg);
        assert_eq!(after_remove, vec![lite]);
        let _ = std::fs::remove_dir_all(&dir);
    }
}
