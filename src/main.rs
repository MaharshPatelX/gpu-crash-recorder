#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::{
    env,
    os::windows::ffi::OsStrExt,
    path::{Path, PathBuf},
    process::ExitCode,
    time::{Duration, Instant},
};

use anyhow::{Context, Result};
use global_hotkey::{
    GlobalHotKeyEvent, GlobalHotKeyManager,
    hotkey::{Code, HotKey, Modifiers},
};
use gpu_crash_recorder::{
    config::AppConfig,
    session::RecordingSession,
    worker::{RecorderCommand, RecorderEvent, RecorderWorker},
};
use single_instance::SingleInstance;
use tray_icon::{
    Icon, TrayIcon, TrayIconBuilder,
    menu::{Menu, MenuEvent, MenuItem, PredefinedMenuItem},
};
use windows::{
    Win32::UI::{
        Shell::ShellExecuteW,
        WindowsAndMessaging::{MB_ICONERROR, MB_OK, MessageBoxW, SW_SHOWNORMAL},
    },
    core::PCWSTR,
};
use winit::{
    event::Event,
    event_loop::{ControlFlow, EventLoop},
};

fn main() -> ExitCode {
    match run() {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => {
            eprintln!("GPU Crash Recorder failed: {error:#}");
            show_error_dialog(&format!("GPU Crash Recorder could not start:\n\n{error:#}"));
            ExitCode::FAILURE
        }
    }
}

fn show_error_dialog(message: &str) {
    let title: Vec<u16> = "GPU Crash Recorder".encode_utf16().chain(Some(0)).collect();
    let message: Vec<u16> = message.encode_utf16().chain(Some(0)).collect();
    unsafe {
        let _ = MessageBoxW(
            None,
            PCWSTR(message.as_ptr()),
            PCWSTR(title.as_ptr()),
            MB_OK | MB_ICONERROR,
        );
    }
}

fn run() -> Result<()> {
    let config = AppConfig::load_or_create()?;
    if let Some(seconds) = smoke_seconds() {
        return run_smoke(config, seconds);
    }

    let instance = SingleInstance::new("GPUCrashRecorder.SystemWideTray")?;
    if !instance.is_single() {
        anyhow::bail!("GPU Crash Recorder is already running");
    }

    let event_loop = EventLoop::new()?;
    event_loop.set_control_flow(ControlFlow::WaitUntil(
        Instant::now() + Duration::from_millis(100),
    ));

    let menu = Menu::new();
    let status = MenuItem::new("Status: Idle", false, None);
    let start_stop = MenuItem::new(
        format!("Start Recording    {}", config.toggle_hotkey),
        true,
        None,
    );
    let marker = MenuItem::new(
        format!("Add Manual Marker    {}", config.marker_hotkey),
        false,
        None,
    );
    let open_current = MenuItem::new("Open Current Session", false, None);
    let open_latest = MenuItem::new("Open Latest Report", true, None);
    let open_sessions = MenuItem::new("Open Sessions Folder", true, None);
    let open_settings = MenuItem::new("Open Settings File", true, None);
    let exit = MenuItem::new("Exit", true, None);
    menu.append_items(&[
        &status,
        &PredefinedMenuItem::separator(),
        &start_stop,
        &marker,
        &PredefinedMenuItem::separator(),
        &open_current,
        &open_latest,
        &open_sessions,
        &open_settings,
        &PredefinedMenuItem::separator(),
        &exit,
    ])?;

    let idle_icon = make_icon([124, 134, 147, 255])?;
    let recording_icon = make_icon([238, 45, 67, 255])?;
    let tray = TrayIconBuilder::new()
        .with_menu(Box::new(menu))
        .with_tooltip("GPU Crash Recorder — Idle")
        .with_icon(idle_icon.clone())
        .build()?;

    let hotkey_manager = GlobalHotKeyManager::new()?;
    let toggle_hotkey = parse_hotkey(&config.toggle_hotkey)
        .with_context(|| format!("invalid toggle hotkey {}", config.toggle_hotkey))?;
    let marker_hotkey = parse_hotkey(&config.marker_hotkey)
        .with_context(|| format!("invalid marker hotkey {}", config.marker_hotkey))?;
    hotkey_manager
        .register(toggle_hotkey)
        .context("the recording hotkey is already in use")?;
    hotkey_manager
        .register(marker_hotkey)
        .context("the marker hotkey is already in use")?;

    let worker = RecorderWorker::spawn(config.clone())?;
    let mut ui = TrayState {
        config,
        worker,
        tray,
        idle_icon,
        recording_icon,
        status,
        start_stop,
        marker,
        open_current,
        open_latest,
        open_sessions,
        open_settings,
        exit,
        toggle_hotkey_id: toggle_hotkey.id(),
        marker_hotkey_id: marker_hotkey.id(),
        recording_started: None,
        current_directory: None,
        latest_directory: None,
        closing: false,
    };

    #[allow(deprecated)]
    event_loop.run(move |event, event_loop| {
        if matches!(event, Event::AboutToWait) {
            ui.handle_events(event_loop);
            event_loop.set_control_flow(ControlFlow::WaitUntil(
                Instant::now() + Duration::from_millis(100),
            ));
        }
    })?;
    Ok(())
}

