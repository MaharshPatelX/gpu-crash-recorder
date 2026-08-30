use std::time::Instant;

use chrono::{Duration, Utc};
use windows::Win32::System::Performance::{QueryPerformanceCounter, QueryPerformanceFrequency};

use crate::model::SampleTime;

#[derive(Clone)]
pub struct SessionClock {
    origin: Instant,
    origin_utc: chrono::DateTime<Utc>,
    origin_qpc: Option<(i64, i64)>,
}

impl SessionClock {
    pub fn new() -> Self {
        let origin_utc = Utc::now();
        let origin = Instant::now();
        let origin_qpc = query_performance_counter();
        Self {
            origin,
            origin_utc,
            origin_qpc,
        }
    }

    pub fn now(&self) -> SampleTime {
        SampleTime {
            utc: Utc::now(),
            monotonic_ns: self.origin.elapsed().as_nanos().min(u64::MAX as u128) as u64,
        }
    }

    pub fn started_utc(&self) -> chrono::DateTime<Utc> {
        self.origin_utc
    }

    pub fn from_utc(&self, utc: chrono::DateTime<Utc>) -> SampleTime {
        let delta_ns = utc
            .signed_duration_since(self.origin_utc)
            .num_nanoseconds()
            .unwrap_or(if utc < self.origin_utc {
                i64::MIN
            } else {
                i64::MAX
            });
        SampleTime {
            utc,
            monotonic_ns: delta_ns.max(0) as u64,
        }
    }

    /// Convert PresentMon's QPC time (seconds) to this session's UTC and monotonic axes.
    /// Falls back to receipt time if Windows QPC calibration is unavailable.
    pub fn from_qpc_seconds(&self, qpc_seconds: f64) -> SampleTime {
        let Some((origin_ticks, frequency)) = self.origin_qpc else {
            return self.now();
        };
        if !qpc_seconds.is_finite() || frequency <= 0 {
            return self.now();
        }

        let source_ticks = qpc_seconds * frequency as f64;
        let delta_ns =
            ((source_ticks - origin_ticks as f64) * 1_000_000_000.0 / frequency as f64).round();
        if !delta_ns.is_finite() {
            return self.now();
        }
        let monotonic_ns = delta_ns.max(0.0).min(u64::MAX as f64) as u64;
        let signed_ns = delta_ns.max(i64::MIN as f64).min(i64::MAX as f64) as i64;
        let utc = self.origin_utc + Duration::nanoseconds(signed_ns);
        SampleTime { utc, monotonic_ns }
    }
}

fn query_performance_counter() -> Option<(i64, i64)> {
    let mut counter = 0_i64;
    let mut frequency = 0_i64;
    unsafe {
        QueryPerformanceCounter(&mut counter).ok()?;
        QueryPerformanceFrequency(&mut frequency).ok()?;
    }
    (frequency > 0).then_some((counter, frequency))
}

impl Default for SessionClock {
    fn default() -> Self {
        Self::new()
    }
}
