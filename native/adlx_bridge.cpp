#include "adlx_bridge.h"

#include "SDK/ADLXHelper/Windows/Cpp/ADLXHelper.h"
#include "SDK/Include/IPerformanceMonitoring3.h"

#include <cstring>
#include <memory>
#include <string>
#include <vector>

using namespace adlx;

namespace {

void copy_text(const std::string& value, char* output, size_t capacity) {
    if (output == nullptr || capacity == 0) {
        return;
    }
    const size_t count = (value.size() < capacity - 1) ? value.size() : capacity - 1;
    std::memcpy(output, value.data(), count);
    output[count] = '\0';
}

void set_error(const std::string& value, char* output, size_t capacity) {
    copy_text(value, output, capacity);
}

bool supported(ADLX_RESULT result, adlx_bool value) {
    return ADLX_SUCCEEDED(result) && value;
}

struct GpuState {
    IADLXGPUPtr gpu;
    IADLXGPUMetricsSupportPtr support;
    IADLXGPUMetricsSupport1Ptr support1;
    IADLXGPUMetricsSupport2Ptr support2;
    IADLXGPUMetricsSupport3Ptr support3;
    std::string name;
    uint64_t supported_mask = 0;
};

struct AdlxContext {
    // ADLXHelper must outlive every smart interface below it. Members are destroyed in reverse order.
    ADLXHelper helper;
    IADLXPerformanceMonitoringServicesPtr performance;
    IADLXGPUListPtr gpu_list;
    std::vector<std::unique_ptr<GpuState>> gpus;
    std::string version;
};

void discover_base_metrics(GpuState& state) {
    adlx_bool value = false;
#define GCR_DISCOVER(method, bit)                         \
    value = false;                                        \
    {                                                     \
        ADLX_RESULT query_result = state.support->method(&value); \
        if (supported(query_result, value)) {             \
            state.supported_mask |= (bit);                \
        }                                                 \
    }
    GCR_DISCOVER(IsSupportedGPUUsage, GCR_ADLX_GPU_USAGE)
    GCR_DISCOVER(IsSupportedGPUClockSpeed, GCR_ADLX_GPU_CLOCK)
    GCR_DISCOVER(IsSupportedGPUVRAMClockSpeed, GCR_ADLX_VRAM_CLOCK)
    GCR_DISCOVER(IsSupportedGPUTemperature, GCR_ADLX_GPU_TEMPERATURE)
    GCR_DISCOVER(IsSupportedGPUHotspotTemperature, GCR_ADLX_HOTSPOT_TEMPERATURE)
    GCR_DISCOVER(IsSupportedGPUPower, GCR_ADLX_GPU_POWER)
    GCR_DISCOVER(IsSupportedGPUTotalBoardPower, GCR_ADLX_BOARD_POWER)
    GCR_DISCOVER(IsSupportedGPUFanSpeed, GCR_ADLX_FAN_SPEED)
    GCR_DISCOVER(IsSupportedGPUVRAM, GCR_ADLX_VRAM_USAGE)
    GCR_DISCOVER(IsSupportedGPUVoltage, GCR_ADLX_VOLTAGE)
    GCR_DISCOVER(IsSupportedGPUIntakeTemperature, GCR_ADLX_INTAKE_TEMPERATURE)
#undef GCR_DISCOVER

    if (state.support1) {
#define GCR_DISCOVER_1(method, bit)                        \
        value = false;                                     \
        {                                                  \
            ADLX_RESULT query_result = state.support1->method(&value); \
            if (supported(query_result, value)) {          \
                state.supported_mask |= (bit);             \
            }                                              \
        }
        GCR_DISCOVER_1(IsSupportedGPUMemoryTemperature, GCR_ADLX_MEMORY_TEMPERATURE)
        GCR_DISCOVER_1(IsSupportedNPUActivityLevel, GCR_ADLX_NPU_ACTIVITY)
        GCR_DISCOVER_1(IsSupportedNPUFrequency, GCR_ADLX_NPU_FREQUENCY)
#undef GCR_DISCOVER_1
    }
    if (state.support2) {
        value = false;
        ADLX_RESULT query_result = state.support2->IsSupportedGPUSharedMemory(&value);
        if (supported(query_result, value)) {
            state.supported_mask |= GCR_ADLX_SHARED_MEMORY;
        }
    }
    if (state.support3) {
        value = false;
        ADLX_RESULT query_result = state.support3->IsSupportedGPUFanDuty(&value);
        if (supported(query_result, value)) {
            state.supported_mask |= GCR_ADLX_FAN_DUTY;
        }
    }
}

}  // namespace

