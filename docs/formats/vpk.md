# VPK archives

Implemented in `crates/source_fs/src/vpk.rs`.

A VPK set is a directory file `<stem>_dir.vpk` plus data archives
`<stem>_000.vpk`, `<stem>_001.vpk`, … Workshop items use the stem `workshop`.

## Header

| Field | Type | Notes |
|-------|------|-------|
| signature | u32 | `0x55aa1234` |
| version | u32 | 1 or 2 |
| tree_size | u32 | bytes of directory tree following the header |
| file_data_section_size | u32 | v2 only |
| archive_md5_section_size | u32 | v2 only |
| other_md5_section_size | u32 | v2 only |
| signature_section_size | u32 | v2 only |

Header length is 12 bytes (v1) or 28 bytes (v2).

## Tree

Three nested NUL-terminated string lists — extension, then directory, then
file name — each terminated by an empty string. A single space means "none"
(root directory, or no extension). Each file name is followed by:

| Field | Type |
|-------|------|
| crc32 | u32 |
| preload_bytes | u16 |
| archive_index | u16 |
| entry_offset | u32 |
| entry_length | u32 |
| terminator | u16 (`0xffff`) |
| preload data | `preload_bytes` bytes |

The file's contents are the preload data followed by `entry_length` bytes at
`entry_offset` in archive `archive_index`. Index `0x7fff` means the data
follows the tree inside the directory file itself.

## Implementation notes

* Paths are normalized to lowercase on load; lookups are case-insensitive.
* Data archives are opened lazily and read with positional I/O, so loads from
  many threads never contend on a file cursor.
* CRC32 is verified in debug builds only.
