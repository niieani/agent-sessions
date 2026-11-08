//! Shared boundaries for SQLite-backed native Stores.
//!
//! Native Stores must remain read-only while still observing live WAL data.
//! This module also owns the typed value encoding and deterministic identity
//! primitives shared by SQLite-backed Adapters.

use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use anyhow::{Context, Result};
use base64::Engine as _;
use rusqlite::types::ValueRef;
use rusqlite::{Connection, ErrorCode, OpenFlags};
use serde_json::Value;

pub(crate) const FNV_OFFSET_BASIS: u64 = 0xcbf2_9ce4_8422_2325;

pub(crate) struct CapturedValue {
    pub value: Value,
    pub value_type: &'static str,
    pub encoding: Option<&'static str>,
}

/// Open a native SQLite Store without permitting writes or hiding live WAL.
///
/// Live WAL content is read with `mode=ro`. An idle Store (WAL absent or
/// empty) cannot create the `-shm` index in read-only mode; if that open
/// cannot map WAL state, retry with `immutable=1` only while the WAL is
/// still idle so pending writes stay visible.
pub(crate) fn open_read_only(path: &Path, source: &str) -> Result<Connection> {
    let flags = OpenFlags::SQLITE_OPEN_READ_ONLY
        | OpenFlags::SQLITE_OPEN_URI
        | OpenFlags::SQLITE_OPEN_NO_MUTEX;
    let live_uri = read_only_uri(path)?;
    let idle_uri = format!("{live_uri}&immutable=1");
    match open_usable(&live_uri, flags) {
        Ok(conn) => Ok(conn),
        Err(error) if is_cant_open(&error) && has_no_pending_wal(path) => {
            open_usable(&idle_uri, flags)
                .with_context(|| format!("opening {source} Store read-only: {}", path.display()))
        }
        Err(error) => Err(error)
            .with_context(|| format!("opening {source} Store read-only: {}", path.display())),
    }
}

fn open_usable(uri: &str, flags: OpenFlags) -> rusqlite::Result<Connection> {
    let conn = Connection::open_with_flags(uri, flags)?;
    conn.busy_timeout(Duration::from_secs(5))?;
    // SQLite may defer WAL/SHM mapping until the first statement. Force it
    // here so CANTOPEN is retried in this function, not later at BEGIN.
    conn.query_row("PRAGMA schema_version", [], |_| Ok(()))?;
    Ok(conn)
}

fn is_cant_open(error: &rusqlite::Error) -> bool {
    matches!(error.sqlite_error_code(), Some(ErrorCode::CannotOpen))
}

/// Whether immutable fallback can safely ignore the WAL index.
fn has_no_pending_wal(path: &Path) -> bool {
    let wal_path = format!("{}-wal", path.display());
    match fs::metadata(wal_path) {
        Ok(metadata) => metadata.len() == 0,
        Err(error) if error.kind() == ErrorKind::NotFound => true,
        Err(_) => false,
    }
}

/// A URI that keeps live WAL visible. `immutable=1` is appended only for an
/// idle Store that cannot map a read-only WAL index.
pub(crate) fn read_only_uri(path: &Path) -> Result<String> {
    let path = path
        .to_str()
        .with_context(|| format!("SQLite Store path is not valid UTF-8: {}", path.display()))?;
    let path = if cfg!(windows) {
        path.replace('\\', "/")
    } else {
        path.to_owned()
    };
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut uri =
        String::with_capacity("file:".len() + path.len() * 3 + "?mode=ro&readonly_shm=1".len());
    uri.push_str("file:");
    for byte in path.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~' | b'/' | b':') {
            uri.push(byte as char);
        } else {
            uri.push('%');
            uri.push(HEX[usize::from(byte >> 4)] as char);
            uri.push(HEX[usize::from(byte & 0x0f)] as char);
        }
    }
    uri.push_str("?mode=ro&readonly_shm=1");
    Ok(uri)
}

pub(crate) fn table_columns(conn: &Connection, table: &str) -> Result<Vec<String>> {
    let sql = format!("PRAGMA table_info({})", quote_identifier(table));
    let mut stmt = conn.prepare(&sql)?;
    Ok(stmt
        .query_map([], |row| row.get(1))?
        .collect::<rusqlite::Result<Vec<String>>>()?)
}

