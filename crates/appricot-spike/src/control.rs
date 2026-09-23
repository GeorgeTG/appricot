//! The run directory's control files: how a person, or a script, steers a running spike.
//!
//! Every tool of one run polls `<run>/control/`. The files are plain, so they can be written
//! from the host through the bind mount with any shell:
//!
//! | File | Meaning |
//! |---|---|
//! | `mark` | Its first line is the current scenario label (`login`, `scroll`, …). Every log line and every recorded tile carries the label in force when it was written. |
//! | `snap-<label>` | Asks the recorder for one PNG per live surface, named after `<label>`. The recorder deletes the file once it has taken them. |
//! | `stop` | Every tool writes its summary and exits. |
//!
//! A label is cut to [`LABEL_MAX`] characters of `[A-Za-z0-9._-]`; anything else becomes `_`,
//! so a label is always safe as part of a file name.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

/// The longest label kept.
pub const LABEL_MAX: usize = 64;

/// The label in force before anyone sets one.
pub const DEFAULT_LABEL: &str = "start";

/// The control directory of one run.
#[derive(Debug, Clone)]
pub struct Control {
    dir: PathBuf,
}

impl Control {
    /// The control directory of the run at `run_dir`, created when missing.
    pub fn open(run_dir: &Path) -> io::Result<Self> {
        let dir = run_dir.join("control");
        fs::create_dir_all(&dir)?;
        Ok(Self { dir })
    }

    /// The directory itself.
    pub fn dir(&self) -> &Path {
        &self.dir
    }

    /// True once `stop` exists.
    pub fn stop_requested(&self) -> bool {
        self.dir.join("stop").exists()
    }

    /// Asks every tool of the run to stop.
    pub fn request_stop(&self) -> io::Result<()> {
        fs::write(self.dir.join("stop"), b"")
    }

    /// The current label: the first line of `mark`, sanitised, or [`DEFAULT_LABEL`].
    pub fn mark(&self) -> String {
        match fs::read_to_string(self.dir.join("mark")) {
            Ok(text) => {
                let label = sanitize_label(text.lines().next().unwrap_or(""));
                if label.is_empty() {
                    DEFAULT_LABEL.to_owned()
                } else {
                    label
                }
            }
            Err(_) => DEFAULT_LABEL.to_owned(),
        }
    }

    /// Sets the current label for every tool of the run.
    pub fn set_mark(&self, label: &str) -> io::Result<()> {
        fs::write(
            self.dir.join("mark"),
            format!("{}\n", sanitize_label(label)),
        )
    }

    /// Asks the recorder for a snapshot named `label`.
    pub fn request_snap(&self, label: &str) -> io::Result<()> {
        fs::write(
            self.dir.join(format!("snap-{}", sanitize_label(label))),
            b"",
        )
    }

    /// Takes the pending snapshot requests: their labels, sorted, with each file deleted.
    pub fn take_snap_requests(&self) -> Vec<String> {
        let Ok(entries) = fs::read_dir(&self.dir) else {
            return Vec::new();
        };
        let mut labels = Vec::new();
        for entry in entries.flatten() {
            let name = entry.file_name();
            let Some(name) = name.to_str() else { continue };
            if let Some(label) = name.strip_prefix("snap-") {
                let label = sanitize_label(label);
                if fs::remove_file(entry.path()).is_ok() && !label.is_empty() {
                    labels.push(label);
                }
            }
        }
        labels.sort();
        labels
    }
}

/// Cuts `label` to [`LABEL_MAX`] characters and replaces anything outside `[A-Za-z0-9._-]`
/// with `_`. Surrounding whitespace is dropped first.
pub fn sanitize_label(label: &str) -> String {
    label
        .trim()
        .chars()
        .take(LABEL_MAX)
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::{Control, DEFAULT_LABEL, sanitize_label};

    fn scratch(name: &str) -> std::path::PathBuf {
        let dir = std::env::temp_dir().join(format!(
            "appricot-spike-control-{name}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&dir);
        dir
    }

    #[test]
    fn labels_are_safe_file_name_parts() {
        assert_eq!(sanitize_label("  table refresh\n"), "table_refresh");
        assert_eq!(sanitize_label("../etc/passwd"), ".._etc_passwd");
        assert_eq!(sanitize_label("λ-1"), "_-1");
        assert_eq!(sanitize_label(&"a".repeat(100)).len(), 64);
    }

    #[test]
    fn the_mark_defaults_until_set() {
        let dir = scratch("mark");
        let control = Control::open(&dir).expect("the control directory is created");
        assert_eq!(control.mark(), DEFAULT_LABEL);
        control
            .set_mark("scroll down")
            .expect("the mark is written");
        assert_eq!(control.mark(), "scroll_down");
        std::fs::write(control.dir().join("mark"), "\n").expect("an empty mark");
        assert_eq!(control.mark(), DEFAULT_LABEL);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn snap_requests_are_taken_once() {
        let dir = scratch("snap");
        let control = Control::open(&dir).expect("the control directory is created");
        control.request_snap("login").expect("a request");
        control.request_snap("after connect").expect("a request");
        assert_eq!(control.take_snap_requests(), ["after_connect", "login"]);
        assert!(control.take_snap_requests().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn stop_is_a_file() {
        let dir = scratch("stop");
        let control = Control::open(&dir).expect("the control directory is created");
        assert!(!control.stop_requested());
        control.request_stop().expect("stop is written");
        assert!(control.stop_requested());
        let _ = std::fs::remove_dir_all(&dir);
    }
}
