//! Byte ranges read asynchronously: the seam between the content readers and
//! wherever the bytes actually live.
//!
//! A native install is read through positional file reads; a browser has no
//! filesystem, only files it was handed (the picked game folder, the
//! browser's private storage), and every read of those is asynchronous. The
//! readers in [`crate::remote`] are written against [`ByteSource`], so the
//! same code serves both, and tests drive it with [`MemorySource`].

use std::fmt;
use std::fs::File;
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use crate::io::read_exact_at;

/// A boxed future, `Send` wherever threads exist.
///
/// In a browser a future usually holds a JavaScript promise, which cannot be
/// sent between threads (there is only one), so the bound is dropped there.
#[cfg(not(target_arch = "wasm32"))]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// A boxed future, `Send` wherever threads exist.
///
/// In a browser a future usually holds a JavaScript promise, which cannot be
/// sent between threads (there is only one), so the bound is dropped there.
#[cfg(target_arch = "wasm32")]
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + 'a>>;

/// A readable run of bytes with a known length: one file.
///
/// Reads take the whole range at once and are independent of each other, so
/// many may be in flight together.
pub trait ByteSource: Send + Sync + fmt::Debug {
    /// Total length in bytes.
    fn len(&self) -> u64;

    /// Whether the source holds no bytes.
    fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Reads exactly `len` bytes starting at `offset`.
    ///
    /// # Errors
    ///
    /// Fails with [`io::ErrorKind::UnexpectedEof`] if the range runs past the
    /// end, or with the underlying error if the read itself fails.
    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Vec<u8>>>;

    /// A short description (usually the location) for logs and errors.
    fn describe(&self) -> String;
}

/// Checks that `offset..offset + len` lies inside a source of `total` bytes.
///
/// # Errors
///
/// [`io::ErrorKind::UnexpectedEof`] when it does not.
pub fn check_range(total: u64, offset: u64, len: usize) -> io::Result<()> {
    let end = offset.checked_add(len as u64);
    if end.is_some_and(|end| end <= total) {
        Ok(())
    } else {
        Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            format!("range of {len} bytes at {offset} is past the end ({total} bytes)"),
        ))
    }
}

/// A [`ByteSource`] over bytes already in memory.
#[derive(Clone)]
pub struct MemorySource {
    label: String,
    data: Arc<[u8]>,
}

impl MemorySource {
    /// A source over `data`, described as `label`.
    #[must_use]
    pub fn new(label: impl Into<String>, data: impl Into<Arc<[u8]>>) -> Self {
        Self {
            label: label.into(),
            data: data.into(),
        }
    }
}

impl fmt::Debug for MemorySource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "MemorySource({}, {} bytes)", self.label, self.data.len())
    }
}

impl ByteSource for MemorySource {
    fn len(&self) -> u64 {
        self.data.len() as u64
    }

    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Vec<u8>>> {
        Box::pin(std::future::ready(
            check_range(self.len(), offset, len).map(|()| {
                let start = offset as usize;
                self.data[start..start + len].to_vec()
            }),
        ))
    }

    fn describe(&self) -> String {
        self.label.clone()
    }
}

/// A [`ByteSource`] over an open file, using positional reads.
///
/// The read happens when the future is first polled, so it is ready at once:
/// a native file has no latency worth yielding for.
pub struct FileSource {
    path: PathBuf,
    file: File,
    len: u64,
}

impl FileSource {
    /// Opens `path` for reading.
    ///
    /// # Errors
    ///
    /// Fails if the file cannot be opened or measured.
    pub fn open(path: impl AsRef<Path>) -> io::Result<Self> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let len = file.metadata()?.len();
        Ok(Self { path, file, len })
    }
}

impl fmt::Debug for FileSource {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "FileSource({}, {} bytes)", self.path.display(), self.len)
    }
}

impl ByteSource for FileSource {
    fn len(&self) -> u64 {
        self.len
    }

    fn read_at(&self, offset: u64, len: usize) -> BoxFuture<'_, io::Result<Vec<u8>>> {
        Box::pin(async move {
            check_range(self.len, offset, len)?;
            let mut buffer = vec![0; len];
            read_exact_at(&self.file, &mut buffer, offset)?;
            Ok(buffer)
        })
    }

    fn describe(&self) -> String {
        self.path.display().to_string()
    }
}

#[cfg(test)]
mod tests {
    use futures_lite::future::block_on;
    use proptest::prelude::*;

    use super::*;

    #[test]
    fn memory_source_reads_a_range() {
        let source = MemorySource::new("test", b"hello world".to_vec());
        assert_eq!(source.len(), 11);
        assert!(!source.is_empty());
        assert_eq!(block_on(source.read_at(6, 5)).unwrap(), b"world");
        assert_eq!(block_on(source.read_at(0, 0)).unwrap(), b"");
        assert_eq!(block_on(source.read_at(11, 0)).unwrap(), b"");
    }

    #[test]
    fn a_range_past_the_end_is_an_error() {
        let source = MemorySource::new("test", b"abc".to_vec());
        let error = block_on(source.read_at(2, 2)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
        let error = block_on(source.read_at(u64::MAX, 2)).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    }

    #[test]
    fn file_source_reads_the_same_bytes_as_memory() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("data.bin");
        let bytes: Vec<u8> = (0..=255).cycle().take(5000).collect();
        std::fs::write(&path, &bytes).unwrap();
        let source = FileSource::open(&path).unwrap();
        assert_eq!(source.len(), 5000);
        assert_eq!(
            block_on(source.read_at(1234, 700)).unwrap(),
            bytes[1234..1934]
        );
        assert!(block_on(source.read_at(4999, 2)).is_err());
        assert!(source.describe().ends_with("data.bin"));
    }

    #[test]
    fn opening_a_missing_file_fails() {
        assert!(FileSource::open("/definitely/not/here.bin").is_err());
    }

    proptest! {
        /// Any in-bounds range reads exactly the matching slice, and any
        /// out-of-bounds range is refused.
        #[test]
        fn ranges_match_slices(
            data in proptest::collection::vec(any::<u8>(), 0..512),
            offset in 0_u64..600,
            len in 0_usize..600,
        ) {
            let source = MemorySource::new("p", data.clone());
            let result = block_on(source.read_at(offset, len));
            let end = offset as usize + len;
            if end <= data.len() {
                prop_assert_eq!(result.unwrap(), data[offset as usize..end].to_vec());
            } else {
                prop_assert!(result.is_err());
            }
        }
    }
}
