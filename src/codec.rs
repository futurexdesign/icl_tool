//! DWIN ICL codec implementation for vendor-style `DGUS_3` files.
//!
//! File layout used by this module:
//! - Bytes `0..6`: magic header `DGUS_3`
//! - Bytes `6..8`: CRC16-MODBUS of bytes `8..end` (little-endian by default)
//! - Bytes `8..12`: total payload/file-size metadata (vendor-specific)
//! - Byte `12`: type byte (expected `0x04`)
//! - Bytes `13..15`: image count (u16, big-endian)
//! - Byte `15`: fixed zero marker
//! - Bytes `16..first_data_offset`: u32 BE offset table (sparse indexes allowed)
//! - At each non-zero offset:
//!   - `u16` width, `u16` height
//!   - `u16` JPEG header length
//!   - `u32` scan-tail length
//!   - Vendor-formatted JPEG payload bytes
//!
//! JPEG payloads are normalized on decode by fixing SOS length semantics and
//! trimming to EOI.

use image::codecs::jpeg::JpegEncoder;
use image::ColorType;
use image::ImageReader;
use indicatif::{ProgressBar, ProgressStyle};
use std::fs;
use std::io::IsTerminal;
use std::path::{Path, PathBuf};

type AppResult<T> = Result<T, String>;

const MAGIC: &[u8; 6] = b"DGUS_3";
const TYPE_BYTE: u8 = 0x04;
const FIXED_ZERO: u8 = 0x00;
const EOI: &[u8; 2] = b"\xFF\xD9";
const SOS: &[u8; 2] = b"\xFF\xDA";

#[derive(Debug, Clone)]
pub struct IclImageEntry {
    pub index: usize,
    pub stored_content: Vec<u8>,
}

impl IclImageEntry {
    /// Reconstruct a standards-compliant JPEG from vendor-formatted stored bytes.
    ///
    /// # Returns
    ///
    /// Returns normalized JPEG bytes ending at EOI with corrected SOS length semantics.
    ///
    /// # Errors
    ///
    /// Returns an error if required JPEG markers are missing or computed marker bounds are invalid.
    pub fn normalized_jpeg(&self) -> AppResult<Vec<u8>> {
        let eoi = find_bytes(&self.stored_content, EOI)
            .ok_or_else(|| "Stored content missing EOI".to_string())?;
        let trimmed = &self.stored_content[..eoi + 2];

        let sos_pos = find_bytes(trimmed, SOS).ok_or_else(|| "Stored content missing SOS".to_string())?;
        let current_sos_len = be_u16(&trimmed[sos_pos + 2..sos_pos + 4]) as usize;
        let ncomp = trimmed[sos_pos + 4] as usize;
        let expected_sos_len = 6 + 2 * ncomp;

        let scan_start = sos_pos + 2 + current_sos_len;
        let standard_scan_start = sos_pos + 2 + expected_sos_len;
        if scan_start > trimmed.len() || standard_scan_start > trimmed.len() {
            return Err("SOS bounds out of range".to_string());
        }

        let mut out = Vec::new();
        out.extend_from_slice(&trimmed[..sos_pos + 2]);
        out.extend_from_slice(&(expected_sos_len as u16).to_be_bytes());
        out.extend_from_slice(&trimmed[sos_pos + 4..standard_scan_start]);
        out.extend_from_slice(&trimmed[scan_start..]);
        Ok(out)
    }
}

#[derive(Debug)]
pub struct ParsedIcl {
    pub stored_crc: u16,
    pub computed_crc: u16,
    pub warnings: Vec<String>,
    pub images: Vec<IclImageEntry>,
}

pub struct IclCodec;

