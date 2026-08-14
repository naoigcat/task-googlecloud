mod atomic_rename;
mod cloud;
mod error;
mod local;
mod normalization_plan;
mod normalize;
mod object_move;
mod storage;
mod transaction;
mod upload;
mod upload_source;

pub use cloud::{Cloud, shell_quote};
pub use error::AppError;
pub use local::{apply_normalization, path_string, rollback_normalization};
pub use normalization_plan::{Entry, build as build_normalization_plan, normalized};
pub use normalize::process_moves;
pub use storage::StorageApi;
pub use storage::{ObjectPath, StorageClient};
pub use transaction::RemoteChange;
pub use upload::{rollback_remote, upload_files_by_directory};

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use signal_hook::consts::{SIGINT, SIGTERM};

pub struct InterruptFlag {
    interrupted: Arc<AtomicBool>,
}

impl InterruptFlag {
    pub fn install() -> Result<Self, AppError> {
        let interrupted = Arc::new(AtomicBool::new(false));
        signal_hook::flag::register(SIGINT, Arc::clone(&interrupted))?;
        signal_hook::flag::register(SIGTERM, Arc::clone(&interrupted))?;
        Ok(Self { interrupted })
    }

    #[doc(hidden)]
    pub fn from_atomic(interrupted: Arc<AtomicBool>) -> Self {
        Self { interrupted }
    }

    pub(crate) fn check(&self) -> Result<(), AppError> {
        if self.interrupted.load(Ordering::Relaxed) {
            return Err(AppError::Interrupted);
        }
        Ok(())
    }
}

#[derive(Debug, Clone)]
pub enum Command {
    Normalize { project: String, bucket: String },
    Upload { project: String },
}

pub fn run(
    command: Command,
    cloud: Cloud,
    storage: StorageApi,
    interrupt: InterruptFlag,
) -> Result<(), AppError> {
    match command {
        Command::Normalize { project, bucket } => {
            normalize::run(&cloud, &storage, &interrupt, &project, &bucket)
        }
        Command::Upload { project } => upload::run(&cloud, &storage, &interrupt, &project),
    }
}
