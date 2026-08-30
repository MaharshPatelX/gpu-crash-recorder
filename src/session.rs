use std::{
    fs,
    path::{Path, PathBuf},
};

use anyhow::{Context, Result};
use chrono::{Local, Utc};

use crate::{
    artifacts::{ArtifactBaseline, ArtifactSignal},
    collector::{CollectorGroup, event_bookmark},
    config::AppConfig,
    database::{
        DatabaseWriter, append_recovery_events, mark_session_finished, open_database,
        spawn_database_writer,
    },
    events::{collect_relevant_events, collect_relevant_events_since, is_live_kernel_event_code},
    export,
    model::{Bookmark, CollectorHealth, Record, SessionManifest},
    system_info,
    timestamp::SessionClock,
};

pub struct RecordingSession {
    id: String,
    started_utc: chrono::DateTime<Utc>,
    active_dir: PathBuf,
    database_path: PathBuf,
    clock: SessionClock,
    database: Option<DatabaseWriter>,
    collectors: Option<CollectorGroup>,
    artifact_baseline: Option<ArtifactBaseline>,
}

impl RecordingSession {
    pub fn start(config: AppConfig) -> Result<Self> {
        let local_now = Local::now();
        let started_utc = Utc::now();
        let id = started_utc.format("%Y%m%dT%H%M%S%.3fZ").to_string();
        let date_dir = config
            .sessions_root
            .join(local_now.format("%Y-%m-%d").to_string());
        fs::create_dir_all(&date_dir)
            .with_context(|| format!("failed to create {}", date_dir.display()))?;
        let active_dir = unique_session_path(
            &date_dir,
            &format!("SESSION_{}.recording", local_now.format("%H-%M-%S")),
        );
        fs::create_dir_all(&active_dir)
            .with_context(|| format!("failed to create {}", active_dir.display()))?;
        for child in ["crash-windows", "dumps", "crash-artifacts", "logs"] {
            fs::create_dir_all(active_dir.join(child))?;
        }

        let (artifact_baseline, artifact_baseline_error) = match ArtifactBaseline::capture(
            &config.artifact_paths,
            &active_dir.join("logs").join("artifact-baseline.json"),
        ) {
            Ok(baseline) => (Some(baseline), None),
            Err(error) => (None, Some(format!("{error:#}"))),
        };

        let manifest = SessionManifest {
            schema_version: 1,
            app_version: env!("CARGO_PKG_VERSION").into(),
            session_id: id.clone(),
            started_utc,
            stopped_utc: None,
            state: "recording".into(),
            recovered: false,
            output_directory: active_dir.to_string_lossy().into_owned(),
        };
        write_manifest(&active_dir, &manifest)?;
        system_info::capture(&active_dir.join("system-info-start.json"))?;

        let database_path = active_dir.join("session.sqlite");
        let database = spawn_database_writer(&database_path, &id, started_utc)?;
        let clock = SessionClock::new();
        database
            .sender
            .send(Record::ClockCalibration(clock.now()))
            .context("database writer stopped during session startup")?;
        if let Some(baseline) = artifact_baseline.as_ref() {
            let _ = database.sender.send(Record::Health(CollectorHealth {
                time: clock.now(),
                collector: "crash_artifacts".into(),
                status: "running".into(),
                detail: format!(
                    "Watching {} artifact roots; {} files baselined",
                    baseline.watched_root_count(),
                    baseline.baseline_file_count()
                ),
            }));
        } else if let Some(error) = artifact_baseline_error {
            let _ = database.sender.send(Record::Health(CollectorHealth {
                time: clock.now(),
                collector: "crash_artifacts".into(),
                status: "failed".into(),
                detail: format!("Could not capture artifact baseline: {error}"),
            }));
        }
        let collectors =
            CollectorGroup::start(config.clone(), clock.clone(), database.sender.clone())?;

        Ok(Self {
            id,
            started_utc,
            active_dir,
            database_path,
            clock,
            database: Some(database),
            collectors: Some(collectors),
            artifact_baseline,
        })
    }

    pub fn directory(&self) -> &Path {
        &self.active_dir
    }

    pub fn started_utc(&self) -> chrono::DateTime<Utc> {
        self.started_utc
    }

    pub fn add_marker(&self, detail: impl Into<String>) -> Result<()> {
        let database = self
            .database
            .as_ref()
            .context("session database is closed")?;
        database
            .sender
            .send(Record::Bookmark(Bookmark {
                time: self.clock.now(),
                trigger: "manual_marker".into(),
                detail: detail.into(),
                related_pid: None,
            }))
            .context("database writer stopped")
    }

