use std::{
    path::Path,
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Utc};
use crossbeam_channel::{Receiver, RecvTimeoutError, Sender, bounded};
use rusqlite::{Connection, Transaction, params};

use crate::model::{
    ArtifactRecord, Bookmark, CollectorHealth, DiagnosticEvent, ForegroundEvent, FrameSample,
    MetricSample, ProcessEvent, ProcessSample, Record,
};

const DATABASE_QUEUE_CAPACITY: usize = 100_000;
const COMMIT_INTERVAL: Duration = Duration::from_secs(1);
const MAX_BATCH: usize = 10_000;

#[derive(Debug, Default)]
pub struct DbStats {
    pub records_written: u64,
    pub batches_committed: u64,
}

pub struct DatabaseWriter {
    pub sender: Sender<Record>,
    pub join: JoinHandle<Result<DbStats>>,
}

pub fn spawn_database_writer(
    path: &Path,
    session_id: &str,
    started_utc: DateTime<Utc>,
) -> Result<DatabaseWriter> {
    let mut connection = open_database(path)?;
    connection.execute(
        "INSERT OR REPLACE INTO sessions (session_id, started_utc, state, app_version) VALUES (?1, ?2, 'recording', ?3)",
        params![session_id, started_utc.to_rfc3339(), env!("CARGO_PKG_VERSION")],
    )?;

    let (sender, receiver) = bounded(DATABASE_QUEUE_CAPACITY);
    let join = thread::Builder::new()
        .name("database-writer".into())
        .spawn(move || writer_loop(&mut connection, receiver))
        .context("failed to start database writer")?;

    Ok(DatabaseWriter { sender, join })
}

pub fn mark_session_finished(
    path: &Path,
    session_id: &str,
    stopped_utc: DateTime<Utc>,
    state: &str,
) -> Result<()> {
    let connection = open_database(path)?;
    connection.execute(
        "UPDATE sessions SET stopped_utc = ?1, state = ?2 WHERE session_id = ?3",
        params![stopped_utc.to_rfc3339(), state, session_id],
    )?;
    connection.execute_batch("PRAGMA wal_checkpoint(TRUNCATE);")?;
    Ok(())
}

pub fn append_recovery_events(
    path: &Path,
    events: Vec<DiagnosticEvent>,
    warnings: Vec<String>,
    health_time: crate::model::SampleTime,
) -> Result<()> {
    let mut connection = open_database(path)?;
    let transaction = connection.transaction()?;
    for event in events {
        if let Some(record_id) = event.record_id {
            let duplicate: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM diagnostic_events WHERE channel = ?1 AND record_id = ?2)",
                params![&event.channel, to_sql_u64(record_id)],
                |row| row.get(0),
            )?;
            if duplicate {
                continue;
            }
        }
        insert_event(&transaction, event)?;
    }
    for warning in warnings {
        insert_health(
            &transaction,
            CollectorHealth {
                time: health_time.clone(),
                collector: "windows_event_log_recovery".into(),
                status: "degraded".into(),
                detail: warning,
            },
        )?;
    }
    transaction.commit()?;
    Ok(())
}

pub fn open_database(path: &Path) -> Result<Connection> {
    let connection = Connection::open(path)
        .with_context(|| format!("failed to open database {}", path.display()))?;
    connection.busy_timeout(Duration::from_secs(5))?;
    connection.execute_batch(
        "PRAGMA journal_mode=WAL;
         PRAGMA synchronous=FULL;
         PRAGMA foreign_keys=ON;
         PRAGMA temp_store=MEMORY;",
    )?;
    create_schema(&connection)?;
    Ok(connection)
}

