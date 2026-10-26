use std::fs;
use std::path::{Path, PathBuf};
use std::process::ExitCode;

use clap::Parser;

#[derive(Debug, Parser)]
#[command(
    name = "cpq-extract",
    version,
    about = "Extract a CopyQ .cpq backup into ordered files by tab and item"
)]
struct Args {
    /// CopyQ backup file
    archive: PathBuf,
    /// New output directory
    output: PathBuf,
    /// Read the backup password from a file
    #[arg(long, value_name = "FILE")]
    password_file: Option<PathBuf>,
}

fn main() -> ExitCode {
    let args = Args::parse();
    let password = match read_password(args.password_file.as_deref()) {
        Ok(password) => password,
        Err(error) => {
            eprintln!("Error reading password file: {error}");
            return ExitCode::FAILURE;
        }
    };

    match cpq_extract::extract(&args.archive, &args.output, password.as_deref()) {
        Ok(summary) => {
            println!(
                "Extracted {} items from {} tabs to {}",
                summary.item_count,
                summary.tab_count,
                summary.output.display()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("Error: {error}");
            ExitCode::FAILURE
        }
    }
}

fn read_password(path: Option<&Path>) -> Result<Option<String>, std::io::Error> {
    let Some(path) = path else {
        return Ok(None);
    };

    let password = fs::read_to_string(path)?;
    Ok(Some(password.trim_end_matches(['\r', '\n']).to_owned()))
}