impl IclCodec {
    /// Encode indexed JPEG byte buffers into a single DWIN ICL file.
    ///
    /// # Arguments
    ///
    /// - `jpegs`: `(index, jpeg_bytes)` pairs. Sparse indices are supported.
    /// - `out_path`: Destination path for the generated `.icl` file.
    /// - `vendor_native_crc`: Controls native-endian vs little-endian CRC field encoding.
    ///
    /// # Errors
    ///
    /// Returns an error for empty input, malformed JPEG payloads, parse/format failures, or output write failures.
    pub fn encode_from_jpegs(
        jpegs: &[(usize, Vec<u8>)],
        out_path: &Path,
        vendor_native_crc: bool,
    ) -> AppResult<()> {
        if jpegs.is_empty() {
            return Err("No JPEGs provided".to_string());
        }

        let max_index = jpegs
            .iter()
            .map(|(idx, _)| *idx)
            .max()
            .ok_or_else(|| "No JPEGs provided".to_string())?;

        let mut header = Vec::new();
        header.extend_from_slice(MAGIC);
        header.extend_from_slice(&pack_crc(0, vendor_native_crc));
        header.extend_from_slice(&0u32.to_be_bytes());
        header.push(TYPE_BYTE);
        header.extend_from_slice(&(jpegs.len() as u16).to_be_bytes());
        header.push(FIXED_ZERO);

        let mut payload = Vec::new();
        let mut positions = vec![0u32; max_index + 1];
        let mut current_offset = header.len() + (max_index + 1) * 4;
        let pb = build_progress_bar(jpegs.len() as u64, "Encoding JPEGs");

        for (idx, jpeg) in jpegs {
            let formatted = format_image_content_vendor(jpeg)?;
            let (stored, img_header_len) = finalize_stored_content(&formatted)?;
            let (width, height) = parse_jpeg_size(&stored)?;
            let scan_tail_len = (stored.len() - img_header_len as usize) as u32;

            payload.extend_from_slice(&width.to_be_bytes());
            payload.extend_from_slice(&height.to_be_bytes());
            payload.extend_from_slice(&img_header_len.to_be_bytes());
            payload.extend_from_slice(&scan_tail_len.to_be_bytes());
            payload.extend_from_slice(&stored);

            positions[*idx] = current_offset as u32;
            current_offset += 10 + stored.len();
            pb.inc(1);
        }
        pb.finish_with_message("Encoded JPEGs");

        let mut table = Vec::new();
        for off in positions {
            table.extend_from_slice(&off.to_be_bytes());
        }

        let mut final_header = Vec::new();
        final_header.extend_from_slice(MAGIC);
        final_header.extend_from_slice(&pack_crc(0, vendor_native_crc));
        final_header.extend_from_slice(&((current_offset - 8) as u32).to_be_bytes());
        final_header.push(TYPE_BYTE);
        final_header.extend_from_slice(&(jpegs.len() as u16).to_be_bytes());
        final_header.push(FIXED_ZERO);

        let mut full = Vec::new();
        full.extend_from_slice(&final_header);
        full.extend_from_slice(&table);
        full.extend_from_slice(&payload);

        let crc = crc16_modbus(&full[8..], 0xFFFF);
        let crc_bytes = pack_crc(crc, vendor_native_crc);
        full[6] = crc_bytes[0];
        full[7] = crc_bytes[1];

        fs::write(out_path, full).map_err(|e| format!("Failed writing {}: {e}", out_path.display()))?;
        Ok(())
    }

    /// Load ordinary image files, convert them to JPEG, then encode into ICL.
    ///
    /// # Arguments
    ///
    /// - `images`: `(index, path)` pairs to source images.
    /// - `out_path`: Destination ICL file path.
    /// - `jpeg_quality`: JPEG quality used during conversion.
    /// - `vendor_native_crc`: Controls CRC field byte ordering behavior.
    ///
    /// # Errors
    ///
    /// Returns an error for image read/decode/encode failures or downstream encode failures.
    pub fn encode_from_images(
        images: &[(usize, PathBuf)],
        out_path: &Path,
        jpeg_quality: u8,
        vendor_native_crc: bool,
    ) -> AppResult<()> {
        let mut jpegs = Vec::new();
        let pb = build_progress_bar(images.len() as u64, "Compressing images");
        for (idx, path) in images {
            jpegs.push((*idx, load_image_as_vendor_jpeg(path, jpeg_quality)?));
            pb.inc(1);
        }
        pb.finish_with_message("Compressed images");
        Self::encode_from_jpegs(&jpegs, out_path, vendor_native_crc)
    }

