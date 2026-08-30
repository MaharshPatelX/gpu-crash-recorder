use std::{
    fs::File,
    io::{BufWriter, Write},
    path::Path,
};

use anyhow::{Context, Result};
use rusqlite::{Connection, params};
use serde::Serialize;

use crate::database::open_database;

#[derive(Debug, Serialize)]
struct Summary {
    session_id: String,
    started_utc: String,
    stopped_utc: Option<String>,
    state: String,
    metric_samples: i64,
    process_samples: i64,
    process_events: i64,
    frame_samples: i64,
    diagnostic_events: i64,
    bookmarks: i64,
    artifacts: i64,
    collector_issues: i64,
}

#[derive(Debug, Clone, Serialize)]
struct Finding {
    category: String,
    confidence: String,
    explanation: String,
    evidence: Vec<String>,
}

#[derive(Serialize)]
struct FrameExportRow {
    utc: String,
    monotonic_ns: i64,
    source_qpc_seconds: Option<f64>,
    pid: i64,
    application: Option<String>,
    swap_chain: Option<String>,
    runtime: Option<String>,
    sync_interval: Option<i64>,
    present_flags: Option<i64>,
    dropped: Option<bool>,
    frame_time_ms: Option<f64>,
    fps: Option<f64>,
    present_api_ms: Option<f64>,
    render_complete_ms: Option<f64>,
    displayed_ms: Option<f64>,
    display_change_ms: Option<f64>,
    flip_delay_ms: Option<f64>,
    render_start_ms: Option<f64>,
    gpu_active_ms: Option<f64>,
    allows_tearing: Option<bool>,
    present_mode: Option<String>,
    quality: String,
}

pub fn generate_all(session_dir: &Path) -> Result<()> {
    let database_path = session_dir.join("session.sqlite");
    let connection = open_database(&database_path)?;
    export_telemetry(&connection, &session_dir.join("telemetry.csv"))?;
    export_processes(&connection, &session_dir.join("processes.csv"))?;
    export_process_events_range(&connection, &session_dir.join("process-events.jsonl"), None)?;
    export_frames(&connection, &session_dir.join("frames.csv"))?;
    export_events(&connection, &session_dir.join("windows-events.jsonl"))?;
    export_crash_windows(&connection, session_dir)?;
    let summary = build_summary(&connection)?;
    let findings = build_findings(&connection)?;
    std::fs::write(
        session_dir.join("summary.json"),
        serde_json::to_string_pretty(&summary)?,
    )?;
    std::fs::write(
        session_dir.join("findings.json"),
        serde_json::to_string_pretty(&findings)?,
    )?;
    export_html(
        &connection,
        &summary,
        &findings,
        &session_dir.join("report.html"),
    )?;
    Ok(())
}

fn export_telemetry(connection: &Connection, path: &Path) -> Result<()> {
    export_telemetry_range(connection, path, None)
}

fn export_telemetry_range(
    connection: &Connection,
    path: &Path,
    range: Option<(i64, i64)>,
) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    writer.write_record([
        "utc",
        "monotonic_ns",
        "source",
        "device",
        "source_timestamp_ms",
        "metric",
        "value",
        "unit",
        "quality",
    ])?;
    let mut statement = connection.prepare(
        "SELECT utc, monotonic_ns, source, device, source_timestamp_ms, metric, value, unit, quality
         FROM metric_samples
         WHERE (?1 IS NULL OR monotonic_ns BETWEEN ?1 AND ?2)
         ORDER BY monotonic_ns, id",
    )?;
    let rows = statement.query_map(
        params![range.map(|value| value.0), range.map(|value| value.1)],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<i64>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, f64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        },
    )?;
    for row in rows {
        let (utc, monotonic, source, device, source_timestamp, metric, value, unit, quality) = row?;
        writer.serialize((
            utc,
            monotonic,
            source,
            device,
            source_timestamp,
            metric,
            value,
            unit,
            quality,
        ))?;
    }
    writer.flush()?;
    Ok(())
}

fn export_processes(connection: &Connection, path: &Path) -> Result<()> {
    export_processes_range(connection, path, None)
}

fn export_processes_range(
    connection: &Connection,
    path: &Path,
    range: Option<(i64, i64)>,
) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    writer.write_record([
        "utc",
        "monotonic_ns",
        "pid",
        "process_started_unix_s",
        "name",
        "executable",
        "cpu_percent",
        "memory_bytes",
        "virtual_memory_bytes",
        "disk_read_bytes",
        "disk_write_bytes",
        "is_foreground",
    ])?;
    let mut statement = connection.prepare(
        "SELECT utc, monotonic_ns, pid, started_unix_s, name, executable, cpu_percent,
                memory_bytes, virtual_memory_bytes, disk_read_bytes, disk_write_bytes, is_foreground
         FROM process_samples
         WHERE (?1 IS NULL OR monotonic_ns BETWEEN ?1 AND ?2)
         ORDER BY monotonic_ns, id",
    )?;
    let rows = statement.query_map(
        params![range.map(|value| value.0), range.map(|value| value.1)],
        |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, f64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
                row.get::<_, i64>(9)?,
                row.get::<_, i64>(10)?,
                row.get::<_, bool>(11)?,
            ))
        },
    )?;
    for row in rows {
        writer.serialize(row?)?;
    }
    writer.flush()?;
    Ok(())
}

fn export_frames(connection: &Connection, path: &Path) -> Result<()> {
    export_frames_range(connection, path, None)
}

