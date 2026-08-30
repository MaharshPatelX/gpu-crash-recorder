use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SampleTime {
    pub utc: DateTime<Utc>,
    pub monotonic_ns: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricSample {
    pub time: SampleTime,
    pub source: String,
    pub device: Option<String>,
    pub source_timestamp_ms: Option<i64>,
    pub metric: String,
    pub value: f64,
    pub unit: String,
    pub quality: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessSample {
    pub time: SampleTime,
    pub pid: u32,
    pub started_unix_s: u64,
    pub name: String,
    pub executable: Option<String>,
    pub cpu_percent: f32,
    pub memory_bytes: u64,
    pub virtual_memory_bytes: u64,
    pub disk_read_bytes: u64,
    pub disk_write_bytes: u64,
    pub is_foreground: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProcessEvent {
    pub time: SampleTime,
    pub kind: String,
    pub pid: u32,
    pub started_unix_s: u64,
    pub parent_pid: Option<u32>,
    pub name: String,
    pub executable: Option<String>,
    pub exit_code: Option<u32>,
    pub detail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ForegroundEvent {
    pub time: SampleTime,
    pub pid: u32,
    pub executable: Option<String>,
    pub window_title: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DiagnosticEvent {
    pub time: SampleTime,
    pub channel: String,
    pub provider: Option<String>,
    pub event_id: Option<u32>,
    pub level: Option<u8>,
    pub record_id: Option<u64>,
    pub message: Option<String>,
    pub raw_xml: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FrameSample {
    pub time: SampleTime,
    pub source_qpc_seconds: Option<f64>,
    pub pid: u32,
    pub application: Option<String>,
    pub swap_chain: Option<String>,
    pub runtime: Option<String>,
    pub sync_interval: Option<i32>,
    pub present_flags: Option<u32>,
    pub dropped: Option<bool>,
    pub frame_time_ms: Option<f64>,
    pub fps: Option<f64>,
    pub present_api_ms: Option<f64>,
    pub render_complete_ms: Option<f64>,
    pub displayed_ms: Option<f64>,
    pub display_change_ms: Option<f64>,
    pub flip_delay_ms: Option<f64>,
    pub render_start_ms: Option<f64>,
    pub gpu_active_ms: Option<f64>,
    pub allows_tearing: Option<bool>,
    pub present_mode: Option<String>,
    pub quality: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Bookmark {
    pub time: SampleTime,
    pub trigger: String,
    pub detail: String,
    pub related_pid: Option<u32>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectorHealth {
    pub time: SampleTime,
    pub collector: String,
    pub status: String,
    pub detail: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    pub time: SampleTime,
    pub original_path: String,
    pub copied_path: Option<String>,
    pub size_bytes: Option<u64>,
    pub modified_utc: Option<DateTime<Utc>>,
    pub sha256: Option<String>,
    pub status: String,
}

#[derive(Debug, Clone)]
pub enum Record {
    Metric(MetricSample),
    ProcessSample(ProcessSample),
    ProcessEvent(ProcessEvent),
    Foreground(ForegroundEvent),
    Event(DiagnosticEvent),
    Frame(FrameSample),
    Bookmark(Bookmark),
    Health(CollectorHealth),
    Artifact(ArtifactRecord),
    ClockCalibration(SampleTime),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionManifest {
    pub schema_version: u32,
    pub app_version: String,
    pub session_id: String,
    pub started_utc: DateTime<Utc>,
    pub stopped_utc: Option<DateTime<Utc>>,
    pub state: String,
    pub recovered: bool,
    pub output_directory: String,
}
