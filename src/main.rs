use clap::{Parser, Subcommand};

use task_googlecloud::{AppError, Cloud, Command as TaskCommand, InterruptFlag, StorageApi, run};

#[derive(Debug, Parser)]
#[command(name = "task-googlecloud")]
#[command(about = "Manage Google Cloud Storage object names and uploads")]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    Normalize { project: String, bucket: String },
    Upload { project: String },
}

fn main() {
    let cli = Cli::parse();
    let result = (|| -> Result<(), AppError> {
        let interrupt = InterruptFlag::install()?;
        let cloud = Cloud::new();
        let storage = StorageApi::new(cloud.clone());
        let command = match cli.command {
            Command::Normalize { project, bucket } => TaskCommand::Normalize { project, bucket },
            Command::Upload { project } => TaskCommand::Upload { project },
        };
        run(command, cloud, storage, interrupt)
    })();

    if let Err(error) = result {
        eprintln!("{error}");
        std::process::exit(1);
    }
}
