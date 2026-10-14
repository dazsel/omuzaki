//! Opening beneath an allowed folder on Windows, never through a symbolic link or a junction.
//!
//! Win32 has no "open relative to this folder handle", so every step is opened by its full path with
//! `FILE_FLAG_OPEN_REPARSE_POINT`, which opens a link itself rather than what it points at, and refused when it is a
//! name surrogate (a symbolic link, a junction, a WSL link): a link that was there before the open is never followed
//! and its target is never touched. A link swapped in *during* the walk could still be followed at an earlier step,
//! because each step re-reads the path, so every handle that is used is checked once more where it really is
//! (`GetFinalPathNameByHandleW`) against the allowed folder, and refused unless it lies inside. Nothing is emptied
//! before that check passes, and a file or folder created by an open that then fails the check is deleted again.
//!
//! Other reparse points (`OneDrive` placeholders, deduplicated files) are not links: they are opened normally, then
//! checked the same way.
// Why unsafe: the Win32 calls above have no safe wrapper in std. Each call gets a NUL-terminated wide string it
// owns, or a buffer sized for the struct it fills, and every handle is owned by an `OwnedHandle` or `File` at once.
#![allow(unsafe_code)]

use std::ffi::c_void;
use std::fs::File;
use std::io;
use std::mem::size_of;
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::ptr;

use windows_sys::Win32::Foundation::{
    ERROR_ALREADY_EXISTS, ERROR_NO_MORE_FILES, GENERIC_READ, GENERIC_WRITE, GetLastError, INVALID_HANDLE_VALUE,
};
use windows_sys::Win32::Storage::FileSystem::{
    CREATE_NEW, CreateDirectoryW, CreateFileW, DELETE, FILE_ATTRIBUTE_DIRECTORY, FILE_ATTRIBUTE_REPARSE_POINT,
    FILE_ATTRIBUTE_TAG_INFO, FILE_CREATION_DISPOSITION, FILE_DISPOSITION_INFO, FILE_FLAG_BACKUP_SEMANTICS,
    FILE_FLAG_OPEN_REPARSE_POINT, FILE_FLAGS_AND_ATTRIBUTES, FILE_ID_BOTH_DIR_INFO, FILE_LIST_DIRECTORY,
    FILE_NAME_NORMALIZED, FILE_READ_ATTRIBUTES, FILE_RENAME_INFO, FILE_SHARE_DELETE, FILE_SHARE_READ, FILE_SHARE_WRITE,
    FILE_STANDARD_INFO, FileAttributeTagInfo, FileDispositionInfo, FileIdBothDirectoryInfo,
    FileIdBothDirectoryRestartInfo, FileRenameInfo, FileStandardInfo, GetFileInformationByHandleEx,
    GetFinalPathNameByHandleW, OPEN_ALWAYS, OPEN_EXISTING, SetFileInformationByHandle, VOLUME_NAME_DOS,
};

use super::{DirEntryInfo, EntryKind, Failure, How, first_free};
use crate::file_access::form::{Lexical, PathKey, Style};

/// Bit 29 of a reparse tag: the reparse point names another file (symbolic links, junctions, WSL links). These are
/// the links refused; other reparse points (cloud placeholders, deduplication) hold data and are followed.
const NAME_SURROGATE: u32 = 0x2000_0000;

/// A folder below the allowed one, every step of the way checked not to be a link.
#[derive(Debug)]
pub(in crate::file_access) struct Dir {
    /// Its full path in `\\?\` form, so no Win32 path rewriting (trailing dots, `..`, `MAX_PATH`) applies.
    path: String,
    /// The allowed folder, as a key: every handle used must really lie inside it.
    anchor: PathKey,
}

impl Dir {
    /// The allowed folder `place` names (its first `depth` names), opened step by step from its drive or share.
    pub(super) fn anchor(place: &Lexical, depth: usize) -> Result<Self, Failure> {
        let root = Self { path: place.verbatim(0), anchor: place.truncated(depth).key(Style::Windows) };
        root.descend(&place.names()[..depth.min(place.depth())], false)
    }

