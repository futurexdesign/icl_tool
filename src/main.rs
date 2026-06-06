mod codec;
mod input;

use clap::{Parser, Subcommand};
use codec::IclCodec;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Parser, Debug)]
#[command(name = "icl_tool")]
#[command(about = "DWIN ICL encoder/decoder")]
struct Cli {
    #[command(subcommand)]
    command: Commands,
}

#[derive(Subcommand, Debug)]
enum Commands {
    EncodeJpegs {
        out_icl: PathBuf,
        inputs: Vec<String>,
        #[arg(long)]
        vendor_native_crc: bool,
    },
    Encode {
        out_icl: PathBuf,
        in_dir: PathBuf,
        #[arg(long, default_value_t = 75)]
        jpeg_quality: u8,
        #[arg(long)]
        vendor_native_crc: bool,
    },
    Decode {
        icl_path: PathBuf,
        out_dir: PathBuf,
    },
}

fn main() {
    if let Err(err) = run() {
        eprintln!("{err}");
        std::process::exit(1);
    }
}

/// Parse CLI arguments and dispatch the selected subcommand.
///
/// # Returns
///
/// Returns `Ok(())` on success.
///
/// # Errors
///
/// Returns an error if indexed input parsing fails, file I/O fails, or codec operations fail.
fn run() -> Result<(), String> {
    let cli = Cli::parse();
    match cli.command {
        Commands::EncodeJpegs { out_icl, inputs, vendor_native_crc } => {
            let pb = build_progress_bar(inputs.len() as u64, "Reading JPEG inputs");
            let mut jpegs = Vec::new();
            for item in inputs {
                let (idx, path) = parse_indexed_path(&item)?;
                let bytes = fs::read(&path).map_err(|e| format!("Failed reading {}: {e}", path.display()))?;
                jpegs.push((idx, bytes));
                pb.inc(1);
            }
            pb.finish_with_message("Read JPEG inputs");
            IclCodec::encode_from_jpegs(&jpegs, &out_icl, vendor_native_crc)?;
            println!("{}", out_icl.display());
            Ok(())
        }
        Commands::Encode { out_icl, in_dir, jpeg_quality, vendor_native_crc } => {
            let images = input::collect_images_from_dir(&in_dir)?;
            IclCodec::encode_from_images(&images, &out_icl, jpeg_quality, vendor_native_crc)?;
            println!("{}", out_icl.display());
            Ok(())
        }
        Commands::Decode { icl_path, out_dir } => {
            let parsed = IclCodec::decode_to_dir(&icl_path, &out_dir)?;
            println!(
                "{{\n  \"stored_crc\": {},\n  \"computed_crc\": {},\n  \"count\": {},\n  \"warnings\": {:?}\n}}",
                parsed.stored_crc,
                parsed.computed_crc,
                parsed.images.len(),
                parsed.warnings
            );
            Ok(())
        }
    }
}

/// Parse an indexed input in the form `index=path`.
///
/// # Arguments
///
/// - `item`: Text containing an unsigned integer index and a file path separated by `=`.
///
/// # Returns
///
/// Returns `(index, path)` on success.
///
/// # Errors
///
/// Returns an error if `=` is missing or the index is not a valid `usize`.
fn parse_indexed_path(item: &str) -> Result<(usize, PathBuf), String> {
    let (idx_s, path_s) = item.split_once('=').ok_or_else(|| format!("Invalid input '{item}', expected index=path"))?;
    let idx = idx_s.parse::<usize>().map_err(|e| format!("Invalid index '{idx_s}': {e}"))?;
    Ok((idx, PathBuf::from(path_s)))
}

/// Build a progress bar for interactive terminals.
///
/// # Arguments
///
/// - `total`: Expected number of steps/items.
/// - `message`: Status label shown next to the bar.
///
/// # Returns
///
/// Returns a visible progress bar on TTY stderr, or a hidden no-op bar when stderr is not a TTY.
fn build_progress_bar(total: u64, message: &str) -> ProgressBar {
    if !std::io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new(total);
    let style = ProgressStyle::with_template(
        "{msg:24} [{elapsed_precise}] {bar:40.cyan/blue} {pos}/{len} ({eta})",
    )
    .expect("valid progress bar template")
    .progress_chars("##-");
    pb.set_style(style);
    pb.set_message(message.to_string());
    pb
}
