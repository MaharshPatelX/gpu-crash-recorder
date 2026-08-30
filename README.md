# GPU Crash Recorder

GPU Crash Recorder is an AMD-first Windows diagnostic tool for unexplained game, GPU, and graphics-driver crashes. Press one hotkey to begin a synchronized system-wide recording, reproduce the problem, then press the hotkey again to generate a complete offline report.

It is observational only: it does not tune clocks or voltage, inject into games, install a driver, or upload data.

> **Beta:** The recorder has been tested on Windows 11 with an AMD Radeon RX 7900 XTX. It is useful today, but hardware and Windows-event coverage varies by system. Always treat automated findings as evidence, not proof of a root cause.

## What it records

- AMD GPU utilization, clocks, VRAM, temperatures, board power, voltage, and fans through ADLX when the card exposes them;
- CPU load, memory pressure, process activity, and foreground-window history;
- system-wide frame times, FPS, presentation mode, latency, dropped frames, tearing, and stutters through PresentMon;
- WHEA, display-driver, LiveKernelEvent, application-crash, WER, kernel, and related Windows events;
- new crash dumps, WER reports, LiveKernelReports, and user-configured game crash folders;
- Windows, BIOS, motherboard, CPU, RAM, GPU, driver, storage, and page-file inventory;
- timestamped bookmarks and focused five-minute-before/one-minute-after crash windows.

Every collector uses a shared timeline. If one source is unavailable, the others keep recording and the report explains the gap.

## Download and use

Download the Windows x64 ZIP from [GitHub Releases](https://github.com/MaharshPatelX/gpu-crash-recorder/releases). This early build is unsigned, so Windows SmartScreen may show a warning.

1. Run `GPUCrashRecorder.exe` and accept the administrator prompt.
2. Press `Ctrl+Alt+F10` to start recording. The tray icon turns red.
3. Reproduce the crash, freeze, stutter, or visual problem.
4. Optionally press `Ctrl+Alt+F11` to add a manual marker.
5. Press `Ctrl+Alt+F10` again to stop and generate the report.
6. Right-click the tray icon and select **Open Latest Report**.

Sessions are stored under:

```text
%LOCALAPPDATA%\GPUCrashRecorder\Sessions\YYYY-MM-DD\SESSION_HH-MM-SS\
```

The main output is a self-contained `report.html`. The session also retains its authoritative SQLite database and CSV, JSON, and JSONL exports for deeper investigation.

## Privacy

Nothing is uploaded automatically. Reports stay on your PC, but they can contain process names and paths, hardware identifiers, Windows events, and crash dumps. Review a session before sharing it publicly. Crash dumps may contain fragments of application memory.

## AMD and other GPUs

AMD cards receive detailed GPU telemetry through the ADLX runtime installed with AMD Software: Adrenalin Edition. Metrics depend on the GPU and driver, and unsupported values are marked unavailable.

On a non-AMD system, the recorder can still collect system, process, Event Log, crash-artifact, and PresentMon evidence, but vendor-specific GPU telemetry is not implemented yet. The collector design is vendor-neutral so NVIDIA and Intel providers can be added later.

## Configuration

The tray menu opens `%LOCALAPPDATA%\GPUCrashRecorder\config.json`. Restart the application after editing it.

- Start/stop hotkey: `Ctrl+Alt+F10`
- Manual marker hotkey: `Ctrl+Alt+F11`
- AMD GPU sample interval: 250 ms
- System sample interval: 1 second
- Process sample interval: 500 ms

Artifact paths are additive. Generic Windows crash locations are included by default; add a game or application's own crash directory in `artifact_paths` when needed.

## Build from source

Requirements:

- Windows 10 or 11 x64;
- Rust stable with the `x86_64-pc-windows-msvc` target;
- Visual Studio Build Tools with the MSVC C++ workload and Windows SDK.

Clone the ADLX submodule and build:

```powershell
git clone --recurse-submodules https://github.com/MaharshPatelX/gpu-crash-recorder.git
cd gpu-crash-recorder
cargo build --release
```

The executable is written to `target\release\gpu-crash-recorder.exe`. PresentMon 2.5.1 is embedded and checksum-verified before use. The application manifest requests administrator privileges for reliable ETW, Event Log, process, and crash-artifact access.

Run the checks with:

```powershell
cargo fmt --all -- --check
cargo test --lib
cargo check --all-targets
```

## Current limitations

- Windows 10/11 x64 only; AMD is the first fully supported GPU vendor.
- The executable is not code-signed and there is no installer or auto-updater.
- CPU temperature and package power require a future optional sensor provider.
- Polling can observe a process exit without knowing its exact exit code unless Windows records separate crash evidence.
- The recorder collects existing crash dumps but does not change the global WER `LocalDumps` policy.
- Low-level PCIe state not exposed by ADLX or Windows is inferred from WHEA and device/event evidence; no kernel driver is installed.

See [plan.md](plan.md) for the detailed architecture, [CONTRIBUTING.md](CONTRIBUTING.md) for development guidance, and [THIRD_PARTY_NOTICES.md](THIRD_PARTY_NOTICES.md) for bundled components.

## License

GPU Crash Recorder is available under the [MIT License](LICENSE). Third-party components retain their own licenses.
