//! Log lines go to stdout and to a log file capped at about [`MAX_BYTES`]
//! (the previous file is kept as `<name>.1`). The latest [`HISTORY_LINES`]
//! are also kept in memory, for [`lines_since`].

use std::{
    collections::VecDeque,
    fmt::Display,
    fs::{self, File, OpenOptions},
    io::{self, Write},
    path::{Path, PathBuf},
    sync::Mutex,
};

const MAX_BYTES: u64 = 1 << 20;
const HISTORY_LINES: usize = 2000;

static FILE: Mutex<Option<LogFile>> = Mutex::new(None);
static HISTORY: Mutex<History> = Mutex::new(History::new());

/// The newest lines, and how many were ever logged (so readers can ask for what's new).
struct History {
    lines: VecDeque<String>,
    total: usize,
}

impl History {
    const fn new() -> Self {
        History {
            lines: VecDeque::new(),
            total: 0,
        }
    }

    fn push(&mut self, line: String) {
        if self.lines.len() == HISTORY_LINES {
            self.lines.pop_front();
        }
        self.lines.push_back(line);
        self.total += 1;
    }

    /// Lines logged after the first `seen` ones that are still held, and the new count to pass next time.
    fn since(&self, seen: usize) -> (Vec<String>, usize) {
        let oldest = self.total - self.lines.len();
        let skip = seen.saturating_sub(oldest).min(self.lines.len());
        (self.lines.iter().skip(skip).cloned().collect(), self.total)
    }
}

/// Log lines (without line ends) not yet seen, plus the count to pass on the next call.
/// Start with 0 to get the whole history.
pub fn lines_since(seen: usize) -> (Vec<String>, usize) {
    HISTORY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .since(seen)
}

/// Starts also writing the log to `path`.
pub fn init(path: &Path) -> io::Result<()> {
    let file = LogFile::open(path.to_path_buf(), MAX_BYTES)?;
    *FILE.lock().unwrap_or_else(|e| e.into_inner()) = Some(file);
    Ok(())
}

pub fn log(message: impl Display) {
    let time = jiff::Zoned::now().strftime("%Y-%m-%d %H:%M:%S%.3f");
    let line = format!("{time}  {message}\n");
    print!("{line}");
    HISTORY
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(line.trim_end().to_string());
    if let Some(file) = FILE.lock().unwrap_or_else(|e| e.into_inner()).as_mut()
        && let Err(e) = file.write(&line)
    {
        eprintln!("cannot write the log file: {e}");
    }
}

struct LogFile {
    path: PathBuf,
    file: File,
    size: u64,
    max_bytes: u64,
}

impl LogFile {
    fn open(path: PathBuf, max_bytes: u64) -> io::Result<LogFile> {
        let file = OpenOptions::new().create(true).append(true).open(&path)?;
        let size = file.metadata()?.len();
        Ok(LogFile {
            path,
            file,
            size,
            max_bytes,
        })
    }

    fn write(&mut self, line: &str) -> io::Result<()> {
        if self.size + line.len() as u64 > self.max_bytes && self.size > 0 {
            fs::rename(&self.path, rotated(&self.path))?;
            *self = LogFile::open(self.path.clone(), self.max_bytes)?;
        }
        self.file.write_all(line.as_bytes())?;
        self.size += line.len() as u64;
        Ok(())
    }
}

fn rotated(path: &Path) -> PathBuf {
    let mut name = path.as_os_str().to_owned();
    name.push(".1");
    PathBuf::from(name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn history_hands_out_only_unseen_lines() {
        let mut history = History::new();
        for n in 0..3 {
            history.push(format!("line {n}"));
        }
        let (all, seen) = history.since(0);
        assert_eq!(all, ["line 0", "line 1", "line 2"]);
        assert_eq!(seen, 3);
        assert!(history.since(seen).0.is_empty());
        history.push("line 3".into());
        assert_eq!(history.since(seen).0, ["line 3"]);
    }

    #[test]
    fn history_drops_the_oldest_lines_when_full() {
        let mut history = History::new();
        for n in 0..HISTORY_LINES + 5 {
            history.push(n.to_string());
        }
        let (lines, seen) = history.since(0);
        assert_eq!(lines.len(), HISTORY_LINES);
        assert_eq!(lines[0], "5");
        assert_eq!(seen, HISTORY_LINES + 5);
        // A reader that fell behind gets what is left, not a gap error.
        assert_eq!(history.since(2).0.len(), HISTORY_LINES);
    }

    #[test]
    fn rotates_when_full_and_keeps_one_old_file() {
        let dir = std::env::temp_dir().join(format!("droidleap-log-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.log");
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(rotated(&path));

        let mut log = LogFile::open(path.clone(), 10).unwrap();
        log.write("12345\n").unwrap();
        log.write("abcd\n").unwrap(); // 11 bytes would exceed 10: rotate first
        log.write("xy\n").unwrap();

        assert_eq!(fs::read_to_string(rotated(&path)).unwrap(), "12345\n");
        assert_eq!(fs::read_to_string(&path).unwrap(), "abcd\nxy\n");
    }

    #[test]
    fn a_line_longer_than_the_cap_is_still_written() {
        let dir =
            std::env::temp_dir().join(format!("droidleap-log-test-long-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("test.log");
        let _ = fs::remove_file(&path);

        let mut log = LogFile::open(path.clone(), 4).unwrap();
        log.write("too long\n").unwrap();
        assert_eq!(fs::read_to_string(&path).unwrap(), "too long\n");
    }
}