    pub fn stop(mut self) -> Result<PathBuf> {
        if let Some(database) = self.database.as_ref() {
            let _ = database.sender.send(Record::Bookmark(Bookmark {
                time: self.clock.now(),
                trigger: "manual_stop".into(),
                detail: "Recording stopped manually by the user".into(),
                related_pid: None,
            }));
        }

        if let Some(collectors) = self.collectors.take() {
            let errors = collectors.stop();
            if let Some(database) = self.database.as_ref() {
                for error in errors {
                    let _ = database.sender.send(Record::Health(CollectorHealth {
                        time: self.clock.now(),
                        collector: "collector_group".into(),
                        status: "failed".into(),
                        detail: format!("{error:#}"),
                    }));
                }
            }
        }
        let final_event_query_from = Utc::now();

        system_info::capture(&self.active_dir.join("system-info-stop.json"))?;
        combine_system_snapshots(&self.active_dir)?;

        let mut artifact_signals: Vec<ArtifactSignal> = Vec::new();
        if let Some(baseline) = self.artifact_baseline.take() {
            match baseline.collect(&self.active_dir, &self.clock) {
                Ok(collection) => {
                    let artifact_count = collection.records.len();
                    let warning_count = collection.warnings.len();
                    artifact_signals = collection.signals;
                    if let Some(database) = self.database.as_ref() {
                        for artifact in collection.records {
                            let _ = database.sender.send(Record::Artifact(artifact));
                        }
                        for warning in collection.warnings {
                            let _ = database.sender.send(Record::Health(CollectorHealth {
                                time: self.clock.now(),
                                collector: "crash_artifacts".into(),
                                status: "degraded".into(),
                                detail: warning,
                            }));
                        }
                        let _ = database.sender.send(Record::Health(CollectorHealth {
                            time: self.clock.now(),
                            collector: "crash_artifacts".into(),
                            status: if warning_count == 0 {
                                "stopped".into()
                            } else {
                                "degraded".into()
                            },
                            detail: format!(
                                "Artifact collection finished: {artifact_count} new/modified files, {warning_count} warnings"
                            ),
                        }));
                    }
                }
                Err(error) => {
                    if let Some(database) = self.database.as_ref() {
                        let _ = database.sender.send(Record::Health(CollectorHealth {
                            time: self.clock.now(),
                            collector: "crash_artifacts".into(),
                            status: "failed".into(),
                            detail: format!("Artifact collection failed: {error:#}"),
                        }));
                    }
                }
            }
        }

        if let Some(database) = self.database.as_ref() {
            let final_events = collect_relevant_events_since(
                final_event_query_from,
                self.started_utc,
                &self.clock,
            );
            let mut live_141_event_times = Vec::new();
            for warning in final_events.warnings {
                let _ = database.sender.send(Record::Health(CollectorHealth {
                    time: self.clock.now(),
                    collector: "windows_event_log_final_backfill".into(),
                    status: "degraded".into(),
                    detail: warning,
                }));
            }
            for event in final_events.events {
                if is_live_kernel_event_code(&event, 141) {
                    live_141_event_times.push(event.time.utc);
                }
                if let Some((trigger, detail)) = event_bookmark(&event) {
                    let _ = database.sender.send(Record::Bookmark(Bookmark {
                        time: event.time.clone(),
                        trigger,
                        detail,
                        related_pid: None,
                    }));
                }
                let _ = database.sender.send(Record::Event(event));
            }
            for signal in artifact_signals {
                let duplicate_event = signal.trigger == "live_kernel_event_141"
                    && live_141_event_times.iter().any(|event_time| {
                        event_time
                            .signed_duration_since(signal.utc)
                            .num_seconds()
                            .unsigned_abs()
                            <= 120
                    });
                if duplicate_event {
                    continue;
                }
                let _ = database.sender.send(Record::Bookmark(Bookmark {
                    time: self.clock.from_utc(signal.utc),
                    trigger: signal.trigger,
                    detail: signal.detail,
                    related_pid: None,
                }));
            }
        }

        if let Some(database) = self.database.as_ref() {
            let _ = database
                .sender
                .send(Record::ClockCalibration(self.clock.now()));
        }

        let database = self.database.take().context("session database is closed")?;
        drop(database.sender);
        let stats = database
            .join
            .join()
            .map_err(|_| anyhow::anyhow!("database writer panicked"))??;

        let stopped_utc = Utc::now();
        mark_session_finished(&self.database_path, &self.id, stopped_utc, "complete")?;

        let mut manifest = SessionManifest {
            schema_version: 1,
            app_version: env!("CARGO_PKG_VERSION").into(),
            session_id: self.id.clone(),
            started_utc: self.started_utc,
            stopped_utc: Some(stopped_utc),
            state: "complete".into(),
            recovered: false,
            output_directory: self.active_dir.to_string_lossy().into_owned(),
        };
        write_manifest(&self.active_dir, &manifest)?;
        fs::write(
            self.active_dir.join("logs").join("finalization.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "records_written": stats.records_written,
                "batches_committed": stats.batches_committed,
                "finalized_utc": stopped_utc,
            }))?,
        )?;
        export::generate_all(&self.active_dir)?;

        let final_dir = final_path_for(&self.active_dir);
        fs::rename(&self.active_dir, &final_dir).with_context(|| {
            format!(
                "failed to finalize session directory {} to {}",
                self.active_dir.display(),
                final_dir.display()
            )
        })?;
        manifest.output_directory = final_dir.to_string_lossy().into_owned();
        write_manifest(&final_dir, &manifest)?;
        Ok(final_dir)
    }
}