pub(crate) fn quote_identifier(value: &str) -> String {
    format!("\"{}\"", value.replace('"', "\"\""))
}

pub(crate) fn capture_value(value: ValueRef<'_>) -> CapturedValue {
    let value_type = value_type(value);
    let (value, encoding) = match value {
        ValueRef::Null => (Value::Null, None),
        ValueRef::Integer(value) => (Value::from(value), None),
        ValueRef::Real(value) => (Value::from(value), None),
        ValueRef::Text(value) | ValueRef::Blob(value) => match std::str::from_utf8(value) {
            Ok(text) => (Value::String(text.to_owned()), None),
            Err(_) => (
                Value::String(base64::engine::general_purpose::STANDARD.encode(value)),
                Some("base64"),
            ),
        },
    };
    CapturedValue {
        value,
        value_type,
        encoding,
    }
}

pub(crate) fn sqlite_i64(value: ValueRef<'_>) -> Option<i64> {
    match value {
        ValueRef::Integer(value) => Some(value),
        ValueRef::Real(value) if value.is_finite() => Some(value as i64),
        ValueRef::Text(value) => std::str::from_utf8(value).ok()?.parse().ok(),
        _ => None,
    }
}

pub(crate) fn hash_bytes(hash: &mut u64, bytes: &[u8]) {
    for byte in bytes {
        *hash ^= u64::from(*byte);
        *hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
}

pub(crate) fn hash_value(hash: &mut u64, value: ValueRef<'_>) {
    hash_bytes(hash, value_type(value).as_bytes());
    match value {
        ValueRef::Null => {}
        ValueRef::Integer(value) => hash_bytes(hash, &value.to_le_bytes()),
        ValueRef::Real(value) => hash_bytes(hash, &value.to_bits().to_le_bytes()),
        ValueRef::Text(value) | ValueRef::Blob(value) => hash_bytes(hash, value),
    }
}

pub(crate) fn value_bytes(value: ValueRef<'_>) -> Vec<u8> {
    match value {
        ValueRef::Text(value) | ValueRef::Blob(value) => value.to_vec(),
        ValueRef::Null => Vec::new(),
        ValueRef::Integer(value) => value.to_string().into_bytes(),
        ValueRef::Real(value) => value.to_string().into_bytes(),
    }
}

pub(crate) fn value_type(value: ValueRef<'_>) -> &'static str {
    match value {
        ValueRef::Text(_) => "text",
        ValueRef::Blob(_) => "blob",
        ValueRef::Null => "null",
        ValueRef::Integer(_) => "integer",
        ValueRef::Real(_) => "real",
    }
}

/// Deterministic, bounded filename stem for a native SQLite unit identity.
pub(crate) fn capture_stem(native_id: &str) -> String {
    crate::path_key::identity_component(native_id)
}

#[cfg(test)]
mod tests {
    use std::fs::remove_file;
    use std::path::PathBuf;

    use super::*;

    #[test]
    fn store_connection_is_actually_read_only() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        Connection::open(temp.path())
            .unwrap()
            .execute("CREATE TABLE data(key TEXT, value BLOB)", [])
            .unwrap();
        let conn = open_read_only(temp.path(), "Test").unwrap();
        assert!(conn.is_readonly("main").unwrap());
        assert!(
            conn.execute("INSERT INTO data VALUES ('x','y')", [])
                .is_err()
        );
    }

    #[test]
    fn read_only_uri_keeps_wal_visible_without_a_writable_shm_mapping() {
        assert_eq!(
            read_only_uri(Path::new("/tmp/SQLite State/state.sqlite")).unwrap(),
            "file:/tmp/SQLite%20State/state.sqlite?mode=ro&readonly_shm=1"
        );
    }

    fn idle_wal_store(directory: &Path) -> PathBuf {
        let path = directory.join("state.sqlite");
        let connection = Connection::open(&path).unwrap();
        connection
            .pragma_update(None, "journal_mode", "WAL")
            .unwrap();
        connection
            .execute("CREATE TABLE data(value TEXT)", [])
            .unwrap();
        connection
            .execute("INSERT INTO data VALUES ('idle')", [])
            .unwrap();
        drop(connection);
        let _ = remove_file(format!("{}-shm", path.display()));
        std::fs::write(format!("{}-wal", path.display()), []).unwrap();
        path
    }

    #[test]
    fn opens_a_wal_catalog_with_an_empty_wal_and_no_shm_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = idle_wal_store(directory.path());
        let mut connection = open_read_only(&path, "Test").unwrap();
        let tx = connection.transaction().unwrap();
        assert!(tx.is_readonly("main").unwrap());
        assert_eq!(
            tx.query_row("SELECT value FROM data", [], |row| row.get::<_, String>(0)),
            Ok("idle".into()),
        );
    }

    #[test]
    fn opens_an_idle_wal_store_even_with_a_leftover_shm_file() {
        let directory = tempfile::tempdir().unwrap();
        let path = idle_wal_store(directory.path());
        std::fs::write(format!("{}-shm", path.display()), b"not-a-wal-index").unwrap();

        let mut connection = open_read_only(&path, "Test").unwrap();
        let tx = connection.transaction().unwrap();
        assert_eq!(
            tx.query_row("SELECT value FROM data", [], |row| row.get::<_, String>(0)),
            Ok("idle".into()),
        );
    }

    #[test]
    fn live_wal_rows_stay_visible_to_a_read_only_connection() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer.execute("CREATE TABLE data(value TEXT)", []).unwrap();
        writer
            .execute("INSERT INTO data VALUES ('checkpointed')", [])
            .unwrap();
        writer
            .execute("INSERT INTO data VALUES ('still-in-wal')", [])
            .unwrap();

        let connection = open_read_only(&path, "Test").unwrap();
        let mut values = Vec::new();
        {
            let mut stmt = connection
                .prepare("SELECT value FROM data ORDER BY rowid")
                .unwrap();
            let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
            for row in rows {
                values.push(row.unwrap());
            }
        }
        assert_eq!(values, ["checkpointed", "still-in-wal"]);
        drop(writer);
    }

    #[test]
    fn idle_live_store_stays_wal_aware_after_opening() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("state.sqlite");
        let writer = Connection::open(&path).unwrap();
        writer.pragma_update(None, "journal_mode", "WAL").unwrap();
        writer.execute("CREATE TABLE data(value TEXT)", []).unwrap();
        writer
            .execute("INSERT INTO data VALUES ('old')", [])
            .unwrap();
        writer
            .pragma_update(None, "wal_checkpoint", "TRUNCATE")
            .unwrap();

        let reader = open_read_only(&path, "Test").unwrap();
        writer
            .execute("INSERT INTO data VALUES ('new')", [])
            .unwrap();

        assert_eq!(
            reader.query_row("SELECT count(*) FROM data", [], |row| row.get::<_, i64>(0)),
            Ok(2),
        );
    }

    #[test]
    fn capture_stems_are_bounded_and_stable() {
        let uuid = "11111111-1111-4111-8111-111111111111";
        assert_eq!(capture_stem(uuid), uuid);
        assert_eq!(capture_stem("draft"), "id-6472616674");
        assert_eq!(
            capture_stem(&"x".repeat(118)).len() + ".meta.json.retry".len(),
            255
        );
        let long = "x".repeat(119);
        assert_eq!(capture_stem(&long).len(), "id-sha256-".len() + 64);
        assert_eq!(capture_stem(&long), capture_stem(&long));
        assert_ne!(capture_stem(&long), capture_stem(&format!("{long}y")));
    }

    #[test]
    fn typed_values_preserve_sqlite_types_and_binary_bytes() {
        let integer = capture_value(ValueRef::Integer(42));
        assert_eq!(integer.value, Value::from(42));
        assert_eq!(integer.value_type, "integer");
        assert_eq!(integer.encoding, None);

        let blob = capture_value(ValueRef::Blob(&[0xff, 0x00]));
        assert_eq!(blob.value, Value::String("/wA=".into()));
        assert_eq!(blob.value_type, "blob");
        assert_eq!(blob.encoding, Some("base64"));

        let mut text_hash = FNV_OFFSET_BASIS;
        hash_value(&mut text_hash, ValueRef::Text(b"same"));
        let mut blob_hash = FNV_OFFSET_BASIS;
        hash_value(&mut blob_hash, ValueRef::Blob(b"same"));
        assert_ne!(text_hash, blob_hash, "SQLite type is part of identity");
    }
}
