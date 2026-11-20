//! The record of what has been imported into the browser's storage.
//!
//! The browser cannot cheaply list a large directory tree, and a read of a
//! file needs its length first, so the import keeps a small text manifest next
//! to the files: one line per file with its size. Mounting reads only this.
//! It is rewritten as the import advances, so an interrupted import can
//! resume, and marked complete when it ends.
//!
//! ```text
//! rustlife-import 1
//! complete
//! 1234<TAB>hl2mp/gameinfo.txt
//! ```

use std::collections::BTreeMap;
use std::fmt::Write as _;

/// First line of a manifest file.
const HEADER: &str = "rustlife-import 1";

/// Whether an import finished.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportStatus {
    /// Files are still missing: the import was interrupted or is running.
    Partial,
    /// Every planned file was copied.
    Complete,
}

/// The files imported so far, by path relative to the install folder.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Manifest {
    status: ImportStatus,
    files: BTreeMap<String, u64>,
}

/// A manifest file that cannot be read.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("the import record is damaged: {0}")]
pub struct ManifestError(String);

impl Default for Manifest {
    fn default() -> Self {
        Self::new()
    }
}

impl Manifest {
    /// An empty, partial manifest.
    #[must_use]
    pub fn new() -> Self {
        Self {
            status: ImportStatus::Partial,
            files: BTreeMap::new(),
        }
    }

    /// Whether the import finished.
    #[must_use]
    pub fn status(&self) -> ImportStatus {
        self.status
    }

    /// Marks the import finished.
    pub fn complete(&mut self) {
        self.status = ImportStatus::Complete;
    }

    /// Records that `path` was copied in full, `size` bytes.
    pub fn record(&mut self, path: &str, size: u64) {
        self.files.insert(path.to_owned(), size);
    }

    /// The size recorded for `path`, if it was copied.
    #[must_use]
    pub fn size(&self, path: &str) -> Option<u64> {
        self.files.get(path).copied()
    }

    /// Every recorded file with its size, in path order.
    pub fn files(&self) -> impl Iterator<Item = (&str, u64)> {
        self.files.iter().map(|(path, size)| (path.as_str(), *size))
    }

    /// Number of recorded files.
    #[must_use]
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Whether nothing is recorded.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Total bytes recorded.
    #[must_use]
    pub fn total_bytes(&self) -> u64 {
        self.files.values().sum()
    }

    /// The manifest as text.
    #[must_use]
    pub fn to_text(&self) -> String {
        let mut text = String::new();
        let status = match self.status {
            ImportStatus::Partial => "partial",
            ImportStatus::Complete => "complete",
        };
        let _ = writeln!(text, "{HEADER}\n{status}");
        for (path, size) in &self.files {
            let _ = writeln!(text, "{size}\t{path}");
        }
        text
    }

    /// Parses manifest text.
    ///
    /// # Errors
    ///
    /// Fails on an unknown header or status, or a line that is not
    /// `size<TAB>path`.
    pub fn parse(text: &str) -> Result<Self, ManifestError> {
        let mut lines = text.lines();
        if lines.next() != Some(HEADER) {
            return Err(ManifestError("it is not a rustlife import record".into()));
        }
        let status = match lines.next() {
            Some("partial") => ImportStatus::Partial,
            Some("complete") => ImportStatus::Complete,
            other => {
                return Err(ManifestError(format!("unknown status {other:?}")));
            }
        };
        let mut files = BTreeMap::new();
        for (number, line) in lines.enumerate() {
            let (size, path) = line
                .split_once('\t')
                .and_then(|(size, path)| Some((size.parse::<u64>().ok()?, path)))
                .filter(|(_, path)| !path.is_empty())
                .ok_or_else(|| ManifestError(format!("line {} is malformed", number + 3)))?;
            files.insert(path.to_owned(), size);
        }
        Ok(Self { status, files })
    }
}

// These run natively; the browser build is tested in `tests/browser.rs`.
#[cfg(all(test, not(target_arch = "wasm32")))]
mod tests {
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn a_manifest_round_trips() {
        let mut manifest = Manifest::new();
        manifest.record("hl2mp/gameinfo.txt", 12);
        manifest.record("hl2/hl2_misc_dir.vpk", 3_000_000_000);
        assert_eq!(manifest.status(), ImportStatus::Partial);
        let parsed = Manifest::parse(&manifest.to_text()).unwrap();
        assert_eq!(parsed, manifest);
        manifest.complete();
        assert_eq!(
            Manifest::parse(&manifest.to_text()).unwrap().status(),
            ImportStatus::Complete
        );
    }

    #[test]
    fn lookups_and_totals() {
        let mut manifest = Manifest::new();
        assert!(manifest.is_empty());
        manifest.record("a", 10);
        manifest.record("b/c", 5);
        manifest.record("a", 20);
        assert_eq!(manifest.size("a"), Some(20), "recording again replaces");
        assert_eq!(manifest.size("missing"), None);
        assert_eq!(manifest.len(), 2);
        assert_eq!(manifest.total_bytes(), 25);
        assert_eq!(
            manifest.files().collect::<Vec<_>>(),
            [("a", 20), ("b/c", 5)]
        );
    }

    #[test]
    fn damaged_text_is_an_error_not_a_panic() {
        for text in [
            "",
            "something else\npartial\n",
            "rustlife-import 1\n",
            "rustlife-import 1\nhalf\n",
            "rustlife-import 1\ncomplete\nno tab here\n",
            "rustlife-import 1\ncomplete\nabc\tpath\n",
            "rustlife-import 1\ncomplete\n12\t\n",
        ] {
            assert!(Manifest::parse(text).is_err(), "{text:?}");
        }
    }

    proptest! {
        #[test]
        fn any_manifest_round_trips(
            files in proptest::collection::btree_map("[a-zA-Z0-9_./ -]{1,30}", any::<u64>(), 0..20),
            complete in any::<bool>(),
        ) {
            let mut manifest = Manifest::new();
            for (path, size) in &files {
                manifest.record(path, *size);
            }
            if complete {
                manifest.complete();
            }
            prop_assert_eq!(Manifest::parse(&manifest.to_text()).unwrap(), manifest);
        }
    }
}
