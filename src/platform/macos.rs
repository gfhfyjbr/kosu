//! macOS bulk directory reader via `getattrlistbulk(2)`.
//!
//! Parses the packed `attribute_set_t` -> attribute fields layout described
//! in `sys/attr.h`. One syscall returns up to ~800 entries with metadata in a
//! 256 KB buffer, dropping the syscall count from O(N) to O(N/batch).

#![cfg(target_os = "macos")]

use std::cell::RefCell;
use std::ffi::{CString, OsStr};
use std::io;
use std::os::unix::ffi::OsStrExt;
use std::path::Path;

use libc::{c_int, c_void, size_t};

use crate::walker::{DirReader, EntryKind};

thread_local! {
    /// Per-thread scratch buffer reused across `read_dir` calls. Saves a
    /// 256 KB allocation per directory (a measurable cost when walking
    /// hundreds of thousands of dirs).
    static ATTR_BUF: RefCell<Vec<u8>> = const { RefCell::new(Vec::new()) };
}

const ATTR_BIT_MAP_COUNT: u16 = 5;

const ATTR_CMN_NAME: u32 = 0x00000001;
const ATTR_CMN_OBJTYPE: u32 = 0x00000008;
const ATTR_CMN_RETURNED_ATTRS: u32 = 0x80000000;
const ATTR_FILE_DATALENGTH: u32 = 0x00000200;

const FSOPT_NOFOLLOW: u64 = 0x00000001;

// fsobj_type_t / vnode types from sys/vnode.h.
const VREG: u32 = 1;
const VDIR: u32 = 2;
const VLNK: u32 = 5;

#[repr(C)]
struct AttrList {
    bitmapcount: u16,
    reserved: u16,
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct AttributeSet {
    commonattr: u32,
    volattr: u32,
    dirattr: u32,
    fileattr: u32,
    forkattr: u32,
}

#[repr(C)]
#[derive(Copy, Clone)]
struct AttrReference {
    attr_dataoffset: i32,
    attr_length: u32,
}

unsafe extern "C" {
    fn getattrlistbulk(
        dirfd: c_int,
        attr_list: *mut AttrList,
        attr_buf: *mut c_void,
        attr_buf_size: size_t,
        options: u64,
    ) -> c_int;
}

/// Bulk syscall scratch buffer. 256 KiB consistently outperforms larger
/// sizes here: per-thread allocation pressure stays low and the FS fits the
/// vast majority of directories in one call anyway.
const BUF_SIZE: usize = 256 * 1024;

pub struct AttrListBulkReader {
    request_size: bool,
}

impl AttrListBulkReader {
    pub fn new() -> Self {
        Self { request_size: false }
    }