fn export_frames_range(
    connection: &Connection,
    path: &Path,
    range: Option<(i64, i64)>,
) -> Result<()> {
    let mut writer = csv::Writer::from_path(path)?;
    let mut statement = connection.prepare(
        "SELECT utc, monotonic_ns, source_qpc_seconds, pid, application, swap_chain, runtime,
                sync_interval, present_flags, dropped, frame_time_ms, fps, present_api_ms,
                render_complete_ms, displayed_ms, display_change_ms, flip_delay_ms,
                render_start_ms, gpu_active_ms, allows_tearing, present_mode, quality
         FROM frame_samples
         WHERE (?1 IS NULL OR monotonic_ns BETWEEN ?1 AND ?2)
         ORDER BY monotonic_ns, id",
    )?;
    let rows = statement.query_map(
        params![range.map(|value| value.0), range.map(|value| value.1)],
        |row| {
            Ok(FrameExportRow {
                utc: row.get(0)?,
                monotonic_ns: row.get(1)?,
                source_qpc_seconds: row.get(2)?,
                pid: row.get(3)?,
                application: row.get(4)?,
                swap_chain: row.get(5)?,
                runtime: row.get(6)?,
                sync_interval: row.get(7)?,
                present_flags: row.get(8)?,
                dropped: row.get(9)?,
                frame_time_ms: row.get(10)?,
                fps: row.get(11)?,
                present_api_ms: row.get(12)?,
                render_complete_ms: row.get(13)?,
                displayed_ms: row.get(14)?,
                display_change_ms: row.get(15)?,
                flip_delay_ms: row.get(16)?,
                render_start_ms: row.get(17)?,
                gpu_active_ms: row.get(18)?,
                allows_tearing: row.get(19)?,
                present_mode: row.get(20)?,
                quality: row.get(21)?,
            })
        },
    )?;
    for row in rows {
        writer.serialize(row?)?;
    }
    writer.flush()?;
    Ok(())
}

fn export_events(connection: &Connection, path: &Path) -> Result<()> {
    export_events_range(connection, path, None)
}

fn export_events_range(
    connection: &Connection,
    path: &Path,
    range: Option<(i64, i64)>,
) -> Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    let mut statement = connection.prepare(
        "SELECT utc, monotonic_ns, channel, provider, event_id, level, record_id, message, raw_xml
         FROM diagnostic_events
         WHERE (?1 IS NULL OR monotonic_ns BETWEEN ?1 AND ?2)
         ORDER BY monotonic_ns, id",
    )?;
    let rows = statement.query_map(
        params![range.map(|value| value.0), range.map(|value| value.1)],
        |row| {
            Ok(serde_json::json!({
                "utc": row.get::<_, String>(0)?,
                "monotonic_ns": row.get::<_, i64>(1)?,
                "channel": row.get::<_, String>(2)?,
                "provider": row.get::<_, Option<String>>(3)?,
                "event_id": row.get::<_, Option<i64>>(4)?,
                "level": row.get::<_, Option<i64>>(5)?,
                "record_id": row.get::<_, Option<i64>>(6)?,
                "message": row.get::<_, Option<String>>(7)?,
                "raw_xml": row.get::<_, String>(8)?,
            }))
        },
    )?;
    for row in rows {
        serde_json::to_writer(&mut writer, &row?)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

fn export_crash_windows(connection: &Connection, session_dir: &Path) -> Result<()> {
    const BEFORE_NS: i64 = 5 * 60 * 1_000_000_000;
    const AFTER_NS: i64 = 60 * 1_000_000_000;
    let root = session_dir.join("crash-windows");
    std::fs::create_dir_all(&root)?;
    let mut statement = connection.prepare(
        "SELECT id, utc, monotonic_ns, trigger, detail, related_pid
         FROM bookmarks ORDER BY monotonic_ns, id LIMIT 100",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, String>(4)?,
            row.get::<_, Option<i64>>(5)?,
        ))
    })?;
    for row in rows {
        let (id, utc, monotonic_ns, trigger, detail, related_pid) = row?;
        let window_start = monotonic_ns.saturating_sub(BEFORE_NS).max(0);
        let window_end = monotonic_ns.saturating_add(AFTER_NS);
        let directory = root.join(bookmark_directory_name(id, &trigger));
        std::fs::create_dir_all(&directory)?;
        std::fs::write(
            directory.join("bookmark.json"),
            serde_json::to_string_pretty(&serde_json::json!({
                "id": id,
                "utc": utc,
                "monotonic_ns": monotonic_ns,
                "trigger": trigger,
                "detail": detail,
                "related_pid": related_pid,
                "window_start_monotonic_ns": window_start,
                "window_end_monotonic_ns": window_end,
                "requested_window": "five minutes before through one minute after, capped by recorded data"
            }))?,
        )?;
        let range = Some((window_start, window_end));
        export_telemetry_range(connection, &directory.join("telemetry.csv"), range)?;
        export_processes_range(connection, &directory.join("processes.csv"), range)?;
        export_frames_range(connection, &directory.join("frames.csv"), range)?;
        export_events_range(connection, &directory.join("windows-events.jsonl"), range)?;
        export_process_events_range(connection, &directory.join("process-events.jsonl"), range)?;
    }
    Ok(())
}

