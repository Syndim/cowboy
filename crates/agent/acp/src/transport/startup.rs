use std::collections::{BTreeMap, BTreeSet};
use std::io::SeekFrom;
use std::path::{Component, Path, PathBuf};

use serde::{Deserialize, Serialize};
use tokio::io::{AsyncReadExt, AsyncSeekExt};

use super::StdioConfig;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StartupReadiness {
    pub log_path: String,
    pub contains: String,
    #[serde(default = "default_timeout_seconds")]
    pub timeout_seconds: u64,
}

fn default_timeout_seconds() -> u64 {
    300
}

impl StartupReadiness {
    pub fn validate(&self) -> anyhow::Result<()> {
        anyhow::ensure!(
            !self.log_path.trim().is_empty(),
            "startup readiness log_path must not be blank"
        );
        anyhow::ensure!(
            !self.contains.trim().is_empty() && !self.contains.contains(['\n', '\r']),
            "startup readiness contains must be a nonblank single-line marker"
        );
        anyhow::ensure!(
            (1..=300).contains(&self.timeout_seconds),
            "startup readiness timeout_seconds must be between 1 and 300"
        );
        anyhow::ensure!(
            self.log_path.matches("{pid}").count() == 1,
            "startup readiness log_path must contain exactly one {{pid}}"
        );
        anyhow::ensure!(
            !self.log_path.contains("**"),
            "startup readiness log_path does not support recursive wildcards"
        );
        anyhow::ensure!(
            !self.log_path.contains(['?', '[', ']']),
            "startup readiness log_path supports only * wildcards"
        );
        let components = Path::new(&self.log_path).components().collect::<Vec<_>>();
        let index = components
            .iter()
            .position(|part| part.as_os_str().to_string_lossy().contains("{pid}"))
            .unwrap();
        anyhow::ensure!(
            index + 1 < components.len(),
            "startup readiness {{pid}} must identify a directory before the log file"
        );
        anyhow::ensure!(
            components[..index]
                .iter()
                .all(|part| !part.as_os_str().to_string_lossy().contains('*')),
            "startup readiness wildcards must be inside or after the {{pid}} directory"
        );
        anyhow::ensure!(
            components
                .iter()
                .all(|part| !matches!(part, Component::ParentDir)),
            "startup readiness log_path must not contain parent components"
        );
        anyhow::ensure!(
            Path::new(&self.log_path).is_absolute()
                || self.log_path.starts_with("~/")
                || self.log_path.starts_with("~\\"),
            "startup readiness log_path must be absolute or start with ~/"
        );
        Ok(())
    }
}

pub(super) struct StartupLog {
    root: PathBuf,
    components: Vec<String>,
    existing: BTreeSet<PathBuf>,
    files: BTreeMap<PathBuf, LogCursor>,
    marker: Vec<u8>,
}

#[derive(Default)]
struct LogCursor {
    offset: u64,
    tail: Vec<u8>,
    matched: bool,
    identity: Option<(u64, u64)>,
    length: u64,
    modified: Option<std::time::SystemTime>,
}

impl LogCursor {
    fn feed(&mut self, data: &[u8], marker: &[u8]) -> bool {
        for byte in data {
            if *byte == b'\n' {
                if self.matched {
                    return true;
                }
                self.tail.clear();
            } else if !self.matched {
                self.tail.push(*byte);
                if self.tail.ends_with(marker) {
                    self.matched = true;
                } else if self.tail.len() >= marker.len() {
                    self.tail.remove(0);
                }
            }
        }
        false
    }
}

