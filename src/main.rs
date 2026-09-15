mod codec;
mod input;
mod sector;

use clap::{ArgGroup, CommandFactory, Parser};
use clap_complete::{generate, Shell};
use codec::IclCodec;
use sector::{analyze, render_report, resolve_dwin_set_path};
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::io::{self, IsTerminal, Write};
use std::path::PathBuf;

const AFTER_HELP: &str = "\
Examples:
  icl_tool --encode-jpegs -o out.icl 0=00.jpg 1=01.jpg
  icl_tool -j -o out.icl 0=00.jpg 1=01.jpg
  icl_tool --encode -o out.icl -i ./images --jpeg-quality 75
  icl_tool -e -o out.icl -i ./images
  icl_tool --decode -i 26.icl -o extracted_dir
  icl_tool -d -i 26.icl -o extracted_dir
  icl_tool --sectors /path/to/project
  icl_tool -s /path/to/project

Shell completions:
  icl_tool --completions > _icl_tool
  # then place _icl_tool on your fpath and run: compinit
";

#[derive(Parser, Debug)]
#[command(name = "icl_tool")]
#[command(version)]
#[command(about = "DWIN ICL encoder/decoder and flash sector tools")]
#[command(after_help = AFTER_HELP)]
#[command(group(
    ArgGroup::new("mode")
        .args(["encode_jpegs", "encode", "decode", "sectors"])
        .multiple(false)
))]
struct Cli {
    /// Encode an ICL from already-JPEG files (one or more `index=path` inputs).
    #[arg(short = 'j', long = "encode-jpegs", requires = "output")]
    encode_jpegs: bool,

    /// Encode an ICL from image files in a directory.
    #[arg(short = 'e', long = "encode", requires_all = ["output", "input"])]
    encode: bool,

    /// Output path: ICL file when encoding, extract directory when decoding.
    #[arg(short = 'o', long = "output-file", alias = "output-dir", value_name = "PATH", conflicts_with = "sectors")]
    output: Option<PathBuf>,

    /// Input path: image directory when encoding, ICL file when decoding.
    #[arg(short = 'i', long = "input-dir", alias = "input-file", value_name = "PATH", conflicts_with_all = ["sectors", "encode_jpegs"])]
    input: Option<PathBuf>,

    /// `index=path` inputs (for `--encode-jpegs`).
    #[arg(value_name = "INPUTS")]
    inputs: Vec<String>,

    /// Extract JPEGs from an ICL into a directory.
    #[arg(short = 'd', long = "decode", requires_all = ["input", "output"])]
    decode: bool,

    /// Analyse DWIN flash sector allocation and detect overlapping files.
    #[arg(short = 's', long = "sectors", value_name = "FOLDER")]
    sectors: Option<PathBuf>,

    /// Generate a ZSH completions script on stdout.
    #[arg(long = "completions", conflicts_with_all = ["encode_jpegs", "encode", "decode", "sectors"])]
    completions: bool,

    /// Write the CRC field using native-endian bytes (compatibility toggle).
    #[arg(long)]
    vendor_native_crc: bool,

    /// Quality for intermediate JPEG encoding when using `--encode` (default: 75).
    #[arg(long, default_value_t = 75)]
    jpeg_quality: u8,
}

fn main() {
    match run() {
        Ok(ExitStatus::Ok) => {}
        Ok(ExitStatus::SectorOverlaps(count)) => std::process::exit(count as i32),
        Err(err) => {
            eprintln!("ERROR: {err}");
            std::process::exit(1);
        }
    }
}

enum ExitStatus {
    Ok,
    SectorOverlaps(usize),
}