    /// Decode an ICL file and write extracted JPEGs and a summary report to a directory.
    ///
    /// # Arguments
    ///
    /// - `icl_path`: Source ICL file.
    /// - `out_dir`: Directory to create/use for extracted `NN.jpg` files and `extract_report.json`.
    ///
    /// # Returns
    ///
    /// Returns parsed metadata and image entries.
    ///
    /// # Errors
    ///
    /// Returns an error if parsing fails, output directory creation fails, JPEG normalization fails, or writes fail.
    pub fn decode_to_dir(icl_path: &Path, out_dir: &Path) -> AppResult<ParsedIcl> {
        let parsed = parse_icl(icl_path)?;
        fs::create_dir_all(out_dir).map_err(|e| format!("Failed creating {}: {e}", out_dir.display()))?;
        let pb = build_progress_bar(parsed.images.len() as u64, "Decoding images");

        for img in &parsed.images {
            let out = out_dir.join(format!("{:02}.jpg", img.index));
            fs::write(&out, img.normalized_jpeg()?).map_err(|e| format!("Failed writing {}: {e}", out.display()))?;
            pb.inc(1);
        }
        pb.finish_with_message("Decoded images");

        let report = format!(
            "{{\n  \"stored_crc\": {},\n  \"computed_crc\": {},\n  \"count\": {},\n  \"warnings\": {:?}\n}}\n",
            parsed.stored_crc,
            parsed.computed_crc,
            parsed.images.len(),
            parsed.warnings
        );
        fs::write(out_dir.join("extract_report.json"), report)
            .map_err(|e| format!("Failed writing report: {e}"))?;

        Ok(parsed)
    }
}

/// Load an image from disk and encode it as RGB JPEG using the requested quality.
///
/// # Arguments
///
/// - `path`: Source image path accepted by the `image` crate.
/// - `jpeg_quality`: Quality setting passed to `JpegEncoder`.
///
/// # Returns
///
/// Returns JPEG bytes.
///
/// # Errors
///
/// Returns an error if file open/decode/encode steps fail.
fn load_image_as_vendor_jpeg(path: &Path, jpeg_quality: u8) -> AppResult<Vec<u8>> {
    let img = ImageReader::open(path)
        .map_err(|e| format!("Failed opening {}: {e}", path.display()))?
        .decode()
        .map_err(|e| format!("Failed decoding {}: {e}", path.display()))?
        .to_rgb8();
    let (w, h) = img.dimensions();

    let mut out = Vec::new();
    let mut enc = JpegEncoder::new_with_quality(&mut out, jpeg_quality);
    enc.encode(img.as_raw(), w, h, ColorType::Rgb8.into())
        .map_err(|e| format!("Failed encoding JPEG {}: {e}", path.display()))?;
    Ok(out)
}

