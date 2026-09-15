use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};

/// Collect supported image files from a directory and assign indices.
///
/// Supported inputs:
/// - `.png`
/// - `.jpg` / `.jpeg`
///
/// Index assignment:
/// - If **all** filenames yield a numeric index and those indices are unique, those values are
///   used (sparse slots are preserved). A stem is accepted when it is fully numeric (`12.jpg`)
///   or begins with a decimal prefix (`01-home.png` → 1, `00005.bmp` → 5).
/// - Otherwise, indices are assigned sequentially in lexicographic filename order starting at 0.
///
/// # Arguments
///
/// - `dir`: Directory to scan (non-recursive).
///
/// # Returns
///
/// Returns `(index, path)` pairs for images discovered in `dir`.
///
/// # Errors
///
/// Returns an error if:
/// - `dir` does not exist or cannot be read,
/// - no supported images are found,
/// - numeric filename indices are duplicated.
pub fn collect_images_from_dir(dir: &Path) -> Result<Vec<(usize, PathBuf)>, String> {
    let mut files: Vec<PathBuf> = fs::read_dir(dir)
        .map_err(|e| format!("Failed reading directory {}: {e}", dir.display()))?
        .filter_map(|ent| ent.ok())
        .map(|ent| ent.path())
        .filter(|p| p.is_file())
        .filter(|p| is_supported_image(p))
        .collect();

    files.sort_by(|a, b| file_name(a).cmp(&file_name(b)));

    if files.is_empty() {
        return Err(format!(
            "No supported images found in {} (expected .png/.jpg/.jpeg)",
            dir.display()
        ));
    }

    let numeric_indices: Vec<Option<usize>> = files
        .iter()
        .map(|p| p.file_stem().and_then(OsStr::to_str).and_then(parse_index_from_stem))
        .collect();

    let all_numeric = numeric_indices.iter().all(|x| x.is_some());
    if all_numeric {
        let mut pairs: Vec<(usize, PathBuf)> = files
            .into_iter()
            .zip(numeric_indices.into_iter())
            .map(|(p, idx)| (idx.expect("all_numeric"), p))
            .collect();

        pairs.sort_by_key(|(idx, _)| *idx);

        for w in pairs.windows(2) {
            if w[0].0 == w[1].0 {
                return Err(format!(
                    "Duplicate numeric image index {} (filenames must have unique numeric stems)",
                    w[0].0
                ));
            }
        }

        return Ok(pairs);
    }

    Ok(files.into_iter().enumerate().map(|(i, p)| (i, p)).collect())
}

/// Parse an ICL slot index from a filename stem.
///
/// Accepts a fully decimal stem (`12`) or a leading decimal prefix before a non-digit
/// (`01-home`, `00005_splash`).
fn parse_index_from_stem(stem: &str) -> Option<usize> {
    if stem.is_empty() {
        return None;
    }
    if let Ok(idx) = stem.parse::<usize>() {
        return Some(idx);
    }
    let digit_len = stem.chars().take_while(|c| c.is_ascii_digit()).count();
    if digit_len == 0 || digit_len == stem.len() {
        return None;
    }
    stem[..digit_len].parse().ok()
}

fn is_supported_image(path: &Path) -> bool {
    let Some(ext) = path.extension().and_then(OsStr::to_str) else {
        return false;
    };
    let ext = ext.to_ascii_lowercase();
    matches!(ext.as_str(), "png" | "jpg" | "jpeg")
}

fn file_name(path: &Path) -> &OsStr {
    path.file_name().unwrap_or_else(|| OsStr::new(""))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{SystemTime, UNIX_EPOCH};

    fn unique_temp_dir() -> PathBuf {
        let mut p = std::env::temp_dir();
        let n = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        p.push(format!("icl_tool_test_{n}"));
        p
    }

    fn write_dummy(path: &Path) {
        fs::write(path, b"dummy").expect("write dummy");
    }

    #[test]
    fn sequential_indices_when_not_all_numeric() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).expect("mkdir");
        write_dummy(&dir.join("b.png"));
        write_dummy(&dir.join("a.jpg"));
        let items = collect_images_from_dir(&dir).expect("collect");
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].0, 0);
        assert!(items[0].1.ends_with("a.jpg"));
        assert_eq!(items[1].0, 1);
        assert!(items[1].1.ends_with("b.png"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn numeric_indices_when_all_numeric() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).expect("mkdir");
        write_dummy(&dir.join("10.jpg"));
        write_dummy(&dir.join("2.png"));
        let items = collect_images_from_dir(&dir).expect("collect");
        assert_eq!(items.iter().map(|(i, _)| *i).collect::<Vec<_>>(), vec![2, 10]);
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn leading_digit_prefixes_preserve_sparse_indices() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).expect("mkdir");
        write_dummy(&dir.join("01-home.png"));
        write_dummy(&dir.join("03-settings.png"));
        write_dummy(&dir.join("00-splash.png"));
        let items = collect_images_from_dir(&dir).expect("collect");
        assert_eq!(
            items.iter().map(|(i, _)| *i).collect::<Vec<_>>(),
            vec![0, 1, 3]
        );
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn error_on_duplicate_numeric_indices() {
        let dir = unique_temp_dir();
        fs::create_dir_all(&dir).expect("mkdir");
        write_dummy(&dir.join("1.jpg"));
        write_dummy(&dir.join("1.png"));
        let err = collect_images_from_dir(&dir).unwrap_err();
        assert!(err.contains("Duplicate numeric image index"));
        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn parse_index_from_stem_accepts_prefix_and_full_numeric() {
        assert_eq!(parse_index_from_stem("12"), Some(12));
        assert_eq!(parse_index_from_stem("01-home"), Some(1));
        assert_eq!(parse_index_from_stem("00005"), Some(5));
        assert_eq!(parse_index_from_stem("splash"), None);
        assert_eq!(parse_index_from_stem(""), None);
    }
}

