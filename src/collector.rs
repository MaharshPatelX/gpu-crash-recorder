use std::{
    collections::{HashMap, HashSet},
    ffi::OsStr,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
    },
    thread::{self, JoinHandle},
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use chrono::{Duration as ChronoDuration, Utc};
use crossbeam_channel::Sender;
use sysinfo::System;
use windows::Win32::System::SystemInformation::{GlobalMemoryStatusEx, MEMORYSTATUSEX};
use windows::Win32::UI::WindowsAndMessaging::{
    GetForegroundWindow, GetWindowTextLengthW, GetWindowTextW, GetWindowThreadProcessId,
};

use crate::{
    adlx::{
        AdlxCollector, BOARD_POWER, FAN_DUTY, FAN_SPEED, GPU_CLOCK, GPU_POWER, GPU_TEMPERATURE,
        GPU_USAGE, HOTSPOT_TEMPERATURE, INTAKE_TEMPERATURE, MEMORY_TEMPERATURE, NPU_ACTIVITY,
        NPU_FREQUENCY, SHARED_MEMORY, VOLTAGE, VRAM_CLOCK, VRAM_USAGE,
    },
    config::AppConfig,
    events::{collect_relevant_events_since, is_live_kernel_event_code},
    model::{
        Bookmark, CollectorHealth, ForegroundEvent, MetricSample, ProcessEvent, ProcessSample,
        Record,
    },
    presentmon,
    timestamp::SessionClock,
};

pub struct CollectorGroup {
    stop: Arc<AtomicBool>,
    joins: Vec<JoinHandle<Result<()>>>,
}

impl CollectorGroup {
    pub fn start(config: AppConfig, clock: SessionClock, sender: Sender<Record>) -> Result<Self> {
        let stop = Arc::new(AtomicBool::new(false));
        let system_stop = Arc::clone(&stop);
        let system_config = config.clone();
        let system_clock = clock.clone();
        let system_sender = sender.clone();
        let system_join = thread::Builder::new()
            .name("system-collector".into())
            .spawn(move || {
                run_system_collector(system_config, system_clock, system_sender, system_stop)
            })
            .context("failed to start system collector")?;

        let adlx_stop = Arc::clone(&stop);
        let adlx_config = config.clone();
        let adlx_clock = clock.clone();
        let adlx_sender = sender.clone();
        let adlx_join = thread::Builder::new()
            .name("amd-adlx-collector".into())
            .spawn(move || run_adlx_collector(adlx_config, adlx_clock, adlx_sender, adlx_stop))
            .context("failed to start AMD ADLX collector")?;

        let presentmon_stop = Arc::clone(&stop);
        let presentmon_clock = clock.clone();
        let presentmon_sender = sender.clone();
        let presentmon_join = thread::Builder::new()
            .name("presentmon-collector".into())
            .spawn(move || presentmon::run(presentmon_clock, presentmon_sender, presentmon_stop))
            .context("failed to start PresentMon collector")?;

        let event_stop = Arc::clone(&stop);
        let event_interval = config.event_poll_seconds;
        let event_join = thread::Builder::new()
            .name("windows-event-collector".into())
            .spawn(move || run_windows_event_collector(event_interval, clock, sender, event_stop))
            .context("failed to start Windows Event Log collector")?;

        Ok(Self {
            stop,
            joins: vec![system_join, adlx_join, presentmon_join, event_join],
        })
    }

    pub fn stop(self) -> Vec<anyhow::Error> {
        self.stop.store(true, Ordering::Release);
        let mut errors = Vec::new();
        for join in self.joins {
            match join.join() {
                Ok(Ok(())) => {}
                Ok(Err(error)) => errors.push(error),
                Err(_) => errors.push(anyhow::anyhow!("collector thread panicked")),
            }
        }
        errors
    }
}