extern "C" void* gcr_adlx_create(char* error, size_t error_capacity) {
    try {
        std::unique_ptr<AdlxContext> context(new AdlxContext());
        ADLX_RESULT result = context->helper.Initialize();
        if (ADLX_FAILED(result)) {
            set_error("ADLX initialization failed with result " + std::to_string(result), error, error_capacity);
            return nullptr;
        }

        const char* version = context->helper.QueryVersion();
        context->version = version == nullptr ? "unknown" : version;
        IADLXSystem* system = context->helper.GetSystemServices();
        if (system == nullptr) {
            set_error("ADLX returned no system interface", error, error_capacity);
            return nullptr;
        }

        result = system->GetPerformanceMonitoringServices(&context->performance);
        if (ADLX_FAILED(result) || !context->performance) {
            set_error("ADLX performance monitoring service is unavailable", error, error_capacity);
            return nullptr;
        }
        result = system->GetGPUs(&context->gpu_list);
        if (ADLX_FAILED(result) || !context->gpu_list) {
            set_error("ADLX GPU list is unavailable", error, error_capacity);
            return nullptr;
        }

        for (adlx_uint index = context->gpu_list->Begin(); index != context->gpu_list->End(); ++index) {
            std::unique_ptr<GpuState> state(new GpuState());
            result = context->gpu_list->At(index, &state->gpu);
            if (ADLX_FAILED(result) || !state->gpu) {
                continue;
            }
            const char* name = nullptr;
            if (ADLX_SUCCEEDED(state->gpu->Name(&name)) && name != nullptr) {
                state->name = name;
            } else {
                state->name = "AMD GPU " + std::to_string(index);
            }
            result = context->performance->GetSupportedGPUMetrics(state->gpu, &state->support);
            if (ADLX_FAILED(result) || !state->support) {
                continue;
            }
            state->support1 = IADLXGPUMetricsSupport1Ptr(state->support);
            state->support2 = IADLXGPUMetricsSupport2Ptr(state->support);
            state->support3 = IADLXGPUMetricsSupport3Ptr(state->support);
            discover_base_metrics(*state);
            context->gpus.push_back(std::move(state));
        }

        if (context->gpus.empty()) {
            set_error("ADLX found no AMD GPU with performance monitoring support", error, error_capacity);
            return nullptr;
        }
        if (error != nullptr && error_capacity > 0) {
            error[0] = '\0';
        }
        return context.release();
    } catch (const std::exception& exception) {
        set_error(std::string("ADLX bridge exception: ") + exception.what(), error, error_capacity);
        return nullptr;
    } catch (...) {
        set_error("Unknown ADLX bridge exception", error, error_capacity);
        return nullptr;
    }
}

extern "C" void gcr_adlx_destroy(void* opaque) {
    delete static_cast<AdlxContext*>(opaque);
}

extern "C" int gcr_adlx_gpu_count(void* opaque) {
    const auto* context = static_cast<const AdlxContext*>(opaque);
    return context == nullptr ? 0 : static_cast<int>(context->gpus.size());
}

extern "C" int gcr_adlx_gpu_name(void* opaque, int index, char* output, size_t output_capacity) {
    const auto* context = static_cast<const AdlxContext*>(opaque);
    if (context == nullptr || index < 0 || static_cast<size_t>(index) >= context->gpus.size()) {
        return 0;
    }
    copy_text(context->gpus[static_cast<size_t>(index)]->name, output, output_capacity);
    return 1;
}

extern "C" int gcr_adlx_version(void* opaque, char* output, size_t output_capacity) {
    const auto* context = static_cast<const AdlxContext*>(opaque);
    if (context == nullptr) {
        return 0;
    }
    copy_text(context->version, output, output_capacity);
    return 1;
}