struct TrayState {
    config: AppConfig,
    worker: RecorderWorker,
    tray: TrayIcon,
    idle_icon: Icon,
    recording_icon: Icon,
    status: MenuItem,
    start_stop: MenuItem,
    marker: MenuItem,
    open_current: MenuItem,
    open_latest: MenuItem,
    open_sessions: MenuItem,
    open_settings: MenuItem,
    exit: MenuItem,
    toggle_hotkey_id: u32,
    marker_hotkey_id: u32,
    recording_started: Option<Instant>,
    current_directory: Option<PathBuf>,
    latest_directory: Option<PathBuf>,
    closing: bool,
}

impl TrayState {
    fn handle_events(&mut self, event_loop: &winit::event_loop::ActiveEventLoop) {
        while let Ok(event) = MenuEvent::receiver().try_recv() {
            if event.id == self.start_stop.id() {
                let _ = self.worker.commands.send(RecorderCommand::Toggle);
            } else if event.id == self.marker.id() {
                let _ = self
                    .worker
                    .commands
                    .send(RecorderCommand::Mark("Manual marker from tray".into()));
            } else if event.id == self.open_current.id() {
                if let Some(path) = self.current_directory.as_deref() {
                    let _ = open_path(path);
                }
            } else if event.id == self.open_latest.id() {
                if let Some(path) = self.latest_directory.as_deref() {
                    let report = path.join("report.html");
                    let _ = open_path(if report.exists() { &report } else { path });
                } else {
                    let _ = open_path(&self.config.sessions_root);
                }
            } else if event.id == self.open_sessions.id() {
                let _ = open_path(&self.config.sessions_root);
            } else if event.id == self.open_settings.id() {
                let _ = open_path(&AppConfig::config_path());
            } else if event.id == self.exit.id() && !self.closing {
                self.closing = true;
                self.status.set_text("Status: Shutting down");
                self.start_stop.set_enabled(false);
                self.marker.set_enabled(false);
                let _ = self.worker.commands.send(RecorderCommand::Shutdown);
            }
        }

        while let Ok(event) = GlobalHotKeyEvent::receiver().try_recv() {
            if event.id == self.toggle_hotkey_id {
                let _ = self.worker.commands.send(RecorderCommand::Toggle);
            } else if event.id == self.marker_hotkey_id && self.recording_started.is_some() {
                let _ = self
                    .worker
                    .commands
                    .send(RecorderCommand::Mark("Manual hotkey marker".into()));
            }
        }

        while let Ok(event) = self.worker.events.try_recv() {
            match event {
                RecorderEvent::Idle => {
                    self.recording_started = None;
                    self.current_directory = None;
                    self.status.set_text("Status: Idle");
                    self.start_stop
                        .set_text(format!("Start Recording    {}", self.config.toggle_hotkey));
                    self.start_stop.set_enabled(!self.closing);
                    self.marker.set_enabled(false);
                    self.open_current.set_enabled(false);
                    let _ = self.tray.set_icon(Some(self.idle_icon.clone()));
                    let _ = self.tray.set_tooltip(Some("GPU Crash Recorder — Idle"));
                }
                RecorderEvent::Starting => {
                    self.status.set_text("Status: Starting…");
                    self.start_stop.set_enabled(false);
                }
                RecorderEvent::Recording { directory } => {
                    self.recording_started = Some(Instant::now());
                    self.current_directory = Some(directory);
                    self.status.set_text("Status: Recording 00:00:00");
                    self.start_stop
                        .set_text(format!("Stop Recording    {}", self.config.toggle_hotkey));
                    self.start_stop.set_enabled(true);
                    self.marker.set_enabled(true);
                    self.open_current.set_enabled(true);
                    let _ = self.tray.set_icon(Some(self.recording_icon.clone()));
                    let _ = self
                        .tray
                        .set_tooltip(Some("GPU Crash Recorder — RECORDING"));
                }
                RecorderEvent::Finalizing => {
                    self.status.set_text("Status: Finalizing report…");
                    self.start_stop.set_enabled(false);
                    self.marker.set_enabled(false);
                }
                RecorderEvent::Stopped { directory } => {
                    self.latest_directory = Some(directory);
                    self.open_latest.set_enabled(true);
                }
                RecorderEvent::Recovered { directories } => {
                    self.latest_directory = directories.last().cloned();
                }
                RecorderEvent::Error { operation, message } => {
                    self.status.set_text(format!(
                        "Error during {operation}: {}",
                        one_line(&message, 72)
                    ));
                    let _ = self.tray.set_tooltip(Some(format!(
                        "GPU Crash Recorder error: {}",
                        one_line(&message, 100)
                    )));
                }
                RecorderEvent::ShutdownComplete => event_loop.exit(),
            }
        }

        if let Some(started) = self.recording_started {
            let seconds = started.elapsed().as_secs();
            self.status.set_text(format!(
                "Status: Recording {:02}:{:02}:{:02}",
                seconds / 3600,
                (seconds / 60) % 60,
                seconds % 60
            ));
        }
    }
}

