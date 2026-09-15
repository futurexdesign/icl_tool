//! DWIN display flash sector allocation analysis.
//!
//! DWIN T5L displays expose 64 × 256 KiB sectors (16 MiB total). Asset filenames in
//! `DWIN_SET` begin with a decimal sector index (for example, `32MAIN.ICL` maps to sector 32).
//! This module scans those assets, builds a sector occupancy map, and detects overlaps.

use std::fs;
use std::path::{Path, PathBuf};

/// Number of flash sectors available on a T5L display.
pub const SECTOR_COUNT: usize = 64;

/// Size of one flash sector in bytes (256 KiB).
pub const SECTOR_SIZE: u64 = 256 * 1024;

/// Total main-flash capacity in bytes (`SECTOR_COUNT × SECTOR_SIZE`).
pub const MAX_FLASH_SIZE: u64 = SECTOR_COUNT as u64 * SECTOR_SIZE;

const MAX_FILE_COUNT: usize = SECTOR_COUNT;

type AppResult<T> = Result<T, String>;

/// A flashable asset discovered under `DWIN_SET`.
///
/// The leading digits of [`file_name`](Self::file_name) encode the starting sector index
/// stored in [`file_id`](Self::file_id). [`has_error`](Self::has_error) is set when the
/// file participates in a sector overlap.
#[derive(Debug, Clone)]
pub struct DwinFile {
    /// Basename of the asset (for example, `32MAIN.ICL`).
    pub file_name: String,
    /// Starting sector index parsed from the filename prefix.
    pub file_id: usize,
    /// On-disk file size in bytes.
    pub file_size: u64,
    /// `true` when this file shares at least one sector with another file.
    pub has_error: bool,
}

impl DwinFile {
    /// Compute how many contiguous sectors this file occupies.
    ///
    /// Sector usage rounds up: any non-zero payload consumes at least one sector, and payloads
    /// larger than one sector span additional sectors as needed.
    ///
    /// # Returns
    ///
    /// Returns `0` when [`file_size`](Self::file_size) is zero; otherwise returns
    /// `ceil(file_size / SECTOR_SIZE)`.
    pub fn sector_count(&self) -> usize {
        if self.file_size == 0 {
            return 0;
        }
        ((self.file_size + SECTOR_SIZE - 1) / SECTOR_SIZE) as usize
    }
}

/// One flash sector and the files mapped into it.
#[derive(Debug, Clone)]
pub struct Sector {
    /// Zero-based sector index (`0..SECTOR_COUNT`).
    pub sector_id: usize,
    /// Files whose allocated range includes this sector (may be empty).
    pub files: Vec<DwinFile>,
}

impl Sector {
    /// Returns whether more than one file occupies this sector.
    ///
    /// # Returns
    ///
    /// Returns `true` when [`files`](Self::files) contains two or more entries.
    pub fn has_error(&self) -> bool {
        self.files.len() > 1
    }