extern "C" int gcr_adlx_sample(
    void* opaque,
    int index,
    GcrAdlxSample* output,
    char* error,
    size_t error_capacity) {
    auto* context = static_cast<AdlxContext*>(opaque);
    if (context == nullptr || output == nullptr || index < 0 ||
        static_cast<size_t>(index) >= context->gpus.size()) {
        set_error("Invalid ADLX sample arguments", error, error_capacity);
        return 0;
    }
    *output = {};
    GpuState& state = *context->gpus[static_cast<size_t>(index)];
    IADLXGPUMetricsPtr metrics;
    ADLX_RESULT result = context->performance->GetCurrentGPUMetrics(state.gpu, &metrics);
    if (ADLX_FAILED(result) || !metrics) {
        set_error("GetCurrentGPUMetrics failed with result " + std::to_string(result), error, error_capacity);
        return 0;
    }

    metrics->TimeStamp(&output->source_timestamp_ms);
#define GCR_READ_DOUBLE(bit, method, field)       \
    if ((state.supported_mask & (bit)) != 0) {    \
        adlx_double value = 0;                    \
        if (ADLX_SUCCEEDED(metrics->method(&value))) { \
            output->field = value;                \
            output->valid_mask |= (bit);          \
        }                                         \
    }
#define GCR_READ_INT(bit, method, field)          \
    if ((state.supported_mask & (bit)) != 0) {    \
        adlx_int value = 0;                       \
        if (ADLX_SUCCEEDED(metrics->method(&value))) { \
            output->field = static_cast<double>(value); \
            output->valid_mask |= (bit);          \
        }                                         \
    }
    GCR_READ_DOUBLE(GCR_ADLX_GPU_USAGE, GPUUsage, gpu_usage_percent)
    GCR_READ_INT(GCR_ADLX_GPU_CLOCK, GPUClockSpeed, gpu_clock_mhz)
    GCR_READ_INT(GCR_ADLX_VRAM_CLOCK, GPUVRAMClockSpeed, vram_clock_mhz)
    GCR_READ_DOUBLE(GCR_ADLX_GPU_TEMPERATURE, GPUTemperature, gpu_temperature_c)
    GCR_READ_DOUBLE(GCR_ADLX_HOTSPOT_TEMPERATURE, GPUHotspotTemperature, hotspot_temperature_c)
    GCR_READ_DOUBLE(GCR_ADLX_GPU_POWER, GPUPower, gpu_power_w)
    GCR_READ_DOUBLE(GCR_ADLX_BOARD_POWER, GPUTotalBoardPower, total_board_power_w)
    GCR_READ_INT(GCR_ADLX_FAN_SPEED, GPUFanSpeed, fan_speed_rpm)
    GCR_READ_INT(GCR_ADLX_VRAM_USAGE, GPUVRAM, vram_usage_mb)
    GCR_READ_INT(GCR_ADLX_VOLTAGE, GPUVoltage, voltage_mv)
    GCR_READ_DOUBLE(GCR_ADLX_INTAKE_TEMPERATURE, GPUIntakeTemperature, intake_temperature_c)
#undef GCR_READ_DOUBLE
#undef GCR_READ_INT

    IADLXGPUMetrics1Ptr metrics1(metrics);
    if (metrics1) {
#define GCR_READ_1_DOUBLE(bit, method, field)          \
        if ((state.supported_mask & (bit)) != 0) {     \
            adlx_double value = 0;                     \
            if (ADLX_SUCCEEDED(metrics1->method(&value))) { \
                output->field = value;                 \
                output->valid_mask |= (bit);           \
            }                                          \
        }
#define GCR_READ_1_INT(bit, method, field)             \
        if ((state.supported_mask & (bit)) != 0) {     \
            adlx_int value = 0;                        \
            if (ADLX_SUCCEEDED(metrics1->method(&value))) { \
                output->field = static_cast<double>(value); \
                output->valid_mask |= (bit);           \
            }                                          \
        }
        GCR_READ_1_DOUBLE(GCR_ADLX_MEMORY_TEMPERATURE, GPUMemoryTemperature, memory_temperature_c)
        GCR_READ_1_INT(GCR_ADLX_NPU_ACTIVITY, NPUActivityLevel, npu_activity_percent)
        GCR_READ_1_INT(GCR_ADLX_NPU_FREQUENCY, NPUFrequency, npu_frequency_mhz)
#undef GCR_READ_1_DOUBLE
#undef GCR_READ_1_INT
    }
    IADLXGPUMetrics2Ptr metrics2(metrics);
    if (metrics2 && (state.supported_mask & GCR_ADLX_SHARED_MEMORY) != 0) {
        adlx_int value = 0;
        if (ADLX_SUCCEEDED(metrics2->GPUSharedMemory(&value))) {
            output->shared_memory_mb = static_cast<double>(value);
            output->valid_mask |= GCR_ADLX_SHARED_MEMORY;
        }
    }
    IADLXGPUMetrics3Ptr metrics3(metrics);
    if (metrics3 && (state.supported_mask & GCR_ADLX_FAN_DUTY) != 0) {
        adlx_int value = 0;
        if (ADLX_SUCCEEDED(metrics3->GPUFanDuty(&value))) {
            output->fan_duty_percent = static_cast<double>(value);
            output->valid_mask |= GCR_ADLX_FAN_DUTY;
        }
    }

    if (error != nullptr && error_capacity > 0) {
        error[0] = '\0';
    }
    return 1;
}