pub fn recover_incomplete_sessions(root: &Path) -> Result<Vec<PathBuf>> {
    if !root.exists() {
        return Ok(Vec::new());
    }
    let mut recovered = Vec::new();
    for date_entry in fs::read_dir(root)? {
        let date_entry = date_entry?;
        if !date_entry.file_type()?.is_dir() {
            continue;
        }
        for session_entry in fs::read_dir(date_entry.path())? {
            let session_entry = session_entry?;
            if !session_entry.file_type()?.is_dir()
                || session_entry.path().extension().and_then(|v| v.to_str()) != Some("recording")
            {
                continue;
            }
            let active = session_entry.path();
            let database_path = active.join("session.sqlite");
            if database_path.exists() {
                let connection = open_database(&database_path)?;
                let (session_id, started_text): (String, String) = connection.query_row(
                    "SELECT session_id, started_utc FROM sessions LIMIT 1",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )?;
                let now = Utc::now();
                connection.execute(
                    "INSERT INTO bookmarks (utc, monotonic_ns, trigger, detail, related_pid)
                     VALUES (?1, 0, 'recovery', 'Previous recording did not stop cleanly', NULL)",
                    [now.to_rfc3339()],
                )?;
                drop(connection);
                if let Ok(started) = chrono::DateTime::parse_from_rfc3339(&started_text) {
                    let clock = SessionClock::new();
                    let collection = collect_relevant_events(started.with_timezone(&Utc), &clock);
                    append_recovery_events(
                        &database_path,
                        collection.events,
                        collection.warnings,
                        clock.now(),
                    )?;
                }
                mark_session_finished(&database_path, &session_id, now, "recovered")?;
                let _ = export::generate_all(&active);
            }
            let mut final_path = final_path_for(&active);
            let recovered_name = format!(
                "{}_RECOVERED",
                final_path
                    .file_name()
                    .and_then(|value| value.to_str())
                    .unwrap_or("SESSION")
            );
            final_path.set_file_name(recovered_name);
            if final_path.exists() {
                final_path = unique_session_path(
                    final_path.parent().unwrap_or(root),
                    final_path
                        .file_name()
                        .and_then(|v| v.to_str())
                        .unwrap_or("SESSION_RECOVERED"),
                );
            }
            fs::rename(&active, &final_path)?;
            recovered.push(final_path);
        }
    }
    Ok(recovered)
}

fn combine_system_snapshots(session_dir: &Path) -> Result<()> {
    let start: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        session_dir.join("system-info-start.json"),
    )?)?;
    let stop: serde_json::Value = serde_json::from_str(&fs::read_to_string(
        session_dir.join("system-info-stop.json"),
    )?)?;
    fs::write(
        session_dir.join("system-info.json"),
        serde_json::to_string_pretty(&serde_json::json!({ "start": start, "stop": stop }))?,
    )?;
    Ok(())
}

fn write_manifest(directory: &Path, manifest: &SessionManifest) -> Result<()> {
    fs::write(
        directory.join("manifest.json"),
        serde_json::to_string_pretty(manifest)?,
    )
    .with_context(|| format!("failed to write manifest in {}", directory.display()))
}

fn unique_session_path(parent: &Path, requested_name: &str) -> PathBuf {
    let requested = parent.join(requested_name);
    if !requested.exists() {
        return requested;
    }
    for index in 2..10_000 {
        let candidate = parent.join(format!("{requested_name}_{index}"));
        if !candidate.exists() {
            return candidate;
        }
    }
    parent.join(format!(
        "{requested_name}_{}",
        Utc::now().timestamp_millis()
    ))
}

fn final_path_for(active: &Path) -> PathBuf {
    let name = active
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("SESSION.recording");
    let final_name = name.strip_suffix(".recording").unwrap_or(name);
    active.with_file_name(final_name)
}
