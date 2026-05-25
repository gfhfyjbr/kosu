//! Linux bulk directory reader via raw `getdents64(2)`.
//!
//! Stub implementation: opens with `O_DIRECTORY`, issues `SYS_getdents64` in
//! a 256 KB buffer, walks the packed `linux_dirent64` records, and falls back
//! to `statx(2)` only when `d_type == DT_UNKNOWN` (some filesystems don't
//! populate `d_type`). No `io_uring` batching yet — that is the obvious next
//! optimisation but the user marked this one as a stub.

#![cfg(target_os = "linux")]

use std::ffi::{CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use libc::{c_int, c_void};

use crate::walker::{DirReader, EntryKind};

const BUF_SIZE: usize = 256 * 1024;

#[repr(C)]
#[derive(Copy, Clone)]
struct LinuxDirent64Header {
    d_ino: u64,
    d_off: i64,
    d_reclen: u16,
    d_type: u8,
    // d_name follows immediately (variable length, NUL-terminated)
}

const DT_UNKNOWN: u8 = 0;
const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

pub struct GetDents64Reader;

impl GetDents64Reader {
    pub fn new() -> Self {
        Self
    }
}

impl DirReader for GetDents64Reader {
    fn read_dir(
        &self,
        dir: &Path,
        each_entry: &mut dyn FnMut(&OsStr, EntryKind, Option<u64>),
    ) -> io::Result<()> {
        let c_dir = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
        // SAFETY: libc::open is a stable C API.
        let fd = unsafe {
            libc::open(
                c_dir.as_ptr(),
                libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC,
                0,
            )
        };
        if fd < 0 {
            return Err(io::Error::last_os_error());
        }
        let _guard = FdGuard(fd);

        let mut buf = vec![0u8; BUF_SIZE];

        loop {
            // SAFETY: SYS_getdents64 writes packed linux_dirent64 records.
            let returned = unsafe {
                libc::syscall(
                    libc::SYS_getdents64,
                    fd,
                    buf.as_mut_ptr() as *mut c_void,
                    buf.len(),
                )
            };
            if returned < 0 {
                return Err(io::Error::last_os_error());
            }
            if returned == 0 {
                return Ok(());
            }
            let total = returned as usize;
            let mut offset: usize = 0;
            while offset < total {
                if offset + size_of::<LinuxDirent64Header>() > total {
                    break;
                }
                // SAFETY: bounds checked.
                let header = unsafe {
                    (buf.as_ptr().add(offset) as *const LinuxDirent64Header).read_unaligned()
                };
                let reclen = header.d_reclen as usize;
                if reclen == 0 || offset + reclen > total {
                    break;
                }

                let name_off = offset + size_of::<LinuxDirent64Header>();
                let name_end_max = offset + reclen;
                // d_name is NUL-terminated; find the actual length.
                let mut name_end = name_off;
                while name_end < name_end_max && buf[name_end] != 0 {
                    name_end += 1;
                }
                let name_bytes = &buf[name_off..name_end];
                if name_bytes != b"." && name_bytes != b".." && !name_bytes.is_empty() {
                    let name = OsStr::from_bytes(name_bytes);
                    let kind = match header.d_type {
                        DT_REG => EntryKind::File,
                        DT_DIR => EntryKind::Dir,
                        DT_LNK => EntryKind::Symlink,
                        DT_UNKNOWN => statx_kind(fd, name_bytes).unwrap_or(EntryKind::Other),
                        _ => EntryKind::Other,
                    };
                    each_entry(name, kind, None);
                }
                offset += reclen;
            }
        }
    }
}

fn statx_kind(dirfd: c_int, name: &[u8]) -> Option<EntryKind> {
    let c_name = CString::new(name).ok()?;
    let mut buf: libc::statx = unsafe { core::mem::zeroed() };
    // SAFETY: dirfd is a live directory fd; c_name is NUL-terminated.
    let result = unsafe {
        libc::statx(
            dirfd,
            c_name.as_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
            libc::STATX_TYPE,
            &mut buf,
        )
    };
    if result != 0 {
        return None;
    }
    let mode = u32::from(buf.stx_mode) & libc::S_IFMT;
    Some(match mode {
        libc::S_IFREG => EntryKind::File,
        libc::S_IFDIR => EntryKind::Dir,
        libc::S_IFLNK => EntryKind::Symlink,
        _ => EntryKind::Other,
    })
}

struct FdGuard(c_int);

impl Drop for FdGuard {
    fn drop(&mut self) {
        // SAFETY: fd was opened in read_dir and is not used after drop.
        unsafe {
            libc::close(self.0);
        }
    }
}