fn create_schema(connection: &Connection) -> Result<()> {
    connection.execute_batch(
        "CREATE TABLE IF NOT EXISTS sessions (
            session_id TEXT PRIMARY KEY,
            started_utc TEXT NOT NULL,
            stopped_utc TEXT,
            state TEXT NOT NULL,
            app_version TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS clock_calibrations (
            id INTEGER PRIMARY KEY,
            utc TEXT NOT NULL,
            monotonic_ns INTEGER NOT NULL
        );
        CREATE TABLE IF NOT EXISTS metric_samples (
            id INTEGER PRIMARY KEY,
            utc TEXT NOT NULL,
            monotonic_ns INTEGER NOT NULL,
            source TEXT NOT NULL,
            device TEXT,
            source_timestamp_ms INTEGER,
            metric TEXT NOT NULL,
            value REAL NOT NULL,
            unit TEXT NOT NULL,
            quality TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS metric_time_idx ON metric_samples(monotonic_ns);
        CREATE INDEX IF NOT EXISTS metric_name_idx ON metric_samples(source, metric);
        CREATE TABLE IF NOT EXISTS process_samples (
            id INTEGER PRIMARY KEY,
            utc TEXT NOT NULL,
            monotonic_ns INTEGER NOT NULL,
            pid INTEGER NOT NULL,
            started_unix_s INTEGER NOT NULL,
            name TEXT NOT NULL,
            executable TEXT,
            cpu_percent REAL NOT NULL,
            memory_bytes INTEGER NOT NULL,
            virtual_memory_bytes INTEGER NOT NULL,
            disk_read_bytes INTEGER NOT NULL,
            disk_write_bytes INTEGER NOT NULL,
            is_foreground INTEGER NOT NULL
        );
        CREATE INDEX IF NOT EXISTS process_sample_time_idx ON process_samples(monotonic_ns);
        CREATE TABLE IF NOT EXISTS process_events (
            id INTEGER PRIMARY KEY,
            utc TEXT NOT NULL,
            monotonic_ns INTEGER NOT NULL,
            kind TEXT NOT NULL,
            pid INTEGER NOT NULL,
            started_unix_s INTEGER NOT NULL,
            parent_pid INTEGER,
            name TEXT NOT NULL,
            executable TEXT,
            exit_code INTEGER,
            detail TEXT
        );
        CREATE TABLE IF NOT EXISTS foreground_events (
            id INTEGER PRIMARY KEY,
            utc TEXT NOT NULL,
            monotonic_ns INTEGER NOT NULL,
            pid INTEGER NOT NULL,
            executable TEXT,
            window_title TEXT
        );
        CREATE TABLE IF NOT EXISTS diagnostic_events (
            id INTEGER PRIMARY KEY,
            utc TEXT NOT NULL,
            monotonic_ns INTEGER NOT NULL,
            channel TEXT NOT NULL,
            provider TEXT,
            event_id INTEGER,
            level INTEGER,
            record_id INTEGER,
            message TEXT,
            raw_xml TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS event_time_idx ON diagnostic_events(monotonic_ns);
        CREATE TABLE IF NOT EXISTS frame_samples (
            id INTEGER PRIMARY KEY,
            utc TEXT NOT NULL,
            monotonic_ns INTEGER NOT NULL,
            source_qpc_seconds REAL,
            pid INTEGER NOT NULL,
            application TEXT,
            swap_chain TEXT,
            runtime TEXT,
            sync_interval INTEGER,
            present_flags INTEGER,
            dropped INTEGER,
            frame_time_ms REAL,
            fps REAL,
            present_api_ms REAL,
            render_complete_ms REAL,
            displayed_ms REAL,
            display_change_ms REAL,
            flip_delay_ms REAL,
            render_start_ms REAL,
            gpu_active_ms REAL,
            allows_tearing INTEGER,
            present_mode TEXT,
            quality TEXT NOT NULL
        );
        CREATE INDEX IF NOT EXISTS frame_time_idx ON frame_samples(monotonic_ns);
        CREATE TABLE IF NOT EXISTS bookmarks (
            id INTEGER PRIMARY KEY,
            utc TEXT NOT NULL,
            monotonic_ns INTEGER NOT NULL,
            trigger TEXT NOT NULL,
            detail TEXT NOT NULL,
            related_pid INTEGER
        );
        CREATE TABLE IF NOT EXISTS collector_health (
            id INTEGER PRIMARY KEY,
            utc TEXT NOT NULL,
            monotonic_ns INTEGER NOT NULL,
            collector TEXT NOT NULL,
            status TEXT NOT NULL,
            detail TEXT NOT NULL
        );
        CREATE TABLE IF NOT EXISTS artifacts (
            id INTEGER PRIMARY KEY,
            discovered_utc TEXT NOT NULL,
            monotonic_ns INTEGER NOT NULL DEFAULT 0,
            original_path TEXT NOT NULL,
            copied_path TEXT,
            size_bytes INTEGER,
            modified_utc TEXT,
            sha256 TEXT,
            status TEXT NOT NULL
        );",
    )?;
    for (name, data_type) in [
        ("source_qpc_seconds", "REAL"),
        ("runtime", "TEXT"),
        ("sync_interval", "INTEGER"),
        ("present_flags", "INTEGER"),
        ("dropped", "INTEGER"),
        ("present_api_ms", "REAL"),
        ("render_complete_ms", "REAL"),
        ("displayed_ms", "REAL"),
        ("display_change_ms", "REAL"),
        ("flip_delay_ms", "REAL"),
        ("render_start_ms", "REAL"),
        ("gpu_active_ms", "REAL"),
        ("allows_tearing", "INTEGER"),
    ] {
        ensure_column(connection, "frame_samples", name, data_type)?;
    }
    ensure_column(
        connection,
        "artifacts",
        "monotonic_ns",
        "INTEGER NOT NULL DEFAULT 0",
    )?;
    Ok(())
}

fn ensure_column(
    connection: &Connection,
    table: &str,
    column: &str,
    data_type: &str,
) -> Result<()> {
    let mut statement = connection.prepare(&format!("PRAGMA table_info({table})"))?;
    let columns = statement.query_map([], |row| row.get::<_, String>(1))?;
    for existing in columns {
        if existing? == column {
            return Ok(());
        }
    }
    connection.execute_batch(&format!(
        "ALTER TABLE {table} ADD COLUMN {column} {data_type}"
    ))?;
    Ok(())
}

fn writer_loop(connection: &mut Connection, receiver: Receiver<Record>) -> Result<DbStats> {
    let mut stats = DbStats::default();
    let mut batch = Vec::with_capacity(2_048);
    let mut last_commit = Instant::now();

    loop {
        match receiver.recv_timeout(Duration::from_millis(100)) {
            Ok(record) => batch.push(record),
            Err(RecvTimeoutError::Timeout) => {}
            Err(RecvTimeoutError::Disconnected) => {
                flush_batch(connection, &mut batch, &mut stats)?;
                break;
            }
        }

        while batch.len() < MAX_BATCH {
            match receiver.try_recv() {
                Ok(record) => batch.push(record),
                Err(_) => break,
            }
        }

        if !batch.is_empty()
            && (last_commit.elapsed() >= COMMIT_INTERVAL || batch.len() >= MAX_BATCH)
        {
            flush_batch(connection, &mut batch, &mut stats)?;
            last_commit = Instant::now();
        }
    }

    Ok(stats)
}

fn flush_batch(
    connection: &mut Connection,
    batch: &mut Vec<Record>,
    stats: &mut DbStats,
) -> Result<()> {
    if batch.is_empty() {
        return Ok(());
    }

    let transaction = connection.transaction()?;
    for record in batch.drain(..) {
        insert_record(&transaction, record)?;
        stats.records_written += 1;
    }
    transaction.commit()?;
    stats.batches_committed += 1;
    Ok(())
}

fn insert_record(transaction: &Transaction<'_>, record: Record) -> Result<()> {
    match record {
        Record::Metric(sample) => insert_metric(transaction, sample)?,
        Record::ProcessSample(sample) => insert_process_sample(transaction, sample)?,
        Record::ProcessEvent(event) => insert_process_event(transaction, event)?,
        Record::Foreground(event) => insert_foreground(transaction, event)?,
        Record::Event(event) => insert_event(transaction, event)?,
        Record::Frame(sample) => insert_frame(transaction, sample)?,
        Record::Bookmark(bookmark) => insert_bookmark(transaction, bookmark)?,
        Record::Health(health) => insert_health(transaction, health)?,
        Record::Artifact(artifact) => insert_artifact(transaction, artifact)?,
        Record::ClockCalibration(time) => {
            transaction.execute(
                "INSERT INTO clock_calibrations (utc, monotonic_ns) VALUES (?1, ?2)",
                params![time.utc.to_rfc3339(), to_sql_u64(time.monotonic_ns)],
            )?;
        }
    }
    Ok(())
}

fn insert_metric(transaction: &Transaction<'_>, sample: MetricSample) -> Result<()> {
    transaction.execute(
        "INSERT INTO metric_samples (utc, monotonic_ns, source, device, source_timestamp_ms, metric, value, unit, quality)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            sample.time.utc.to_rfc3339(),
            to_sql_u64(sample.time.monotonic_ns),
            sample.source,
            sample.device,
            sample.source_timestamp_ms,
            sample.metric,
            sample.value,
            sample.unit,
            sample.quality,
        ],
    )?;
    Ok(())
}

fn insert_process_sample(transaction: &Transaction<'_>, sample: ProcessSample) -> Result<()> {
    transaction.execute(
        "INSERT INTO process_samples
         (utc, monotonic_ns, pid, started_unix_s, name, executable, cpu_percent, memory_bytes,
          virtual_memory_bytes, disk_read_bytes, disk_write_bytes, is_foreground)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
        params![
            sample.time.utc.to_rfc3339(),
            to_sql_u64(sample.time.monotonic_ns),
            sample.pid,
            to_sql_u64(sample.started_unix_s),
            sample.name,
            sample.executable,
            sample.cpu_percent,
            to_sql_u64(sample.memory_bytes),
            to_sql_u64(sample.virtual_memory_bytes),
            to_sql_u64(sample.disk_read_bytes),
            to_sql_u64(sample.disk_write_bytes),
            sample.is_foreground,
        ],
    )?;
    Ok(())
}

fn insert_process_event(transaction: &Transaction<'_>, event: ProcessEvent) -> Result<()> {
    transaction.execute(
        "INSERT INTO process_events
         (utc, monotonic_ns, kind, pid, started_unix_s, parent_pid, name, executable, exit_code, detail)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
        params![
            event.time.utc.to_rfc3339(),
            to_sql_u64(event.time.monotonic_ns),
            event.kind,
            event.pid,
            to_sql_u64(event.started_unix_s),
            event.parent_pid,
            event.name,
            event.executable,
            event.exit_code,
            event.detail,
        ],
    )?;
    Ok(())
}

fn insert_foreground(transaction: &Transaction<'_>, event: ForegroundEvent) -> Result<()> {
    transaction.execute(
        "INSERT INTO foreground_events
         (utc, monotonic_ns, pid, executable, window_title) VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            event.time.utc.to_rfc3339(),
            to_sql_u64(event.time.monotonic_ns),
            event.pid,
            event.executable,
            event.window_title,
        ],
    )?;
    Ok(())
}