fn make_icon(color: [u8; 4]) -> Result<Icon> {
    let size = 32_u32;
    let mut rgba = Vec::with_capacity((size * size * 4) as usize);
    let center = (size as f32 - 1.0) / 2.0;
    for y in 0..size {
        for x in 0..size {
            let dx = x as f32 - center;
            let dy = y as f32 - center;
            if (dx * dx + dy * dy).sqrt() <= center - 2.0 {
                rgba.extend_from_slice(&color);
            } else {
                rgba.extend_from_slice(&[0, 0, 0, 0]);
            }
        }
    }
    Icon::from_rgba(rgba, size, size).context("failed to create tray icon")
}

fn open_path(path: &Path) -> Result<()> {
    let path_wide: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let operation: Vec<u16> = "open".encode_utf16().chain(Some(0)).collect();
    unsafe {
        let result = ShellExecuteW(
            None,
            PCWSTR(operation.as_ptr()),
            PCWSTR(path_wide.as_ptr()),
            PCWSTR::null(),
            PCWSTR::null(),
            SW_SHOWNORMAL,
        );
        if result.0 as isize <= 32 {
            anyhow::bail!("Windows could not open {}", path.display());
        }
    }
    Ok(())
}

fn one_line(value: &str, max_chars: usize) -> String {
    value
        .replace(['\r', '\n'], " ")
        .chars()
        .take(max_chars)
        .collect()
}

fn parse_hotkey(specification: &str) -> Result<HotKey> {
    let mut modifiers = Modifiers::empty();
    let mut code = None;
    for token in specification.split('+').map(str::trim) {
        match token.to_ascii_lowercase().as_str() {
            "ctrl" | "control" => modifiers |= Modifiers::CONTROL,
            "alt" => modifiers |= Modifiers::ALT,
            "shift" => modifiers |= Modifiers::SHIFT,
            "win" | "windows" | "super" => modifiers |= Modifiers::SUPER,
            key => {
                if code.is_some() {
                    anyhow::bail!("hotkey contains more than one non-modifier key");
                }
                code = Some(match key {
                    "f1" => Code::F1,
                    "f2" => Code::F2,
                    "f3" => Code::F3,
                    "f4" => Code::F4,
                    "f5" => Code::F5,
                    "f6" => Code::F6,
                    "f7" => Code::F7,
                    "f8" => Code::F8,
                    "f9" => Code::F9,
                    "f10" => Code::F10,
                    "f11" => Code::F11,
                    "f12" => Code::F12,
                    "f13" => Code::F13,
                    "f14" => Code::F14,
                    "f15" => Code::F15,
                    "f16" => Code::F16,
                    "f17" => Code::F17,
                    "f18" => Code::F18,
                    "f19" => Code::F19,
                    "f20" => Code::F20,
                    "f21" => Code::F21,
                    "f22" => Code::F22,
                    "f23" => Code::F23,
                    "f24" => Code::F24,
                    _ => anyhow::bail!(
                        "unsupported key {token:?}; this build accepts F1 through F24"
                    ),
                });
            }
        }
    }
    let code = code.context("hotkey has no function key")?;
    Ok(HotKey::new(
        (!modifiers.is_empty()).then_some(modifiers),
        code,
    ))
}

fn smoke_seconds() -> Option<u64> {
    let mut arguments = env::args().skip(1);
    while let Some(argument) = arguments.next() {
        if argument == "--smoke-seconds" {
            return arguments.next().and_then(|value| value.parse().ok());
        }
    }
    None
}

fn run_smoke(mut config: AppConfig, seconds: u64) -> Result<()> {
    if let Ok(root) = env::var("GPU_CRASH_RECORDER_TEST_ROOT") {
        config.sessions_root = PathBuf::from(root);
    }
    println!("Starting {seconds}-second diagnostic smoke session…");
    let session = RecordingSession::start(config)?;
    println!("Recording to {}", session.directory().display());
    std::thread::sleep(Duration::from_secs(seconds));
    let output = session.stop()?;
    println!("Finalized {}", output.display());
    Ok(())
}
