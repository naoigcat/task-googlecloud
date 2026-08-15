#[cfg(unix)]
use std::ffi::CString;
use std::fs;
use std::fs::File;
#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;
#[cfg(unix)]
use std::os::unix::fs::OpenOptionsExt;
#[cfg(unix)]
use std::os::unix::io::{AsRawFd, FromRawFd, RawFd};
#[cfg(unix)]
use std::path::Component;
use std::path::Path;

use crate::atomic_rename::{
    DirectoryIdentity, FileIdentity, directory_identity, directory_identity_from_metadata,
    file_identity,
};
use crate::error::AppError;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct UploadSourceIdentity {
    pub(crate) file: FileIdentity,
    pub(crate) directory: DirectoryIdentity,
}

#[cfg(unix)]
pub(crate) fn open(
    root: Option<&Path>,
    source: &Path,
    expected_root: Option<DirectoryIdentity>,
    expected_source: Option<UploadSourceIdentity>,
) -> Result<File, AppError> {
    ensure_upload_root_identity_supported(expected_root, expected_source)?;
    if let Some(root) = root {
        let relative = source
            .strip_prefix(root)
            .map_err(|_| rejected_source(format!("outside {root:?}: {source:?}")))?;
        return open_relative_without_following_links(
            root,
            relative,
            expected_root,
            expected_source,
        );
    }
    open_without_upload_root(source, expected_source)
}

#[cfg(not(unix))]
pub(crate) fn open(
    _root: Option<&Path>,
    source: &Path,
    expected_root: Option<DirectoryIdentity>,
    expected_source: Option<UploadSourceIdentity>,
) -> Result<File, AppError> {
    ensure_upload_root_identity_supported(expected_root, expected_source)?;
    let metadata = fs::symlink_metadata(source).map_err(AppError::UploadSource)?;
    if !metadata.file_type().is_file() {
        return Err(rejected_source(format!("not a regular file: {source:?}")));
    }
    let file = File::open(source).map_err(AppError::UploadSource)?;
    if let Some(expected_source) = expected_source
        && file_identity(&file).map_err(AppError::UploadSource)? != expected_source.file
    {
        return Err(rejected_source(format!(
            "upload source was replaced before opening: {source:?}"
        )));
    }
    Ok(file)
}

fn ensure_upload_root_identity_supported(
    expected_root: Option<DirectoryIdentity>,
    expected_source: Option<UploadSourceIdentity>,
) -> Result<(), AppError> {
    if expected_root.is_some() || expected_source.is_some() {
        #[cfg(not(any(target_os = "linux", target_os = "macos")))]
        return Err(AppError::UnsupportedPlatform(
            std::env::consts::OS.to_string(),
        ));
    }
    Ok(())
}

#[cfg(unix)]
fn open_relative_without_following_links(
    root: &Path,
    source: &Path,
    expected_root: Option<DirectoryIdentity>,
    expected_source: Option<UploadSourceIdentity>,
) -> Result<File, AppError> {
    let directory = open_directory(root)?;
    if let Some(expected_root) = expected_root {
        let actual_root = directory_identity(&directory).map_err(AppError::UploadSource)?;
        if actual_root != expected_root {
            return Err(rejected_source(format!(
                "upload root was replaced before opening: {root:?}"
            )));
        }
    }
    open_path_components(directory, source, expected_source)
}

/// Every local rejection is an UploadSource failure, so callers can tell it
/// apart from a failure that may already have reached Cloud Storage.
fn rejected_source(message: String) -> AppError {
    AppError::UploadSource(std::io::Error::new(
        std::io::ErrorKind::InvalidInput,
        message,
    ))
}

#[cfg(unix)]
fn open_without_upload_root(
    source: &Path,
    expected_source: Option<UploadSourceIdentity>,
) -> Result<File, AppError> {
    let metadata = fs::symlink_metadata(source).map_err(AppError::UploadSource)?;
    if !metadata.file_type().is_file() {
        return Err(rejected_source(format!("not a regular file: {source:?}")));
    }
    let file = fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NOFOLLOW)
        .open(source)
        .map_err(AppError::UploadSource)?;
    if let Some(expected_source) = expected_source {
        let parent = source
            .parent()
            .ok_or_else(|| rejected_source(format!("source has no parent: {source:?}")))?;
        let actual_directory = directory_identity_from_metadata(
            &fs::symlink_metadata(parent).map_err(AppError::UploadSource)?,
        );
        if actual_directory != expected_source.directory
            || file_identity(&file).map_err(AppError::UploadSource)? != expected_source.file
        {
            return Err(rejected_source(format!(
                "upload source was replaced before opening: {source:?}"
            )));
        }
    }
    Ok(file)
}

#[cfg(unix)]
fn open_path_components(
    mut directory: File,
    source: &Path,
    expected_source: Option<UploadSourceIdentity>,
) -> Result<File, AppError> {
    let mut components = source.components().peekable();

    while let Some(component) = components.next() {
        match component {
            Component::CurDir => {}
            Component::RootDir => {
                return Err(rejected_source(format!("absolute path: {source:?}")));
            }
            Component::ParentDir => {
                return Err(rejected_source(format!(
                    "contains a parent path component: {source:?}"
                )));
            }
            Component::Normal(name) if components.peek().is_some() => {
                let fd = open_at(directory.as_raw_fd(), name, directory_flags())?;
                directory = unsafe { File::from_raw_fd(fd) };
            }
            Component::Normal(name) => {
                if let Some(expected_source) = expected_source
                    && directory_identity(&directory).map_err(AppError::UploadSource)?
                        != expected_source.directory
                {
                    return Err(rejected_source(format!(
                        "upload source directory was replaced before opening: {source:?}"
                    )));
                }
                let fd = open_at(directory.as_raw_fd(), name, file_flags())?;
                let file = unsafe { File::from_raw_fd(fd) };
                if !file
                    .metadata()
                    .map_err(AppError::UploadSource)?
                    .file_type()
                    .is_file()
                {
                    return Err(rejected_source(format!("not a regular file: {source:?}")));
                }
                if let Some(expected_source) = expected_source
                    && file_identity(&file).map_err(AppError::UploadSource)? != expected_source.file
                {
                    return Err(rejected_source(format!(
                        "upload source was replaced before opening: {source:?}"
                    )));
                }
                return Ok(file);
            }
            _ => {
                return Err(rejected_source(format!("not a regular file: {source:?}")));
            }
        }
    }

    Err(rejected_source(format!("not a regular file: {source:?}")))
}

#[cfg(unix)]
fn open_directory(path: &Path) -> Result<File, AppError> {
    let fd = open_at(libc::AT_FDCWD, path.as_os_str(), directory_flags())?;
    Ok(unsafe { File::from_raw_fd(fd) })
}

#[cfg(unix)]
fn open_at(directory: RawFd, name: &std::ffi::OsStr, flags: i32) -> Result<RawFd, AppError> {
    let name = CString::new(name.as_bytes())
        .map_err(|_| rejected_source(format!("path contains NUL: {name:?}")))?;
    let fd = unsafe { libc::openat(directory, name.as_ptr(), flags) };
    if fd < 0 {
        return Err(AppError::UploadSource(std::io::Error::last_os_error()));
    }
    Ok(fd)
}

#[cfg(unix)]
fn directory_flags() -> i32 {
    libc::O_RDONLY | libc::O_DIRECTORY | libc::O_CLOEXEC | libc::O_NOFOLLOW
}

#[cfg(unix)]
fn file_flags() -> i32 {
    libc::O_RDONLY | libc::O_CLOEXEC | libc::O_NOFOLLOW
}
