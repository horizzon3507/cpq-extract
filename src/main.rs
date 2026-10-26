use std::path::PathBuf;

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

fn main() {
    let args = Args::parse();
    let password = match args.password_file {
        Some(path) => match std::fs::read_to_string(path) {
            Ok(password) => Some(password.trim_end_matches(['\r', '\n']).to_owned()),
            Err(error) => {
                eprintln!("Error reading password file: {error}");
                std::process::exit(1);
            }
        },
        None => None,
    };

    match cpq_extract::extract(&args.archive, &args.output, password.as_deref()) {
        Ok(summary) => println!(
            "Extracted {} items from {} tabs to {}",
            summary.item_count,
            summary.tab_count,
            summary.output.display()
        ),
        Err(error) => {
            eprintln!("Error: {error}");
            std::process::exit(1);
        }
    }
}