fn run_windows_event_collector(
    poll_seconds: u64,
    clock: SessionClock,
    sender: Sender<Record>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    send_health(
        &sender,
        &clock,
        "windows_event_log",
        "running",
        "Polling synchronized System and Application event evidence",
    );
    let session_started = clock.started_utc();
    let mut query_from = session_started;
    let mut seen = HashSet::new();
    let mut reported_warnings = HashSet::new();
    let mut stored_events = 0_u64;
    let poll_interval = Duration::from_secs(poll_seconds.max(5));

    loop {
        let final_pass = stop.load(Ordering::Acquire);
        let query_started = Utc::now();
        let collection = collect_relevant_events_since(query_from, session_started, &clock);
        for warning in collection.warnings {
            if reported_warnings.insert(warning.clone()) {
                send_health(&sender, &clock, "windows_event_log", "degraded", &warning);
            }
        }
        for event in collection.events {
            let key = format!(
                "{}:{}:{}:{}",
                event.channel,
                event.record_id.unwrap_or(0),
                event.event_id.unwrap_or(0),
                event.time.utc.timestamp_nanos_opt().unwrap_or(0)
            );
            if !seen.insert(key) {
                continue;
            }
            if let Some((trigger, detail)) = event_bookmark(&event) {
                let _ = sender.try_send(Record::Bookmark(Bookmark {
                    time: event.time.clone(),
                    trigger,
                    detail,
                    related_pid: None,
                }));
            }
            if sender.try_send(Record::Event(event)).is_ok() {
                stored_events += 1;
            }
        }
        query_from = query_started - ChronoDuration::seconds(1);

        if final_pass {
            break;
        }
        let wait_started = Instant::now();
        while wait_started.elapsed() < poll_interval && !stop.load(Ordering::Acquire) {
            thread::sleep(Duration::from_millis(100));
        }
    }

    send_health(
        &sender,
        &clock,
        "windows_event_log",
        "stopped",
        &format!("Windows Event Log collector stopped; {stored_events} unique events stored"),
    );
    Ok(())
}

pub(crate) fn event_bookmark(event: &crate::model::DiagnosticEvent) -> Option<(String, String)> {
    let provider = event.provider.as_deref().unwrap_or_default();
    let provider_lower = provider.to_ascii_lowercase();
    let level = event.level.unwrap_or(4);
    let event_id = event.event_id.unwrap_or(0);
    let trigger = if provider_lower.contains("whea") {
        "whea_hardware_event"
    } else if provider_lower == "display"
        || provider_lower.contains("amdwddmg")
        || provider_lower.contains("amdkmdag")
        || provider_lower.contains("dxgkrnl")
    {
        if level > 3 && event_id != 4101 {
            return None;
        }
        "display_driver_event"
    } else if provider_lower.contains("application error")
        || provider_lower.contains("application hang")
    {
        "application_failure_event"
    } else if provider_lower.contains("windows error reporting") {
        if is_live_kernel_event_code(event, 141) {
            "live_kernel_event_141"
        } else {
            "windows_error_reporting_event"
        }
    } else if provider_lower.contains("bugcheck") {
        "kernel_failure_event"
    } else if provider_lower.contains("kernel-power") {
        if event_id != 41 && level > 2 {
            return None;
        }
        "kernel_failure_event"
    } else {
        return None;
    };
    Some((
        trigger.into(),
        format!(
            "{} event {} in {}{}",
            provider,
            event.event_id.unwrap_or(0),
            event.channel,
            event
                .message
                .as_deref()
                .map(|message| format!(": {}", message.chars().take(240).collect::<String>()))
                .unwrap_or_default()
        ),
    ))
}

#[derive(Clone)]
struct KnownProcess {
    pid: u32,
    started_unix_s: u64,
    parent_pid: Option<u32>,
    name: String,
    executable: Option<String>,
}

