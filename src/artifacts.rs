use std::{
    collections::{BTreeMap, HashSet},
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use anyhow::{Context, Result};
use chrono::{DateTime, Local, TimeZone, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{
    events::embedded_live_kernel_dump_local_time, model::ArtifactRecord, timestamp::SessionClock,
};

const MAX_SCAN_FILES: usize = 100_000;
const MAX_SCAN_DEPTH: usize = 12;
const MAX_COPY_FILE_BYTES: u64 = 128 * 1024 * 1024;
const MAX_TOTAL_COPY_BYTES: u64 = 512 * 1024 * 1024;

#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
struct Fingerprint {
    size_bytes: u64,
    modified_unix_ms: Option<u64>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ArtifactBaseline {
    roots: Vec<PathBuf>,
    files: BTreeMap<String, Fingerprint>,
    warnings: Vec<String>,
}

#[derive(Debug, Default)]
pub struct ArtifactCollection {
    pub records: Vec<ArtifactRecord>,
    pub signals: Vec<ArtifactSignal>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone)]
pub struct ArtifactSignal {
    pub utc: DateTime<Utc>,
    pub trigger: String,
    pub detail: String,
}

impl ArtifactBaseline {
    pub fn capture(roots: &[PathBuf], log_path: &Path) -> Result<Self> {
        let (files, warnings) = scan_roots(roots, None);
        let baseline = Self {
            roots: deduplicate_roots(roots),
            files,
            warnings,
        };
        fs::write(log_path, serde_json::to_string_pretty(&baseline)?)
            .with_context(|| format!("failed to write {}", log_path.display()))?;
        Ok(baseline)
    }

    pub fn watched_root_count(&self) -> usize {
        self.roots.len()
    }

    pub fn baseline_file_count(&self) -> usize {
        self.files.len()
    }

    pub fn collect(&self, session_dir: &Path, clock: &SessionClock) -> Result<ArtifactCollection> {
        let (current, mut warnings) = scan_roots(&self.roots, Some(session_dir));
        warnings.extend(self.warnings.iter().cloned());
        let destination_root = session_dir.join("crash-artifacts");
        fs::create_dir_all(&destination_root)?;
        let mut records = Vec::new();
        let mut signals = Vec::new();
        let mut signal_keys = HashSet::new();
        let mut copied_total = 0_u64;

        for (original_text, fingerprint) in current {
            let change = match self.files.get(&original_text) {
                None => "new",
                Some(previous) if previous != &fingerprint => "modified",
                Some(_) => continue,
            };
            let original = PathBuf::from(&original_text);
            if let Some(signal) = live_kernel_signal(&original, clock) {
                let key = format!("{}:{}", signal.trigger, signal.utc.timestamp() / 60);
                if signal_keys.insert(key) {
                    signals.push(signal);
                }
            }
            let modified_utc = fingerprint.modified_unix_ms.and_then(|millis| {
                DateTime::<Utc>::from_timestamp_millis(millis.min(i64::MAX as u64) as i64)
            });
            let should_copy = fingerprint.size_bytes <= MAX_COPY_FILE_BYTES
                && copied_total.saturating_add(fingerprint.size_bytes) <= MAX_TOTAL_COPY_BYTES;

            let (copied_path, sha256, status) = if should_copy {
                let destination = unique_destination(&destination_root, &original);
                match copy_and_hash(&original, &destination) {
                    Ok(hash) => {
                        copied_total = copied_total.saturating_add(fingerprint.size_bytes);
                        (
                            Some(relative_text(session_dir, &destination)),
                            Some(hash),
                            format!("copied_{change}"),
                        )
                    }
                    Err(error) => {
                        warnings.push(format!(
                            "Could not copy artifact {}: {error:#}",
                            original.display()
                        ));
                        (
                            None,
                            hash_file(&original).ok(),
                            format!("copy_failed_{change}"),
                        )
                    }
                }
            } else {
                (
                    None,
                    hash_file(&original).ok(),
                    format!("referenced_large_{change}"),
                )
            };

            records.push(ArtifactRecord {
                time: clock.now(),
                original_path: original_text,
                copied_path,
                size_bytes: Some(fingerprint.size_bytes),
                modified_utc,
                sha256,
                status,
            });
        }

        let collection = ArtifactCollection {
            records,
            signals,
            warnings,
        };
        fs::write(
            session_dir.join("artifacts.json"),
            serde_json::to_string_pretty(&collection.records)?,
        )?;
        Ok(collection)
    }
}

fn live_kernel_signal(path: &Path, clock: &SessionClock) -> Option<ArtifactSignal> {
    if !path
        .file_name()
        .and_then(|value| value.to_str())
        .is_some_and(|name| name.eq_ignore_ascii_case("Report.wer"))
    {
        return None;
    }
    let text = read_text_lossy(path).ok()?;
    if wer_live_kernel_code(&text) != Some(141) {
        return None;
    }
    let dump_local = embedded_live_kernel_dump_local_time(&text)?;
    let session_local = clock.started_utc().with_timezone(&Local).naive_local();
    let now_local = Local::now().naive_local();
    if dump_local < session_local - chrono::Duration::minutes(2)
        || dump_local > now_local + chrono::Duration::minutes(2)
    {
        return None;
    }
    let dump_utc = Local
        .from_local_datetime(&dump_local)
        .earliest()?
        .with_timezone(&Utc);
    Some(ArtifactSignal {
        utc: dump_utc,
        trigger: "live_kernel_event_141".into(),
        detail: format!(
            "New Windows LiveKernelEvent 141 GPU watchdog report: {}",
            path.display()
        ),
    })
}

fn wer_live_kernel_code(text: &str) -> Option<u32> {
    let fields: BTreeMap<String, String> = text
        .lines()
        .filter_map(|line| line.trim().split_once('='))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_string()))
        .collect();
    if !fields
        .get("eventtype")
        .is_some_and(|value| value.eq_ignore_ascii_case("LiveKernelEvent"))
    {
        return None;
    }
    for (key, value) in &fields {
        if !key.ends_with(".name") || !value.eq_ignore_ascii_case("Code") {
            continue;
        }
        let value_key = format!("{}.value", key.trim_end_matches(".name"));
        if let Some(code) = fields.get(&value_key).and_then(|value| value.parse().ok()) {
            return Some(code);
        }
    }
    fields.get("code").and_then(|value| value.parse().ok())
}