fn export_process_events_range(
    connection: &Connection,
    path: &Path,
    range: Option<(i64, i64)>,
) -> Result<()> {
    let file = File::create(path)?;
    let mut writer = BufWriter::new(file);
    let mut statement = connection.prepare(
        "SELECT utc, monotonic_ns, kind, pid, started_unix_s, parent_pid, name, executable,
                exit_code, detail
         FROM process_events WHERE (?1 IS NULL OR monotonic_ns BETWEEN ?1 AND ?2)
         ORDER BY monotonic_ns, id",
    )?;
    let rows = statement.query_map(
        params![range.map(|value| value.0), range.map(|value| value.1)],
        |row| {
            Ok(serde_json::json!({
                "utc": row.get::<_, String>(0)?,
                "monotonic_ns": row.get::<_, i64>(1)?,
                "kind": row.get::<_, String>(2)?,
                "pid": row.get::<_, i64>(3)?,
                "process_started_unix_s": row.get::<_, i64>(4)?,
                "parent_pid": row.get::<_, Option<i64>>(5)?,
                "name": row.get::<_, String>(6)?,
                "executable": row.get::<_, Option<String>>(7)?,
                "exit_code": row.get::<_, Option<i64>>(8)?,
                "detail": row.get::<_, Option<String>>(9)?,
            }))
        },
    )?;
    for row in rows {
        serde_json::to_writer(&mut writer, &row?)?;
        writer.write_all(b"\n")?;
    }
    writer.flush()?;
    Ok(())
}

fn bookmark_directory_name(id: i64, trigger: &str) -> String {
    let safe: String = trigger
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '_'
            }
        })
        .take(60)
        .collect();
    format!("{id:04}_{safe}")
}

fn build_summary(connection: &Connection) -> Result<Summary> {
    let (session_id, started_utc, stopped_utc, state) = connection.query_row(
        "SELECT session_id, started_utc, stopped_utc, state FROM sessions LIMIT 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
    )?;
    Ok(Summary {
        session_id,
        started_utc,
        stopped_utc,
        state,
        metric_samples: count(connection, "metric_samples")?,
        process_samples: count(connection, "process_samples")?,
        process_events: count(connection, "process_events")?,
        frame_samples: count(connection, "frame_samples")?,
        diagnostic_events: count(connection, "diagnostic_events")?,
        bookmarks: count(connection, "bookmarks")?,
        artifacts: count(connection, "artifacts")?,
        collector_issues: connection.query_row(
            "SELECT COUNT(*) FROM collector_health WHERE status NOT IN ('running', 'stopped', 'ok')",
            [],
            |row| row.get(0),
        )?,
    })
}

fn count(connection: &Connection, table: &str) -> Result<i64> {
    let query = format!("SELECT COUNT(*) FROM {table}");
    Ok(connection.query_row(&query, [], |row| row.get(0))?)
}