    /// The folder `names` below this one, each a real folder and never a link; missing ones are made with `create`.
    pub(super) fn descend(self, names: &[String], create: bool) -> Result<Self, Failure> {
        names.iter().try_fold(self, |dir, name| dir.child(name, create))
    }

    fn child(&self, name: &str, create: bool) -> Result<Self, Failure> {
        let path = self.join(name);
        let mut made = false;
        let handle = match open(&path, FILE_READ_ATTRIBUTES, OPEN_EXISTING, FOLDER_LINK) {
            Err(Failure::NotFound) if create => {
                made = make_folder(&path)?;
                // A folder this open made is checked like a file below, and deleted again if it is not inside.
                let access = if made { FILE_READ_ATTRIBUTES | DELETE } else { FILE_READ_ATTRIBUTES };
                open(&path, access, OPEN_EXISTING, FOLDER_LINK)?.handle
            }
            opened => opened?.handle,
        };
        let checked = check_folder(&handle).and_then(|()| if made { self.verify(&handle) } else { Ok(()) });
        if let Err(failure) = checked {
            if made {
                delete_on_close(&handle);
            }
            return Err(failure);
        }
        Ok(Self { path, anchor: self.anchor.clone() })
    }

    /// The regular file `name` in this folder, opened as `how` says and never through a link.
    pub(super) fn file(&self, name: &str, how: How) -> Result<File, Failure> {
        let path = self.join(name);
        let (access, disposition) = match how {
            How::Read => (GENERIC_READ, OPEN_EXISTING),
            How::Write => (GENERIC_WRITE | DELETE, OPEN_ALWAYS),
            How::New => (GENERIC_WRITE | DELETE, CREATE_NEW),
        };
        let access = access | FILE_READ_ATTRIBUTES;
        let mut opened = open(&path, access, disposition, FILE_LINK)?;
        let made = how == How::New || (how == How::Write && !opened.existed);
        if let Err(failure) = self.check_file(&mut opened.handle, &path, access, how) {
            if made {
                delete_on_close(&opened.handle);
            }
            return Err(failure);
        }
        let file = File::from(opened.handle);
        // Emptied only now, after the checks: `OPEN_ALWAYS` keeps the contents, unlike `CREATE_ALWAYS`.
        if how == How::Write {
            file.set_len(0).map_err(Failure::Os)?;
        }
        Ok(file)
    }

