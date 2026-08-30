# GPU Crash Recorder — Standalone System-Wide Recorder

## Summary

Build a standalone Windows x64 Rust application that lives in the system tray and records a complete diagnostic session between two hotkey presses.

No game or process selection is required. The recorder observes the whole system, automatically identifies foreground and crashing applications, and correlates GPU telemetry, frame timing, process activity, Windows events, driver failures, and crash artifacts.

Default workflow:

1. Launch the application as administrator.
2. Press `Ctrl+Alt+F10` to start recording.
3. Use any game or application.
4. After a crash, press the hotkey again.
5. Open the generated timestamped session folder and HTML report.

The application remains diagnostic-only: no injection, tuning, optimization, overclocking, undervolting, or network uploads.

## Tray Application

- Build with stable Rust and the MSVC toolchain.
- Run without a permanent main window or dashboard.
- Show a gray tray icon while idle and red while recording.
- Tray menu:
  - Start/Stop Recording
  - Add Manual Marker
  - Recording duration
  - Open Current Session
  - Open Latest Report
  - Open Sessions Folder
  - Settings
  - Exit
- Default hotkeys:
  - `Ctrl+Alt+F10`: start or stop recording.
  - `Ctrl+Alt+F11`: add a marker without stopping, useful for severe stutters or visual corruption.
- Use native Windows notifications to confirm start, stop, recovery, and collector failures.
- Include only a small settings dialog; there is no live graphing interface.
- Require elevation through the application manifest.
- Enforce a single instance and expose `start`, `stop`, `mark`, `status`, and `open-latest` commands over a secured local named pipe so Windows shortcuts can also control the recorder.

## System-Wide Recording

### Time synchronization

- Give every record:
  - precise UTC timestamp;
  - Query Performance Counter timestamp;
  - source-native timestamp when available;
  - source and sequence identifiers;
  - quality and availability flags.
- Capture UTC/QPC calibration pairs at startup and every 30 seconds.
- Use monotonic time for correlation so Windows clock adjustments cannot reorder evidence.

### AMD GPU telemetry

- Load the driver-installed AMD ADLX runtime through a small native C bridge.
- Monitor every detected AMD GPU rather than assuming a particular device number.
- Query supported capabilities at runtime.
- Record all available metrics, including:
  - GPU utilization;
  - core and VRAM clocks;
  - VRAM usage;
  - edge, hotspot, intake, and memory temperature when exposed;
  - GPU and total-board power;
  - voltage;
  - fan speed;
  - performance-state-related metrics available through the installed ADLX version.
- Request 250 ms sampling or the closest supported ADLX interval.
- Never call ADLX tuning interfaces.

### CPU and memory

- Record once per second:
  - total and per-core CPU utilization;
  - available Windows-reported CPU clocks;
  - physical and available RAM;
  - committed memory and commit limit;
  - page-file use;
  - paged and nonpaged pools;
  - handle, thread, and process counts;
  - memory/resource-pressure notifications.
- The standalone v1 will not require HWiNFO or bundle a low-level sensor driver.
- CPU temperature and package power will be explicitly reported as unavailable when Windows cannot expose them safely.
- Leave a provider interface for optional hardware-sensor support in a later version.

### Process activity

- Use system-wide ETW process events to record every process start and stop.
- Track PID plus creation time to prevent PID-reuse errors.
- Record executable path, parent PID, command line when accessible, start/end time, and exit status.
- Use foreground-window events to build a timeline of which application the user was actively using.
- Sample detailed CPU, RAM, I/O, handle, thread, and GPU-engine usage for:
  - the foreground process;
  - its process tree;
  - processes with meaningful GPU activity;
  - recently foregrounded processes.
- A nonzero exit creates an automatic bookmark but does not stop recording.
- Classify uncertain exits as “abnormal or externally terminated,” not automatically as crashes.

### FPS and frame time

- Bundle a pinned PresentMon component inside the application distribution and launch it invisibly.
- The user will not need to install or operate PresentMon separately.
- Observe all presenting processes, then retain compact per-frame data for foreground and GPU-active applications.
- Record:
  - frame time;
  - derived FPS;
  - present runtime/mode;
  - dropped or discarded frames;
  - swap-chain identity;
  - frame-time spikes;
  - percentile and stutter statistics.
- Do not inject into game processes.

### Windows and driver events

- Subscribe in real time to relevant Windows Event Log channels.
- Store raw event XML and rendered messages.
- Include:
  - WHEA-Logger;
  - PCIe hardware errors;
  - Display event 4101;
  - `amdwddmg` and `amdkmdag`;
  - DxgKrnl, DXGI, and DirectX-related events;
  - Application Error;
  - Windows Error Reporting;
  - Kernel-Power;
  - resource exhaustion;
  - unexpected shutdown and bug-check information.
- Backfill the entire session interval at finalization in case a live subscription missed an event.
- Query Reliability Monitor records created during the session.
- Critical WHEA, display-driver, WER, or application-crash events create automatic bookmarks while recording continues.

### Static system information

Capture at session start and stop:

- Windows edition, version, build, and boot time;
- CPU model and topology;
- physical DIMM sizes, speeds, manufacturers, and part numbers when exposed;
- motherboard and BIOS information;
- GPUs, device IDs, VBIOS information, and driver versions;
- PCIe device topology and reported link capabilities;
- storage, page-file, and memory configuration;
- Hardware-Accelerated GPU Scheduling and related graphics settings;
- relevant overlays, hardware monitors, RGB tools, capture programs, and background applications.

