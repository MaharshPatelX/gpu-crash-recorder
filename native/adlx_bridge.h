#pragma once

#include <stddef.h>
#include <stdint.h>

#ifdef __cplusplus
extern "C" {
#endif

enum GcrAdlxMetricMask : uint64_t {
    GCR_ADLX_GPU_USAGE = 1ull << 0,
    GCR_ADLX_GPU_CLOCK = 1ull << 1,
    GCR_ADLX_VRAM_CLOCK = 1ull << 2,
    GCR_ADLX_GPU_TEMPERATURE = 1ull << 3,
    GCR_ADLX_HOTSPOT_TEMPERATURE = 1ull << 4,
    GCR_ADLX_GPU_POWER = 1ull << 5,
    GCR_ADLX_BOARD_POWER = 1ull << 6,
    GCR_ADLX_FAN_SPEED = 1ull << 7,
    GCR_ADLX_VRAM_USAGE = 1ull << 8,
    GCR_ADLX_VOLTAGE = 1ull << 9,
    GCR_ADLX_INTAKE_TEMPERATURE = 1ull << 10,
    GCR_ADLX_MEMORY_TEMPERATURE = 1ull << 11,
    GCR_ADLX_SHARED_MEMORY = 1ull << 12,
    GCR_ADLX_FAN_DUTY = 1ull << 13,
    GCR_ADLX_NPU_ACTIVITY = 1ull << 14,
    GCR_ADLX_NPU_FREQUENCY = 1ull << 15,
};

typedef struct GcrAdlxSample {
    int64_t source_timestamp_ms;
    uint64_t valid_mask;
    double gpu_usage_percent;
    double gpu_clock_mhz;
    double vram_clock_mhz;
    double gpu_temperature_c;
    double hotspot_temperature_c;
    double gpu_power_w;
    double total_board_power_w;
    double fan_speed_rpm;
    double vram_usage_mb;
    double voltage_mv;
    double intake_temperature_c;
    double memory_temperature_c;
    double shared_memory_mb;
    double fan_duty_percent;
    double npu_activity_percent;
    double npu_frequency_mhz;
} GcrAdlxSample;

void* gcr_adlx_create(char* error, size_t error_capacity);
void gcr_adlx_destroy(void* context);
int gcr_adlx_gpu_count(void* context);
int gcr_adlx_gpu_name(void* context, int index, char* output, size_t output_capacity);
int gcr_adlx_version(void* context, char* output, size_t output_capacity);
int gcr_adlx_sample(
    void* context,
    int index,
    GcrAdlxSample* output,
    char* error,
    size_t error_capacity);

#ifdef __cplusplus
}
#endif