impl StartupLog {
    pub(super) async fn prepare(
        readiness: &StartupReadiness,
        config: &StdioConfig,
    ) -> anyhow::Result<Self> {
        readiness.validate()?;
        let path = if let Some(suffix) = readiness
            .log_path
            .strip_prefix("~/")
            .or_else(|| readiness.log_path.strip_prefix("~\\"))
        {
            let home = |name: &str| {
                config
                    .env
                    .iter()
                    .rev()
                    .find(|(key, _)| environment_key_matches(key, name))
                    .map(|(_, value)| std::ffi::OsString::from(value))
                    .or_else(|| {
                        if !config.clear_env
                            || config
                                .allowed_env
                                .iter()
                                .any(|key| environment_key_matches(key, name))
                        {
                            std::env::var_os(name)
                        } else {
                            None
                        }
                    })
                    .filter(|value| !value.is_empty())
            };
            let home = home("HOME")
                .or_else(|| home("USERPROFILE"))
                .ok_or_else(|| {
                    anyhow::anyhow!("startup readiness home directory is unavailable")
                })?;
            PathBuf::from(home).join(suffix)
        } else {
            PathBuf::from(&readiness.log_path)
        };
        anyhow::ensure!(
            path.is_absolute(),
            "startup readiness resolved log_path must be absolute"
        );
        let mut root = PathBuf::new();
        let mut components = Vec::new();
        for component in path.components() {
            let text = component.as_os_str().to_string_lossy();
            if components.is_empty() && !text.contains("{pid}") {
                root.push(component);
            } else {
                components.push(text.into_owned());
            }
        }
        let existing = entries(&root)
            .await?
            .into_iter()
            .map(|(path, _)| path)
            .collect();
        Ok(Self {
            root,
            components,
            existing,
            files: BTreeMap::new(),
            marker: readiness.contains.as_bytes().to_vec(),
        })
    }