    /// Returns whether no file is mapped to this sector.
    ///
    /// # Returns
    ///
    /// Returns `true` when [`files`](Self::files) is empty.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Aggregated output of the sector allocation pipeline.
#[derive(Debug)]
pub struct AllocationReport {
    /// All flashable files discovered in `DWIN_SET`, with overlap flags applied.
    pub dwin_files: Vec<DwinFile>,
    /// Full sector map (`SECTOR_COUNT` entries, indexed by sector id).
    pub sectors: Vec<Sector>,
    /// Subset of [`dwin_files`](Self::dwin_files) flagged as overlapping.
    pub files_with_errors: Vec<DwinFile>,
    /// Column width used when rendering file labels in the CLI report.
    pub label_width: usize,
}

/// Resolve the `DWIN_SET` directory from a project folder or a direct path.
///
/// If `folder_path/DWIN_SET` exists and is a directory, that path is returned. Otherwise
/// `folder_path` itself is treated as the `DWIN_SET` directory (with a warning printed to
/// stdout).
///
/// # Arguments
///
/// - `folder_path`: Project root or direct `DWIN_SET` directory.
///
/// # Returns
///
/// Returns the canonical absolute path to the `DWIN_SET` directory.
///
/// # Errors
///
/// Returns an error when the path does not exist or canonicalization fails.
pub fn resolve_dwin_set_path(folder_path: &Path) -> AppResult<PathBuf> {
    if !folder_path.exists() {
        return Err(format!("{} does not exist", folder_path.display()));
    }

    let candidate = folder_path.join("DWIN_SET");
    if candidate.is_dir() {
        return candidate
            .canonicalize()
            .map_err(|e| format!("Failed resolving {}: {e}", candidate.display()));
    }

    println!(
        "WARNING: Assuming '{}' is the DWIN_SET folder",
        folder_path.display()
    );
    folder_path
        .canonicalize()
        .map_err(|e| format!("Failed resolving {}: {e}", folder_path.display()))
}

/// Scan `dwin_set_path` for flashable `.ICL`, `.HZK`, and `.BIN` assets.
///
/// Only regular files whose names match `^\d+(.*?)\.(ICL|HZK|BIN)$` (case-insensitive) are
/// included. Non-flashable files (for example, `T5LCFG.CFG`) are ignored.
///
/// # Arguments
///
/// - `dwin_set_path`: Canonical or relative path to the `DWIN_SET` directory.
///
/// # Returns
///
/// Returns a vector of [`DwinFile`] records sorted in directory-read order (not by sector).
///
/// # Errors
///
/// Returns an error when the directory cannot be read, a flashable filename lacks a leading
/// sector index, more than [`SECTOR_COUNT`] flashable files are present, a file resolves
/// outside the directory (symlink traversal), or a file exceeds [`MAX_FLASH_SIZE`].
pub fn parse_dwin_files(dwin_set_path: &Path) -> AppResult<Vec<DwinFile>> {
    let base = dwin_set_path
        .canonicalize()
        .map_err(|e| format!("Failed resolving {}: {e}", dwin_set_path.display()))?;

    let entries = fs::read_dir(&base).map_err(|e| format!("Could not read directory: {e}"))?;

    let mut raw_files = Vec::new();
    for entry in entries {
        let entry = entry.map_err(|e| format!("Could not read directory: {e}"))?;
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
            continue;
        };
        if is_flashable_filename(name) {
            raw_files.push((name.to_string(), path));
        }
    }

    if raw_files.len() > MAX_FILE_COUNT {
        return Err(format!(
            "Found {} flashable files; maximum supported is {MAX_FILE_COUNT}.",
            raw_files.len()
        ));
    }

    let mut dwin_files = Vec::new();
    for (name, path) in raw_files {
        let resolved = path
            .canonicalize()
            .map_err(|e| format!("Failed resolving {}: {e}", path.display()))?;
        if !resolved.starts_with(&base) {
            return Err(format!(
                "File {name:?} resolves to {}, which is outside the DWIN_SET directory — possible symlink traversal.",
                resolved.display()
            ));
        }

        let Some(file_id) = parse_file_id(&name) else {
            return Err(format!("Unable to extract a sector index from filename {name:?}."));
        };

        let file_size = fs::metadata(&resolved)
            .map_err(|e| format!("Failed reading {}: {e}", resolved.display()))?
            .len();
        if file_size > MAX_FLASH_SIZE {
            return Err(format!(
                "File {name:?} is {file_size} bytes, which exceeds the total flash capacity of {MAX_FLASH_SIZE} bytes."
            ));
        }

        dwin_files.push(DwinFile {
            file_name: name,
            file_id,
            file_size,
            has_error: false,
        });
    }

    Ok(dwin_files)
}

/// Map each file into every sector its payload spans.
///
/// # Arguments
///
/// - `dwin_files`: Parsed flashable assets (typically from [`parse_dwin_files`]).
///
/// # Returns
///
/// Returns a fixed-size vector of [`SECTOR_COUNT`] [`Sector`] values. Each sector lists every
/// file whose `[file_id, file_id + sector_count)` range includes that sector.
///
/// # Errors
///
/// Returns an error when a file's starting sector is `>= SECTOR_COUNT` or its span extends
/// past the last sector.
pub fn build_sector_map(dwin_files: &[DwinFile]) -> AppResult<Vec<Sector>> {
    let mut sectors: Vec<Sector> = (0..SECTOR_COUNT).map(|i| Sector { sector_id: i, files: Vec::new() }).collect();

    for dwin_file in dwin_files {
        if dwin_file.file_id >= SECTOR_COUNT {
            return Err(format!(
                "File {:?} has starting sector {}, which exceeds the maximum of {}.",
                dwin_file.file_name,
                dwin_file.file_id,
                SECTOR_COUNT - 1
            ));
        }

        for sector_id in dwin_file.file_id..dwin_file.file_id + dwin_file.sector_count() {
            if sector_id >= SECTOR_COUNT {
                return Err(format!(
                    "File {:?} extends into sector {sector_id}, which exceeds the maximum of {}.",
                    dwin_file.file_name,
                    SECTOR_COUNT - 1
                ));
            }
            sectors[sector_id].files.push(dwin_file.clone());
        }
    }

    Ok(sectors)
}