fn run_system_collector(
    config: AppConfig,
    clock: SessionClock,
    sender: Sender<Record>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    send_health(
        &sender,
        &clock,
        "system",
        "running",
        "Windows system and process collector started",
    );
    send_health(
        &sender,
        &clock,
        "cpu_hardware_sensors",
        "unsupported",
        "Standalone mode does not install a low-level CPU sensor driver",
    );

    let mut system = System::new_all();
    let mut known: HashMap<(u32, u64), KnownProcess> = HashMap::new();
    let mut foreground_pid = 0_u32;
    let mut recently_foreground: HashSet<u32> = HashSet::new();
    let process_interval = Duration::from_millis(config.process_sample_ms.max(250));
    let system_interval = Duration::from_millis(config.system_sample_ms.max(500));
    let mut last_system = Instant::now() - system_interval;
    let mut last_calibration = Instant::now() - Duration::from_secs(30);
    let mut first_snapshot = true;

    while !stop.load(Ordering::Acquire) {
        let iteration_started = Instant::now();
        system.refresh_all();
        let now = clock.now();

        if last_calibration.elapsed() >= Duration::from_secs(30) {
            let _ = sender.try_send(Record::ClockCalibration(now.clone()));
            last_calibration = Instant::now();
        }

        let (current_foreground, title) = foreground_process();
        if current_foreground != foreground_pid {
            foreground_pid = current_foreground;
            if foreground_pid != 0 {
                recently_foreground.insert(foreground_pid);
            }
            let executable = system
                .process(sysinfo::Pid::from_u32(foreground_pid))
                .and_then(|process| process.exe())
                .map(path_to_string);
            let _ = sender.try_send(Record::Foreground(ForegroundEvent {
                time: now.clone(),
                pid: foreground_pid,
                executable,
                window_title: title,
            }));
        }

        let mut current = HashMap::with_capacity(system.processes().len());
        for (pid, process) in system.processes() {
            let identity = (pid.as_u32(), process.start_time());
            let details = KnownProcess {
                pid: pid.as_u32(),
                started_unix_s: process.start_time(),
                parent_pid: process.parent().map(|parent| parent.as_u32()),
                name: os_to_string(process.name()),
                executable: process.exe().map(path_to_string),
            };

            if !known.contains_key(&identity) {
                let _ = sender.try_send(Record::ProcessEvent(ProcessEvent {
                    time: now.clone(),
                    kind: if first_snapshot { "observed" } else { "start" }.into(),
                    pid: details.pid,
                    started_unix_s: details.started_unix_s,
                    parent_pid: details.parent_pid,
                    name: details.name.clone(),
                    executable: details.executable.clone(),
                    exit_code: None,
                    detail: None,
                }));
            }

            let is_foreground = details.pid == foreground_pid;
            let should_sample = is_foreground
                || process.cpu_usage() >= 1.0
                || process.memory() >= 256 * 1024 * 1024
                || recently_foreground.contains(&details.pid);
            if should_sample {
                let disk = process.disk_usage();
                let _ = sender.try_send(Record::ProcessSample(ProcessSample {
                    time: now.clone(),
                    pid: details.pid,
                    started_unix_s: details.started_unix_s,
                    name: details.name.clone(),
                    executable: details.executable.clone(),
                    cpu_percent: process.cpu_usage(),
                    memory_bytes: process.memory(),
                    virtual_memory_bytes: process.virtual_memory(),
                    disk_read_bytes: disk.total_read_bytes,
                    disk_write_bytes: disk.total_written_bytes,
                    is_foreground,
                }));
            }
            current.insert(identity, details);
        }

        for (identity, stopped_process) in known.drain() {
            if current.contains_key(&identity) {
                continue;
            }
            let was_foreground = recently_foreground.remove(&stopped_process.pid);
            let _ = sender.try_send(Record::ProcessEvent(ProcessEvent {
                time: now.clone(),
                kind: "stop".into(),
                pid: stopped_process.pid,
                started_unix_s: stopped_process.started_unix_s,
                parent_pid: stopped_process.parent_pid,
                name: stopped_process.name.clone(),
                executable: stopped_process.executable.clone(),
                exit_code: None,
                detail: Some("Exit code unavailable from polling collector".into()),
            }));
            if was_foreground {
                let _ = sender.try_send(Record::Bookmark(Bookmark {
                    time: now.clone(),
                    trigger: "foreground_process_exit".into(),
                    detail: format!(
                        "Recently foreground process {} (PID {}) exited; exit status is unknown",
                        stopped_process.name, stopped_process.pid
                    ),
                    related_pid: Some(stopped_process.pid),
                }));
            }
        }
        known = current;
        first_snapshot = false;

        if last_system.elapsed() >= system_interval {
            record_system_metrics(&sender, &clock, &system);
            last_system = Instant::now();
        }

        let elapsed = iteration_started.elapsed();
        if elapsed < process_interval {
            thread::sleep(process_interval - elapsed);
        }
    }

    send_health(
        &sender,
        &clock,
        "system",
        "stopped",
        "Windows system and process collector stopped cleanly",
    );
    Ok(())
}