fn insert_event(transaction: &Transaction<'_>, event: DiagnosticEvent) -> Result<()> {
    transaction.execute(
        "INSERT INTO diagnostic_events
         (utc, monotonic_ns, channel, provider, event_id, level, record_id, message, raw_xml)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
        params![
            event.time.utc.to_rfc3339(),
            to_sql_u64(event.time.monotonic_ns),
            event.channel,
            event.provider,
            event.event_id,
            event.level,
            event.record_id.map(to_sql_u64),
            event.message,
            event.raw_xml,
        ],
    )?;
    Ok(())
}

fn insert_frame(transaction: &Transaction<'_>, sample: FrameSample) -> Result<()> {
    transaction.execute(
        "INSERT INTO frame_samples
         (utc, monotonic_ns, source_qpc_seconds, pid, application, swap_chain, runtime,
          sync_interval, present_flags, dropped, frame_time_ms, fps, present_api_ms,
          render_complete_ms, displayed_ms, display_change_ms, flip_delay_ms, render_start_ms,
          gpu_active_ms, allows_tearing, present_mode, quality)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                 ?16, ?17, ?18, ?19, ?20, ?21, ?22)",
        params![
            sample.time.utc.to_rfc3339(),
            to_sql_u64(sample.time.monotonic_ns),
            sample.source_qpc_seconds,
            sample.pid,
            sample.application,
            sample.swap_chain,
            sample.runtime,
            sample.sync_interval,
            sample.present_flags,
            sample.dropped,
            sample.frame_time_ms,
            sample.fps,
            sample.present_api_ms,
            sample.render_complete_ms,
            sample.displayed_ms,
            sample.display_change_ms,
            sample.flip_delay_ms,
            sample.render_start_ms,
            sample.gpu_active_ms,
            sample.allows_tearing,
            sample.present_mode,
            sample.quality,
        ],
    )?;
    Ok(())
}