    pub(super) async fn ready(&mut self, pid: u32) -> anyhow::Result<bool> {
        let mut paths = vec![self.root.clone()];
        for (index, template) in self.components.iter().enumerate() {
            let pattern = template.replace("{pid}", &pid.to_string());
            let mut matching = Vec::new();
            for parent in paths {
                for (path, name) in entries(&parent).await? {
                    if (index != 0 || !self.existing.contains(&path))
                        && matches_component(&pattern, &name)
                    {
                        if index + 1 < self.components.len() {
                            match tokio::fs::metadata(&path).await {
                                Ok(metadata) if metadata.is_dir() => {}
                                Ok(_) => continue,
                                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                                    continue;
                                }
                                Err(error) => return Err(log_error(error)),
                            }
                        }
                        matching.push(path);
                    }
                }
            }
            paths = matching;
        }
        for path in paths {
            let mut file = match tokio::fs::File::open(&path).await {
                Ok(file) => file,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
                Err(error) => return Err(log_error(error)),
            };
            let metadata = file.metadata().await.map_err(log_error)?;
            if !metadata.is_file() {
                continue;
            }
            let identity = file_identity(&file, &metadata)?;
            let cursor = self.files.entry(path).or_default();
            let modified = metadata.modified().map_err(log_error)?;
            if cursor.identity != Some(identity)
                || cursor.length != metadata.len()
                || cursor.modified != Some(modified)
            {
                *cursor = LogCursor {
                    identity: Some(identity),
                    length: metadata.len(),
                    modified: Some(modified),
                    ..LogCursor::default()
                };
            }
            file.seek(SeekFrom::Start(cursor.offset))
                .await
                .map_err(log_error)?;
            let mut buffer = vec![0; 65536];
            while cursor.offset < metadata.len() {
                let remaining = (metadata.len() - cursor.offset).min(buffer.len() as u64) as usize;
                let size = file
                    .read(&mut buffer[..remaining])
                    .await
                    .map_err(log_error)?;
                if size == 0 {
                    break;
                }
                cursor.offset += size as u64;
                if cursor.feed(&buffer[..size], &self.marker) {
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }
}

fn environment_key_matches(left: &str, right: &str) -> bool {
    if cfg!(windows) {
        left.eq_ignore_ascii_case(right)
    } else {
        left == right
    }
}

#[cfg(unix)]
fn file_identity(
    _file: &tokio::fs::File,
    metadata: &std::fs::Metadata,
) -> anyhow::Result<(u64, u64)> {
    use std::os::unix::fs::MetadataExt;
    Ok((metadata.dev(), metadata.ino()))
}

#[cfg(windows)]
fn file_identity(
    file: &tokio::fs::File,
    _metadata: &std::fs::Metadata,
) -> anyhow::Result<(u64, u64)> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{
        BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
    };
    let mut information = std::mem::MaybeUninit::<BY_HANDLE_FILE_INFORMATION>::uninit();
    if unsafe { GetFileInformationByHandle(file.as_raw_handle(), information.as_mut_ptr()) } == 0 {
        return Err(log_error(std::io::Error::last_os_error()));
    }
    let information = unsafe { information.assume_init() };
    Ok((
        u64::from(information.dwVolumeSerialNumber),
        (u64::from(information.nFileIndexHigh) << 32) | u64::from(information.nFileIndexLow),
    ))
}

#[cfg(not(any(unix, windows)))]
fn file_identity(
    _file: &tokio::fs::File,
    _metadata: &std::fs::Metadata,
) -> anyhow::Result<(u64, u64)> {
    anyhow::bail!("startup readiness file identity is unsupported on this platform")
}

fn log_error(error: std::io::Error) -> anyhow::Error {
    anyhow::anyhow!("startup readiness log I/O failed: {error}")
}

async fn entries(path: &Path) -> anyhow::Result<Vec<(PathBuf, String)>> {
    let mut directory = match tokio::fs::read_dir(path).await {
        Ok(directory) => directory,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(log_error(error)),
    };
    let mut result = Vec::new();
    while let Some(entry) = directory.next_entry().await.map_err(log_error)? {
        result.push((
            entry.path(),
            entry.file_name().to_string_lossy().into_owned(),
        ));
    }
    Ok(result)
}

fn matches_component(pattern: &str, name: &str) -> bool {
    let (pattern, name) = (pattern.as_bytes(), name.as_bytes());
    let (mut p, mut n, mut star, mut retry) = (0, 0, None, 0);
    while n < name.len() {
        if p < pattern.len() && pattern[p] != b'*' && pattern[p] == name[n] {
            p += 1;
            n += 1;
        } else if p < pattern.len() && pattern[p] == b'*' {
            star = Some(p);
            p += 1;
            retry = n;
        } else if let Some(index) = star {
            retry += 1;
            n = retry;
            p = index + 1;
        } else {
            return false;
        }
    }
    while p < pattern.len() && pattern[p] == b'*' {
        p += 1;
    }
    p == pattern.len()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn readiness(root: &Path) -> StartupReadiness {
        StartupReadiness {
            log_path: root
                .join("session_*_{pid}/process-*.log")
                .to_string_lossy()
                .into_owned(),
            contains: "ACP ready".into(),
            timeout_seconds: 300,
        }
    }

    fn config() -> StdioConfig {
        StdioConfig {
            command: "unused".into(),
            args: vec![],
            clear_env: true,
            allowed_env: vec![],
            env: vec![],
            startup_readiness: None,
        }
    }

    #[test]
    fn validates_readiness_and_defaults_to_five_minutes() {
        let directory = tempfile::tempdir().unwrap();
        let config: StartupReadiness = serde_json::from_value(serde_json::json!({"log_path": directory.path().join("session_*_{pid}/process-*.log"), "contains": "ACP ready"})).unwrap();
        config.validate().unwrap();
        assert_eq!(config.timeout_seconds, 300);
        for path in [
            "",
            "/tmp/*.log",
            "/tmp/*/session_{pid}/*.log",
            "/tmp/**/session_{pid}/*.log",
            "/tmp/{pid}/{pid}.log",
            "/tmp/{pid}",
            "relative/{pid}/*.log",
            "/tmp/../{pid}/*.log",
        ] {
            assert!(
                StartupReadiness {
                    log_path: path.into(),
                    ..config.clone()
                }
                .validate()
                .is_err(),
                "{path}"
            );
        }
        for marker in ["", " ", "ready\nnext", "ready\rnext"] {
            assert!(
                StartupReadiness {
                    contains: marker.into(),
                    ..config.clone()
                }
                .validate()
                .is_err()
            );
        }
        for timeout_seconds in [0, 301] {
            assert!(
                StartupReadiness {
                    timeout_seconds,
                    ..config.clone()
                }
                .validate()
                .is_err()
            );
        }
    }

    #[test]
    fn component_wildcards_are_anchored() {
        for (pattern, name, expected) in [
            ("session_*_12", "session_test_12", true),
            ("session_*_12", "session_test_123", false),
            ("process-*.log", "process-1.log", true),
            ("process-*.log", "other.log", false),
            ("a*b*c", "abbc", true),
            ("a*b*c", "abbd", false),
        ] {
            assert_eq!(matches_component(pattern, name), expected);
        }
    }

    #[test]
    fn requires_a_complete_line_and_preserves_marker_fragments() {
        let mut cursor = LogCursor::default();
        assert!(!cursor.feed(b"private payload\nAC", b"ACP ready"));
        assert!(!cursor.feed(b"P ready", b"ACP ready"));
        assert!(cursor.feed(b"\n", b"ACP ready"));
        let mut cursor = LogCursor::default();
        assert!(!cursor.feed(b"ACP\n ready\n", b"ACP ready"));
        assert!(cursor.tail.len() < b"ACP ready".len());
    }

    #[tokio::test]
    async fn ignores_old_and_wrong_pid_logs_and_reads_new_appends() {
        let directory = tempfile::tempdir().unwrap();
        let old = directory.path().join("session_old_12");
        tokio::fs::create_dir(&old).await.unwrap();
        tokio::fs::write(old.join("process-old.log"), b"ACP ready\n")
            .await
            .unwrap();
        let mut log = StartupLog::prepare(&readiness(directory.path()), &config())
            .await
            .unwrap();
        let wrong = directory.path().join("session_other_123");
        tokio::fs::create_dir(&wrong).await.unwrap();
        tokio::fs::write(wrong.join("process-new.log"), b"ACP ready\n")
            .await
            .unwrap();
        assert!(!log.ready(12).await.unwrap());
        let fresh = directory.path().join("session_new_12");
        tokio::fs::create_dir(&fresh).await.unwrap();
        let file = fresh.join("process-new.log");
        tokio::fs::write(&file, b"AC").await.unwrap();
        assert!(!log.ready(12).await.unwrap());
        tokio::fs::write(&file, b"ACP ready").await.unwrap();
        assert!(!log.ready(12).await.unwrap());
        tokio::fs::write(&file, b"ACP ready\n").await.unwrap();
        assert!(log.ready(12).await.unwrap());
    }

    #[tokio::test]
    async fn replacement_logs_do_not_share_offsets_or_partial_markers() {
        let directory = tempfile::tempdir().unwrap();
        let mut log = StartupLog::prepare(&readiness(directory.path()), &config())
            .await
            .unwrap();
        let session = directory.path().join("session_current_12");
        tokio::fs::create_dir(&session).await.unwrap();
        let path = session.join("process-current.log");
        tokio::fs::write(&path, b"ACP rea").await.unwrap();
        assert!(!log.ready(12).await.unwrap());
        let replacement = session.join("replacement.tmp");
        tokio::fs::write(&replacement, b"xxxxxxxdy\n")
            .await
            .unwrap();
        tokio::fs::remove_file(&path).await.unwrap();
        tokio::fs::rename(&replacement, &path).await.unwrap();
        assert!(!log.ready(12).await.unwrap());
        tokio::fs::write(&replacement, b"ACP ready\nno marker after old offset\n")
            .await
            .unwrap();
        tokio::fs::remove_file(&path).await.unwrap();
        tokio::fs::rename(&replacement, &path).await.unwrap();
        assert!(log.ready(12).await.unwrap());
    }

    #[tokio::test]
    async fn rewritten_partial_lines_cannot_complete_an_old_marker() {
        let directory = tempfile::tempdir().unwrap();
        let mut log = StartupLog::prepare(&readiness(directory.path()), &config())
            .await
            .unwrap();
        let session = directory.path().join("session_current_12");
        tokio::fs::create_dir(&session).await.unwrap();
        let path = session.join("process-current.log");
        tokio::fs::write(&path, b"ACP ready").await.unwrap();
        assert!(!log.ready(12).await.unwrap());
        tokio::fs::write(&path, b"not ready\n").await.unwrap();
        assert!(!log.ready(12).await.unwrap());
        tokio::fs::write(&path, b"ACP ready\n").await.unwrap();
        assert!(log.ready(12).await.unwrap());
    }

    #[tokio::test]
    async fn growing_logs_find_markers_beyond_the_first_read_buffer() {
        use tokio::io::AsyncWriteExt;

        let directory = tempfile::tempdir().unwrap();
        let mut log = StartupLog::prepare(&readiness(directory.path()), &config())
            .await
            .unwrap();
        let session = directory.path().join("session_current_12");
        tokio::fs::create_dir(&session).await.unwrap();
        let path = session.join("process-current.log");
        tokio::fs::write(&path, vec![b'x'; 2 * 65536])
            .await
            .unwrap();
        assert!(!log.ready(12).await.unwrap());
        let mut file = tokio::fs::OpenOptions::new()
            .append(true)
            .open(&path)
            .await
            .unwrap();
        for chunk in [b"\nAC".as_slice(), b"P ready".as_slice()] {
            file.write_all(chunk).await.unwrap();
            file.flush().await.unwrap();
            assert!(!log.ready(12).await.unwrap());
        }
        file.write_all(b"\nmore log output\n").await.unwrap();
        file.flush().await.unwrap();
        assert!(log.ready(12).await.unwrap());
    }

    #[tokio::test]
    async fn intermediate_wildcards_ignore_regular_files() {
        let directory = tempfile::tempdir().unwrap();
        let mut readiness = readiness(directory.path());
        readiness.log_path = directory
            .path()
            .join("session_{pid}/*/process-*.log")
            .to_string_lossy()
            .into_owned();
        let mut log = StartupLog::prepare(&readiness, &config()).await.unwrap();
        let session = directory.path().join("session_12");
        tokio::fs::create_dir_all(session.join("logs"))
            .await
            .unwrap();
        tokio::fs::write(session.join("ordinary-file"), b"not a directory")
            .await
            .unwrap();
        tokio::fs::write(session.join("logs/process-current.log"), b"ACP ready\n")
            .await
            .unwrap();
        assert!(log.ready(12).await.unwrap());
    }

    #[test]
    fn environment_names_follow_platform_case_rules() {
        assert!(environment_key_matches("HOME", "HOME"));
        assert_eq!(environment_key_matches("home", "HOME"), cfg!(windows));
        assert_eq!(environment_key_matches("Home", "HOME"), cfg!(windows));
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn explicit_child_home_lookup_is_case_insensitive() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = config();
        config.env.push((
            "home".into(),
            directory.path().to_string_lossy().into_owned(),
        ));
        let mut readiness = readiness(directory.path());
        readiness.log_path = "~/session_*_{pid}/process-*.log".into();
        let log = StartupLog::prepare(&readiness, &config).await.unwrap();
        assert_eq!(log.root, directory.path());
    }

    #[tokio::test]
    async fn expands_the_child_home_not_an_unavailable_ambient_home() {
        let directory = tempfile::tempdir().unwrap();
        let mut config = config();
        config.env.push((
            "HOME".into(),
            directory.path().to_string_lossy().into_owned(),
        ));
        let mut readiness = readiness(directory.path());
        readiness.log_path = "~/session_*_{pid}/process-*.log".into();
        let log = StartupLog::prepare(&readiness, &config).await.unwrap();
        assert_eq!(log.root, directory.path());
        config.env.clear();
        assert!(StartupLog::prepare(&readiness, &config).await.is_err());
    }
}