    /// Refuses a link, a folder, a second name (for an in-place write) and a handle that is not inside the anchor.
    /// A data reparse point is reopened without `FILE_FLAG_OPEN_REPARSE_POINT` so its filter can supply the contents.
    fn check_file(&self, handle: &mut OwnedHandle, path: &str, access: u32, how: How) -> Result<(), Failure> {
        let tag = attribute_tag(handle)?;
        if tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            if tag.ReparseTag & NAME_SURROGATE != 0 {
                return Err(Failure::Refused);
            }
            *handle = open(path, access, OPEN_EXISTING, 0)?.handle;
        }
        let standard = standard_info(handle)?;
        if standard.Directory {
            return Err(Failure::NotAFile);
        }
        if how == How::Write && standard.NumberOfLinks > 1 {
            return Err(Failure::Linked);
        }
        self.verify(handle)
    }

    /// Whether the handle really lies inside the anchor, wherever the path that opened it led.
    fn verify(&self, handle: &OwnedHandle) -> Result<(), Failure> {
        let real = final_path(handle)?;
        let real = PathKey::absolute(&real, Style::Windows).ok_or(Failure::Refused)?;
        if self.anchor.covers(&real) { Ok(()) } else { Err(Failure::Refused) }
    }

    /// The entries of this folder, read from a handle opened on it now: not a link, and really inside the anchor.
    /// Each entry's kind comes from its attributes and reparse tag as the folder reports them, so a link is never
    /// opened, let alone followed.
    pub(super) fn list(&self) -> Result<Vec<DirEntryInfo>, Failure> {
        let handle = open(&self.path, FILE_LIST_DIRECTORY | FILE_READ_ATTRIBUTES, OPEN_EXISTING, FOLDER_LINK)?.handle;
        check_folder(&handle)?;
        self.verify(&handle)?;
        // 64 KiB, 8-byte aligned for `FILE_ID_BOTH_DIR_INFO`; each call fills as many whole entries as fit.
        let mut buffer = vec![0_u64; 8192];
        let size = u32::try_from(buffer.len() * size_of::<u64>()).map_err(|_| Failure::Refused)?;
        let mut entries = Vec::new();
        let mut class = FileIdBothDirectoryRestartInfo;
        loop {
            // SAFETY: `buffer` is `size` bytes; the call writes whole `FILE_ID_BOTH_DIR_INFO` records into it.
            let ok = unsafe {
                GetFileInformationByHandleEx(handle.as_raw_handle(), class, buffer.as_mut_ptr().cast::<c_void>(), size)
            };
            class = FileIdBothDirectoryInfo;
            if ok == 0 {
                let error = io::Error::last_os_error();
                if error.raw_os_error() == i32::try_from(ERROR_NO_MORE_FILES).ok() {
                    return Ok(entries);
                }
                return Err(Failure::Os(error));
            }
            let first = buffer.as_ptr().cast::<FILE_ID_BOTH_DIR_INFO>();
            let mut offset = 0_usize;
            loop {
                // SAFETY: `offset` starts at 0 and then follows `NextEntryOffset`, which the OS wrote, so it points at
                // the start of a record inside the buffer; records are 8-byte aligned.
                let record = unsafe { first.byte_add(offset) };
                // SAFETY: as above; the name is `FileNameLength` bytes of UTF-16 right after the fixed fields.
                let (next, attributes, tag, name) = unsafe {
                    let length = usize::try_from((*record).FileNameLength).map_err(|_| Failure::Refused)? / 2;
                    let units = std::slice::from_raw_parts(ptr::addr_of!((*record).FileName).cast::<u16>(), length);
                    ((*record).NextEntryOffset, (*record).FileAttributes, (*record).EaSize, String::from_utf16(units))
                };
                if let Ok(name) = name
                    && name != "."
                    && name != ".."
                {
                    // For a reparse point the folder stores its tag where `EaSize` is.
                    let link = attributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && tag & NAME_SURROGATE != 0;
                    let kind = if link {
                        EntryKind::Link
                    } else if attributes & FILE_ATTRIBUTE_DIRECTORY != 0 {
                        EntryKind::Folder
                    } else {
                        EntryKind::File
                    };
                    entries.push(DirEntryInfo { name, kind });
                }
                if next == 0 {
                    break;
                }
                offset += usize::try_from(next).map_err(|_| Failure::Refused)?;
            }
        }
    }

    fn join(&self, name: &str) -> String {
        if self.path.ends_with('\\') { format!("{}{name}", self.path) } else { format!("{}\\{name}", self.path) }
    }
}

/// A replacement's new file, under a random name (Windows has no unnamed files). It is renamed and deleted by its
/// handle, so swapping something else under that name changes nothing.
#[derive(Debug)]
pub(in crate::file_access) struct Temp {
    pub(super) file: File,
}

impl Dir {
    /// A new, empty file in this folder for a replacement, under the first name from `names` that nothing holds yet.
    ///
    /// Its security comes from the folder, not from the file it replaces: like any new file it gets the ACEs the
    /// folder passes on, and the rename keeps them. A replaced file that had an explicit ACL of its own (narrower than
    /// its folder's) loses it, and its attributes (read-only, hidden) are not carried over either.
    pub(super) fn temp(
        &self,
        _replacing: &str,
        names: &mut dyn Iterator<Item = Result<String, Failure>>,
    ) -> Result<Temp, Failure> {
        first_free(names, |name| self.file(name, How::New)).map(|(file, _)| Temp { file })
    }
}

