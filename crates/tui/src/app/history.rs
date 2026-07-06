use std::fs::{self, File, OpenOptions};
use std::io::{self, BufRead, BufReader, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use fs2::FileExt;
use serde::{Deserialize, Serialize};

pub(super) const HISTORY_FILE_NAME: &str = "input_history";
pub(super) const HISTORY_LOCK_FILE_NAME: &str = "input_history.lock";
pub(super) const HISTORY_LOAD_LIMIT: usize = 1_000;

const HISTORY_RECORD_VERSION: u8 = 1;
const HISTORY_COMPACT_RECORD_LIMIT: usize = 1_200;
const HISTORY_COMPACT_SIZE_LIMIT: u64 = 256 * 1024;
const HISTORY_COMPACT_RETAIN_LIMIT: usize = HISTORY_LOAD_LIMIT;

const HISTORY_LOCK_RETRY_DELAY: Duration = Duration::from_millis(5);
const HISTORY_LOCK_RETRY_ATTEMPTS: usize = 20;

#[derive(Debug, Clone, Copy)]
enum LockMode {
    Shared,
    Exclusive,
}

#[derive(Debug, Clone)]
pub(super) struct InputHistory {
    state_dir: PathBuf,
}

impl InputHistory {
    pub(super) fn new(state_dir: PathBuf) -> Self {
        Self { state_dir }
    }

    pub(super) fn load(&self) -> Vec<String> {
        match self.load_locked() {
            Ok(entries) => entries,
            Err(err) => {
                tracing::warn!(error = ?err, "failed to load TUI input history");
                Vec::new()
            }
        }
    }

    pub(super) fn append(&self, entry: &str) -> Option<Vec<String>> {
        match self.append_locked(entry) {
            Ok(entries) => Some(entries),
            Err(err) => {
                tracing::warn!(error = ?err, "failed to append TUI input history");
                None
            }
        }
    }

    #[cfg(test)]
    pub(super) fn history_path(&self) -> PathBuf {
        self.history_path_unchecked()
    }

    #[cfg(test)]
    pub(super) fn lock_path(&self) -> PathBuf {
        self.lock_path_unchecked()
    }

    fn load_locked(&self) -> io::Result<Vec<String>> {
        if !self.history_path_unchecked().exists() {
            return Ok(Vec::new());
        }

        let lock_file = self.open_lock_file()?;
        try_lock_history(&lock_file, LockMode::Shared)?;
        let entries = self.read_entries();
        unlock_history(lock_file, "load")?;
        entries.map(retain_loaded_entries)
    }

    fn append_locked(&self, entry: &str) -> io::Result<Vec<String>> {
        fs::create_dir_all(&self.state_dir)?;
        let lock_file = self.open_lock_file()?;
        try_lock_history(&lock_file, LockMode::Exclusive)?;

        let mut entries = self.read_entries()?;
        if entries.last().is_some_and(|newest| newest == entry) {
            unlock_history(lock_file, "duplicate skip")?;
            return Ok(retain_loaded_entries(entries));
        }

        let record = HistoryRecord::new(entry.to_string());
        let encoded = serde_json::to_string(&record).map_err(io::Error::other)?;
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(self.history_path_unchecked())?;
        writeln!(file, "{encoded}")?;
        file.flush()?;
        let file_len = file.metadata()?.len();
        drop(file);
        entries.push(entry.to_string());
        if should_compact(entries.len(), file_len) {
            match self.compact_entries(entries) {
                Ok(compacted_entries) => entries = compacted_entries,
                Err(err) => {
                    tracing::warn!(error = ?err, "failed to compact TUI input history");
                    entries = self.read_entries()?;
                }
            }
        }

        unlock_history(lock_file, "append")?;
        Ok(retain_loaded_entries(entries))
    }

    fn open_lock_file(&self) -> io::Result<File> {
        OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(self.lock_path_unchecked())
    }

    fn read_entries(&self) -> io::Result<Vec<String>> {
        let file = match File::open(self.history_path_unchecked()) {
            Ok(file) => file,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(err) => return Err(err),
        };

        let mut entries = Vec::new();
        for (line_index, line) in BufReader::new(file).lines().enumerate() {
            let line = line?;
            let Some(entry) = parse_history_line(&line, line_index + 1) else {
                continue;
            };
            entries.push(entry);
        }

        Ok(entries)
    }

    fn compact_entries(&self, entries: Vec<String>) -> io::Result<Vec<String>> {
        let retained = retain_compacted_entries(entries);
        let temporary_path = self.state_dir.join("input_history.tmp");
        {
            let mut temporary = OpenOptions::new()
                .create(true)
                .truncate(true)
                .write(true)
                .open(&temporary_path)?;
            for entry in &retained {
                let encoded = serde_json::to_string(&HistoryRecord::new(entry.clone()))
                    .map_err(io::Error::other)?;
                writeln!(temporary, "{encoded}")?;
            }
            temporary.flush()?;
        }

        fs::rename(&temporary_path, self.history_path_unchecked())?;
        Ok(retained)
    }

    fn history_path_unchecked(&self) -> PathBuf {
        self.state_dir.join(HISTORY_FILE_NAME)
    }

    fn lock_path_unchecked(&self) -> PathBuf {
        self.state_dir.join(HISTORY_LOCK_FILE_NAME)
    }
}

#[derive(Debug, Deserialize, Serialize)]
struct HistoryRecord {
    version: u8,
    entry: String,
}

impl HistoryRecord {
    fn new(entry: String) -> Self {
        Self {
            version: HISTORY_RECORD_VERSION,
            entry,
        }
    }
}

fn parse_history_line(line: &str, line_number: usize) -> Option<String> {
    if line.trim().is_empty() {
        return None;
    }

    let record = match serde_json::from_str::<HistoryRecord>(line) {
        Ok(record) => record,
        Err(err) => {
            tracing::warn!(
                line = line_number,
                error = ?err,
                "skipping corrupt TUI input history record"
            );
            return None;
        }
    };

    if record.version != HISTORY_RECORD_VERSION {
        tracing::warn!(
            line = line_number,
            version = record.version,
            "skipping unsupported TUI input history record"
        );
        return None;
    }

    Some(record.entry)
}

fn retain_loaded_entries(entries: Vec<String>) -> Vec<String> {
    retain_newest(entries, HISTORY_LOAD_LIMIT)
}

fn retain_compacted_entries(entries: Vec<String>) -> Vec<String> {
    retain_newest(entries, HISTORY_COMPACT_RETAIN_LIMIT)
}

fn retain_newest(mut entries: Vec<String>, limit: usize) -> Vec<String> {
    if entries.len() > limit {
        let keep_from = entries.len() - limit;
        entries.drain(0..keep_from);
    }

    entries
}

fn should_compact(record_count: usize, file_len: u64) -> bool {
    record_count > HISTORY_COMPACT_RECORD_LIMIT || file_len > HISTORY_COMPACT_SIZE_LIMIT
}

fn try_lock_history(lock_file: &File, mode: LockMode) -> io::Result<()> {
    for attempt in 0..HISTORY_LOCK_RETRY_ATTEMPTS {
        let result = match mode {
            LockMode::Shared => FileExt::try_lock_shared(lock_file),
            LockMode::Exclusive => FileExt::try_lock_exclusive(lock_file),
        };

        match result {
            Ok(()) => return Ok(()),
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => {
                if attempt + 1 == HISTORY_LOCK_RETRY_ATTEMPTS {
                    return Err(err);
                }

                thread::sleep(HISTORY_LOCK_RETRY_DELAY);
            }
            Err(err) => return Err(err),
        }
    }

    Err(io::Error::new(
        io::ErrorKind::WouldBlock,
        "timed out waiting for TUI input history lock",
    ))
}

fn unlock_history(lock_file: File, operation: &'static str) -> io::Result<()> {
    lock_file.unlock().map_err(|err| {
        tracing::warn!(error = ?err, operation, "failed to unlock TUI input history");
        err
    })
}

#[cfg(test)]
mod tests {
    use std::collections::HashSet;
    use std::path::Path;
    use std::process::{Child, Command};
    use std::sync::{Arc, Barrier};
    use std::thread;
    use std::time::{Duration, Instant};

    use super::*;

    fn write_record(path: &Path, entry: &str) {
        let encoded = serde_json::to_string(&HistoryRecord::new(entry.to_string())).unwrap();
        let mut file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(path)
            .unwrap();
        writeln!(file, "{encoded}").unwrap();
    }

    #[test]
    fn missing_history_file_loads_empty_without_creating_files() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let history = InputHistory::new(state_dir);

        assert_eq!(history.load(), Vec::<String>::new());
        assert!(!history.history_path().exists());
        assert!(!history.lock_path().exists());
    }

    #[test]
    fn valid_history_loads_versioned_json_lines_and_multiline_entries() {
        let dir = tempfile::tempdir().unwrap();
        let history = InputHistory::new(dir.path().join("state"));
        fs::create_dir_all(history.history_path().parent().unwrap()).unwrap();
        write_record(&history.history_path(), "first request");
        write_record(&history.history_path(), "line one\nline two");

        assert_eq!(
            history.load(),
            vec![
                "first request".to_string(),
                "line one\nline two".to_string()
            ]
        );
    }

    #[test]
    fn corrupt_and_unsupported_records_are_skipped_nonfatally() {
        let dir = tempfile::tempdir().unwrap();
        let history = InputHistory::new(dir.path().join("state"));
        fs::create_dir_all(history.history_path().parent().unwrap()).unwrap();
        fs::write(
            history.history_path(),
            concat!(
                "not json\n",
                r#"{"version":2,"entry":"unsupported"}"#,
                "\n",
                r#"{"version":1,"entry":"kept"}"#,
                "\n"
            ),
        )
        .unwrap();

        assert_eq!(history.load(), vec!["kept".to_string()]);
    }

    #[test]
    fn append_creates_state_dir_lock_file_and_history_file() {
        let dir = tempfile::tempdir().unwrap();
        let history = InputHistory::new(dir.path().join("state"));

        assert_eq!(history.append("alpha"), Some(vec!["alpha".to_string()]));

        assert!(history.history_path().exists());
        assert!(history.lock_path().exists());
    }

    #[test]
    fn append_writes_one_newline_terminated_json_record_per_input() {
        let dir = tempfile::tempdir().unwrap();
        let history = InputHistory::new(dir.path().join("state"));

        history.append("line one\nline two").unwrap();
        let raw = fs::read_to_string(history.history_path()).unwrap();

        assert!(raw.ends_with('\n'));
        assert_eq!(raw.lines().count(), 1);
        assert_eq!(history.load(), vec!["line one\nline two".to_string()]);
    }

    #[test]
    fn history_file_name_is_exact_and_not_log_file() {
        let dir = tempfile::tempdir().unwrap();
        let history = InputHistory::new(dir.path().join("state"));

        history.append("alpha").unwrap();

        assert_eq!(
            history.history_path().file_name().unwrap(),
            HISTORY_FILE_NAME
        );
        assert_eq!(
            history.lock_path().file_name().unwrap(),
            HISTORY_LOCK_FILE_NAME
        );
        assert!(
            !history
                .history_path()
                .with_file_name("input-history.log")
                .exists()
        );
        assert!(!history.history_path().with_extension("log").exists());
    }

    #[test]
    fn adjacent_duplicate_append_is_suppressed_from_newest_on_disk_entry() {
        let dir = tempfile::tempdir().unwrap();
        let history = InputHistory::new(dir.path().join("state"));

        history.append("same").unwrap();
        history.append("same").unwrap();

        assert_eq!(history.load(), vec!["same".to_string()]);
        assert_eq!(
            fs::read_to_string(history.history_path())
                .unwrap()
                .lines()
                .count(),
            1
        );
    }

    #[test]
    fn load_returns_empty_when_lock_is_contended_without_blocking() {
        let dir = tempfile::tempdir().unwrap();
        let history = InputHistory::new(dir.path().join("state"));
        fs::create_dir_all(history.history_path().parent().unwrap()).unwrap();
        write_record(&history.history_path(), "kept on disk");
        let ready_path = dir.path().join("ready");
        let mut helper = spawn_lock_helper(&history.lock_path(), &ready_path);
        wait_for_ready(&ready_path);

        let started = Instant::now();
        let entries = history.load();
        let elapsed = started.elapsed();

        stop_helper(&mut helper);
        assert_eq!(entries, Vec::<String>::new());
        assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
    }

    #[test]
    fn append_returns_none_when_lock_is_contended_without_blocking_or_writing() {
        let dir = tempfile::tempdir().unwrap();
        let history = InputHistory::new(dir.path().join("state"));
        fs::create_dir_all(history.history_path().parent().unwrap()).unwrap();
        write_record(&history.history_path(), "kept on disk");
        let ready_path = dir.path().join("ready");
        let mut helper = spawn_lock_helper(&history.lock_path(), &ready_path);
        wait_for_ready(&ready_path);

        let started = Instant::now();
        let result = history.append("not written");
        let elapsed = started.elapsed();

        stop_helper(&mut helper);
        assert_eq!(result, None);
        assert!(elapsed < Duration::from_secs(1), "elapsed: {elapsed:?}");
        assert!(
            !fs::read_to_string(history.history_path())
                .unwrap()
                .contains("not written")
        );
    }

    #[test]
    fn load_limit_keeps_newest_entries() {
        let dir = tempfile::tempdir().unwrap();
        let history = InputHistory::new(dir.path().join("state"));
        fs::create_dir_all(history.history_path().parent().unwrap()).unwrap();
        for index in 0..(HISTORY_LOAD_LIMIT + 2) {
            write_record(&history.history_path(), &format!("entry-{index}"));
        }

        let entries = history.load();

        assert_eq!(entries.len(), HISTORY_LOAD_LIMIT);
        assert_eq!(entries.first(), Some(&"entry-2".to_string()));
        assert_eq!(
            entries.last(),
            Some(&format!("entry-{}", HISTORY_LOAD_LIMIT + 1))
        );
    }

    #[test]
    fn compaction_preserves_newest_retained_entries() {
        let dir = tempfile::tempdir().unwrap();
        let history = InputHistory::new(dir.path().join("state"));
        fs::create_dir_all(history.history_path().parent().unwrap()).unwrap();
        for index in 0..HISTORY_COMPACT_RECORD_LIMIT {
            write_record(&history.history_path(), &format!("entry-{index}"));
        }

        let entries = history.append("trigger").unwrap();
        let raw = fs::read_to_string(history.history_path()).unwrap();

        assert_eq!(entries.len(), HISTORY_COMPACT_RETAIN_LIMIT);
        assert_eq!(raw.lines().count(), HISTORY_COMPACT_RETAIN_LIMIT);
        assert_eq!(entries.last(), Some(&"trigger".to_string()));
        assert!(!entries.contains(&"entry-0".to_string()));
        assert!(
            !history
                .history_path()
                .with_file_name("input_history.tmp")
                .exists()
        );
    }

    #[test]
    fn concurrent_appends_are_not_lost_or_interleaved() {
        let dir = tempfile::tempdir().unwrap();
        let history = Arc::new(InputHistory::new(dir.path().join("state")));
        let workers = 16;
        let barrier = Arc::new(Barrier::new(workers));
        let mut handles = Vec::new();

        for index in 0..workers {
            let history = Arc::clone(&history);
            let barrier = Arc::clone(&barrier);
            handles.push(thread::spawn(move || {
                barrier.wait();
                history.append(&format!("entry-{index}")).unwrap();
            }));
        }

        for handle in handles {
            handle.join().unwrap();
        }

        let entries = history.load();
        let unique = entries.iter().cloned().collect::<HashSet<_>>();

        assert_eq!(entries.len(), workers);
        assert_eq!(unique.len(), workers);
        for index in 0..workers {
            assert!(unique.contains(&format!("entry-{index}")));
        }
    }

    #[test]
    fn history_paths_are_under_state_dir_and_separate_from_workflow_store() {
        let dir = tempfile::tempdir().unwrap();
        let state_dir = dir.path().join("state");
        let workflow_store = dir.path().join("workflow.redb");
        let history = InputHistory::new(state_dir.clone());

        assert!(history.history_path().starts_with(&state_dir));
        assert!(history.lock_path().starts_with(&state_dir));
        assert_ne!(history.history_path(), workflow_store);
        assert_ne!(history.lock_path(), workflow_store);
    }

    fn spawn_lock_helper(lock_path: &Path, ready_path: &Path) -> Child {
        Command::new(std::env::current_exe().unwrap())
            .arg("--exact")
            .arg("app::history::tests::hold_history_lock_helper")
            .arg("--ignored")
            .env("COWBOY_HISTORY_LOCK_PATH", lock_path)
            .env("COWBOY_HISTORY_LOCK_READY", ready_path)
            .spawn()
            .unwrap()
    }

    fn wait_for_ready(ready_path: &Path) {
        let started = Instant::now();
        while !ready_path.exists() {
            assert!(
                started.elapsed() < Duration::from_secs(2),
                "lock helper did not become ready"
            );
            thread::sleep(Duration::from_millis(10));
        }
    }

    fn stop_helper(helper: &mut Child) {
        let _ = helper.kill();
        let _ = helper.wait();
    }

    #[test]
    #[ignore]
    fn hold_history_lock_helper() {
        let Ok(lock_path) = std::env::var("COWBOY_HISTORY_LOCK_PATH") else {
            return;
        };
        let ready_path = std::env::var("COWBOY_HISTORY_LOCK_READY").unwrap();
        let lock_file = OpenOptions::new()
            .create(true)
            .read(true)
            .write(true)
            .open(lock_path)
            .unwrap();
        FileExt::lock_exclusive(&lock_file).unwrap();
        fs::write(ready_path, "ready").unwrap();
        thread::sleep(Duration::from_secs(10));
    }
}
