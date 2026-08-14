use std::path::{Component, Path};

use crate::error::AppError;

#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::ffi::{CString, OsStr};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::fs::File;
#[cfg(target_os = "macos")]
use std::mem::MaybeUninit;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::ffi::OsStrExt;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::os::unix::io::{AsRawFd, FromRawFd};

pub fn rename_without_overwrite(root: &Path, source: &Path, target: &Path) -> Result<(), AppError> {
    if source == target {
        return Ok(());
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let root_directory = open_directory(root)?;
        let (source_parent, source_name) = relative_parent(&root_directory, root, source)?;
        let (target_parent, target_name) = relative_parent(&root_directory, root, target)?;

        #[cfg(target_os = "macos")]
        if same_path_entry(&source_parent, source_name, &target_parent, target_name)? {
            return Ok(());
        }

        let source_name = c_string(source_name)?;
        let target_name = c_string(target_name)?;
        #[cfg(target_os = "linux")]
        let status = unsafe {
            libc::renameat2(
                source_parent.as_raw_fd(),
                source_name.as_ptr(),
                target_parent.as_raw_fd(),
                target_name.as_ptr(),
                libc::RENAME_NOREPLACE,
            )
        };
        #[cfg(target_os = "macos")]
        let status = unsafe {
            libc::renameatx_np(
                source_parent.as_raw_fd(),
                source_name.as_ptr(),
                target_parent.as_raw_fd(),
                target_name.as_ptr(),
                libc::RENAME_EXCL,
            )
        };
        if status == 0 {
            return Ok(());
        }
        Err(std::io::Error::last_os_error().into())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (root, source, target);
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
