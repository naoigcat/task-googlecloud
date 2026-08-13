use std::ffi::CString;
use std::path::Path;

use crate::error::AppError;

#[cfg(unix)]
use std::os::unix::ffi::OsStrExt;

pub fn rename_without_overwrite(source: &Path, target: &Path) -> Result<(), AppError> {
    if source == target {
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
