use std::{
    collections::HashMap,
    fs::{self, File},
    io::{BufReader, Read},
    os::windows::process::CommandExt,
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread,
    time::{Duration, Instant},
};

use anyhow::{Context, Result, bail};
use crossbeam_channel::{Sender, TrySendError};
use csv::StringRecord;
use sha2::{Digest, Sha256};

use crate::{
    model::{CollectorHealth, FrameSample, Record},
    timestamp::SessionClock,
};

const PRESENTMON_VERSION: &str = "2.5.1";
const PRESENTMON_SHA256: &str = "9bec3083069f58f911e6a512f4806db51a27bd096103087bc1d05ef54c80a191";
const PRESENTMON_BYTES: &[u8] = include_bytes!("../vendor/presentmon/PresentMon-2.5.1-x64.exe");
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const PID_COLUMNS: &[&str] = &["ProcessID", "ProcessId", "PID"];
const FRAME_TIME_COLUMNS: &[&str] = &["msBetweenPresents", "MsBetweenPresents", "FrameTime"];

#[derive(Debug, Default)]
struct ParseStats {
    rows: u64,
    malformed: u64,
    queue_drops: u64,
    header_seen: bool,
    diagnostic: Option<String>,
}

pub fn run(clock: SessionClock, sender: Sender<Record>, stop: Arc<AtomicBool>) -> Result<()> {
    let executable = ensure_runtime_binary()?;
    let session_name = format!(
        "GPUCrashRecorder_{}_{}",
        std::process::id(),
        chrono::Utc::now().timestamp_millis()
    );
    let output_path = executable
        .parent()
        .unwrap_or_else(|| std::path::Path::new("."))
        .join(format!("{session_name}.csv"));
    let mut child = spawn_capture(&executable, &session_name, &output_path)?;
    let stderr = child
        .stderr
        .take()
        .context("PresentMon stderr was not piped")?;

    send_health(
        &sender,
        &clock,
        "presentmon",
        "running",
        &format!(
            "PresentMon {PRESENTMON_VERSION} system-wide ETW capture started (pinned SHA-256 {PRESENTMON_SHA256})"
        ),
    );

    let reader_clock = clock.clone();
    let reader_sender = sender.clone();
    let capture_done = Arc::new(AtomicBool::new(false));
    let reader_done = Arc::clone(&capture_done);
    let reader_output_path = output_path.clone();
    let reader_join = thread::Builder::new()
        .name("presentmon-csv-reader".into())
        .spawn(move || {
            parse_growing_file(
                &reader_output_path,
                reader_done,
                reader_clock,
                reader_sender,
            )
        })
        .context("failed to start PresentMon CSV reader")?;
    let stderr_join = thread::Builder::new()
        .name("presentmon-stderr-reader".into())
        .spawn(move || {
            let mut text = String::new();
            let mut reader = BufReader::new(stderr);
            let _ = reader.read_to_string(&mut text);
            text
        })
        .context("failed to start PresentMon stderr reader")?;

    let mut unexpected_status = None;
    while !stop.load(Ordering::Acquire) {
        if let Some(status) = child.try_wait().context("failed to poll PresentMon")? {
            unexpected_status = Some(status);
            break;
        }
        thread::sleep(Duration::from_millis(100));
    }

    if unexpected_status.is_none() {
        let _ = stop_capture(&executable, &session_name);
        if !wait_for_exit(&mut child, Duration::from_secs(5))? {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
    capture_done.store(true, Ordering::Release);

    let parse_result = reader_join
        .join()
        .map_err(|_| anyhow::anyhow!("PresentMon CSV reader panicked"))?;
    let stderr_text = stderr_join.join().unwrap_or_default();
    let _ = fs::remove_file(&output_path);

    let stats = match parse_result {
        Ok(stats) => stats,
        Err(error) => {
            send_health(
                &sender,
                &clock,
                "presentmon",
                "failed",
                &format!(
                    "PresentMon CSV parsing failed: {error:#}; {}",
                    concise_stderr(&stderr_text)
                ),
            );
            return Ok(());
        }
    };

    if let Some(status) = unexpected_status {
        send_health(
            &sender,
            &clock,
            "presentmon",
            "failed",
            &format!(
                "PresentMon exited unexpectedly with {status}; parsed {} rows. {}",
                stats.rows,
                concise_stderr(&stderr_text)
            ),
        );
        return Ok(());
    }

    if !stats.header_seen {
        send_health(
            &sender,
            &clock,
            "presentmon",
            "unavailable",
            &format!(
                "PresentMon produced no frame CSV data: {}; {}",
                stats
                    .diagnostic
                    .as_deref()
                    .unwrap_or("no compatible header was emitted"),
                concise_stderr(&stderr_text)
            ),
        );
        return Ok(());
    }

    let status = if stats.malformed == 0 && stats.queue_drops == 0 {
        "stopped"
    } else {
        "degraded"
    };
    send_health(
        &sender,
        &clock,
        "presentmon",
        status,
        &format!(
            "PresentMon stopped cleanly: {} frame rows, {} malformed rows, {} queue drops",
            stats.rows, stats.malformed, stats.queue_drops
        ),
    );
    Ok(())
}

fn ensure_runtime_binary() -> Result<PathBuf> {
    let runtime_dir = dirs::data_local_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("GPUCrashRecorder")
        .join("Runtime");
    fs::create_dir_all(&runtime_dir)
        .with_context(|| format!("failed to create {}", runtime_dir.display()))?;
    let path = runtime_dir.join(format!("PresentMon-{PRESENTMON_VERSION}-x64.exe"));

    let valid = path
        .is_file()
        .then(|| fs::read(&path).ok())
        .flatten()
        .is_some_and(|bytes| sha256_hex(&bytes) == PRESENTMON_SHA256);
    if !valid {
        fs::write(&path, PRESENTMON_BYTES).with_context(|| {
            format!(
                "failed to extract embedded PresentMon to {}",
                path.display()
            )
        })?;
    }
    let actual = sha256_hex(&fs::read(&path)?);
    if actual != PRESENTMON_SHA256 {
        bail!("extracted PresentMon checksum mismatch: expected {PRESENTMON_SHA256}, got {actual}");
    }
    Ok(path)
}

fn sha256_hex(bytes: &[u8]) -> String {
    format!("{:x}", Sha256::digest(bytes))
}

fn spawn_capture(executable: &PathBuf, session_name: &str, output_path: &PathBuf) -> Result<Child> {
    let mut command = Command::new(executable);
    command
        .args([
            "--output_file",
            &output_path.to_string_lossy(),
            "--no_console_stats",
            "--qpc_time_ms",
            "--v1_metrics",
            "--no_track_input",
            "--session_name",
            session_name,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::piped())
        .creation_flags(CREATE_NO_WINDOW);
    command
        .spawn()
        .with_context(|| format!("failed to start {}", executable.display()))
}

fn parse_growing_file(
    path: &PathBuf,
    capture_done: Arc<AtomicBool>,
    clock: SessionClock,
    sender: Sender<Record>,
) -> Result<ParseStats> {
    loop {
        match File::open(path) {
            Ok(file) => {
                return parse_stream(FollowFile { file, capture_done }, clock, sender);
            }
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                if capture_done.load(Ordering::Acquire) {
                    return Ok(ParseStats {
                        diagnostic: Some("no CSV file was created".into()),
                        ..Default::default()
                    });
                }
                thread::sleep(Duration::from_millis(50));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

struct FollowFile {
    file: File,
    capture_done: Arc<AtomicBool>,
}

impl Read for FollowFile {
    fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
        loop {
            let read = self.file.read(buffer)?;
            if read > 0 || self.capture_done.load(Ordering::Acquire) {
                return Ok(read);
            }
            thread::sleep(Duration::from_millis(20));
        }
    }
}

fn stop_capture(executable: &PathBuf, session_name: &str) -> Result<()> {
    let status = Command::new(executable)
        .args([
            "--terminate_existing_session",
            "--session_name",
            session_name,
        ])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(CREATE_NO_WINDOW)
        .status()
        .context("failed to invoke PresentMon trace-session shutdown")?;
    if !status.success() {
        bail!("PresentMon trace-session shutdown returned {status}");
    }
    Ok(())
}

fn wait_for_exit(child: &mut Child, timeout: Duration) -> Result<bool> {
    let deadline = Instant::now() + timeout;
    loop {
        if child.try_wait()?.is_some() {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        thread::sleep(Duration::from_millis(50));
    }
}

fn parse_stream(
    stdout: impl Read,
    clock: SessionClock,
    sender: Sender<Record>,
) -> Result<ParseStats> {
    let mut reader = csv::ReaderBuilder::new()
        .has_headers(false)
        .flexible(true)
        .from_reader(BufReader::new(stdout));
    let mut stats = ParseStats::default();
    let mut headers = None;
    let mut preamble = Vec::new();
    for row in reader.records() {
        let row = match row {
            Ok(row) => row,
            Err(_) => {
                stats.malformed += 1;
                continue;
            }
        };
        if headers.is_none() {
            let candidate = HeaderMap::new(&row);
            if candidate.has_any(PID_COLUMNS) && candidate.has_any(FRAME_TIME_COLUMNS) {
                stats.header_seen = true;
                headers = Some(candidate);
            } else if preamble.len() < 3 && row.iter().any(|value| !value.trim().is_empty()) {
                preamble.push(row.iter().collect::<Vec<_>>().join(","));
            }
            continue;
        }
        let Some(frame) = parse_record(headers.as_ref().expect("header was set"), &row, &clock)
        else {
            stats.malformed += 1;
            continue;
        };
        match sender.try_send(Record::Frame(frame)) {
            Ok(()) => stats.rows += 1,
            Err(TrySendError::Full(_)) => stats.queue_drops += 1,
            Err(TrySendError::Disconnected(_)) => break,
        }
    }
    if headers.is_none() {
        stats.diagnostic = Some(if preamble.is_empty() {
            "no output was emitted".to_string()
        } else {
            format!("first output: {}", preamble.join(" | "))
        });
    }
    Ok(stats)
}

fn parse_record(
    headers: &HeaderMap,
    row: &StringRecord,
    clock: &SessionClock,
) -> Option<FrameSample> {
    let pid = parse::<u32>(headers.get_any(row, PID_COLUMNS))?;
    let frame_time_ms = parse::<f64>(headers.get_any(row, FRAME_TIME_COLUMNS));
    let source_qpc_seconds = parse::<f64>(headers.get_any(row, &["TimeInSeconds", "CPUStartTime"]))
        .or_else(|| {
            parse::<f64>(headers.get_any(row, &["TimeInMs", "QPCTime", "CPUStartQPCTime"]))
                .map(|value| value / 1_000.0)
        });
    let time = source_qpc_seconds
        .map(|value| clock.from_qpc_seconds(value))
        .unwrap_or_else(|| clock.now());
    let dropped = parse_bool(headers.get(row, "Dropped"));
    let quality = match (dropped, frame_time_ms) {
        (Some(true), _) => "dropped",
        (_, Some(value)) if value >= 100.0 => "major_stutter",
        (_, Some(value)) if value >= 50.0 => "stutter",
        _ => "ok",
    };

    Some(FrameSample {
        time,
        source_qpc_seconds,
        pid,
        application: text_value(headers.get(row, "Application")),
        swap_chain: text_value(headers.get(row, "SwapChainAddress")),
        runtime: text_value(headers.get_any(row, &["PresentRuntime", "Runtime"])),
        sync_interval: parse(headers.get(row, "SyncInterval")),
        present_flags: parse(headers.get(row, "PresentFlags")),
        dropped,
        frame_time_ms,
        fps: frame_time_ms
            .filter(|value| *value > 0.001)
            .map(|value| 1_000.0 / value),
        present_api_ms: parse(headers.get(row, "msInPresentAPI")),
        render_complete_ms: parse(
            headers.get_any(row, &["msUntilRenderComplete", "MsRenderPresentLatency"]),
        ),
        displayed_ms: parse(headers.get(row, "msUntilDisplayed")),
        display_change_ms: parse(headers.get(row, "msBetweenDisplayChange")),
        flip_delay_ms: parse(headers.get(row, "msFlipDelay")),
        render_start_ms: parse(headers.get(row, "msUntilRenderStart")),
        gpu_active_ms: parse(headers.get_any(row, &["msGPUActive", "GPUBusy", "GPUTime"])),
        allows_tearing: parse_bool(headers.get(row, "AllowsTearing")),
        present_mode: text_value(headers.get(row, "PresentMode")),
        quality: quality.into(),
    })
}

fn parse<T: std::str::FromStr>(value: Option<&str>) -> Option<T> {
    clean_value(value?).trim().parse().ok()
}

fn parse_bool(value: Option<&str>) -> Option<bool> {
    match clean_value(value?).trim() {
        "1" | "true" | "True" => Some(true),
        "0" | "false" | "False" => Some(false),
        _ => None,
    }
}

fn text_value(value: Option<&str>) -> Option<String> {
    value
        .map(clean_value)
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn clean_value(value: &str) -> String {
    value
        .trim_start_matches('\u{feff}')
        .chars()
        .filter(|character| *character != '\0')
        .collect()
}

struct HeaderMap(HashMap<String, usize>);

impl HeaderMap {
    fn new(headers: &StringRecord) -> Self {
        Self(
            headers
                .iter()
                .enumerate()
                .map(|(index, name)| (canonical_header(name), index))
                .collect(),
        )
    }

    fn has_any(&self, names: &[&str]) -> bool {
        names
            .iter()
            .any(|name| self.0.contains_key(&canonical_header(name)))
    }

    fn get<'a>(&self, row: &'a StringRecord, name: &str) -> Option<&'a str> {
        self.0
            .get(&canonical_header(name))
            .and_then(|index| row.get(*index))
    }

    fn get_any<'a>(&self, row: &'a StringRecord, names: &[&str]) -> Option<&'a str> {
        names.iter().find_map(|name| self.get(row, name))
    }
}

fn canonical_header(name: &str) -> String {
    clean_value(name)
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

fn send_health(
    sender: &Sender<Record>,
    clock: &SessionClock,
    collector: &str,
    status: &str,
    detail: &str,
) {
    let _ = sender.try_send(Record::Health(CollectorHealth {
        time: clock.now(),
        collector: collector.into(),
        status: status.into(),
        detail: detail.into(),
    }));
}

fn concise_stderr(stderr: &str) -> String {
    stderr
        .lines()
        .find(|line| !line.trim().is_empty())
        .unwrap_or("no PresentMon diagnostic text")
        .trim()
        .chars()
        .take(300)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_presentmon_v1_row() {
        let headers = StringRecord::from(vec![
            "Application",
            "ProcessID",
            "SwapChainAddress",
            "PresentRuntime",
            "Dropped",
            "msBetweenPresents",
            "AllowsTearing",
            "PresentMode",
            "QPCTime",
        ]);
        let row = StringRecord::from(vec![
            "Game.exe",
            "4242",
            "0x1234",
            "DXGI",
            "0",
            "16.6667",
            "1",
            "Hardware: Independent Flip",
            "1000.5",
        ]);
        let frame = parse_record(&HeaderMap::new(&headers), &row, &SessionClock::new()).unwrap();
        assert_eq!(frame.pid, 4242);
        assert_eq!(frame.application.as_deref(), Some("Game.exe"));
        assert!((frame.fps.unwrap() - 60.0).abs() < 0.01);
        assert_eq!(frame.allows_tearing, Some(true));
        assert_eq!(frame.quality, "ok");
    }

    #[test]
    fn scans_past_preamble_and_accepts_presentmon_251_header() {
        let input = concat!(
            "PresentMon 2.5.1\n",
            "Application,ProcessID,SwapChainAddress,PresentRuntime,SyncInterval,PresentFlags,AllowsTearing,PresentMode,TimeInMs,MsBetweenSimulationStart,MsBetweenPresents,MsBetweenDisplayChange,MsInPresentAPI,MsRenderPresentLatency,MsUntilDisplayed,Dropped\n",
            "Game.exe,4242,0x1234,DXGI,1,0,1,Hardware: Independent Flip,1000500,16.6,16.6667,16.6,0.1,8.0,12.0,0\n"
        );
        let (sender, receiver) = crossbeam_channel::unbounded();
        let stats = parse_stream(input.as_bytes(), SessionClock::new(), sender).unwrap();
        assert_eq!(stats.rows, 1);
        let Record::Frame(frame) = receiver.recv().unwrap() else {
            panic!("expected a frame record");
        };
        assert_eq!(frame.pid, 4242);
        assert_eq!(frame.runtime.as_deref(), Some("DXGI"));
        assert!((frame.source_qpc_seconds.unwrap() - 1000.5).abs() < 0.001);
        assert_eq!(frame.render_complete_ms, Some(8.0));
    }

    #[test]
    fn accepts_v2_pid_and_frame_time_aliases() {
        let headers = StringRecord::from(vec!["Application", "PID", "FrameTime"]);
        let row = StringRecord::from(vec!["Game.exe", "99", "8.333"]);
        let frame = parse_record(&HeaderMap::new(&headers), &row, &SessionClock::new()).unwrap();
        assert_eq!(frame.pid, 99);
        assert!((frame.fps.unwrap() - 120.0).abs() < 0.01);
    }

    #[test]
    fn classifies_large_frame_times() {
        let headers = StringRecord::from(vec!["ProcessID", "msBetweenPresents", "Dropped"]);
        let row = StringRecord::from(vec!["1", "125", "0"]);
        let frame = parse_record(&HeaderMap::new(&headers), &row, &SessionClock::new()).unwrap();
        assert_eq!(frame.quality, "major_stutter");
    }
}