/// Flag every file that occupies at least one sector with a conflict.
///
/// A sector conflict exists when two or more files map to the same sector. All files involved
/// in such a sector have [`DwinFile::has_error`] set to `true`.
///
/// # Arguments
///
/// - `dwin_files`: Mutable file list to update in place.
/// - `sectors`: Sector map produced by [`build_sector_map`].
pub fn mark_overlapping_files(dwin_files: &mut [DwinFile], sectors: &[Sector]) {
    for dwin_file in dwin_files.iter_mut() {
        for sector_id in dwin_file.file_id..dwin_file.file_id + dwin_file.sector_count() {
            if sectors[sector_id].has_error() {
                dwin_file.has_error = true;
                break;
            }
        }
    }
}

/// Run the full sector allocation analysis pipeline.
///
/// Parses flashable files, builds the sector map, flags overlaps, and computes report layout
/// metadata.
///
/// # Arguments
///
/// - `dwin_set_path`: Path to the `DWIN_SET` directory (already resolved).
///
/// # Returns
///
/// Returns an [`AllocationReport`] containing files, sectors, overlapping files, and label width.
///
/// # Errors
///
/// Propagates errors from [`parse_dwin_files`] and [`build_sector_map`].
pub fn analyze(dwin_set_path: &Path) -> AppResult<AllocationReport> {
    let dwin_files = parse_dwin_files(dwin_set_path)?;
    let sectors = build_sector_map(&dwin_files)?;
    let mut dwin_files = dwin_files;
    mark_overlapping_files(&mut dwin_files, &sectors);

    let label_width = dwin_files
        .iter()
        .map(|f| f.file_name.len())
        .max()
        .unwrap_or(7)
        + 2;

    let files_with_errors: Vec<DwinFile> = dwin_files.iter().filter(|f| f.has_error).cloned().collect();

    Ok(AllocationReport {
        dwin_files,
        sectors,
        files_with_errors,
        label_width,
    })
}

/// Print the colour-coded sector map and overlap summary to stdout.
///
/// # Arguments
///
/// - `report`: Analysis output from [`analyze`].
pub fn render_report(report: &AllocationReport) {
    render_allocation(report);
    render_result(report);
}

/// Render the sector grid and per-file usage rows.
fn render_allocation(report: &AllocationReport) {
    let label_width = report.label_width;
    let sectors = &report.sectors;
    let dwin_files = &report.dwin_files;

    println!();
    let header = format!(
        "{:<width$}",
        "Sector allocation overview",
        width = label_width + sectors.len()
    );
    println!("{}{}{header}{RESET}", bg(BLUE), fg(BLACK));

    write_label("Sectors", label_width, false);
    let allocated = sectors.iter().filter(|s| !s.is_empty()).count();
    for sector in sectors {
        write_sector_cell(sector.has_error(), sector.is_empty());
    }
    write_row_summary(
        &format!("{allocated} / {} sectors allocated", sectors.len()),
        false,
    );

    let mut sorted: Vec<&DwinFile> = dwin_files.iter().collect();
    sorted.sort_by_key(|f| f.file_id);

    for dwin_file in sorted {
        write_label(&dwin_file.file_name, label_width, dwin_file.has_error);

        let start = dwin_file.file_id;
        let end = start + dwin_file.sector_count().saturating_sub(1);
        for sector in sectors {
            let in_range = start <= sector.sector_id && sector.sector_id <= end;
            write_sector_cell(in_range && sector.has_error(), !in_range);
        }

        let size_kb = dwin_file.file_size as f64 / 1024.0;
        let used = dwin_file.sector_count();
        let slack_kb = (used as u64 * SECTOR_SIZE - dwin_file.file_size) as f64 / 1024.0;
        let plural = if used == 1 { "" } else { "s" };
        let summary = format!(
            "{:<8} | {used} sector{plural} used ({} KB left)",
            format!("{} KB", round_half_even(size_kb)),
            round_half_even(slack_kb)
        );
        write_row_summary(&summary, dwin_file.has_error);
    }

    println!();
}

