//! Platform-specific bulk directory readers.
//!
//! Each platform exposes a struct implementing [`crate::walker::DirReader`]:
//!
//! - macOS: `getattrlistbulk(2)` — one syscall per ~800 entries (with sizes).
//!          `readdir(3)` — faster when sizes are NOT needed.
//! - Linux: `getdents64(2)` + optional `statx(2)` fallback for `DT_UNKNOWN`.
//! - Windows: `NtQueryDirectoryFile` with `FILE_DIRECTORY_INFORMATION`.

#[cfg(target_os = "linux")]
pub mod linux;
#[cfg(target_os = "macos")]
pub mod macos;
#[cfg(target_os = "windows")]
pub mod windows;

use crate::walker::DirReader;

#[derive(Debug, Clone, Copy, Default)]
pub struct ReaderOptions {
    /// Request file sizes. On macOS this selects `getattrlistbulk` with
    /// `ATTR_FILE_DATALENGTH`; without it, `readdir(3)` is used instead
    /// (faster on APFS since the kernel skips attribute resolution).
    pub with_size: bool,
}

pub fn build_reader(options: ReaderOptions) -> Box<dyn DirReader> {
    #[cfg(target_os = "macos")]
    {
        if options.with_size {
            // getattrlistbulk returns name+type+size in ONE syscall batch —
            // much faster than readdir + N × lstat.
            Box::new(macos::AttrListBulkReader::new().with_size(true))
        } else {
            // readdir is faster than getattrlistbulk on APFS when only
            // names + d_type are needed (simpler kernel path).
            Box::new(macos::ReaddirReader::new())
        }
    }
    #[cfg(target_os = "linux")]
    {
        let _ = options;
        Box::new(linux::GetDents64Reader::new())
    }
    #[cfg(target_os = "windows")]
    {
        let _ = options;
        Box::new(windows::NtQueryReader::new())
    }
}