fn read_text_lossy(path: &Path) -> Result<String> {
    let bytes = fs::read(path)?;
    let utf16_le = bytes.starts_with(&[0xff, 0xfe])
        || bytes
            .iter()
            .skip(1)
            .step_by(2)
            .take(128)
            .filter(|byte| **byte == 0)
            .count()
            > 32;
    if utf16_le {
        let offset = if bytes.starts_with(&[0xff, 0xfe]) {
            2
        } else {
            0
        };
        let words = bytes[offset..]
            .chunks_exact(2)
            .map(|pair| u16::from_le_bytes([pair[0], pair[1]]))
            .collect::<Vec<_>>();
        return Ok(String::from_utf16_lossy(&words));
    }
    Ok(String::from_utf8_lossy(&bytes).into_owned())
}

fn scan_roots(
    roots: &[PathBuf],
    excluded_tree: Option<&Path>,
) -> (BTreeMap<String, Fingerprint>, Vec<String>) {
    let mut files = BTreeMap::new();
    let mut warnings = Vec::new();
    let mut stack: Vec<(PathBuf, usize)> = deduplicate_roots(roots)
        .into_iter()
        .map(|root| (root, 0))
        .collect();

    while let Some((path, depth)) = stack.pop() {
        if files.len() >= MAX_SCAN_FILES {
            warnings.push(format!(
                "Artifact scan stopped at the safety limit of {MAX_SCAN_FILES} files"
            ));
            break;
        }
        if excluded_tree.is_some_and(|excluded| path.starts_with(excluded)) {
            continue;
        }
        let metadata = match fs::symlink_metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                warnings.push(format!("Could not inspect {}: {error}", path.display()));
                continue;
            }
        };
        if metadata.file_type().is_symlink() {
            continue;
        }
        if metadata.is_file() {
            files.insert(
                path.to_string_lossy().into_owned(),
                Fingerprint {
                    size_bytes: metadata.len(),
                    modified_unix_ms: metadata.modified().ok().and_then(system_time_millis),
                },
            );
            continue;
        }
        if !metadata.is_dir() || depth >= MAX_SCAN_DEPTH {
            continue;
        }
        match fs::read_dir(&path) {
            Ok(entries) => {
                for entry in entries {
                    match entry {
                        Ok(entry) => stack.push((entry.path(), depth + 1)),
                        Err(error) => warnings.push(format!(
                            "Could not enumerate an entry under {}: {error}",
                            path.display()
                        )),
                    }
                }
            }
            Err(error) => warnings.push(format!("Could not enumerate {}: {error}", path.display())),
        }
    }
    (files, warnings)
}

fn deduplicate_roots(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    roots
        .iter()
        .filter(|path| seen.insert(path.to_string_lossy().to_ascii_lowercase()))
        .cloned()
        .collect()
}