    pub fn with_size(mut self, request_size: bool) -> Self {
        self.request_size = request_size;
        self
    }
}

impl DirReader for AttrListBulkReader {
    fn read_dir(
        &self,
        dir: &Path,
        each_entry: &mut dyn FnMut(&OsStr, EntryKind, Option<u64>),
    ) -> io::Result<()> {
        let c_dir = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
        // SAFETY: standard libc open(2) with O_DIRECTORY; fd closed by FdGuard.
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

        let fileattr = if self.request_size {
            ATTR_FILE_DATALENGTH
        } else {
            0
        };
        let mut attr_list = AttrList {
            bitmapcount: ATTR_BIT_MAP_COUNT,
            reserved: 0,
            commonattr: ATTR_CMN_RETURNED_ATTRS | ATTR_CMN_NAME | ATTR_CMN_OBJTYPE,
            volattr: 0,
            dirattr: 0,
            fileattr,
            forkattr: 0,
        };
        ATTR_BUF.with(|cell| {
            let mut buf_ref = cell.borrow_mut();
            if buf_ref.is_empty() {
                buf_ref.resize(BUF_SIZE, 0);
            }
            let buf = buf_ref.as_mut_slice();
            loop {
                // SAFETY: fd is valid, buf is thread-local scratch (BUF_SIZE
                // bytes), attr_list lives on this stack frame.
                let entries_returned = unsafe {
                    getattrlistbulk(
                        fd,
                        &mut attr_list,
                        buf.as_mut_ptr() as *mut c_void,
                        buf.len(),
                        FSOPT_NOFOLLOW,
                    )
                };
                if entries_returned < 0 {
                    return Err(io::Error::last_os_error());
                }
                if entries_returned == 0 {
                    return Ok(());
                }
                parse_buffer(buf, entries_returned as usize, each_entry);
            }
        })
    }
}

fn parse_buffer(
    buf: &[u8],
    entries: usize,
    each_entry: &mut dyn FnMut(&OsStr, EntryKind, Option<u64>),
) {
    let mut cursor: usize = 0;
    for _ in 0..entries {
        let entry_start = cursor;
        if cursor + 4 > buf.len() {
            return;
        }
        // SAFETY: bounds checked above.
        let length = unsafe { (buf.as_ptr().add(cursor) as *const u32).read_unaligned() } as usize;
        if length == 0 || entry_start + length > buf.len() {
            return;
        }
        let entry_end = entry_start + length;
        let mut field = entry_start + 4;

        if field + size_of::<AttributeSet>() > entry_end {
            cursor = entry_end;
            continue;
        }
        // SAFETY: bounds checked.
        let returned = unsafe {
            (buf.as_ptr().add(field) as *const AttributeSet).read_unaligned()
        };
        field += size_of::<AttributeSet>();

        let mut name_slot: Option<&OsStr> = None;
        if returned.commonattr & ATTR_CMN_NAME != 0 {
            if field + size_of::<AttrReference>() > entry_end {
                cursor = entry_end;
                continue;
            }
            let attr_ref_pos = field;
            // SAFETY: bounds checked.
            let name_ref = unsafe {
                (buf.as_ptr().add(attr_ref_pos) as *const AttrReference).read_unaligned()
            };
            field += size_of::<AttrReference>();

            let data_offset = name_ref.attr_dataoffset as i64;
            let data_len = name_ref.attr_length as usize;
            let name_off = attr_ref_pos as i64 + data_offset;
            if name_off >= 0 && data_len > 0 {
                let name_off = name_off as usize;
                let raw_end = name_off + data_len;
                if raw_end <= buf.len() {
                    // attr_length includes the trailing NUL byte.
                    let no_nul = name_off + data_len - 1;
                    name_slot = Some(OsStr::from_bytes(&buf[name_off..no_nul]));
                }
            }
        }

        let mut kind = EntryKind::Other;
        if returned.commonattr & ATTR_CMN_OBJTYPE != 0
            && field + 4 <= entry_end {
                // SAFETY: bounds checked.
                let object_type =
                    unsafe { (buf.as_ptr().add(field) as *const u32).read_unaligned() };
                kind = match object_type {
                    VREG => EntryKind::File,
                    VDIR => EntryKind::Dir,
                    VLNK => EntryKind::Symlink,
                    _ => EntryKind::Other,
                };
                field += 4;
            }

        let mut size: Option<u64> = None;
        if returned.fileattr & ATTR_FILE_DATALENGTH != 0
            && field + 8 <= entry_end
        {
            // SAFETY: bounds checked.
            let bytes = unsafe { (buf.as_ptr().add(field) as *const u64).read_unaligned() };
            size = Some(bytes);
        }

        if let Some(name) = name_slot
            && !name.is_empty()
        {
            each_entry(name, kind, size);
        }

        cursor = entry_end;
    }
}

struct FdGuard(c_int);

impl Drop for FdGuard {
    fn drop(&mut self) {
        // SAFETY: fd was opened in read_dir and is not used after this drop.
        unsafe {
            libc::close(self.0);
        }
    }
}

// ---------------------------------------------------------------------------
// readdir(3)-based reader — faster than getattrlistbulk on APFS when only
// names + d_type are needed (no file sizes). Apple kernel engineer:
//   "If all you need is filenames and no other attributes, readdir is usually
//    faster than getattrlistbulk because it doesn't have to do as much work."
// ---------------------------------------------------------------------------

const DT_DIR: u8 = 4;
const DT_REG: u8 = 8;
const DT_LNK: u8 = 10;

pub struct ReaddirReader {
    want_size: bool,
}

impl ReaddirReader {
    pub fn new() -> Self {
        Self { want_size: false }
    }