## Storage and Session Lifecycle

- Default session root:

```text
%LOCALAPPDATA%\GPUCrashRecorder\Sessions\
```

- Organize sessions by date:

```text
Sessions/
└── 2026-08-30/
    └── SESSION_23-41-18/
        ├── session.sqlite
        ├── manifest.json
        ├── summary.json
        ├── report.html
        ├── telemetry.csv
        ├── processes.csv
        ├── frames.csv
        ├── windows-events.jsonl
        ├── system-info.json
        ├── crash-windows/
        ├── dumps/
        ├── crash-artifacts/
        └── logs/
```

- Create the folder as `SESSION_23-41-18.recording` while active and rename it after successful finalization.
- SQLite is the authoritative live store.
- Use WAL mode, full synchronization, prepared statements, and one-second transaction batches.
- Database tables cover sessions, time calibration, metric samples, processes, process samples, frames, events, bookmarks, artifacts, system snapshots, and collector health.
- Keep a five-minute in-memory ring for fast bookmark snapshots while retaining the complete session in SQLite.
- Generate CSV, JSON/JSONL, and HTML exports after the stop hotkey.
- Never delete sessions automatically.

## Dumps and Crash Artifacts

- During first-run setup, offer an explicit opt-in for session-only Windows minidumps.
- When enabled:
  - journal the existing global WER `LocalDumps` configuration;
  - direct minidumps from applications crashing during the recording window into the active session;
  - restore the exact previous registry state when recording stops;
  - restore interrupted settings automatically on the next launch.
- Do not enable full-memory dumps by default.
- Detect new or modified files in:
  - `%LOCALAPPDATA%\CrashDumps`;
  - Windows WER ReportArchive and ReportQueue;
  - `%WINDIR%\LiveKernelReports`;
  - configurable application crash directories.
- Support user-configured game and application crash directories without making any title a built-in target.
- Copy minidumps and reasonably sized reports into the session.
- For large kernel dumps, store their original path, size, timestamp, hash, and accessibility rather than duplicating them automatically.

## Report and Diagnostic Findings

Generate a self-contained offline `report.html` containing:

- synchronized system timeline;
- GPU telemetry charts;
- CPU and memory charts;
- foreground-application history;
- frame-time and FPS graphs;
- major stutter markers;
- process exits and exception codes;
- Windows, WHEA, driver, and WER events;
- crash dump and artifact links;
- system and driver inventory;
- collector availability and recording gaps.

For each automatic or manual bookmark, generate a detailed window covering five minutes before and one minute after the bookmark or until recording stops.

Findings may identify evidence consistent with:

- AMD driver reset or timeout;
- WHEA or PCIe hardware failure;
- thermal or power abnormality;
- VRAM pressure or clock instability;
- resource exhaustion;
- CPU or memory-related Windows errors;
- application-only abnormal termination;
- insufficient or conflicting evidence.

Every finding must reference its supporting timestamps, metrics, events, or artifacts. Correlation must never be presented as definitive proof of root cause.

## Recovery and Reliability

- Mark active sessions as dirty until finalization completes.
- Flush bookmarks immediately.
- On startup, recover unfinished SQLite/WAL data and restore journaled WER settings.
- Detect whether the previous session ended because of application termination, Windows shutdown, freeze, bug check, or power loss when evidence is available.
- After reboot, query Kernel-Power, WHEA, bug-check, display-driver, and WER events associated with the unfinished session.
- Record dropped samples, collector restarts, queue overruns, permission failures, ADLX failures, PresentMon failures, and timestamp discontinuities.
- A collector failure must not terminate other collectors.

## Test Plan

- Unit-test timestamp correlation, clock changes, database migrations, exit-code handling, bookmarks, registry restoration, artifact deduplication, spike detection, and diagnostic rules.
- Test helper applications that:
  - exit normally;
  - return nonzero;
  - raise an exception;
  - corrupt a heap;
  - hang;
  - are externally terminated.
- Test missing or failed ADLX, PresentMon, Event Log, and performance-counter collectors.
- Terminate the recorder during an active session and verify recovery and WER restoration.
- Test normal Windows shutdown and simulated dirty-session recovery.
- Run a six-hour high-FPS recording and validate bounded memory, database integrity, report generation, and storage growth.
- Validate GPU values against AMD Software: Adrenalin Edition across available AMD test hardware.
- Acceptance targets:
  - no game injection;
  - no performance tuning;
  - no network traffic;
  - no loss of more than the current one-second uncommitted batch;
  - memory below 200 MB during normal recording;
  - average recorder overhead below 2% total CPU on the target system;
  - every requested field shown as recorded, unsupported, unavailable, or failed—never silently missing.

## Defaults and Boundaries

- Initial platform: Windows 10/11 x64.
- Initial vendor-specific telemetry: AMD ADLX.
- Application type: portable elevated tray application without an installer or updater.
- Recording control: fully manual start and stop.
- Process selection: none required.
- Default GPU interval: 250 ms.
- Default system interval: one second.
- Default detailed-process interval: 500 ms.
- Frame capture: per present for foreground and GPU-active applications.
- No HWiNFO or third-party monitoring application is required.
- No persistent dashboard or overlay is included.
- The application is generic; game-specific crash folders are user-configured rather than core dependencies.
