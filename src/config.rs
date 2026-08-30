use std::{fs, path::PathBuf};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub sessions_root: PathBuf,
    pub toggle_hotkey: String,
    pub marker_hotkey: String,
    pub gpu_sample_ms: u64,
    pub system_sample_ms: u64,
    pub process_sample_ms: u64,
    pub event_poll_seconds: u64,
    pub enable_session_minidumps: bool,
    pub artifact_paths: Vec<PathBuf>,
}

impl Default for AppConfig {
    fn default() -> Self {
        let local = dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("GPUCrashRecorder");
        let local_app_data = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));

        Self {
            sessions_root: local.join("Sessions"),
            toggle_hotkey: "Ctrl+Alt+F10".to_string(),
            marker_hotkey: "Ctrl+Alt+F11".to_string(),
            gpu_sample_ms: 250,
            system_sample_ms: 1_000,
            process_sample_ms: 500,
            event_poll_seconds: 15,
            enable_session_minidumps: false,
            artifact_paths: default_artifact_paths(&local_app_data),
        }
    }
}

impl AppConfig {
    pub fn config_path() -> PathBuf {
        dirs::data_local_dir()
            .unwrap_or_else(|| PathBuf::from("."))
            .join("GPUCrashRecorder")
            .join("config.json")
    }

    pub fn load_or_create() -> Result<Self> {
        let path = Self::config_path();
        if path.exists() {
            let text = fs::read_to_string(&path)
                .with_context(|| format!("failed to read {}", path.display()))?;
            let mut config: Self = serde_json::from_str(&text)
                .with_context(|| format!("failed to parse {}", path.display()))?;
            config.add_builtin_artifact_paths();
            config.save()?;
            return Ok(config);
        }

        let config = Self::default();
        config.save()?;
        Ok(config)
    }

    pub fn save(&self) -> Result<()> {
        let path = Self::config_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)
                .with_context(|| format!("failed to create {}", parent.display()))?;
        }
        let json = serde_json::to_string_pretty(self)?;
        fs::write(&path, json).with_context(|| format!("failed to write {}", path.display()))
    }

    fn add_builtin_artifact_paths(&mut self) {
        let local = dirs::data_local_dir().unwrap_or_else(|| PathBuf::from("."));
        for path in default_artifact_paths(&local) {
            if !self.artifact_paths.iter().any(|existing| {
                existing
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&path.to_string_lossy())
            }) {
                self.artifact_paths.push(path);
            }
        }
    }
}

fn default_artifact_paths(local_app_data: &std::path::Path) -> Vec<PathBuf> {
    let mut paths = vec![
        local_app_data.join("CrashDumps"),
        local_app_data
            .join("Microsoft")
            .join("Windows")
            .join("WER")
            .join("ReportArchive"),
        local_app_data
            .join("Microsoft")
            .join("Windows")
            .join("WER")
            .join("ReportQueue"),
    ];
    if let Some(program_data) = std::env::var_os("PROGRAMDATA") {
        let wer = PathBuf::from(program_data)
            .join("Microsoft")
            .join("Windows")
            .join("WER");
        paths.push(wer.join("ReportArchive"));
        paths.push(wer.join("ReportQueue"));
    }
    if let Some(windows) = std::env::var_os("WINDIR") {
        paths.push(PathBuf::from(windows).join("LiveKernelReports"));
    }
    paths
}

#[cfg(test)]
mod tests {
    use std::path::Path;

    use super::default_artifact_paths;

    #[test]
    fn default_artifact_paths_are_system_wide() {
        let local = Path::new(r"C:\Users\Example\AppData\Local");
        let paths = default_artifact_paths(local);

        assert!(paths.contains(&local.join("CrashDumps")));
        assert!(
            paths.contains(
                &local
                    .join("Microsoft")
                    .join("Windows")
                    .join("WER")
                    .join("ReportArchive")
            )
        );
        assert!(
            paths
                .iter()
                .all(|path| !path.to_string_lossy().contains("Saved\\Crashes"))
        );
    }
}