    pub fn with_size(mut self, want: bool) -> Self {
        self.want_size = want;
        self
    }
}

impl DirReader for ReaddirReader {
    fn read_dir(
        &self,
        dir: &Path,
        each_entry: &mut dyn FnMut(&OsStr, EntryKind, Option<u64>),
    ) -> io::Result<()> {
        let c_dir = CString::new(dir.as_os_str().as_bytes())
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidInput, "path contains NUL byte"))?;
        // SAFETY: c_dir is NUL-terminated, opendir returns NULL on failure.
        let dirp = unsafe { libc::opendir(c_dir.as_ptr()) };
        if dirp.is_null() {
            return Err(io::Error::last_os_error());
        }
        let _guard = DirpGuard(dirp);

        // For lstat: reuse a stack buffer for the full child path.
        let dir_bytes = dir.as_os_str().as_bytes();
        let mut path_buf: Vec<u8> = Vec::with_capacity(dir_bytes.len() + 256);
        path_buf.extend_from_slice(dir_bytes);
        if !dir_bytes.ends_with(b"/") {
            path_buf.push(b'/');
        }
        let prefix_len = path_buf.len();

        loop {
            *errno_ptr() = 0;
            // SAFETY: dirp is a live DIR* opened above.
            let entry = unsafe { libc::readdir(dirp) };
            if entry.is_null() {
                let error = *errno_ptr();
                if error != 0 {
                    return Err(io::Error::from_raw_os_error(error));
                }
                return Ok(());
            }
            // SAFETY: entry is non-null and points inside the DIR stream's
            // internal buffer, valid until the next readdir/closedir.
            let d = unsafe { &*entry };
            let name_ptr = d.d_name.as_ptr();
            // SAFETY: d_name is NUL-terminated by the kernel.
            let name_bytes = unsafe { std::ffi::CStr::from_ptr(name_ptr) }.to_bytes();
            if name_bytes == b"." || name_bytes == b".." {
                continue;
            }
            let name = OsStr::from_bytes(name_bytes);
            let kind = match d.d_type {
                DT_REG => EntryKind::File,
                DT_DIR => EntryKind::Dir,
                DT_LNK => EntryKind::Symlink,
                _ => EntryKind::Other,
            };

            let size = if self.want_size && kind == EntryKind::File {
                // Build NUL-terminated path in reusable buffer.
                path_buf.truncate(prefix_len);
                path_buf.extend_from_slice(name_bytes);
                path_buf.push(0);
                let mut stat_buf: libc::stat = unsafe { core::mem::zeroed() };
                // SAFETY: path_buf is NUL-terminated; stat_buf is zeroed out.
                let result = unsafe {
                    libc::lstat(
                        path_buf.as_ptr() as *const libc::c_char,
                        &mut stat_buf,
                    )
                };
                path_buf.pop(); // remove NUL
                if result == 0 {
                    Some(stat_buf.st_size as u64)
                } else {
                    None
                }
            } else {
                None
            };
            each_entry(name, kind, size);
        }
    }
}

fn errno_ptr() -> &'static mut i32 {
    // SAFETY: __error() returns a per-thread errno pointer on macOS.
    unsafe { &mut *libc::__error() }
}

struct DirpGuard(*mut libc::DIR);

impl Drop for DirpGuard {
    fn drop(&mut self) {
        // SAFETY: dirp was opened in read_dir and is not used after drop.
        unsafe {
            libc::closedir(self.0);
        }
    }
}