fn build_findings(connection: &Connection) -> Result<Vec<Finding>> {
    let whea = event_count(connection, "%whea%")?;
    let live_kernel_141_events = connection.query_row(
        "SELECT COUNT(*) FROM diagnostic_events
         WHERE lower(COALESCE(provider, '')) LIKE '%windows error reporting%'
           AND (lower(COALESCE(message, '')) LIKE '%event name: livekernelevent%'
                OR lower(raw_xml) LIKE '%event name: livekernelevent%')
           AND (lower(COALESCE(message, '')) LIKE '%p1: 141%'
                OR lower(COALESCE(message, '')) LIKE '%p1:141%'
                OR lower(raw_xml) LIKE '%p1: 141%'
                OR lower(raw_xml) LIKE '%p1:141%')",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let live_kernel_141_artifacts = connection.query_row(
        "SELECT COUNT(*) FROM bookmarks WHERE trigger = 'live_kernel_event_141'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let live_kernel_141 = live_kernel_141_events + live_kernel_141_artifacts;
    let display_driver = connection.query_row(
        "SELECT COUNT(*) FROM diagnostic_events
         WHERE lower(COALESCE(provider, '')) = 'display'
            OR lower(COALESCE(provider, '')) LIKE '%amdwddmg%'
            OR lower(COALESCE(provider, '')) LIKE '%amdkmdag%'
            OR lower(COALESCE(provider, '')) LIKE '%dxgkrnl%'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let application_failure = connection.query_row(
        "SELECT COUNT(*) FROM diagnostic_events
         WHERE lower(COALESCE(provider, '')) LIKE '%application error%'
            OR lower(COALESCE(provider, '')) LIKE '%application hang%'
            OR (lower(COALESCE(provider, '')) LIKE '%windows error reporting%'
                AND lower(COALESCE(message, '')) NOT LIKE '%event name: livekernelevent%'
                AND lower(raw_xml) NOT LIKE '%event name: livekernelevent%')",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let kernel_failure = connection.query_row(
        "SELECT COUNT(*) FROM diagnostic_events
         WHERE (lower(COALESCE(provider, '')) LIKE '%kernel-power%'
                AND (event_id = 41 OR level <= 2))
            OR lower(COALESCE(provider, '')) LIKE '%bugcheck%'",
        [],
        |row| row.get::<_, i64>(0),
    )?;
    let resource_events = event_count(connection, "%resource-exhaustion%")?;
    let major_stutters: i64 = connection.query_row(
        "SELECT COUNT(*) FROM frame_samples WHERE frame_time_ms >= 100.0 OR dropped = 1",
        [],
        |row| row.get(0),
    )?;
    let max_hotspot = max_metric(connection, "gpu.hotspot_temperature")?;
    let max_memory_temperature = max_metric(connection, "gpu.memory_temperature")?;
    let max_memory_load = max_metric(connection, "memory.load")?;
    let mut findings = Vec::new();

    if whea > 0 {
        findings.push(Finding {
            category: "WHEA / hardware evidence".into(),
            confidence: "high".into(),
            explanation: "Windows recorded one or more WHEA hardware events during the session. Inspect their raw XML to identify the reporting component and error type.".into(),
            evidence: vec![format!("{whea} WHEA event(s) in windows-events.jsonl")],
        });
    }
    if live_kernel_141 > 0 {
        findings.push(Finding {
            category: "GPU engine timeout / LiveKernelEvent 141".into(),
            confidence: "high".into(),
            explanation: "Windows recorded a GPU-engine timeout and recovery report. This proves that the graphics stack hung, but it does not by itself distinguish a driver problem from GPU, VRAM, power, or application-triggered instability.".into(),
            evidence: vec![
                format!("{live_kernel_141_events} current-session Event Log record(s)"),
                format!("{live_kernel_141_artifacts} newly collected watchdog artifact bookmark(s)"),
            ],
        });
    }
    if display_driver > 0 {
        findings.push(Finding {
            category: "Display-driver evidence".into(),
            confidence: "high".into(),
            explanation: "Windows recorded display, AMD display-driver, or DxgKrnl events during the session. This is consistent with a driver/GPU graphics-stack interruption, but does not alone prove its cause.".into(),
            evidence: vec![format!("{display_driver} matching driver/graphics event(s)")],
        });
    }
    if kernel_failure > 0 {
        findings.push(Finding {
            category: "Kernel shutdown / bug-check evidence".into(),
            confidence: "high".into(),
            explanation: "Kernel-Power or bug-check evidence was recorded in the session interval. Correlate the event timestamp with telemetry and any LiveKernelReport artifact.".into(),
            evidence: vec![format!("{kernel_failure} kernel failure event(s)")],
        });
    }
    if max_hotspot.is_some_and(|value| value >= 105.0)
        || max_memory_temperature.is_some_and(|value| value >= 95.0)
    {
        findings.push(Finding {
            category: "High GPU temperature evidence".into(),
            confidence: "medium".into(),
            explanation: "A recorded GPU hotspot or memory temperature reached a high diagnostic threshold. Review the chart around the bookmark; a threshold crossing does not prove that temperature caused the failure.".into(),
            evidence: vec![
                format_optional("Maximum hotspot", max_hotspot, "°C"),
                format_optional("Maximum memory temperature", max_memory_temperature, "°C"),
            ],
        });
    }
    if resource_events > 0 || max_memory_load.is_some_and(|value| value >= 95.0) {
        findings.push(Finding {
            category: "System memory pressure".into(),
            confidence: if resource_events > 0 { "high" } else { "medium" }.into(),
            explanation: "Windows resource-exhaustion evidence or very high physical-memory load was observed. Check committed memory, commit limit, and the responsible process near the event.".into(),
            evidence: vec![
                format!("{resource_events} resource-exhaustion event(s)"),
                format_optional("Maximum memory load", max_memory_load, "%"),
            ],
        });
    }
    if application_failure > 0 {
        findings.push(Finding {
            category: "Application failure evidence".into(),
            confidence: "high".into(),
            explanation: if whea == 0
                && live_kernel_141 == 0
                && display_driver == 0
                && kernel_failure == 0
            {
                "Application Error, Application Hang, or WER evidence was recorded without accompanying WHEA, display-driver, or kernel-failure evidence in this session. This pattern is more consistent with an application/software-only failure, while still not proving causation."
            } else {
                "Application Error, Application Hang, or WER evidence was recorded. Because hardware/driver/kernel evidence is also present, use timestamps to determine ordering rather than assuming the application failed independently."
            }
            .into(),
            evidence: vec![format!("{application_failure} application/ WER event(s)")],
        });
    }
    if major_stutters > 0 {
        findings.push(Finding {
            category: "Frame-time disruption".into(),
            confidence: "observed".into(),
            explanation: "PresentMon recorded dropped presents or frame intervals of at least 100 ms. These are timing observations, not a diagnosis of why the disruption occurred.".into(),
            evidence: vec![format!("{major_stutters} dropped/major-stutter frame row(s)")],
        });
    }
    if findings.is_empty() {
        findings.push(Finding {
            category: "No decisive failure evidence".into(),
            confidence: "low".into(),
            explanation: "No WHEA, display-driver, kernel-failure, application-failure, thermal-threshold, resource-exhaustion, or major frame-disruption rule matched. Review the synchronized raw data; absence of a matching event does not prove hardware health.".into(),
            evidence: vec!["See collector availability and full session.sqlite data".into()],
        });
    }
    Ok(findings)
}

fn event_count(connection: &Connection, provider_pattern: &str) -> Result<i64> {
    Ok(connection.query_row(
        "SELECT COUNT(*) FROM diagnostic_events WHERE lower(COALESCE(provider, '')) LIKE ?1",
        [provider_pattern],
        |row| row.get(0),
    )?)
}

fn max_metric(connection: &Connection, metric: &str) -> Result<Option<f64>> {
    Ok(connection.query_row(
        "SELECT MAX(value) FROM metric_samples WHERE metric = ?1",
        [metric],
        |row| row.get(0),
    )?)
}

fn format_optional(label: &str, value: Option<f64>, unit: &str) -> String {
    value
        .map(|value| format!("{label}: {value:.1}{unit}"))
        .unwrap_or_else(|| format!("{label}: unavailable"))
}

fn export_html(
    connection: &Connection,
    summary: &Summary,
    findings: &[Finding],
    path: &Path,
) -> Result<()> {
    let mut metric_rows = String::new();
    let mut statement = connection.prepare(
        "SELECT source, COALESCE(device, ''), metric, unit, COUNT(*), MIN(value), AVG(value), MAX(value)
         FROM metric_samples GROUP BY source, device, metric, unit ORDER BY source, device, metric",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, f64>(5)?,
            row.get::<_, f64>(6)?,
            row.get::<_, f64>(7)?,
        ))
    })?;
    for row in rows {
        let (source, device, metric, unit, samples, min, average, max) = row?;
        metric_rows.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{samples}</td><td>{min:.2}</td><td>{average:.2}</td><td>{max:.2}</td><td>{}</td></tr>",
            escape_html(&source), escape_html(&device), escape_html(&metric), escape_html(&unit)
        ));
    }

    let mut bookmarks = String::new();
    let mut statement = connection.prepare(
        "SELECT id, utc, trigger, detail, related_pid FROM bookmarks ORDER BY monotonic_ns, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<i64>>(4)?,
        ))
    })?;
    for row in rows {
        let (id, utc, trigger, detail, pid) = row?;
        let href = format!(
            "crash-windows/{}/bookmark.json",
            bookmark_directory_name(id, &trigger)
        );
        bookmarks.push_str(&format!(
            "<tr><td>{}</td><td><a href=\"{}\">{}</a></td><td>{}</td><td>{}</td></tr>",
            escape_html(&utc),
            escape_html(&href),
            escape_html(&trigger),
            pid.map(|value| value.to_string()).unwrap_or_default(),
            escape_html(&detail)
        ));
    }

    let mut health = String::new();
    let mut statement = connection.prepare(
        "SELECT utc, collector, status, detail FROM collector_health ORDER BY monotonic_ns, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (utc, collector, status, detail) = row?;
        health.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape_html(&utc),
            escape_html(&collector),
            escape_html(&status),
            escape_html(&detail)
        ));
    }

    let mut findings_html = String::new();
    for finding in findings {
        let evidence = finding
            .evidence
            .iter()
            .map(|item| format!("<li>{}</li>", escape_html(item)))
            .collect::<String>();
        findings_html.push_str(&format!(
            "<article class=\"finding\"><div><span class=\"badge\">{}</span><h3>{}</h3></div><p>{}</p><ul>{}</ul></article>",
            escape_html(&finding.confidence),
            escape_html(&finding.category),
            escape_html(&finding.explanation),
            evidence
        ));
    }

    let charts = build_charts(connection)?;
    let event_rows = event_table(connection)?;
    let process_rows = process_exit_table(connection)?;
    let foreground_rows = foreground_table(connection)?;
    let frame_rows = frame_summary_table(connection)?;
    let artifact_rows = artifact_table(connection)?;

    let html = format!(
        r#"<!doctype html>
<html lang="en"><head><meta charset="utf-8"><meta name="viewport" content="width=device-width,initial-scale=1">
<title>GPU Crash Recorder — {id}</title>
<style>body{{font-family:Segoe UI,Arial,sans-serif;margin:0;background:#11151b;color:#e7edf5}}main{{max-width:1400px;margin:auto;padding:28px}}h1{{margin-bottom:4px}}h2{{margin-top:2px}}h3{{display:inline;margin-left:10px}}a{{color:#70b9ff}}.muted{{color:#9aa8b8}}.cards{{display:grid;grid-template-columns:repeat(auto-fit,minmax(160px,1fr));gap:12px;margin:22px 0}}.card,section,.finding{{background:#19212b;border:1px solid #2b3947;border-radius:10px;padding:16px}}.value{{font-size:28px;font-weight:650;color:#ff4b55}}section{{margin:16px 0;overflow:auto}}.finding{{margin:10px 0}}.finding p{{margin-bottom:6px}}.badge{{display:inline-block;background:#334455;color:#dbe8f4;border-radius:12px;padding:3px 8px;font-size:12px;text-transform:uppercase}}.charts{{display:grid;grid-template-columns:repeat(auto-fit,minmax(480px,1fr));gap:12px}}.chart{{background:#121820;border:1px solid #2b3947;border-radius:8px;padding:8px;min-width:0}}.chart svg{{width:100%;height:auto}}table{{width:100%;border-collapse:collapse;font-size:13px}}th,td{{padding:8px;border-bottom:1px solid #2b3947;text-align:left;vertical-align:top}}th{{color:#aab8c7;position:sticky;top:0;background:#19212b}}code{{color:#ff8890}}.files{{display:flex;gap:14px;flex-wrap:wrap;margin:15px 0}}</style></head>
<body><main><h1>GPU Crash Recorder</h1><div class="muted">Session <code>{id}</code><br>{started} → {stopped}</div>
<nav class="files"><a href="session.sqlite">SQLite database</a><a href="telemetry.csv">Telemetry CSV</a><a href="frames.csv">Frames CSV</a><a href="processes.csv">Processes CSV</a><a href="process-events.jsonl">Process lifecycle</a><a href="windows-events.jsonl">Windows events</a><a href="system-info.json">System inventory</a><a href="findings.json">Findings JSON</a><a href="artifacts.json">Artifacts JSON</a></nav>
<div class="cards"><div class="card"><div class="value">{metrics}</div>metric samples</div><div class="card"><div class="value">{process_samples}</div>process samples</div><div class="card"><div class="value">{frames}</div>frame samples</div><div class="card"><div class="value">{events}</div>Windows events</div><div class="card"><div class="value">{bookmarks_count}</div>bookmarks</div><div class="card"><div class="value">{artifacts}</div>crash artifacts</div></div>
<section><h2>Evidence-based findings</h2><p class="muted">Rules summarize recorded evidence and never prove causation by themselves.</p>{findings_html}</section>
<section><h2>Bookmarks</h2><table><thead><tr><th>UTC</th><th>Trigger</th><th>PID</th><th>Detail</th></tr></thead><tbody>{bookmarks}</tbody></table></section>
<section><h2>Synchronized telemetry</h2><p class="muted">All charts share the same session-relative horizontal time axis. Full-resolution values remain in SQLite and CSV.</p><div class="charts">{charts}</div></section>
<section><h2>Frame-time summary by application</h2><table><thead><tr><th>Application</th><th>Frames</th><th>Average ms</th><th>Maximum ms</th><th>≥50 ms / dropped</th></tr></thead><tbody>{frame_rows}</tbody></table></section>
<section><h2>Windows and driver events</h2><table><thead><tr><th>UTC</th><th>Provider</th><th>ID</th><th>Level</th><th>Message</th></tr></thead><tbody>{event_rows}</tbody></table></section>
<section><h2>Process exits</h2><table><thead><tr><th>UTC</th><th>Process</th><th>PID</th><th>Exit code</th><th>Detail</th></tr></thead><tbody>{process_rows}</tbody></table></section>
<section><h2>Foreground application timeline</h2><table><thead><tr><th>UTC</th><th>PID</th><th>Executable</th><th>Window</th></tr></thead><tbody>{foreground_rows}</tbody></table></section>
<section><h2>Crash artifacts</h2><table><thead><tr><th>UTC</th><th>Status</th><th>Size</th><th>Original path</th><th>Collected copy</th></tr></thead><tbody>{artifact_rows}</tbody></table></section>
<section><h2>Metric summary</h2><table><thead><tr><th>Source</th><th>Device</th><th>Metric</th><th>Samples</th><th>Min</th><th>Average</th><th>Max</th><th>Unit</th></tr></thead><tbody>{metric_rows}</tbody></table></section>
<section><h2>Collector availability</h2><table><thead><tr><th>UTC</th><th>Collector</th><th>Status</th><th>Detail</th></tr></thead><tbody>{health}</tbody></table></section>
<p class="muted">This report presents observed evidence and does not claim causation. Full data is retained in <code>session.sqlite</code> and the accompanying exports.</p>
</main></body></html>"#,
        id = escape_html(&summary.session_id),
        started = escape_html(&summary.started_utc),
        stopped = escape_html(summary.stopped_utc.as_deref().unwrap_or("recording")),
        metrics = summary.metric_samples,
        process_samples = summary.process_samples,
        frames = summary.frame_samples,
        events = summary.diagnostic_events,
        bookmarks_count = summary.bookmarks,
        artifacts = summary.artifacts,
        findings_html = findings_html,
        charts = charts,
        event_rows = event_rows,
        process_rows = process_rows,
        foreground_rows = foreground_rows,
        frame_rows = frame_rows,
        artifact_rows = artifact_rows,
    );
    std::fs::write(path, html).with_context(|| format!("failed to write {}", path.display()))
}

fn build_charts(connection: &Connection) -> Result<String> {
    let max_monotonic: i64 = connection.query_row(
        "SELECT MAX(value) FROM (
             SELECT COALESCE(MAX(monotonic_ns), 0) AS value FROM metric_samples
             UNION ALL SELECT COALESCE(MAX(monotonic_ns), 0) FROM frame_samples
         )",
        [],
        |row| row.get(0),
    )?;
    if max_monotonic <= 0 {
        return Ok("<p class=\"muted\">No chartable samples were recorded.</p>".into());
    }
    let bucket_ns = (max_monotonic / 700).max(1_000_000);
    let gpu_device: Option<String> = connection
        .query_row(
            "SELECT device FROM metric_samples
             WHERE source = 'amd_adlx' AND metric = 'gpu.utilization' AND device IS NOT NULL
             GROUP BY device
             ORDER BY CASE WHEN lower(device) LIKE '%7900%' THEN 0 ELSE 1 END, COUNT(*) DESC
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    let gpu_label = gpu_device.as_deref().unwrap_or("AMD GPU");
    let specifications = vec![
        (
            format!("GPU utilization — {gpu_label}"),
            "amd_adlx".to_string(),
            "gpu.utilization".to_string(),
            "%".to_string(),
            gpu_device.clone(),
        ),
        (
            "GPU hotspot temperature".into(),
            "amd_adlx".into(),
            "gpu.hotspot_temperature".into(),
            "°C".into(),
            gpu_device.clone(),
        ),
        (
            "GPU memory temperature".into(),
            "amd_adlx".into(),
            "gpu.memory_temperature".into(),
            "°C".into(),
            gpu_device.clone(),
        ),
        (
            "GPU total board power".into(),
            "amd_adlx".into(),
            "gpu.total_board_power".into(),
            "W".into(),
            gpu_device.clone(),
        ),
        (
            "GPU core clock".into(),
            "amd_adlx".into(),
            "gpu.core_clock".into(),
            "MHz".into(),
            gpu_device.clone(),
        ),
        (
            "GPU VRAM usage".into(),
            "amd_adlx".into(),
            "gpu.vram_usage".into(),
            "MB".into(),
            gpu_device,
        ),
        (
            "System CPU utilization".into(),
            "windows".into(),
            "cpu.utilization".into(),
            "%".into(),
            None,
        ),
        (
            "Physical memory load".into(),
            "windows_memory".into(),
            "memory.load".into(),
            "%".into(),
            None,
        ),
    ];
    let mut charts = String::new();
    for (title, source, metric, unit, device) in specifications {
        let mut statement = connection.prepare(
            "SELECT (monotonic_ns / ?1) * ?1 AS bucket, AVG(value)
             FROM metric_samples
             WHERE source = ?2 AND metric = ?3 AND (?4 IS NULL OR device = ?4)
             GROUP BY monotonic_ns / ?1 ORDER BY bucket",
        )?;
        let rows = statement.query_map(params![bucket_ns, source, metric, device], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })?;
        let points = rows
            .filter_map(|row| row.ok())
            .filter(|(_, value)| value.is_finite())
            .collect::<Vec<_>>();
        if !points.is_empty() {
            charts.push_str(&render_chart(
                &title,
                &unit,
                &points,
                max_monotonic,
                "#ff4b55",
            ));
        }
    }

    let frame_application: Option<String> = connection
        .query_row(
            "SELECT application FROM frame_samples
             WHERE application IS NOT NULL
               AND lower(application) NOT IN ('dwm.exe', 'presentmon.exe', 'gpu-crash-recorder.exe', '<unknown>')
             GROUP BY application ORDER BY COUNT(*) DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .ok();
    if let Some(application) = frame_application {
        let mut statement = connection.prepare(
            "SELECT (monotonic_ns / ?1) * ?1 AS bucket, AVG(frame_time_ms)
             FROM frame_samples WHERE application = ?2 AND frame_time_ms IS NOT NULL
             GROUP BY monotonic_ns / ?1 ORDER BY bucket",
        )?;
        let rows = statement.query_map(params![bucket_ns, application], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
        })?;
        let points = rows.filter_map(|row| row.ok()).collect::<Vec<_>>();
        if !points.is_empty() {
            charts.push_str(&render_chart(
                &format!("Frame time — {application}"),
                "ms",
                &points,
                max_monotonic,
                "#70b9ff",
            ));
        }
    }
    if charts.is_empty() {
        charts.push_str("<p class=\"muted\">No chartable samples were recorded.</p>");
    }
    Ok(charts)
}

