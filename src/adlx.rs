use std::{ffi::c_void, ptr::NonNull};

use anyhow::{Context, Result};

const ERROR_CAPACITY: usize = 512;

pub const GPU_USAGE: u64 = 1 << 0;
pub const GPU_CLOCK: u64 = 1 << 1;
pub const VRAM_CLOCK: u64 = 1 << 2;
pub const GPU_TEMPERATURE: u64 = 1 << 3;
pub const HOTSPOT_TEMPERATURE: u64 = 1 << 4;
pub const GPU_POWER: u64 = 1 << 5;
pub const BOARD_POWER: u64 = 1 << 6;
pub const FAN_SPEED: u64 = 1 << 7;
pub const VRAM_USAGE: u64 = 1 << 8;
pub const VOLTAGE: u64 = 1 << 9;
pub const INTAKE_TEMPERATURE: u64 = 1 << 10;
pub const MEMORY_TEMPERATURE: u64 = 1 << 11;
pub const SHARED_MEMORY: u64 = 1 << 12;
pub const FAN_DUTY: u64 = 1 << 13;
pub const NPU_ACTIVITY: u64 = 1 << 14;
pub const NPU_FREQUENCY: u64 = 1 << 15;

#[repr(C)]
#[derive(Debug, Clone, Copy, Default)]
pub struct AdlxSample {
    pub source_timestamp_ms: i64,
    pub valid_mask: u64,
    pub gpu_usage_percent: f64,
    pub gpu_clock_mhz: f64,
    pub vram_clock_mhz: f64,
    pub gpu_temperature_c: f64,
    pub hotspot_temperature_c: f64,
    pub gpu_power_w: f64,
    pub total_board_power_w: f64,
    pub fan_speed_rpm: f64,
    pub vram_usage_mb: f64,
    pub voltage_mv: f64,
    pub intake_temperature_c: f64,
    pub memory_temperature_c: f64,
    pub shared_memory_mb: f64,
    pub fan_duty_percent: f64,
    pub npu_activity_percent: f64,
    pub npu_frequency_mhz: f64,
}

#[derive(Debug, Clone)]
pub struct AdlxGpu {
    pub index: i32,
    pub name: String,
}

pub struct AdlxCollector {
    context: NonNull<c_void>,
    pub version: String,
    pub gpus: Vec<AdlxGpu>,
}

// The opaque ADLX context is created, sampled, and destroyed on one collector thread.
unsafe impl Send for AdlxCollector {}

impl AdlxCollector {
    pub fn new() -> Result<Self> {
        let mut error = [0_i8; ERROR_CAPACITY];
        let context = unsafe { gcr_adlx_create(error.as_mut_ptr(), error.len()) };
        let context = NonNull::new(context).with_context(|| buffer_text(&error))?;
        let count = unsafe { gcr_adlx_gpu_count(context.as_ptr()) };
        if count <= 0 {
            unsafe { gcr_adlx_destroy(context.as_ptr()) };
            anyhow::bail!("ADLX initialized but returned no monitored GPUs");
        }

        let mut version_buffer = [0_i8; 128];
        unsafe {
            gcr_adlx_version(
                context.as_ptr(),
                version_buffer.as_mut_ptr(),
                version_buffer.len(),
            )
        };
        let version = buffer_text(&version_buffer);
        let mut gpus = Vec::with_capacity(count as usize);
        for index in 0..count {
            let mut name_buffer = [0_i8; 256];
            let succeeded = unsafe {
                gcr_adlx_gpu_name(
                    context.as_ptr(),
                    index,
                    name_buffer.as_mut_ptr(),
                    name_buffer.len(),
                )
            };
            let name = if succeeded != 0 {
                buffer_text(&name_buffer)
            } else {
                format!("AMD GPU {index}")
            };
            gpus.push(AdlxGpu { index, name });
        }

        Ok(Self {
            context,
            version,
            gpus,
        })
    }

    pub fn sample(&self, index: i32) -> Result<AdlxSample> {
        let mut sample = AdlxSample::default();
        let mut error = [0_i8; ERROR_CAPACITY];
        let succeeded = unsafe {
            gcr_adlx_sample(
                self.context.as_ptr(),
                index,
                &mut sample,
                error.as_mut_ptr(),
                error.len(),
            )
        };
        if succeeded == 0 {
            anyhow::bail!("{}", buffer_text(&error));
        }
        Ok(sample)
    }
}

impl Drop for AdlxCollector {
    fn drop(&mut self) {
        unsafe { gcr_adlx_destroy(self.context.as_ptr()) };
    }
}

fn buffer_text(buffer: &[i8]) -> String {
    let length = buffer
        .iter()
        .position(|value| *value == 0)
        .unwrap_or(buffer.len());
    let bytes: Vec<u8> = buffer[..length].iter().map(|value| *value as u8).collect();
    String::from_utf8_lossy(&bytes).trim().to_string()
}

unsafe extern "C" {
    fn gcr_adlx_create(error: *mut i8, error_capacity: usize) -> *mut c_void;
    fn gcr_adlx_destroy(context: *mut c_void);
    fn gcr_adlx_gpu_count(context: *mut c_void) -> i32;
    fn gcr_adlx_gpu_name(
        context: *mut c_void,
        index: i32,
        output: *mut i8,
        output_capacity: usize,
    ) -> i32;
    fn gcr_adlx_version(context: *mut c_void, output: *mut i8, output_capacity: usize) -> i32;
    fn gcr_adlx_sample(
        context: *mut c_void,
        index: i32,
        output: *mut AdlxSample,
        error: *mut i8,
        error_capacity: usize,
    ) -> i32;
}