fn run_adlx_collector(
    config: AppConfig,
    clock: SessionClock,
    sender: Sender<Record>,
    stop: Arc<AtomicBool>,
) -> Result<()> {
    let collector = match AdlxCollector::new() {
        Ok(collector) => collector,
        Err(error) => {
            send_health(
                &sender,
                &clock,
                "amd_adlx",
                "unavailable",
                &format!("{error:#}"),
            );
            return Ok(());
        }
    };
    let gpu_description = collector
        .gpus
        .iter()
        .map(|gpu| gpu.name.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    send_health(
        &sender,
        &clock,
        "amd_adlx",
        "running",
        &format!("ADLX {} monitoring: {gpu_description}", collector.version),
    );

    let interval = Duration::from_millis(config.gpu_sample_ms.max(100));
    let mut last_error = Instant::now() - Duration::from_secs(30);
    while !stop.load(Ordering::Acquire) {
        let started = Instant::now();
        for gpu in &collector.gpus {
            match collector.sample(gpu.index) {
                Ok(sample) => {
                    let time = clock.now();
                    let source_timestamp =
                        (sample.source_timestamp_ms != 0).then_some(sample.source_timestamp_ms);
                    for (bit, metric, value, unit) in [
                        (
                            GPU_USAGE,
                            "gpu.utilization",
                            sample.gpu_usage_percent,
                            "percent",
                        ),
                        (GPU_CLOCK, "gpu.core_clock", sample.gpu_clock_mhz, "MHz"),
                        (VRAM_CLOCK, "gpu.vram_clock", sample.vram_clock_mhz, "MHz"),
                        (
                            GPU_TEMPERATURE,
                            "gpu.temperature",
                            sample.gpu_temperature_c,
                            "C",
                        ),
                        (
                            HOTSPOT_TEMPERATURE,
                            "gpu.hotspot_temperature",
                            sample.hotspot_temperature_c,
                            "C",
                        ),
                        (GPU_POWER, "gpu.power", sample.gpu_power_w, "W"),
                        (
                            BOARD_POWER,
                            "gpu.total_board_power",
                            sample.total_board_power_w,
                            "W",
                        ),
                        (FAN_SPEED, "gpu.fan_speed", sample.fan_speed_rpm, "RPM"),
                        (VRAM_USAGE, "gpu.vram_usage", sample.vram_usage_mb, "MB"),
                        (VOLTAGE, "gpu.voltage", sample.voltage_mv, "mV"),
                        (
                            INTAKE_TEMPERATURE,
                            "gpu.intake_temperature",
                            sample.intake_temperature_c,
                            "C",
                        ),
                        (
                            MEMORY_TEMPERATURE,
                            "gpu.memory_temperature",
                            sample.memory_temperature_c,
                            "C",
                        ),
                        (
                            SHARED_MEMORY,
                            "gpu.shared_memory",
                            sample.shared_memory_mb,
                            "MB",
                        ),
                        (FAN_DUTY, "gpu.fan_duty", sample.fan_duty_percent, "percent"),
                        (
                            NPU_ACTIVITY,
                            "npu.activity",
                            sample.npu_activity_percent,
                            "percent",
                        ),
                        (
                            NPU_FREQUENCY,
                            "npu.frequency",
                            sample.npu_frequency_mhz,
                            "MHz",
                        ),
                    ] {
                        if sample.valid_mask & bit == 0 {
                            continue;
                        }
                        let _ = sender.try_send(Record::Metric(MetricSample {
                            time: time.clone(),
                            source: "amd_adlx".into(),
                            device: Some(gpu.name.clone()),
                            source_timestamp_ms: source_timestamp,
                            metric: metric.into(),
                            value,
                            unit: unit.into(),
                            quality: "ok".into(),
                        }));
                    }
                }
                Err(error) if last_error.elapsed() >= Duration::from_secs(30) => {
                    send_health(
                        &sender,
                        &clock,
                        "amd_adlx",
                        "degraded",
                        &format!("GPU sample failed: {error:#}"),
                    );
                    last_error = Instant::now();
                }
                Err(_) => {}
            }
        }

        let elapsed = started.elapsed();
        if elapsed < interval {
            thread::sleep(interval - elapsed);
        }
    }

    send_health(
        &sender,
        &clock,
        "amd_adlx",
        "stopped",
        "ADLX collector stopped cleanly",
    );
    Ok(())
}

fn record_system_metrics(sender: &Sender<Record>, clock: &SessionClock, system: &System) {
    let time = clock.now();
    let metrics = [
        (
            "cpu.utilization",
            system.global_cpu_usage() as f64,
            "percent",
        ),
        ("memory.total", system.total_memory() as f64, "bytes"),
        ("memory.used", system.used_memory() as f64, "bytes"),
        (
            "memory.available",
            system.available_memory() as f64,
            "bytes",
        ),
        ("memory.total_swap", system.total_swap() as f64, "bytes"),
        ("memory.used_swap", system.used_swap() as f64, "bytes"),
        ("process.count", system.processes().len() as f64, "count"),
    ];
    for (metric, value, unit) in metrics {
        let _ = sender.try_send(Record::Metric(MetricSample {
            time: time.clone(),
            source: "windows".into(),
            device: None,
            source_timestamp_ms: None,
            metric: metric.into(),
            value,
            unit: unit.into(),
            quality: "ok".into(),
        }));
    }

    if let Some(memory) = windows_memory_status() {
        for (metric, value, unit) in [
            ("memory.load", memory.dwMemoryLoad as f64, "percent"),
            ("memory.physical_total", memory.ullTotalPhys as f64, "bytes"),
            (
                "memory.physical_available",
                memory.ullAvailPhys as f64,
                "bytes",
            ),
            (
                "memory.commit_limit",
                memory.ullTotalPageFile as f64,
                "bytes",
            ),
            (
                "memory.committed",
                memory
                    .ullTotalPageFile
                    .saturating_sub(memory.ullAvailPageFile) as f64,
                "bytes",
            ),
            (
                "memory.commit_available",
                memory.ullAvailPageFile as f64,
                "bytes",
            ),
            (
                "memory.virtual_total",
                memory.ullTotalVirtual as f64,
                "bytes",
            ),
            (
                "memory.virtual_available",
                memory.ullAvailVirtual as f64,
                "bytes",
            ),
        ] {
            let _ = sender.try_send(Record::Metric(MetricSample {
                time: time.clone(),
                source: "windows_memory".into(),
                device: None,
                source_timestamp_ms: None,
                metric: metric.into(),
                value,
                unit: unit.into(),
                quality: "ok".into(),
            }));
        }
    }

    for (index, cpu) in system.cpus().iter().enumerate() {
        for (suffix, value, unit) in [
            ("utilization", cpu.cpu_usage() as f64, "percent"),
            ("frequency", cpu.frequency() as f64, "MHz"),
        ] {
            let _ = sender.try_send(Record::Metric(MetricSample {
                time: time.clone(),
                source: "windows".into(),
                device: Some(format!("cpu.{index}")),
                source_timestamp_ms: None,
                metric: format!("cpu.core.{suffix}"),
                value,
                unit: unit.into(),
                quality: "ok".into(),
            }));
        }
    }
}

fn windows_memory_status() -> Option<MEMORYSTATUSEX> {
    let mut status = MEMORYSTATUSEX {
        dwLength: std::mem::size_of::<MEMORYSTATUSEX>() as u32,
        ..Default::default()
    };
    unsafe { GlobalMemoryStatusEx(&mut status).ok()? };
    Some(status)
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

fn foreground_process() -> (u32, Option<String>) {
    unsafe {
        let window = GetForegroundWindow();
        if window.0.is_null() {
            return (0, None);
        }

        let mut pid = 0_u32;
        GetWindowThreadProcessId(window, Some(&mut pid));
        let length = GetWindowTextLengthW(window);
        if length <= 0 {
            return (pid, None);
        }
        let mut buffer = vec![0_u16; length as usize + 1];
        let written = GetWindowTextW(window, &mut buffer);
        let title = (written > 0).then(|| String::from_utf16_lossy(&buffer[..written as usize]));
        (pid, title)
    }
}

fn os_to_string(value: &OsStr) -> String {
    value.to_string_lossy().into_owned()
}

fn path_to_string(value: &std::path::Path) -> String {
    value.to_string_lossy().into_owned()
}