fn render_chart(
    title: &str,
    unit: &str,
    points: &[(i64, f64)],
    max_monotonic: i64,
    color: &str,
) -> String {
    let minimum = points
        .iter()
        .map(|(_, value)| *value)
        .fold(f64::INFINITY, f64::min);
    let maximum = points
        .iter()
        .map(|(_, value)| *value)
        .fold(f64::NEG_INFINITY, f64::max);
    let average = points.iter().map(|(_, value)| *value).sum::<f64>() / points.len() as f64;
    let range = (maximum - minimum).abs().max(0.001);
    let coordinates = points
        .iter()
        .map(|(time, value)| {
            let x = 55.0 + (*time as f64 / max_monotonic as f64) * 920.0;
            let y = 195.0 - ((*value - minimum) / range) * 160.0;
            format!("{x:.1},{y:.1}")
        })
        .collect::<Vec<_>>()
        .join(" ");
    let duration = max_monotonic as f64 / 1_000_000_000.0;
    format!(
        "<article class=\"chart\"><strong>{}</strong><div class=\"muted\">min {:.1} · avg {:.1} · max {:.1} {}</div><svg viewBox=\"0 0 1000 225\" role=\"img\" aria-label=\"{}\"><line x1=\"55\" y1=\"35\" x2=\"55\" y2=\"195\" stroke=\"#435160\"/><line x1=\"55\" y1=\"195\" x2=\"975\" y2=\"195\" stroke=\"#435160\"/><line x1=\"55\" y1=\"115\" x2=\"975\" y2=\"115\" stroke=\"#273441\"/><text x=\"4\" y=\"40\" fill=\"#9aa8b8\" font-size=\"12\">{:.1}</text><text x=\"4\" y=\"198\" fill=\"#9aa8b8\" font-size=\"12\">{:.1}</text><text x=\"55\" y=\"216\" fill=\"#9aa8b8\" font-size=\"12\">0s</text><text x=\"930\" y=\"216\" fill=\"#9aa8b8\" font-size=\"12\">{:.0}s</text><polyline fill=\"none\" stroke=\"{}\" stroke-width=\"2\" points=\"{}\"/></svg></article>",
        escape_html(title),
        minimum,
        average,
        maximum,
        escape_html(unit),
        escape_html(title),
        maximum,
        minimum,
        duration,
        color,
        coordinates
    )
}

