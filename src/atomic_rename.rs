use std::ffi::CString;
use std::path::Path;

use crate::error::AppError;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

#[cfg(target_os = "macos")]
use std::os::unix::fs::MetadataExt;
#[cfg(target_os = "macos")]
use unicode_normalization::UnicodeNormalization;

pub(crate) fn path_entry_exists(path: &Path) -> bool {
    std::fs::symlink_metadata(path).is_ok()
}

#[cfg(target_os = "macos")]
pub(crate) fn same_path_entry(source: &Path, target: &Path) -> Result<bool, AppError> {
    if source == target {
        return Ok(true);
    }

    let Some(source_name) = source.file_name().and_then(|name| name.to_str()) else {
        return Ok(false);
    };
    let Some(target_name) = target.file_name().and_then(|name| name.to_str()) else {
        return Ok(false);
    };
    let source_parent = source
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let target_parent = target
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if std::fs::canonicalize(source_parent)? != std::fs::canonicalize(target_parent)?
        || !source_name.nfc().eq(target_name.nfc())
    {
        return Ok(false);
    }

    let source_metadata = std::fs::symlink_metadata(source)?;
    let target_metadata = std::fs::symlink_metadata(target)?;
    Ok(source_metadata.dev() == target_metadata.dev()
        && source_metadata.ino() == target_metadata.ino())
}

#[cfg(not(target_os = "macos"))]
pub(crate) fn same_path_entry(_source: &Path, _target: &Path) -> Result<bool, AppError> {
    Ok(false)
}

pub fn rename_without_overwrite(source: &Path, target: &Path) -> Result<(), AppError> {
    if source == target
        || (path_entry_exists(source)
            && path_entry_exists(target)
            && same_path_entry(source, target)?)
    {
        return Ok(());
    }

    #[cfg(target_os = "linux")]
    {
        let source = CString::new(source.as_os_str().as_bytes())
            .map_err(|_| AppError::Message(format!("Path contains NUL: {source:?}")))?;
        let target = CString::new(target.as_os_str().as_bytes())
            .map_err(|_| AppError::Message(format!("Path contains NUL: {target:?}")))?;
        let status = unsafe {
            libc::renameat2(
                libc::AT_FDCWD,
                source.as_ptr(),
                libc::AT_FDCWD,
                target.as_ptr(),
                1,
            )
        };
        if status == 0 {
            return Ok(());
        }
        Err(std::io::Error::last_os_error().into())
    }

    #[cfg(target_os = "macos")]
    {
        let source = CString::new(source.as_os_str().as_bytes())
            .map_err(|_| AppError::Message(format!("Path contains NUL: {source:?}")))?;
        let target = CString::new(target.as_os_str().as_bytes())
            .map_err(|_| AppError::Message(format!("Path contains NUL: {target:?}")))?;
        let status = unsafe { libc::renamex_np(source.as_ptr(), target.as_ptr(), 0x0000_0004) };
        if status == 0 {
            return Ok(());
        }
        Err(std::io::Error::last_os_error().into())
    }

    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(AppError::UnsupportedPlatform(
            std::env::consts::OS.to_string(),
        ))
    }
}
