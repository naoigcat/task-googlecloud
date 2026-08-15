use std::path::{Component, Path};
use std::{fs::File, io};

use crate::error::AppError;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::{CString, OsStr};
#[cfg(target_os = "macos")]
use std::mem::MaybeUninit;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStrExt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::fs::MetadataExt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::io::{AsRawFd, FromRawFd};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::sync::Arc;

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Debug)]
/// Identity of a directory captured while its descriptor is kept open.
pub struct DirectoryIdentity {
    device: u64,
    inode: u64,
    // Keeping the descriptor open prevents Linux from recycling this inode
    // while a planned operation still relies on the captured identity.
    _descriptor: Arc<File>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl PartialEq for DirectoryIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Eq for DirectoryIdentity {}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[derive(Clone, Debug)]
/// Identity of a file captured while its descriptor is kept open.
pub struct FileIdentity {
    device: u64,
    inode: u64,
    // Keeping the descriptor open prevents Linux from recycling this inode
    // while a planned operation still relies on the captured identity.
    _descriptor: Arc<File>,
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl PartialEq for FileIdentity {
    fn eq(&self, other: &Self) -> bool {
        self.device == other.device && self.inode == other.inode
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
impl Eq for FileIdentity {}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DirectoryIdentity;

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FileIdentity;

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn directory_identity_from_path(
    path: &std::path::Path,
) -> io::Result<DirectoryIdentity> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    directory_identity(&file)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn file_identity_from_path(path: &std::path::Path) -> io::Result<FileIdentity> {
    let file = std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_CLOEXEC | libc::O_NOFOLLOW)
        .open(path)?;
    if !file.metadata()?.file_type().is_file() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "identity source is not a regular file",
        ));
    }
    file_identity(&file)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn directory_identity_from_path(
    path: &std::path::Path,
) -> io::Result<DirectoryIdentity> {
    let file = File::open(path)?;
    directory_identity(&file)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn file_identity_from_path(path: &std::path::Path) -> io::Result<FileIdentity> {
    let file = File::open(path)?;
    file_identity(&file)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn directory_identity(file: &File) -> io::Result<DirectoryIdentity> {
    let metadata = file.metadata()?;
    Ok(DirectoryIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        _descriptor: Arc::new(file.try_clone()?),
    })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(crate) fn file_identity(file: &File) -> io::Result<FileIdentity> {
    let metadata = file.metadata()?;
    Ok(FileIdentity {
        device: metadata.dev(),
        inode: metadata.ino(),
        _descriptor: Arc::new(file.try_clone()?),
    })
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
pub(crate) fn identity_descriptor_is_unlinked(identity: &FileIdentity) -> io::Result<bool> {
    Ok(identity._descriptor.metadata()?.nlink() == 0)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn file_identity(_file: &File) -> io::Result<FileIdentity> {
    Ok(FileIdentity)
}

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
pub(crate) fn directory_identity(_file: &File) -> io::Result<DirectoryIdentity> {
    Ok(DirectoryIdentity)
}

/// Atomically renames a path without replacing an existing target.
///
/// Linux and macOS provide the required no-replace primitive; other platforms
/// return [`AppError::UnsupportedPlatform`].
pub fn rename_without_overwrite(root: &Path, source: &Path, target: &Path) -> Result<(), AppError> {
    rename_without_overwrite_with_identity(root, None, None, None, None, source, target)
}

pub(crate) fn rename_without_overwrite_with_identity(
    root: &Path,
    expected_root: Option<DirectoryIdentity>,
    expected_source: Option<FileIdentity>,
    expected_source_parent: Option<DirectoryIdentity>,
    expected_target_parent: Option<DirectoryIdentity>,
    source: &Path,
    target: &Path,
) -> Result<(), AppError> {
    if source == target {
        return Ok(());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        // Resolve both parents from an open root descriptor so a concurrent
        // replacement of the root or an intermediate directory is detected.
        let root_directory = open_directory(root)?;
        if let Some(expected_root) = expected_root.as_ref()
            && !directory_identity(&root_directory)?.eq(expected_root)
        {
            return Err(AppError::Message(format!(
                "Input root was replaced before renaming: {root:?}"
            )));
        }
        let (source_parent, source_name) = relative_parent(&root_directory, root, source)?;
        let (target_parent, target_name_os) = relative_parent(&root_directory, root, target)?;
        if let Some(expected_source_parent) = expected_source_parent.as_ref()
            && !directory_identity(&source_parent)?.eq(expected_source_parent)
        {
            return Err(AppError::Message(format!(
                "Input source directory was replaced before renaming: {source:?}"
            )));
        }
        if let Some(expected_target_parent) = expected_target_parent.as_ref()
            && !directory_identity(&target_parent)?.eq(expected_target_parent)
        {
            return Err(AppError::Message(format!(
                "Input target directory was replaced before renaming: {target:?}"
            )));
        }
        if let Some(expected_source) = expected_source.as_ref()
            && !file_identity_at(&source_parent, source_name)?.eq(expected_source)
        {
            return Err(AppError::Message(format!(
                "Input file was replaced before renaming: {source:?}"
            )));
        }

        #[cfg(target_os = "macos")]
        if same_path_entry(&source_parent, source_name, &target_parent, target_name_os)? {
            return Ok(());
        }

        let source_name = c_string(source_name)?;
        let target_name = c_string(target_name_os)?;
        rename_noreplace(&source_parent, &source_name, &target_parent, &target_name)?;
        if let Some(expected_source) = expected_source.as_ref() {
            let actual = file_identity_at(&target_parent, target_name_os).map_err(|error| {
                AppError::rollback(
                    AppError::Message(format!(
                        "Input file was replaced during renaming: {source:?}"
                    )),
                    vec![error],
                )
            })?;
            if !actual.eq(expected_source) {
                return Err(AppError::Recovery {
                    paths: format!("{:?} and {:?}", source, target),
                    operation: "normalize upload source".to_string(),
                    details: format!(
                        "Input file was replaced during renaming: {source:?}; manual recovery is required"
                    ),
                });
            }
        }
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (
            root,
            expected_root,
            expected_source,
            expected_source_parent,
            expected_target_parent,
            source,
            target,
        );
        Err(AppError::UnsupportedPlatform(
            std::env::consts::OS.to_string(),
        ))
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn relative_parent<'a>(
    root_directory: &File,
    root: &Path,
    path: &'a Path,
) -> Result<(File, &'a OsStr), AppError> {
    let relative = path
        .strip_prefix(root)
        .map_err(|_| AppError::Message(format!("Path is outside {root:?}: {path:?}")))?;
    let mut parents = Vec::new();
    let mut name = None;
    for component in relative.components() {
        match component {
            Component::CurDir => {}
            Component::Normal(component) => {
                if let Some(previous) = name.replace(component) {
                    parents.push(previous);
                }
            }
            Component::RootDir | Component::ParentDir | Component::Prefix(_) => {
                return Err(AppError::Message(format!(
                    "Path contains an unsupported component: {path:?}"
                )));
            }
        }
    }
    let name = name.ok_or_else(|| AppError::Message(format!("Path has no file name: {path:?}")))?;
    let mut directory = root_directory.try_clone()?;
    for component in parents {
        directory = open_directory_at(&directory, component)?;
    }
    Ok((directory, name))
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_directory(path: &Path) -> Result<File, AppError> {
    let path = c_string(path.as_os_str())?;
    open_directory_at_fd(libc::AT_FDCWD, &path)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_directory_at(directory: &File, name: &OsStr) -> Result<File, AppError> {
    let name = c_string(name)?;
    open_directory_at_fd(directory.as_raw_fd(), &name)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn open_directory_at_fd(directory: i32, name: &CString) -> Result<File, AppError> {
    let fd = unsafe {
        libc::openat(
            directory,
            name.as_ptr(),
            libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn file_identity_at(directory: &File, name: &OsStr) -> Result<FileIdentity, AppError> {
    let name = c_string(name)?;
    let fd = unsafe {
        libc::openat(
            directory.as_raw_fd(),
            name.as_ptr(),
            libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW,
        )
    };
    if fd < 0 {
        return Err(std::io::Error::last_os_error().into());
    }
    let file = unsafe { File::from_raw_fd(fd) };
    Ok(file_identity(&file)?)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn rename_noreplace(
    source_directory: &File,
    source_name: &CString,
    target_directory: &File,
    target_name: &CString,
) -> io::Result<()> {
    // Use the platform's atomic exclusive rename rather than a check-then-rename
    // sequence, which would allow another process to create the target in between.
    #[cfg(target_os = "linux")]
    let status = unsafe {
        libc::renameat2(
            source_directory.as_raw_fd(),
            source_name.as_ptr(),
            target_directory.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_NOREPLACE,
        )
    };
    #[cfg(target_os = "macos")]
    let status = unsafe {
        libc::renameatx_np(
            source_directory.as_raw_fd(),
            source_name.as_ptr(),
            target_directory.as_raw_fd(),
            target_name.as_ptr(),
            libc::RENAME_EXCL,
        )
    };
    if status == 0 {
        Ok(())
    } else {
        Err(std::io::Error::last_os_error())
    }
}

#[cfg(target_os = "macos")]
fn same_path_entry(
    source_directory: &File,
    source_name: &OsStr,
    target_directory: &File,
    target_name: &OsStr,
) -> Result<bool, AppError> {
    let source_metadata = entry_metadata(source_directory, source_name)?;
    let target_metadata = entry_metadata(target_directory, target_name)?;
    Ok(match (source_metadata, target_metadata) {
        (Some(source), Some(target)) => {
            source.st_dev == target.st_dev && source.st_ino == target.st_ino
        }
        _ => false,
    })
}

#[cfg(target_os = "macos")]
fn entry_metadata(directory: &File, name: &OsStr) -> Result<Option<libc::stat>, AppError> {
    let name = c_string(name)?;
    let mut metadata = MaybeUninit::<libc::stat>::uninit();
    let status = unsafe {
        libc::fstatat(
            directory.as_raw_fd(),
            name.as_ptr(),
            metadata.as_mut_ptr(),
            libc::AT_SYMLINK_NOFOLLOW,
        )
    };
    if status == 0 {
        return Ok(Some(unsafe { metadata.assume_init() }));
    }
    let error = std::io::Error::last_os_error();
    if error.kind() == std::io::ErrorKind::NotFound {
        return Ok(None);
    }
    Err(error.into())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn c_string(value: &OsStr) -> Result<CString, AppError> {
    CString::new(value.as_bytes())
        .map_err(|_| AppError::Message(format!("Path contains NUL: {value:?}")))
}
