//! NeoSymbian (`target_os = "symbian"`): P.I.P.S.'s `mmap` is unusable, so a "map" is a heap buffer.
//!
//! A file map reads the file's range into the buffer; a writable shared map writes its range back on `flush` and
//! drop. The cost is heap memory for the whole range (fonts, for Servo), and other processes don't see writes until
//! a flush. Advice, locking and protection changes are accepted and do nothing.

use std::fs::File;
use std::io;
use std::mem::ManuallyDrop;
use std::os::unix::fs::FileExt;
use std::os::unix::io::{FromRawFd, RawFd};

/// The `libc` names the shared code uses: P.I.P.S. has no `madvise`, so it is a no-op here.
pub mod libc {
    pub use ::libc::*;
    pub const MADV_NORMAL: c_int = 0;
    pub const MADV_RANDOM: c_int = 1;
    pub const MADV_SEQUENTIAL: c_int = 2;
    pub const MADV_WILLNEED: c_int = 3;
    pub const MADV_DONTNEED: c_int = 4;

    /// # Safety
    /// Does nothing.
    pub unsafe fn madvise(_addr: *mut c_void, _len: size_t, _advice: c_int) -> c_int {
        0
    }
}

pub struct MmapInner {
    buf: Box<[u8]>,
    /// For a writable shared map: where `buf` goes back to on flush.
    write_back: Option<(RawFd, u64)>,
}

/// The file behind `fd`, without closing it.
fn file(fd: RawFd) -> ManuallyDrop<File> {
    // SAFETY: the caller's fd stays open; ManuallyDrop never closes it.
    ManuallyDrop::new(unsafe { File::from_raw_fd(fd) })
}

impl MmapInner {
    fn read(len: usize, fd: RawFd, offset: u64, write_back: bool) -> io::Result<MmapInner> {
        let mut buf = vec![0u8; len].into_boxed_slice();
        let f = file(fd);
        let mut done = 0;
        // a short file leaves zeros past its end, as a map past EOF would (minus the SIGBUS)
        while done < len {
            match f.read_at(&mut buf[done..], offset + done as u64)? {
                0 => break,
                n => done += n,
            }
        }
        Ok(MmapInner {
            buf,
            write_back: write_back.then_some((fd, offset)),
        })
    }

    pub fn map(
        len: usize,
        file: RawFd,
        offset: u64,
        _: bool,
        _: bool,
        _: bool,
    ) -> io::Result<MmapInner> {
        MmapInner::read(len, file, offset, false)
    }

    pub fn map_exec(
        len: usize,
        file: RawFd,
        offset: u64,
        _: bool,
        _: bool,
        _: bool,
    ) -> io::Result<MmapInner> {
        MmapInner::read(len, file, offset, false)
    }

    pub fn map_mut(
        len: usize,
        file: RawFd,
        offset: u64,
        _: bool,
        _: bool,
        _: bool,
    ) -> io::Result<MmapInner> {
        MmapInner::read(len, file, offset, true)
    }

    pub fn map_copy(
        len: usize,
        file: RawFd,
        offset: u64,
        _: bool,
        _: bool,
        _: bool,
    ) -> io::Result<MmapInner> {
        MmapInner::read(len, file, offset, false)
    }

    pub fn map_copy_read_only(
        len: usize,
        file: RawFd,
        offset: u64,
        _: bool,
        _: bool,
        _: bool,
    ) -> io::Result<MmapInner> {
        MmapInner::read(len, file, offset, false)
    }

    pub fn map_anon(len: usize, _: bool, _: bool, _: Option<u8>, _: bool) -> io::Result<MmapInner> {
        Ok(MmapInner {
            buf: vec![0u8; len].into_boxed_slice(),
            write_back: None,
        })
    }

    pub fn flush(&self, offset: usize, len: usize) -> io::Result<()> {
        match self.write_back {
            Some((fd, base)) => {
                file(fd).write_all_at(&self.buf[offset..offset + len], base + offset as u64)
            }
            None => Ok(()),
        }
    }

    pub fn flush_async(&self, offset: usize, len: usize) -> io::Result<()> {
        self.flush(offset, len)
    }

    pub fn make_read_only(&mut self) -> io::Result<()> {
        Ok(())
    }

    pub fn make_exec(&mut self) -> io::Result<()> {
        Ok(())
    }

    pub fn make_mut(&mut self) -> io::Result<()> {
        Ok(())
    }

    pub fn ptr(&self) -> *const u8 {
        self.buf.as_ptr()
    }

    pub fn mut_ptr(&mut self) -> *mut u8 {
        self.buf.as_mut_ptr()
    }

    pub fn len(&self) -> usize {
        self.buf.len()
    }

    pub unsafe fn advise(&self, _advice: libc::c_int, offset: usize, len: usize) -> io::Result<()> {
        if offset > self.buf.len() || len > self.buf.len() {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        Ok(())
    }

    pub fn lock(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn unlock(&self) -> io::Result<()> {
        Ok(())
    }

    pub fn check_safe_to_map(
        _fd: RawFd,
        _offset: u64,
        _len: usize,
    ) -> Result<(), crate::MapIfSafeError> {
        Err(crate::MapIfSafeError::new_not_supported())
    }
}

impl Drop for MmapInner {
    fn drop(&mut self) {
        // as munmap of a shared map: the writes reach the file (errors can't be reported from drop)
        let _ = self.flush(0, self.buf.len());
    }
}

pub fn file_len(fd: RawFd) -> io::Result<u64> {
    Ok(file(fd).metadata()?.len())
}

#[cfg(test)]
mod tests {
    #[test]
    fn heap_map_round_trip() {
        use std::io::Write;
        use std::os::unix::io::AsRawFd;
        let mut f = tempfile_like();
        f.write_all(b"hello map").unwrap();
        let m = super::MmapInner::map(5, f.as_raw_fd(), 6, false, false, false).unwrap();
        // SAFETY: ptr/len describe the buffer.
        assert_eq!(
            unsafe { std::slice::from_raw_parts(m.ptr(), m.len()) },
            b"map\0\0"
        );
    }

    fn tempfile_like() -> std::fs::File {
        let p = std::env::temp_dir().join("memmap2-symbian-test");
        std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(true)
            .open(p)
            .unwrap()
    }
}