/// Render the success banner or the overlapping-files error summary.
fn render_result(report: &AllocationReport) {
    if report.files_with_errors.is_empty() {
        println!(
            "{}{}No overlapping files have been detected{RESET}",
            bg(GREEN),
            fg(WHITE)
        );
        return;
    }

    println!(
        "{}{}The following files are overlapping:{RESET}",
        bg(RED),
        fg(WHITE)
    );
    for dwin_file in &report.files_with_errors {
        write_label(&dwin_file.file_name, report.label_width, true);
        println!();
    }
    println!(
        "{}{}Please move and reallocate these files to different sectors{RESET}",
        bg(RED),
        fg(WHITE)
    );
}

/// Returns whether `name` is a flashable DWIN asset filename.
///
/// Matches Python's `^\d+(.*?)\.(ICL|HZK|BIN)$` (case-insensitive).
fn is_flashable_filename(name: &str) -> bool {
    let digit_len = name.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_len == 0 {
        return false;
    }
    let upper = name[digit_len..].to_ascii_uppercase();
    upper.ends_with(".ICL") || upper.ends_with(".HZK") || upper.ends_with(".BIN")
}

/// Round a floating-point kilobyte value like Python 3's built-in `round()` (half-to-even).
fn round_half_even(value: f64) -> i64 {
    let rounded = value.round();
    if (value - value.floor() - 0.5).abs() < f64::EPSILON && (rounded as i64) % 2 != 0 {
        rounded as i64 - 1
    } else {
        rounded as i64
    }
}

/// Parse the leading decimal sector index from a flashable filename.
///
/// # Arguments
///
/// - `name`: Basename such as `32MAIN.ICL`.
///
/// # Returns
///
/// Returns `Some(index)` when the name begins with one or more ASCII digits; otherwise `None`.
fn parse_file_id(name: &str) -> Option<usize> {
    let digits: String = name.chars().take_while(|c| c.is_ascii_digit()).collect();
    if digits.is_empty() {
        None
    } else {
        digits.parse().ok()
    }
}

const RESET: &str = "\x1b[0m";

const BLACK: u8 = 0;
const RED: u8 = 1;
const GREEN: u8 = 2;
const BLUE: u8 = 4;
const CYAN: u8 = 6;
const WHITE: u8 = 7;

fn fg(color: u8) -> String {
    format!("\x1b[{}m", 30 + color)
}

fn bg(color: u8) -> String {
    format!("\x1b[{}m", 40 + color)
}

fn bright_fg(color: u8) -> String {
    format!("\x1b[{}m", 90 + color)
}

fn write_label(label: &str, width: usize, has_error: bool) {
    let bg_code = if has_error { bg(RED) } else { bg(BLACK) };
    print!("{bg_code}{}{:<width$}{RESET}", fg(WHITE), label, width = width);
}

fn write_sector_cell(has_error: bool, empty: bool) {
    let ch = if empty { '░' } else { '█' };
    let color = if empty {
        bright_fg(BLACK)
    } else if has_error {
        fg(RED)
    } else {
        fg(CYAN)
    };
    print!("{}{color}{ch}{RESET}", bg(BLACK));
}