/// Parse an ICL file into structured metadata and image payload entries.
///
/// # Arguments
///
/// - `path`: Path to the ICL blob.
///
/// # Returns
///
/// Returns CRC info, warnings, and image payloads.
///
/// # Errors
///
/// Returns an error for invalid header/magic, malformed offset table, out-of-bounds records, or read failures.
fn parse_icl(path: &Path) -> AppResult<ParsedIcl> {
    let blob = fs::read(path).map_err(|e| format!("Failed reading {}: {e}", path.display()))?;
    if blob.len() < 16 || &blob[..6] != MAGIC {
        return Err("Not a DGUS_3 ICL file".to_string());
    }

    let stored_crc = le_u16(&blob[6..8]);
    let computed_crc = crc16_modbus(&blob[8..], 0xFFFF);
    let image_count = be_u16(&blob[13..15]);

    let mut first_data_offset = None;
    let mut pos = 16usize;
    while pos + 4 <= blob.len() {
        let val = be_u32(&blob[pos..pos + 4]) as usize;
        if val != 0 {
            first_data_offset = Some(val);
            break;
        }
        pos += 4;
    }

    let first_data_offset = first_data_offset.ok_or_else(|| "No image offsets found".to_string())?;
    if first_data_offset < 16 || (first_data_offset - 16) % 4 != 0 {
        return Err(format!("Invalid first data offset: {first_data_offset}"));
    }

    let table_entries = (first_data_offset - 16) / 4;
    let mut offsets = Vec::with_capacity(table_entries);
    for i in 0..table_entries {
        offsets.push(be_u32(&blob[16 + 4 * i..20 + 4 * i]));
    }

    let mut warnings = Vec::new();
    let nonzero_count = offsets.iter().filter(|x| **x != 0).count();
    if nonzero_count != image_count as usize {
        warnings.push(format!(
            "Header image count ({}) != non-zero offsets ({})",
            image_count, nonzero_count
        ));
    }

    let mut images = Vec::new();
    for (idx, off) in offsets.iter().enumerate() {
        if *off == 0 {
            continue;
        }
        let off = *off as usize;
        if off + 10 > blob.len() {
            return Err(format!("Image entry {} points outside file", idx));
        }

        let _width = be_u16(&blob[off..off + 2]);
        let _height = be_u16(&blob[off + 2..off + 4]);
        let img_header_len = be_u16(&blob[off + 4..off + 6]) as usize;
        let scan_tail_len = be_u32(&blob[off + 6..off + 10]) as usize;

        let next_off = offsets
            .iter()
            .filter(|x| **x as usize > off)
            .map(|x| *x as usize)
            .min()
            .unwrap_or(blob.len());
        if next_off > blob.len() || next_off <= off + 10 {
            return Err(format!("Invalid next image offset for entry {}", idx));
        }

        let stored_content = blob[off + 10..next_off].to_vec();
        if img_header_len > stored_content.len() {
            warnings.push(format!("Image {} has header_len beyond content size", idx));
        }
        if scan_tail_len > stored_content.len() {
            warnings.push(format!("Image {} has scan_tail_len beyond content size", idx));
        }

        images.push(IclImageEntry {
            index: idx,
            stored_content,
        });
    }

    Ok(ParsedIcl {
        stored_crc,
        computed_crc,
        warnings,
        images,
    })
}

/// Rewrite JPEG SOS metadata/padding to vendor-compatible in-file layout.
///
/// # Arguments
///
/// - `jpeg`: Source JPEG bytes.
///
/// # Returns
///
/// Returns bytes with adjusted SOS length field and vendor alignment padding.
///
/// # Errors
///
/// Returns an error if required JPEG scan markers cannot be located.
fn format_image_content_vendor(jpeg: &[u8]) -> AppResult<Vec<u8>> {
    let scan_offset = find_scan_data_offset(jpeg)?;
    let sos_pos = find_bytes(jpeg, SOS).ok_or_else(|| "Missing SOS".to_string())?;
    let sos_len = be_u16(&jpeg[sos_pos + 2..sos_pos + 4]) as usize;
    let corrected_scan_offset = if sos_pos + 2 + sos_len != scan_offset {
        sos_pos + 2 + sos_len
    } else {
        scan_offset
    };

    // Vendor format expects scan data aligned as if an additional 10-byte image record header
    // preceded this JPEG body.
    let ff_len = (4 - ((corrected_scan_offset + 10) % 4)) % 4;

    let mut out = Vec::new();
    out.extend_from_slice(&jpeg[..sos_pos + 2]);
    out.extend_from_slice(&((corrected_scan_offset - (sos_pos + 2) + ff_len) as u16).to_be_bytes());
    out.extend_from_slice(&jpeg[sos_pos + 4..corrected_scan_offset]);
    out.extend(std::iter::repeat_n(0xFF, ff_len));
    out.extend_from_slice(&jpeg[corrected_scan_offset..]);
    Ok(out)
}