fn event_table(connection: &Connection) -> Result<String> {
    let mut html = String::new();
    let mut statement = connection.prepare(
        "SELECT utc, COALESCE(provider, ''), COALESCE(event_id, 0), COALESCE(level, 0),
                COALESCE(message, '')
         FROM diagnostic_events ORDER BY monotonic_ns, id LIMIT 300",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (utc, provider, id, level, message) = row?;
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{id}</td><td>{level}</td><td>{}</td></tr>",
            escape_html(&utc),
            escape_html(&provider),
            escape_html(&message.chars().take(600).collect::<String>())
        ));
    }
    Ok(empty_row(
        html,
        5,
        "No matching Windows events were recorded.",
    ))
}

fn process_exit_table(connection: &Connection) -> Result<String> {
    let mut html = String::new();
    let mut statement = connection.prepare(
        "SELECT utc, name, pid, exit_code, COALESCE(detail, '') FROM process_events
         WHERE kind = 'stop' ORDER BY monotonic_ns, id LIMIT 200",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, Option<i64>>(3)?,
            row.get::<_, String>(4)?,
        ))
    })?;
    for row in rows {
        let (utc, name, pid, exit_code, detail) = row?;
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{pid}</td><td>{}</td><td>{}</td></tr>",
            escape_html(&utc),
            escape_html(&name),
            exit_code
                .map(|value| format!("0x{value:08X}"))
                .unwrap_or_default(),
            escape_html(&detail)
        ));
    }
    Ok(empty_row(
        html,
        5,
        "No observed process exits were recorded.",
    ))
}

