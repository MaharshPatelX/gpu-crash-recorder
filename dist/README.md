# GPU Crash Recorder 0.1.0

1. Run `GPUCrashRecorder.exe` and accept the administrator prompt.
2. Press `Ctrl+Alt+F10` to start recording. The tray icon turns red.
3. Reproduce the crash or problem.
4. Optionally press `Ctrl+Alt+F11` during a severe stutter or visual problem to add a marker.
5. Press `Ctrl+Alt+F10` again to stop and finalize the report.
6. Right-click the tray icon and choose **Open Latest Report**.

Sessions are saved under `%LOCALAPPDATA%\GPUCrashRecorder\Sessions\YYYY-MM-DD\`.

This application only observes the machine. It does not tune the GPU, inject into a game, change clocks or voltages, or upload data.

This beta executable is not code-signed, so Windows SmartScreen may show a warning. Reports stay on this PC, but they can contain process paths, Windows events, hardware identifiers, and application memory in crash dumps. Review a report before sharing it.

The AMD ADLX runtime is supplied by an installed AMD Radeon display driver. If ADLX is unavailable, the system, process, Event Log, artifact, and PresentMon collectors continue and the report records the missing capability.

Project: https://github.com/MaharshPatelX/gpu-crash-recorder