fn insert_bookmark(transaction: &Transaction<'_>, bookmark: Bookmark) -> Result<()> {
    transaction.execute(
        "INSERT INTO bookmarks (utc, monotonic_ns, trigger, detail, related_pid)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            bookmark.time.utc.to_rfc3339(),
            to_sql_u64(bookmark.time.monotonic_ns),
            bookmark.trigger,
            bookmark.detail,
            bookmark.related_pid,
        ],
    )?;
    Ok(())
}

fn insert_health(transaction: &Transaction<'_>, health: CollectorHealth) -> Result<()> {
    transaction.execute(
        "INSERT INTO collector_health (utc, monotonic_ns, collector, status, detail)
         VALUES (?1, ?2, ?3, ?4, ?5)",
        params![
            health.time.utc.to_rfc3339(),
            to_sql_u64(health.time.monotonic_ns),
            health.collector,
            health.status,
            health.detail,
        ],
    )?;
    Ok(())
}

fn insert_artifact(transaction: &Transaction<'_>, artifact: ArtifactRecord) -> Result<()> {
    transaction.execute(
        "INSERT INTO artifacts
         (discovered_utc, monotonic_ns, original_path, copied_path, size_bytes, modified_utc,
          sha256, status)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
        params![
            artifact.time.utc.to_rfc3339(),
            to_sql_u64(artifact.time.monotonic_ns),
            artifact.original_path,
            artifact.copied_path,
            artifact.size_bytes.map(to_sql_u64),
            artifact.modified_utc.map(|value| value.to_rfc3339()),
            artifact.sha256,
            artifact.status,
        ],
    )?;
    Ok(())
}