/// Parse CLI arguments and dispatch the selected mode.
///
/// # Returns
///
/// Returns `Ok(())` on success.
///
/// # Errors
///
/// Returns an error if indexed input parsing fails, file I/O fails, or codec operations fail.
fn run() -> Result<ExitStatus, String> {
    let cli = Cli::parse();

    if cli.completions {
        print_completions()?;
        return Ok(ExitStatus::Ok);
    }

    if cli.encode_jpegs {
        let out_icl = cli
            .output
            .ok_or_else(|| "--encode-jpegs requires --output-file".to_string())?;
        if cli.inputs.is_empty() {
            return Err("--encode-jpegs requires at least one index=path input".into());
        }
        return run_encode_jpegs(&out_icl, &cli.inputs, cli.vendor_native_crc);
    }

    if cli.encode {
        let out_icl = cli
            .output
            .ok_or_else(|| "--encode requires --output-file".to_string())?;
        let in_dir = cli
            .input
            .ok_or_else(|| "--encode requires --input-dir".to_string())?;
        return run_encode(&out_icl, &in_dir, cli.jpeg_quality, cli.vendor_native_crc);
    }

    if cli.decode {
        let icl_path = cli
            .input
            .ok_or_else(|| "--decode requires --input-file".to_string())?;
        let out_dir = cli
            .output
            .ok_or_else(|| "--decode requires --output-dir".to_string())?;
        return run_decode(&icl_path, &out_dir);
    }

    if let Some(folder_path) = cli.sectors {
        return run_sector_allocation(&folder_path);
    }

    Cli::command().print_help().map_err(|e| e.to_string())?;
    println!();
    Ok(ExitStatus::Ok)
}

fn run_encode_jpegs(out_icl: &PathBuf, inputs: &[String], vendor_native_crc: bool) -> Result<ExitStatus, String> {
    let pb = build_progress_bar(inputs.len() as u64, "Reading JPEG inputs");
    let mut jpegs = Vec::new();
    for item in inputs {
        let (idx, path) = parse_indexed_path(item)?;
        let bytes = fs::read(&path).map_err(|e| format!("Failed reading {}: {e}", path.display()))?;
        jpegs.push((idx, bytes));
        pb.inc(1);
    }
    pb.finish_with_message("Read JPEG inputs");
    IclCodec::encode_from_jpegs(&jpegs, out_icl, vendor_native_crc)?;
    println!("{}", out_icl.display());
    Ok(ExitStatus::Ok)
}

fn run_encode(
    out_icl: &PathBuf,
    in_dir: &PathBuf,
    jpeg_quality: u8,
    vendor_native_crc: bool,
) -> Result<ExitStatus, String> {
    let images = input::collect_images_from_dir(in_dir)?;
    IclCodec::encode_from_images(&images, out_icl, jpeg_quality, vendor_native_crc)?;
    println!("{}", out_icl.display());
    Ok(ExitStatus::Ok)
}

fn run_decode(icl_path: &PathBuf, out_dir: &PathBuf) -> Result<ExitStatus, String> {
    let parsed = IclCodec::decode_to_dir(icl_path, out_dir)?;
    println!(
        "{{\n  \"stored_crc\": {},\n  \"computed_crc\": {},\n  \"count\": {},\n  \"warnings\": {:?}\n}}",
        parsed.stored_crc,
        parsed.computed_crc,
        parsed.images.len(),
        parsed.warnings
    );
    Ok(ExitStatus::Ok)
}

/// Write a ZSH completion script for this CLI to stdout.
fn print_completions() -> Result<(), String> {
    let mut cmd = Cli::command();
    generate(Shell::Zsh, &mut cmd, "icl_tool", &mut io::stdout());
    io::stdout().flush().map_err(|e| e.to_string())
}

/// Analyse DWIN_SET sector allocation and print a visual map.
fn run_sector_allocation(folder_path: &PathBuf) -> Result<ExitStatus, String> {
    let dwin_set_path = resolve_dwin_set_path(folder_path)?;
    println!("Checking files in {}", dwin_set_path.display());

    let report = analyze(&dwin_set_path)?;
    println!(
        "{} file(s) are flashable to the main flash space",
        report.dwin_files.len()
    );
    render_report(&report);

    let overlap_count = report.files_with_errors.len();
    if overlap_count > 0 {
        return Ok(ExitStatus::SectorOverlaps(overlap_count));
    }
    Ok(ExitStatus::Ok)
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
    if !io::stderr().is_terminal() {
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