/// Finalize vendor-stored JPEG content by appending reserved bytes and tail padding.
///
/// # Arguments
///
/// - `formatted`: Vendor-formatted JPEG bytes.
///
/// # Returns
///
/// Returns `(content, img_header_len)` where `img_header_len` points to scan data start.
///
/// # Errors
///
/// Returns an error if scan data offset cannot be determined.
fn finalize_stored_content(formatted: &[u8]) -> AppResult<(Vec<u8>, u16)> {
    let mut content = formatted.to_vec();
    content.extend(std::iter::repeat_n(0u8, 8));

    let mut img_header_len = find_scan_data_offset(&content)? as u16;
    let tail_len = content.len() - img_header_len as usize;
    if tail_len % 4 != 0 {
        content.extend(std::iter::repeat_n(0u8, 4 - (tail_len % 4)));
    }
    img_header_len = find_scan_data_offset(&content)? as u16;
    Ok((content, img_header_len))
}

/// Walk JPEG markers and return the scan-data start offset.
///
/// The returned offset points just after the SOS segment header where entropy-coded data starts.
///
/// # Arguments
///
/// - `jpeg`: JPEG-like byte stream.
///
/// # Returns
///
/// Returns the entropy-coded scan start offset.
///
/// # Errors
///
/// Returns an error for malformed marker streams or truncated data.
fn find_scan_data_offset(jpeg: &[u8]) -> AppResult<usize> {
    let mut start = 0x02usize;
    loop {
        if start + 4 > jpeg.len() {
            return Err("Invalid JPEG marker walk".to_string());
        }
        let marker = &jpeg[start..start + 2];
        if marker[0] != 0xFF {
            return Err("Invalid JPEG marker walk".to_string());
        }
        let seglen = be_u16(&jpeg[start + 2..start + 4]) as usize;
        start += 2 + seglen;
        if marker == SOS {
            return Ok(start);
        }
    }
}

/// Extract image dimensions from JPEG SOF markers.
///
/// # Arguments
///
/// - `jpeg`: JPEG byte stream.
///
/// # Returns
///
/// Returns `(width, height)` when a supported SOF marker is found.
///
/// # Errors
///
/// Returns an error when dimensions cannot be located due to malformed or unsupported data.
fn parse_jpeg_size(jpeg: &[u8]) -> AppResult<(u16, u16)> {
    let mut i = 2usize;
    while i + 9 < jpeg.len() {
        if jpeg[i] != 0xFF {
            i += 1;
            continue;
        }
        while i < jpeg.len() && jpeg[i] == 0xFF {
            i += 1;
        }
        if i >= jpeg.len() {
            break;
        }
        let marker = jpeg[i];
        i += 1;
        if marker == 0xD8 || marker == 0xD9 {
            continue;
        }
        let seglen = be_u16(&jpeg[i..i + 2]) as usize;
        if i + seglen > jpeg.len() {
            break;
        }
        let payload = &jpeg[i + 2..i + seglen];
        if marker == 0xC0 || marker == 0xC1 || marker == 0xC2 {
            return Ok((be_u16(&payload[3..5]), be_u16(&payload[1..3])));
        }
        i += seglen;
    }
    Err("Could not find JPEG size".to_string())
}

/// Compute CRC16-MODBUS over `data` starting from an initial `crc` value.
///
/// # Arguments
///
/// - `data`: Bytes to checksum.
/// - `crc`: Initial CRC seed (typically `0xFFFF`).
///
/// # Returns
///
/// Returns the final CRC16-MODBUS checksum.
fn crc16_modbus(data: &[u8], mut crc: u16) -> u16 {
    for b in data {
        crc ^= *b as u16;
        for _ in 0..8 {
            if crc & 0x0001 == 0x0001 {
                crc = (crc >> 1) ^ 0xA001;
            } else {
                crc >>= 1;
            }
        }
    }
    crc
}