/// Puts a finished replacement in place: the temporary file is renamed, by its handle, to `name` in the folder it is
/// in (a rename to a bare name never leaves the file's folder, so no path is read again).
pub(super) fn commit(
    _dir: &Dir,
    temp: &mut Temp,
    name: &str,
    _names: &mut dyn Iterator<Item = Result<String, Failure>>,
) -> Result<(), Failure> {
    let file = &temp.file;
    let name: Vec<u16> = name.encode_utf16().collect();
    let name_bytes = name.len() * size_of::<u16>();
    let size = size_of::<FILE_RENAME_INFO>() + name_bytes;
    let too_long = |_| Failure::Os(io::ErrorKind::InvalidFilename.into());
    let length = u32::try_from(name_bytes).map_err(too_long)?;
    let size_u32 = u32::try_from(size).map_err(too_long)?;
    // Zeroed and 8-byte aligned (the struct holds a handle), with room for the name after the header.
    let mut buffer = vec![0_u64; size.div_ceil(size_of::<u64>())];
    let info = buffer.as_mut_ptr().cast::<FILE_RENAME_INFO>();
    // SAFETY: `info` points at a zeroed buffer at least `size` bytes long and aligned for `FILE_RENAME_INFO`; the
    // name is copied into the trailing `FileName` array, which the buffer was sized to hold.
    let ok = unsafe {
        (*info).Anonymous.ReplaceIfExists = true;
        (*info).RootDirectory = ptr::null_mut();
        (*info).FileNameLength = length;
        ptr::copy_nonoverlapping(name.as_ptr(), ptr::addr_of_mut!((*info).FileName).cast::<u16>(), name.len());
        SetFileInformationByHandle(file.as_raw_handle(), FileRenameInfo, info.cast::<c_void>(), size_u32)
    };
    if ok == 0 { Err(Failure::Os(io::Error::last_os_error())) } else { Ok(()) }
}

/// Throws an unfinished replacement away: deleted by its handle when it is closed.
pub(super) fn discard(_dir: &Dir, temp: &Temp) {
    delete_by_handle(temp.file.as_raw_handle());
}

/// Flags for one step of a walk: a folder (which needs backup semantics to be opened at all), and the link itself
/// rather than what it points at.
const FOLDER_LINK: FILE_FLAGS_AND_ATTRIBUTES = FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT;
/// Flags for the file at the end: the link itself, if it is one.
const FILE_LINK: FILE_FLAGS_AND_ATTRIBUTES = FILE_FLAG_OPEN_REPARSE_POINT;

/// A handle, and whether `OPEN_ALWAYS` found something already there.
struct Opened {
    handle: OwnedHandle,
    existed: bool,
}

/// `CreateFileW` on a `\\?\` path, sharing like std does.
fn open(
    path: &str,
    access: u32,
    disposition: FILE_CREATION_DISPOSITION,
    flags: FILE_FLAGS_AND_ATTRIBUTES,
) -> Result<Opened, Failure> {
    let wide = wide(path);
    let share = FILE_SHARE_READ | FILE_SHARE_WRITE | FILE_SHARE_DELETE;
    // SAFETY: `wide` is NUL-terminated and outlives the call; no security attributes or template handle.
    let raw = unsafe { CreateFileW(wide.as_ptr(), access, share, ptr::null(), disposition, flags, ptr::null_mut()) };
    if raw == INVALID_HANDLE_VALUE || raw.is_null() {
        return Err(os_failure(io::Error::last_os_error()));
    }
    // SAFETY: read right after the call that set it, before anything else can change it.
    let existed = unsafe { GetLastError() } == ERROR_ALREADY_EXISTS;
    // SAFETY: `raw` is a valid handle that nothing else owns.
    Ok(Opened { handle: unsafe { OwnedHandle::from_raw_handle(raw) }, existed })
}

/// Makes the folder at `path`; `false` if one was already there.
fn make_folder(path: &str) -> Result<bool, Failure> {
    let wide = wide(path);
    // SAFETY: `wide` is NUL-terminated and outlives the call; default security.
    if unsafe { CreateDirectoryW(wide.as_ptr(), ptr::null()) } != 0 {
        return Ok(true);
    }
    let error = io::Error::last_os_error();
    if error.kind() == io::ErrorKind::AlreadyExists { Ok(false) } else { Err(os_failure(error)) }
}