fn to_sql_u64(value: u64) -> i64 {
    value.min(i64::MAX as u64) as i64
}

#[cfg(test)]
mod tests {
    use chrono::Utc;
    use tempfile::tempdir;

    use super::*;
    use crate::model::{Bookmark, MetricSample, Record, SampleTime};

    #[test]
    fn writer_persists_records_and_finishes_session() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("session.sqlite");
        let writer = spawn_database_writer(&path, "test", Utc::now()).unwrap();
        writer
            .sender
            .send(Record::Metric(MetricSample {
                time: SampleTime {
                    utc: Utc::now(),
                    monotonic_ns: 42,
                },
                source: "test".into(),
                device: None,
                source_timestamp_ms: None,
                metric: "value".into(),
                value: 7.0,
                unit: "count".into(),
                quality: "ok".into(),
            }))
            .unwrap();
        writer
            .sender
            .send(Record::Bookmark(Bookmark {
                time: SampleTime {
                    utc: Utc::now(),
                    monotonic_ns: 42,
                },
                trigger: "manual_marker".into(),
                detail: "test marker".into(),
                related_pid: None,
            }))
            .unwrap();
        drop(writer.sender);
        writer.join.join().unwrap().unwrap();
        mark_session_finished(&path, "test", Utc::now(), "complete").unwrap();

        let connection = open_database(&path).unwrap();
        let count: i64 = connection
            .query_row("SELECT COUNT(*) FROM metric_samples", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        drop(connection);
        crate::export::generate_all(dir.path()).unwrap();
        assert!(dir.path().join("report.html").is_file());
        assert!(dir.path().join("findings.json").is_file());
        assert!(
            dir.path()
                .join("crash-windows")
                .join("0001_manual_marker")
                .join("bookmark.json")
                .is_file()
        );
    }
}