/// Pack CRC bytes according to vendor-endianness mode.
///
/// # Arguments
///
/// - `crc`: Computed checksum.
/// - `vendor_native`: If true, use native-endian bytes; otherwise use little-endian bytes.
///
/// # Returns
///
/// Returns a two-byte CRC representation.
fn pack_crc(crc: u16, vendor_native: bool) -> [u8; 2] {
    if vendor_native {
        crc.to_ne_bytes()
    } else {
        crc.to_le_bytes()
    }
}

/// Find the first occurrence of `needle` in `haystack`.
///
/// # Returns
///
/// Returns `Some(start_index)` if found; otherwise `None`.
fn find_bytes(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack.windows(needle.len()).position(|w| w == needle)
}

/// Read a big-endian `u16` from a two-byte slice.
fn be_u16(s: &[u8]) -> u16 {
    u16::from_be_bytes([s[0], s[1]])
}

/// Read a little-endian `u16` from a two-byte slice.
fn le_u16(s: &[u8]) -> u16 {
    u16::from_le_bytes([s[0], s[1]])
}

/// Read a big-endian `u32` from a four-byte slice.
fn be_u32(s: &[u8]) -> u32 {
    u32::from_be_bytes([s[0], s[1], s[2], s[3]])
}

/// Build a progress bar for interactive terminals.
///
/// # Arguments
///
/// - `total`: Expected number of items.
/// - `message`: Label for the progress display.
///
/// # Returns
///
/// Returns a visible progress bar on TTY stderr, or a hidden bar otherwise.
fn build_progress_bar(total: u64, message: &str) -> ProgressBar {
    if !std::io::stderr().is_terminal() {
        return ProgressBar::hidden();
    }
    let pb = ProgressBar::new(total);
    let style = ProgressStyle::with_template(
        "{msg:24} [{elapsed_precise}] {bar:40.green/black} {pos}/{len} ({eta})",
    )
    .expect("valid progress bar template")
    .progress_chars("##-");
    pb.set_style(style);
    pb.set_message(message.to_string());
    pb
}

#[cfg(test)]
mod tests {
    use super::*;
    use image::RgbImage;

    fn make_test_jpeg() -> Vec<u8> {
        let mut img = RgbImage::new(2, 3);
        for (x, y, pixel) in img.enumerate_pixels_mut() {
            *pixel = image::Rgb([(x * 30) as u8, (y * 40) as u8, 127]);
        }
        let mut out = Vec::new();
        let mut enc = JpegEncoder::new_with_quality(&mut out, 80);
        enc.encode(img.as_raw(), 2, 3, ColorType::Rgb8.into())
            .expect("test jpeg should encode");
        out
    }

    #[test]
    fn crc16_modbus_matches_known_vector() {
        let crc = crc16_modbus(b"123456789", 0xFFFF);
        assert_eq!(crc, 0x4B37);
    }

    #[test]
    fn parse_jpeg_size_reads_dimensions() {
        let jpeg = make_test_jpeg();
        let (w, h) = parse_jpeg_size(&jpeg).expect("size should parse");
        assert_eq!((w, h), (2, 3));
    }

    #[test]
    fn finalize_stored_content_aligns_scan_tail_to_four() {
        let jpeg = make_test_jpeg();
        let formatted = format_image_content_vendor(&jpeg).expect("format should succeed");
        let (stored, header_len) = finalize_stored_content(&formatted).expect("finalize should succeed");
        let tail_len = stored.len() - header_len as usize;
        assert_eq!(tail_len % 4, 0);
    }

    #[test]
    fn normalized_jpeg_contains_markers() {
        let jpeg = make_test_jpeg();
        let formatted = format_image_content_vendor(&jpeg).expect("format should succeed");
        let (stored, _) = finalize_stored_content(&formatted).expect("finalize should succeed");
        let entry = IclImageEntry {
            index: 0,
            stored_content: stored,
        };
        let restored = entry.normalized_jpeg().expect("normalize should succeed");
        assert_eq!(&restored[..2], b"\xFF\xD8");
        assert!(restored.ends_with(EOI));
    }
}