/// Refuses a link, and a file where a folder should be.
fn check_folder(handle: &OwnedHandle) -> Result<(), Failure> {
    let tag = attribute_tag(handle)?;
    let link = tag.FileAttributes & FILE_ATTRIBUTE_REPARSE_POINT != 0 && tag.ReparseTag & NAME_SURROGATE != 0;
    let folder = tag.FileAttributes & FILE_ATTRIBUTE_DIRECTORY != 0;
    if link || !folder { Err(Failure::Refused) } else { Ok(()) }
}

fn attribute_tag(handle: &OwnedHandle) -> Result<FILE_ATTRIBUTE_TAG_INFO, Failure> {
    let mut info = FILE_ATTRIBUTE_TAG_INFO { FileAttributes: 0, ReparseTag: 0 };
    query(handle, FileAttributeTagInfo, (&raw mut info).cast(), size_of::<FILE_ATTRIBUTE_TAG_INFO>())?;
    Ok(info)
}

fn standard_info(handle: &OwnedHandle) -> Result<FILE_STANDARD_INFO, Failure> {
    let mut info = FILE_STANDARD_INFO {
        AllocationSize: 0,
        EndOfFile: 0,
        NumberOfLinks: 0,
        DeletePending: false,
        Directory: false,
    };
    query(handle, FileStandardInfo, (&raw mut info).cast(), size_of::<FILE_STANDARD_INFO>())?;
    Ok(info)
}

fn query(handle: &OwnedHandle, class: i32, out: *mut c_void, size: usize) -> Result<(), Failure> {
    let size = u32::try_from(size).map_err(|_| Failure::Refused)?;
    // SAFETY: `out` points at a struct of `size` bytes of the type `class` fills.
    if unsafe { GetFileInformationByHandleEx(handle.as_raw_handle(), class, out, size) } == 0 {
        return Err(Failure::Os(io::Error::last_os_error()));
    }
    Ok(())
}

/// Where the handle really is: `\\?\C:\...` or `\\?\UNC\server\share\...`, links and short names resolved.
fn final_path(handle: &OwnedHandle) -> Result<String, Failure> {
    let mut buffer = vec![0_u16; 512];
    loop {
        let capacity = u32::try_from(buffer.len()).map_err(|_| Failure::Refused)?;
        // SAFETY: `buffer` has `capacity` elements; the call writes at most that many.
        let written = unsafe {
            GetFinalPathNameByHandleW(
                handle.as_raw_handle(),
                buffer.as_mut_ptr(),
                capacity,
                FILE_NAME_NORMALIZED | VOLUME_NAME_DOS,
            )
        };
        let written = usize::try_from(written).map_err(|_| Failure::Refused)?;
        if written == 0 {
            return Err(Failure::Os(io::Error::last_os_error()));
        }
        if written < buffer.len() {
            return String::from_utf16(&buffer[..written]).map_err(|_| Failure::Refused);
        }
        // Too small: `written` is the size needed, terminating NUL included.
        buffer.resize(written + 1, 0);
    }
}

fn delete_on_close(handle: &OwnedHandle) {
    delete_by_handle(handle.as_raw_handle());
}

/// Marks the open file or folder for deletion when its last handle closes. Best effort: it only cleans up after a
/// refusal or an abandoned replacement.
fn delete_by_handle(raw: *mut c_void) {
    let info = FILE_DISPOSITION_INFO { DeleteFile: true };
    let Ok(size) = u32::try_from(size_of::<FILE_DISPOSITION_INFO>()) else { return };
    // SAFETY: `info` is a valid `FILE_DISPOSITION_INFO` for the duration of the call.
    let _ = unsafe { SetFileInformationByHandle(raw, FileDispositionInfo, (&raw const info).cast(), size) };
}

fn os_failure(error: io::Error) -> Failure {
    match error.kind() {
        io::ErrorKind::NotFound => Failure::NotFound,
        io::ErrorKind::AlreadyExists => Failure::Exists,
        _ => Failure::Os(error),
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}
