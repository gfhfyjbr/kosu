//! Windows bulk directory reader via `NtQueryDirectoryFile`.
//!
//! Stub implementation: opens a directory handle with `CreateFileW` +
//! `FILE_FLAG_BACKUP_SEMANTICS`, then loops over `NtQueryDirectoryFile` with
//! `FileDirectoryInformation` records into a 64 KB buffer. Per record we get
//! attributes, EOF (size), and the wide-char file name inline.

#![cfg(target_os = "windows")]

use std::ffi::OsStr;
use std::io;
use std::os::windows::ffi::{OsStrExt, OsStringExt};
use std::path::Path;
use std::ptr;

use windows_sys::Wdk::Storage::FileSystem::{
    FILE_DIRECTORY_INFORMATION, FILE_INFORMATION_CLASS, NtQueryDirectoryFile,
};
use windows_sys::Win32::Foundation::{
    CloseHandle, HANDLE, INVALID_HANDLE_VALUE, NTSTATUS, STATUS_NO_MORE_FILES, STATUS_SUCCESS,
};
use windows_sys::Win32::Storage::FileSystem::{
    CreateFileW, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_FLAG_BACKUP_SEMANTICS, FILE_LIST_DIRECTORY, FILE_SHARE_DELETE, FILE_SHARE_READ,
    FILE_SHARE_WRITE, OPEN_EXISTING,
};
use windows_sys::Win32::System::IO::IO_STATUS_BLOCK;

use crate::walker::{DirReader, EntryKind};

const FILE_DIRECTORY_INFORMATION_CLASS: FILE_INFORMATION_CLASS = 1;
const BUF_SIZE: usize = 64 * 1024;

pub struct NtQueryReader;

impl NtQueryReader {
    pub fn new() -> Self {
        Self
    }
}

impl DirReader for NtQueryReader {
    fn read_dir(
        &self,
        dir: &Path,
        each_entry: &mut dyn FnMut(&OsStr, EntryKind, Option<u64>),
    ) -> io::Result<()> {
        let wide = dir
            .as_os_str()
            .encode_wide()
            .chain(std::iter::once(0))
            .collect::<Vec<u16>>();

        // SAFETY: CreateFileW with FILE_FLAG_BACKUP_SEMANTICS opens a dir handle.
        let handle = unsafe {
            CreateFileW(
                wide.as_ptr(),
                FILE_LIST_DIRECTORY,
                FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE,
                ptr::null(),
                OPEN_EXISTING,
                FILE_FLAG_BACKUP_SEMANTICS,
                ptr::null_mut(),
            )
        };
        if handle == INVALID_HANDLE_VALUE {
            return Err(io::Error::last_os_error());
        }
        let _guard = HandleGuard(handle);

        let mut buf = vec![0u8; BUF_SIZE];
        let mut first = true;

        loop {
            let mut io_status: IO_STATUS_BLOCK = unsafe { core::mem::zeroed() };
            // SAFETY: NtQueryDirectoryFile fills FILE_DIRECTORY_INFORMATION records.
            let status: NTSTATUS = unsafe {
                NtQueryDirectoryFile(
                    handle,
                    ptr::null_mut(),
                    None,
                    ptr::null(),
                    &mut io_status,
                    buf.as_mut_ptr() as *mut _,
                    buf.len() as u32,
                    FILE_DIRECTORY_INFORMATION_CLASS,
                    0,
                    ptr::null(),
                    u8::from(first),
                )
            };
            first = false;
            if status == STATUS_NO_MORE_FILES {
                return Ok(());
            }
            if status != STATUS_SUCCESS {
                return Err(io::Error::from_raw_os_error(status as i32));
            }

            let mut offset: usize = 0;
            loop {
                if offset + size_of::<FILE_DIRECTORY_INFORMATION>() > buf.len() {
                    break;
                }
                // SAFETY: bounds checked; record is unaligned-read safely.
                let record = unsafe {
                    (buf.as_ptr().add(offset) as *const FILE_DIRECTORY_INFORMATION).read_unaligned()
                };
                let name_offset = offset + core::mem::offset_of!(FILE_DIRECTORY_INFORMATION, FileName);
                let name_chars = (record.FileNameLength as usize) / 2;
                if name_offset + name_chars * 2 > buf.len() {
                    break;
                }
                // SAFETY: bounds checked; UTF-16 buffer is laid out inline.
                let name_slice = unsafe {
                    std::slice::from_raw_parts(
                        buf.as_ptr().add(name_offset) as *const u16,
                        name_chars,
                    )
                };
                let name_os = std::ffi::OsString::from_wide(name_slice);
                if name_os != "." && name_os != ".." {
                    let attributes = record.FileAttributes;
                    let kind = if attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                        EntryKind::Symlink
                    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                        EntryKind::Dir
                    } else {
                        EntryKind::File
                    };
                    let size = record.EndOfFile as u64;
                    each_entry(name_os.as_os_str(), kind, Some(size));
                }
                if record.NextEntryOffset == 0 {
                    break;
                }
                offset += record.NextEntryOffset as usize;
            }
        }
    }
}

struct HandleGuard(HANDLE);

impl Drop for HandleGuard {
    fn drop(&mut self) {
        // SAFETY: handle was opened in read_dir and is not used after drop.
        unsafe {
            CloseHandle(self.0);
        }
    }
}