fn foreground_table(connection: &Connection) -> Result<String> {
    let mut html = String::new();
    let mut statement = connection.prepare(
        "SELECT utc, pid, COALESCE(executable, ''), COALESCE(window_title, '')
         FROM foreground_events ORDER BY monotonic_ns, id LIMIT 300",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, String>(3)?,
        ))
    })?;
    for row in rows {
        let (utc, pid, executable, title) = row?;
        html.push_str(&format!(
            "<tr><td>{}</td><td>{pid}</td><td>{}</td><td>{}</td></tr>",
            escape_html(&utc),
            escape_html(&executable),
            escape_html(&title)
        ));
    }
    Ok(empty_row(
        html,
        4,
        "No foreground-window changes were recorded.",
    ))
}

fn frame_summary_table(connection: &Connection) -> Result<String> {
    let mut html = String::new();
    let mut statement = connection.prepare(
        "SELECT COALESCE(application, '<unknown>'), COUNT(*), AVG(frame_time_ms),
                MAX(frame_time_ms),
                SUM(CASE WHEN frame_time_ms >= 50.0 OR dropped = 1 THEN 1 ELSE 0 END)
         FROM frame_samples GROUP BY application ORDER BY COUNT(*) DESC LIMIT 30",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, Option<f64>>(2)?,
            row.get::<_, Option<f64>>(3)?,
            row.get::<_, i64>(4)?,
        ))
    })?;
    for row in rows {
        let (application, frames, average, maximum, disrupted) = row?;
        html.push_str(&format!(
            "<tr><td>{}</td><td>{frames}</td><td>{}</td><td>{}</td><td>{disrupted}</td></tr>",
            escape_html(&application),
            average
                .map(|value| format!("{value:.2}"))
                .unwrap_or_default(),
            maximum
                .map(|value| format!("{value:.2}"))
                .unwrap_or_default()
        ));
    }
    Ok(empty_row(html, 5, "No frame samples were recorded."))
}

