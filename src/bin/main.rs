use std::{
    io::ErrorKind,
    path::{Path, PathBuf},
    time::{SystemTime, UNIX_EPOCH},
};

use clap::Parser;

use color_eyre::eyre::{ContextCompat, WrapErr};
use gcgen::Input;

mod cli {
    use std::path::PathBuf;

    use clap::Parser;

    #[derive(Parser, Debug)]
    #[command(author, version, about, long_about = None)]
    pub struct Cli {
        /// Path to the input file
        #[arg(value_name = "FILE")]
        pub input: Option<PathBuf>,

        /// Directory to write the config files into [default: home directory]
        #[arg(long, short, value_name = "DIR")]
        pub output_dir: Option<PathBuf>,

        /// Print json schema for the input file and exit
        #[arg(long)]
        pub schema: bool,
    }
}

fn main() -> color_eyre::Result<()> {
    color_eyre::install()?;

    let cli = cli::Cli::parse();
    if cli.schema {
        let schema = schemars::schema_for!(Input);
        println!("{}", serde_json::to_string_pretty(&schema)?);
        return Ok(());
    }
    let input_path = cli.input.as_ref().context("Input file path is required")?;
    let input = Input::from_file_path(input_path)?;
    let output_dir = match cli.output_dir {
        Some(dir) => dir,
        None => std::env::home_dir().context("Could not determine the home directory")?,
    };
    // One timestamp per run, so all backups from the same run share a suffix.
    let timestamp = SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs();
    for file in input.render()? {
        let path = output_dir.join(&file.file_name);
        let contents = file.config.to_string();
        if backup(&path, &contents, timestamp)? {
            std::fs::write(&path, contents)
                .wrap_err_with(|| format!("Failed to write {}", path.display()))?;
            eprintln!("Wrote {}", path.display());
        } else {
            eprintln!("Unchanged {}", path.display());
        }
    }
    Ok(())
}

/// Copies an existing `path` to `<path>.bak-<timestamp>`.
/// Returns whether `path` needs writing, i.e. is missing or differs from `contents`.
fn backup(path: &Path, contents: &str, timestamp: u64) -> color_eyre::Result<bool> {
    let existing = match std::fs::read(path) {
        Ok(existing) => existing,
        Err(err) if err.kind() == ErrorKind::NotFound => return Ok(true),
        Err(err) => return Err(err).wrap_err_with(|| format!("Failed to read {}", path.display())),
    };
    if existing == contents.as_bytes() {
        return Ok(false);
    }

    let mut backup_path = path.as_os_str().to_owned();
    backup_path.push(format!(".bak-{timestamp}"));
    let backup_path = PathBuf::from(backup_path);
    std::fs::write(&backup_path, existing)
        .wrap_err_with(|| format!("Failed to write backup {}", backup_path.display()))?;
    eprintln!("Backed up {} to {}", path.display(), backup_path.display());
    Ok(true)
}