fn system_time_millis(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_millis().min(u64::MAX as u128) as u64)
}

fn unique_destination(root: &Path, original: &Path) -> PathBuf {
    let path_hash = format!(
        "{:x}",
        Sha256::digest(original.to_string_lossy().as_bytes())
    );
    let file_name = original
        .file_name()
        .and_then(|value| value.to_str())
        .unwrap_or("artifact.bin");
    let safe_name: String = file_name
        .chars()
        .map(|character| match character {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            _ => character,
        })
        .collect();
    let requested = root.join(format!("{}_{}", &path_hash[..12], safe_name));
    if !requested.exists() {
        return requested;
    }
    for index in 2..10_000 {
        let candidate = root.join(format!("{}_{}_{}", &path_hash[..12], index, safe_name));
        if !candidate.exists() {
            return candidate;
        }
    }
    root.join(format!(
        "{}_{}_{}",
        &path_hash[..12],
        std::process::id(),
        safe_name
    ))
}

fn copy_and_hash(source: &Path, destination: &Path) -> Result<String> {
    let mut input = File::open(source)?;
    let mut output = File::create(destination)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
        output.write_all(&buffer[..read])?;
    }
    output.flush()?;
    Ok(format!("{:x}", hasher.finalize()))
}

fn hash_file(path: &Path) -> Result<String> {
    let mut input = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buffer = vec![0_u8; 1024 * 1024];
    loop {
        let read = input.read(&mut buffer)?;
        if read == 0 {
            break;
        }
        hasher.update(&buffer[..read]);
    }
    Ok(format!("{:x}", hasher.finalize()))
}

fn relative_text(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .to_string_lossy()
        .into_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn collects_only_files_changed_after_baseline() {
        let source = tempdir().unwrap();
        let session = tempdir().unwrap();
        fs::create_dir_all(session.path().join("logs")).unwrap();
        fs::create_dir_all(session.path().join("crash-artifacts")).unwrap();
        fs::write(source.path().join("old.dmp"), b"old").unwrap();
        let baseline = ArtifactBaseline::capture(
            &[source.path().to_path_buf()],
            &session.path().join("logs").join("baseline.json"),
        )
        .unwrap();
        fs::write(source.path().join("new.dmp"), b"new crash dump").unwrap();

        let result = baseline
            .collect(session.path(), &SessionClock::new())
            .unwrap();
        assert_eq!(result.records.len(), 1);
        assert!(result.records[0].original_path.ends_with("new.dmp"));
        assert_eq!(result.records[0].status, "copied_new");
        assert!(result.records[0].copied_path.is_some());
    }

    #[test]
    fn recognizes_new_live_kernel_141_report() {
        let source = tempdir().unwrap();
        let session = tempdir().unwrap();
        fs::create_dir_all(session.path().join("logs")).unwrap();
        let clock = SessionClock::new();
        let baseline = ArtifactBaseline::capture(
            &[source.path().to_path_buf()],
            &session.path().join("logs").join("baseline.json"),
        )
        .unwrap();
        let dump_stamp = Local::now().format("%Y%m%d-%H%M");
        let report = format!(
            "EventType=LiveKernelEvent\r\nSig[0].Name=Code\r\nSig[0].Value=141\r\nFile[0].Original.Path=C:\\\\Windows\\\\LiveKernelReports\\\\WATCHDOG\\\\WATCHDOG-{dump_stamp}.dmp\r\n"
        );
        fs::write(source.path().join("Report.wer"), report).unwrap();

        let result = baseline.collect(session.path(), &clock).unwrap();
        assert_eq!(result.signals.len(), 1);
        assert_eq!(result.signals[0].trigger, "live_kernel_event_141");
    }

    #[test]
    fn ignores_historical_live_kernel_report_reprocessed_now() {
        let source = tempdir().unwrap();
        let session = tempdir().unwrap();
        fs::create_dir_all(session.path().join("logs")).unwrap();
        let clock = SessionClock::new();
        let baseline = ArtifactBaseline::capture(
            &[source.path().to_path_buf()],
            &session.path().join("logs").join("baseline.json"),
        )
        .unwrap();
        fs::write(
            source.path().join("Report.wer"),
            "EventType=LiveKernelEvent\r\nSig[0].Name=Code\r\nSig[0].Value=141\r\nFile[0].Original.Path=WATCHDOG-20200101-0000.dmp\r\n",
        )
        .unwrap();

        let result = baseline.collect(session.path(), &clock).unwrap();
        assert!(result.signals.is_empty());
    }
}