fn artifact_table(connection: &Connection) -> Result<String> {
    let mut html = String::new();
    let mut statement = connection.prepare(
        "SELECT discovered_utc, status, COALESCE(size_bytes, 0), original_path, copied_path
         FROM artifacts ORDER BY monotonic_ns, id",
    )?;
    let rows = statement.query_map([], |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, i64>(2)?,
            row.get::<_, String>(3)?,
            row.get::<_, Option<String>>(4)?,
        ))
    })?;
    for row in rows {
        let (utc, status, size, original, copied) = row?;
        let copy_link = copied
            .map(|path| {
                let href = path.replace('\\', "/");
                format!(
                    "<a href=\"{}\">{}</a>",
                    escape_html(&href),
                    escape_html(&path)
                )
            })
            .unwrap_or_default();
        html.push_str(&format!(
            "<tr><td>{}</td><td>{}</td><td>{}</td><td>{}</td><td>{}</td></tr>",
            escape_html(&utc),
            escape_html(&status),
            format_bytes(size.max(0) as u64),
            escape_html(&original),
            copy_link
        ));
    }
    Ok(empty_row(
        html,
        5,
        "No new or modified crash artifacts were found.",
    ))
}

fn empty_row(html: String, columns: usize, message: &str) -> String {
    if html.is_empty() {
        format!(
            "<tr><td colspan=\"{columns}\" class=\"muted\">{}</td></tr>",
            escape_html(message)
        )
    } else {
        html
    }
}

fn format_bytes(bytes: u64) -> String {
    const UNITS: [&str; 5] = ["B", "KiB", "MiB", "GiB", "TiB"];
    let mut value = bytes as f64;
    let mut unit = 0;
    while value >= 1024.0 && unit < UNITS.len() - 1 {
        value /= 1024.0;
        unit += 1;
    }
    format!("{value:.1} {}", UNITS[unit])
}

fn escape_html(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn classifies_live_kernel_141_as_gpu_timeout_not_application_failure() {
        let directory = tempdir().unwrap();
        let connection = open_database(&directory.path().join("session.sqlite")).unwrap();
        connection
            .execute(
                "INSERT INTO diagnostic_events
                 (utc, monotonic_ns, channel, provider, event_id, level, record_id, message, raw_xml)
                 VALUES ('2026-08-30T08:16:51Z', 1, 'Application', 'Windows Error Reporting',
                         1001, 4, 99, 'Event Name: LiveKernelEvent\nP1: 141', '<Event/>')",
                [],
            )
            .unwrap();

        let findings = build_findings(&connection).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.category.contains("LiveKernelEvent 141"))
        );
        assert!(
            findings
                .iter()
                .all(|finding| finding.category != "Application failure evidence")
        );
    }

    #[test]
    fn recognizes_live_kernel_141_artifact_bookmark() {
        let directory = tempdir().unwrap();
        let connection = open_database(&directory.path().join("session.sqlite")).unwrap();
        connection
            .execute(
                "INSERT INTO bookmarks (utc, monotonic_ns, trigger, detail, related_pid)
                 VALUES ('2026-08-30T08:16:46Z', 1, 'live_kernel_event_141',
                         'New watchdog artifact', NULL)",
                [],
            )
            .unwrap();

        let findings = build_findings(&connection).unwrap();
        assert!(
            findings
                .iter()
                .any(|finding| finding.category.contains("LiveKernelEvent 141"))
        );
    }
}
