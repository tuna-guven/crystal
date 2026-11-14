//! Reading Crystal's files that are not secret: the two small plaintext
//! files, `VERSION` and the `*.gen` counters, both of the form "decimal
//! number, newline" (docs/decisions/0012-public-bytes.md), and the
//! ciphertext files, always with a size limit. Writing goes through the
//! atomic writer.

use std::os::fd::OwnedFd;

use rustix::fs::{AtFlags, FileType, Mode, OFlags, Stat, fstat, openat, statat};
use rustix::io::{Errno, read};

/// Why a small public file could not be read. Errnos only.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ReadError {
    /// A symlink, directory, FIFO, device or socket.
    NotARegularFile,
    Open(Errno),
    Stat(Errno),
    Read(Errno),
}

impl std::fmt::Display for ReadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NotARegularFile => write!(f, "not a regular file"),
            Self::Open(errno) => write!(f, "open failed: {errno}"),
            Self::Stat(errno) => write!(f, "stat failed: {errno}"),
            Self::Read(errno) => write!(f, "read failed: {errno}"),
        }
    }
}

/// Reads `name` in `dir`: at most `max_len + 1` bytes (one byte past the
/// limit shows that the file is too long, without reading the rest), or
/// `None` if it does not exist. Only a regular file is read: `O_NOFOLLOW`
/// refuses a symlink, `O_NONBLOCK` keeps a FIFO from blocking the open, and
/// `fstat` refuses anything that is not a regular file. The buffer is sized
/// from the file's size once, capped at the limit. Used for the small
/// plaintext files and for ciphertext (`config.age`, `identity.age`,
/// `vault.age`), which is not secret.
pub fn read_bounded(
    dir: &OwnedFd,
    name: &str,
    max_len: usize,
) -> Result<Option<Vec<u8>>, ReadError> {
    Ok(read_bounded_with_stamp(dir, name, max_len)?.map(|(bytes, _)| bytes))
}

/// One version of a file: its device, inode, size, and modification and
/// change times. Replacing a file atomically (Crystal's writer, Syncthing)
/// makes a new inode, and any write changes the change time, so a file
/// with the same stamp has not been touched. Plain numbers, not secret.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct FileStamp([i128; 7]);

impl FileStamp {
    fn of(stat: &Stat) -> Self {
        Self([
            i128::from(stat.st_dev),
            i128::from(stat.st_ino),
            i128::from(stat.st_size),
            i128::from(stat.st_mtime),
            i128::from(stat.st_mtime_nsec),
            i128::from(stat.st_ctime),
            i128::from(stat.st_ctime_nsec),
        ])
    }
}

/// The stamp of `name` in `dir` now, without following a symlink; `None`
/// if it does not exist.
pub fn stamp(dir: &OwnedFd, name: &str) -> Result<Option<FileStamp>, Errno> {
    match statat(dir, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(FileStamp::of(&stat))),
        Err(Errno::NOENT) => Ok(None),
        Err(errno) => Err(errno),
    }
}

/// As `read_bounded`, with the stamp of the file that was read, taken from
/// the same open file, so it describes exactly these bytes.
pub fn read_bounded_with_stamp(
    dir: &OwnedFd,
    name: &str,
    max_len: usize,
) -> Result<Option<(Vec<u8>, FileStamp)>, ReadError> {
    let flags = OFlags::RDONLY | OFlags::NOFOLLOW | OFlags::NONBLOCK | OFlags::CLOEXEC;
    let file = match openat(dir, name, flags, Mode::empty()) {
        Ok(file) => file,
        Err(Errno::NOENT) => return Ok(None),
        Err(Errno::LOOP) => return Err(ReadError::NotARegularFile),
        Err(errno) => return Err(ReadError::Open(errno)),
    };
    let stat = fstat(&file).map_err(ReadError::Stat)?;
    if FileType::from_raw_mode(stat.st_mode) != FileType::RegularFile {
        return Err(ReadError::NotARegularFile);
    }
    let limit = max_len.saturating_add(1);
    let size_hint = usize::try_from(stat.st_size).unwrap_or(usize::MAX);
    let mut buf = vec![0u8; size_hint.saturating_add(1).min(limit)];
    let mut filled = 0;
    loop {
        if filled == buf.len() {
            if buf.len() == limit {
                break;
            }
            // The file grew since fstat: grow up to the limit.
            buf.resize((buf.len() * 2).clamp(1, limit), 0);
        }
        match read(&file, &mut buf[filled..]) {
            Ok(0) => break,
            Ok(n) => filled += n,
            Err(Errno::INTR) => {}
            Err(errno) => return Err(ReadError::Read(errno)),
        }
    }
    buf.truncate(filled);
    // fstat again after reading: a write during the read changes the
    // change time, so the stamp never vouches for bytes it did not see.
    let after = fstat(&file).map_err(ReadError::Stat)?;
    Ok(Some((buf, FileStamp::of(&after))))
}

/// Parses "decimal number, newline" with exactly one spelling per number:
/// ASCII digits without sign or leading zero (`0` itself is fine), then
/// one `\n`, nothing else (no `\r`, spaces or second line). `None` for
/// anything else, including a number over `u64::MAX`.
pub fn parse_decimal_line(bytes: &[u8]) -> Option<u64> {
    let digits = bytes
        .strip_suffix(b"\n")
        .filter(|d| !d.is_empty() && d.iter().all(u8::is_ascii_digit))
        .filter(|d| d.len() == 1 || d[0] != b'0')?;
    std::str::from_utf8(digits).ok()?.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_lines() {
        let ok: [(&[u8], u64); 4] = [
            (b"0\n", 0),
            (b"1\n", 1),
            (b"65535\n", 65535),
            (b"18446744073709551615\n", u64::MAX),
        ];
        for (bytes, n) in ok {
            assert_eq!(parse_decimal_line(bytes), Some(n), "{bytes:?}");
        }
        let bad: [&[u8]; 14] = [
            b"",
            b"1",
            b"\n",
            b"01\n",
            b"00\n",
            b"+1\n",
            b"-1\n",
            b" 1\n",
            b"1 \n",
            b"1\r\n",
            b"1\n\n",
            b"1\nx",
            b"1.0\n",
            b"18446744073709551616\n",
        ];
        for bytes in bad {
            assert_eq!(parse_decimal_line(bytes), None, "{bytes:?}");
        }
    }
}
