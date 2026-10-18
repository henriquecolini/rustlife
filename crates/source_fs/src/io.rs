//! Small portable I/O helpers.

use std::fs::File;
use std::io;

/// Reads exactly `buf.len()` bytes from `file` at absolute `offset` without
/// moving any shared cursor.
///
/// Positional reads let many threads read from one open archive concurrently
/// (the asset server loads textures in parallel) without locking around
/// `seek` + `read`.
pub(crate) fn read_exact_at(file: &File, buf: &mut [u8], offset: u64) -> io::Result<()> {
    #[cfg(unix)]
    {
        std::os::unix::fs::FileExt::read_exact_at(file, buf, offset)
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::FileExt;
        let mut filled = 0;
        while filled < buf.len() {
            match file.seek_read(&mut buf[filled..], offset + filled as u64) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "failed to fill whole buffer",
                    ));
                }
                Ok(n) => filled += n,
                Err(e) if e.kind() == io::ErrorKind::Interrupted => {}
                Err(e) => return Err(e),
            }
        }
        Ok(())
    }
}

/// A bounds-checked little-endian cursor over an in-memory byte slice.
///
/// Every read returns `None` instead of panicking when the data is truncated,
/// which lets parsers turn malformed input into a typed error at the call
/// site.
#[derive(Debug, Clone)]
pub(crate) struct ByteReader<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> ByteReader<'a> {
    pub(crate) fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    pub(crate) fn position(&self) -> usize {
        self.position
    }

    pub(crate) fn bytes(&mut self, len: usize) -> Option<&'a [u8]> {
        let end = self.position.checked_add(len)?;
        let slice = self.data.get(self.position..end)?;
        self.position = end;
        Some(slice)
    }

    pub(crate) fn u16(&mut self) -> Option<u16> {
        self.bytes(2).map(|b| u16::from_le_bytes([b[0], b[1]]))
    }

    pub(crate) fn u32(&mut self) -> Option<u32> {
        self.bytes(4)
            .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
    }

    /// Reads a NUL-terminated string. Invalid UTF-8 is replaced lossily, since
    /// content paths are ASCII in practice and a lossy name is more useful for
    /// diagnostics than an error.
    pub(crate) fn cstr(&mut self) -> Option<String> {
        let rest = self.data.get(self.position..)?;
        let len = rest.iter().position(|&b| b == 0)?;
        let text = String::from_utf8_lossy(&rest[..len]).into_owned();
        self.position += len + 1;
        Some(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    #[test]
    fn byte_reader_reads_and_bounds_checks() {
        let data = [1, 0, 2, 0, 0, 0, b'h', b'i', 0, 9];
        let mut reader = ByteReader::new(&data);
        assert_eq!(reader.u16(), Some(1));
        assert_eq!(reader.u32(), Some(2));
        assert_eq!(reader.cstr().as_deref(), Some("hi"));
        assert_eq!(reader.position(), 9);
        assert_eq!(reader.u16(), None, "only one byte left");
        assert_eq!(reader.cstr(), None, "no terminator");
    }

    #[test]
    fn positional_reads() {
        let mut file = tempfile::tempfile().unwrap();
        file.write_all(b"0123456789").unwrap();
        let mut buf = [0; 3];
        read_exact_at(&file, &mut buf, 4).unwrap();
        assert_eq!(&buf, b"456");
        assert!(read_exact_at(&file, &mut buf, 9).is_err());
    }
}