fn write_row_summary(text: &str, has_error: bool) {
    let color = if has_error { fg(RED) } else { fg(CYAN) };
    println!("{color} {text}{RESET}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn make_file(file_id: usize, sectors: usize, name: Option<&str>) -> DwinFile {
        let file_name = name.unwrap_or(&format!("{file_id}TEST.BIN")).to_string();
        DwinFile {
            file_name,
            file_id,
            file_size: sectors as u64 * SECTOR_SIZE,
            has_error: false,
        }
    }

    fn unique_temp_dir(prefix: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        p.push(format!("{prefix}_{n}"));
        p
    }

    fn write_file(dir: &Path, name: &str, content: &[u8]) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, content).expect("write file");
        path
    }

    #[test]
    fn sector_count_rounds_up() {
        assert_eq!(make_file(0, 1, None).sector_count(), 1);
        assert_eq!(
            DwinFile {
                file_name: "0A.BIN".into(),
                file_id: 0,
                file_size: SECTOR_SIZE + 1,
                has_error: false,
            }
            .sector_count(),
            2
        );
        assert_eq!(make_file(0, 2, None).sector_count(), 2);
        assert_eq!(
            DwinFile {
                file_name: "0A.BIN".into(),
                file_id: 0,
                file_size: 1,
                has_error: false,
            }
            .sector_count(),
            1
        );
        assert_eq!(
            DwinFile {
                file_name: "0A.BIN".into(),
                file_id: 0,
                file_size: 0,
                has_error: false,
            }
            .sector_count(),
            0
        );
    }

    #[test]
    fn sector_error_detection() {
        let empty = Sector { sector_id: 0, files: vec![] };
        assert!(empty.is_empty());
        assert!(!empty.has_error());

        let one = Sector {
            sector_id: 0,
            files: vec![make_file(0, 1, None)],
        };
        assert!(!one.is_empty());
        assert!(!one.has_error());

        let two = Sector {
            sector_id: 0,
            files: vec![make_file(0, 1, None), make_file(1, 1, None)],
        };
        assert!(two.has_error());
    }

    #[test]
    fn build_sector_map_layout() {
        let sectors = build_sector_map(&[]).expect("empty map");
        assert_eq!(sectors.len(), SECTOR_COUNT);
        assert!(sectors.iter().all(|s| s.is_empty()));

        let f = make_file(5, 1, None);
        let sectors = build_sector_map(&[f.clone()]).expect("map");
        assert_eq!(sectors[5].files.len(), 1);
        assert!(sectors[4].is_empty());
        assert!(sectors[6].is_empty());

        let f = make_file(10, 3, None);
        let sectors = build_sector_map(&[f.clone()]).expect("map");
        for i in [10, 11, 12] {
            assert_eq!(sectors[i].files.len(), 1);
        }
    }

    #[test]
    fn overlapping_files_share_sector() {
        let f1 = make_file(0, 3, None);
        let f2 = make_file(2, 1, Some("2B.BIN"));
        let sectors = build_sector_map(&[f1, f2]).expect("map");
        assert!(sectors[2].has_error());
    }

    #[test]
    fn out_of_bounds_start_raises() {
        let f = DwinFile {
            file_name: "64BAD.BIN".into(),
            file_id: SECTOR_COUNT,
            file_size: SECTOR_SIZE,
            has_error: false,
        };
        assert!(build_sector_map(&[f]).is_err());
    }

    #[test]
    fn out_of_bounds_span_raises() {
        let f = DwinFile {
            file_name: "63BAD.BIN".into(),
            file_id: SECTOR_COUNT - 1,
            file_size: 2 * SECTOR_SIZE,
            has_error: false,
        };
        assert!(build_sector_map(&[f]).is_err());
    }

    #[test]
    fn parse_dwin_files_filters_and_reads() {
        let dir = unique_temp_dir("sector_parse");
        fs::create_dir_all(&dir).expect("mkdir");
        write_file(&dir, "32MAIN.ICL", &vec![b'a'; 1024]);
        write_file(&dir, "T5LCFG.CFG", b"cfg");
        write_file(&dir, "0LOGO.ICL", b"x");
        write_file(&dir, "T5L_OS_V22.BIN", b"os");

        let files = parse_dwin_files(&dir).expect("parse");
        assert_eq!(files.len(), 2);
        assert!(files.iter().any(|f| f.file_name == "32MAIN.ICL" && f.file_id == 32));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_dwin_files_accepts_case_insensitive_extensions() {
        let dir = unique_temp_dir("sector_case");
        fs::create_dir_all(&dir).expect("mkdir");
        write_file(&dir, "5logo.icl", b"x");
        write_file(&dir, "6font.hzk", b"x");
        write_file(&dir, "7data.bin", b"x");

        let files = parse_dwin_files(&dir).expect("parse");
        assert_eq!(files.len(), 3);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_dwin_files_rejects_oversized_file() {
        let dir = unique_temp_dir("sector_oversize");
        fs::create_dir_all(&dir).expect("mkdir");
        write_file(&dir, "0BIG.BIN", &vec![0u8; MAX_FLASH_SIZE as usize + 1]);

        let err = parse_dwin_files(&dir).expect_err("oversize");
        assert!(err.contains("exceeds the total flash capacity"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn mark_overlapping_files_flags_conflicts() {
        let f1 = make_file(0, 3, None);
        let f2 = make_file(2, 1, Some("2B.BIN"));
        let sectors = build_sector_map(&[f1.clone(), f2.clone()]).expect("map");
        let mut files = vec![f1, f2];
        mark_overlapping_files(&mut files, &sectors);
        assert!(files.iter().all(|f| f.has_error));
    }

    #[test]
    fn mark_overlapping_files_does_not_scan_extra_sector() {
        let f1 = make_file(0, 3, None);
        let sectors = build_sector_map(&[f1.clone()]).expect("map");
        assert!(!sectors[3].has_error());

        let mut files = vec![f1];
        mark_overlapping_files(&mut files, &sectors);
        assert!(!files[0].has_error);
    }

    #[test]
    fn pipeline_no_overlap() {
        let dir = unique_temp_dir("sector_pipeline_ok");
        fs::create_dir_all(&dir).expect("mkdir");
        write_file(&dir, "0A.BIN", &vec![b'x'; SECTOR_SIZE as usize]);
        write_file(&dir, "1B.BIN", &vec![b'x'; SECTOR_SIZE as usize]);

        let mut files = parse_dwin_files(&dir).expect("parse");
        let sectors = build_sector_map(&files).expect("map");
        mark_overlapping_files(&mut files, &sectors);
        assert!(files.iter().all(|f| !f.has_error));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn pipeline_detects_overlap() {
        let dir = unique_temp_dir("sector_pipeline_bad");
        fs::create_dir_all(&dir).expect("mkdir");
        write_file(&dir, "0A.BIN", &vec![b'x'; (2 * SECTOR_SIZE) as usize]);
        write_file(&dir, "1B.BIN", b"x");

        let mut files = parse_dwin_files(&dir).expect("parse");
        let sectors = build_sector_map(&files).expect("map");
        mark_overlapping_files(&mut files, &sectors);
        assert_eq!(files.iter().filter(|f| f.has_error).count(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn analyze_reports_clean_allocation() {
        let dir = unique_temp_dir("sector_analyze_ok");
        fs::create_dir_all(&dir).expect("mkdir");
        write_file(&dir, "0A.BIN", &vec![b'x'; SECTOR_SIZE as usize]);
        write_file(&dir, "2B.BIN", &vec![b'x'; SECTOR_SIZE as usize]);

        let report = analyze(&dir).expect("analyze");
        assert_eq!(report.dwin_files.len(), 2);
        assert!(report.files_with_errors.is_empty());
        assert!(report.label_width >= "0A.BIN".len() + 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn analyze_reports_overlap() {
        let dir = unique_temp_dir("sector_analyze_bad");
        fs::create_dir_all(&dir).expect("mkdir");
        write_file(&dir, "0A.BIN", &vec![b'x'; (2 * SECTOR_SIZE) as usize]);
        write_file(&dir, "1B.BIN", b"x");

        let report = analyze(&dir).expect("analyze");
        assert_eq!(report.files_with_errors.len(), 2);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_dwin_set_path_from_project_root() {
        let project = unique_temp_dir("sector_project");
        let dwin_set = project.join("DWIN_SET");
        fs::create_dir_all(&dwin_set).expect("mkdir");

        let resolved = resolve_dwin_set_path(&project).expect("resolve");
        assert_eq!(resolved, dwin_set.canonicalize().expect("canonical"));
        let _ = fs::remove_dir_all(&project);
    }

    #[test]
    fn resolve_dwin_set_path_from_direct_folder() {
        let dir = unique_temp_dir("sector_direct");
        fs::create_dir_all(&dir).expect("mkdir");

        let resolved = resolve_dwin_set_path(&dir).expect("resolve");
        assert_eq!(resolved, dir.canonicalize().expect("canonical"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn resolve_dwin_set_path_missing_path() {
        let missing = unique_temp_dir("sector_missing");
        assert!(resolve_dwin_set_path(&missing).is_err());
    }

    #[test]
    fn is_flashable_filename_matches_expected_patterns() {
        assert!(is_flashable_filename("32MAIN.ICL"));
        assert!(is_flashable_filename("0LOGO.hzk"));
        assert!(is_flashable_filename("10DATA.bin"));
        assert!(!is_flashable_filename("T5LCFG.CFG"));
        assert!(!is_flashable_filename("MAIN.ICL"));
        assert!(!is_flashable_filename("32MAIN.JPG"));
    }

    #[test]
    fn parse_file_id_extracts_leading_digits() {
        assert_eq!(parse_file_id("32MAIN.ICL"), Some(32));
        assert_eq!(parse_file_id("0LOGO.ICL"), Some(0));
        assert_eq!(parse_file_id("MAIN.ICL"), None);
    }

    #[test]
    fn round_half_even_matches_python() {
        assert_eq!(round_half_even(2.5), 2);
        assert_eq!(round_half_even(3.5), 4);
        assert_eq!(round_half_even(256.0), 256);
        assert_eq!(round_half_even(255.5), 256);
    }
}
