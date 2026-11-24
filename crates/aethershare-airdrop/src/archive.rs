//! Pure-Rust POSIX.1 octet-oriented (`070707` ODC) CPIO + Gzip archive codec for AirDrop `/Upload`.
//!
//! AirDrop transfers files wrapped in a gzip-compressed ODC CPIO stream where each
//! header is 76 ASCII bytes (`070707` magic + 11× 6-char octal fields + 11-char octal filesize),
//! followed by the NUL-terminated path (`./<filename>`), followed by the file bytes,
//! and terminated by `TRAILER!!!\0`.

use std::fs::{self, File};
use std::io::{Cursor, Read, Write};
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, anyhow};
use flate2::Compression;
use flate2::read::GzDecoder;
use flate2::write::GzEncoder;

const ODC_MAGIC: &[u8; 6] = b"070707";
const ODC_HEADER_LEN: usize = 76;
const TRAILER_NAME: &str = "TRAILER!!!";

/// Packs a list of files into a gzip-compressed ODC CPIO archive as expected by AirDrop `/Upload`.
pub fn create_cpio_gz(files: &[PathBuf]) -> Result<Vec<u8>, anyhow::Error> {
    let mut gz = GzEncoder::new(Vec::new(), Compression::fast());

    for (idx, file_path) in files.iter().enumerate() {
        let meta = fs::metadata(file_path)
            .with_context(|| format!("Failed to stat {:?}", file_path))?;
        if !meta.is_file() {
            continue;
        }
        let file_name = file_path
            .file_name()
            .and_then(|s| s.to_str())
            .ok_or_else(|| anyhow!("Invalid UTF-8 filename {:?}", file_path))?;
        let entry_path = format!("./{file_name}");
        let mut data = Vec::with_capacity(meta.len() as usize);
        File::open(file_path)?.read_to_end(&mut data)?;

        write_odc_entry(
            &mut gz,
            &entry_path,
            (idx + 1) as u32,
            0o100644,
            &data,
        )?;
    }

    // Write CPIO trailer
    write_odc_entry(&mut gz, TRAILER_NAME, 0, 0, &[])?;
    Ok(gz.finish()?)
}

fn write_odc_entry<W: Write>(
    w: &mut W,
    name: &str,
    ino: u32,
    mode: u32,
    data: &[u8],
) -> Result<(), anyhow::Error> {
    let name_with_nul = format!("{name}\0");
    let namesize = name_with_nul.len();
    let filesize = data.len();
    let header = format!(
        "070707{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:06o}{:011o}{:06o}{:011o}",
        0u32,               // c_dev
        ino & 0o777_777,    // c_ino
        mode & 0o777_777,   // c_mode
        0u32,               // c_uid
        0u32,               // c_gid
        1u32,               // c_nlink
        0u32,               // c_rdev
        0u64,               // c_mtime
        namesize & 0o777_777,
        filesize as u64,
    );
    debug_assert_eq!(header.len(), ODC_HEADER_LEN);
    w.write_all(header.as_bytes())?;
    w.write_all(name_with_nul.as_bytes())?;
    w.write_all(data)?;
    Ok(())
}

/// Safely extracts a gzip-compressed (or uncompressed) ODC CPIO archive into `dest_dir`,
/// stripping any `..` or root prefixes to prevent path traversal.
pub fn extract_cpio_gz(archive_bytes: &[u8], dest_dir: &Path) -> Result<Vec<PathBuf>, anyhow::Error> {
    fs::create_dir_all(dest_dir)?;

    // Auto-detect gzip header (0x1f 0x8b) vs raw cpio
    let raw_cpio = if archive_bytes.starts_with(&[0x1f, 0x8b]) {
        let mut decoder = GzDecoder::new(Cursor::new(archive_bytes));
        let mut out = Vec::new();
        decoder.read_to_end(&mut out)?;
        out
    } else {
        archive_bytes.to_vec()
    };

    let mut cursor = Cursor::new(raw_cpio.as_slice());
    let mut saved_files = Vec::new();

    loop {
        let mut hdr = [0u8; ODC_HEADER_LEN];
        if cursor.read_exact(&mut hdr).is_err() {
            break;
        }
        if &hdr[0..6] != ODC_MAGIC {
            return Err(anyhow!("Invalid ODC CPIO magic: {:?}", &hdr[0..6]));
        }

        let mode_str = std::str::from_utf8(&hdr[18..24])?;
        let mode = u32::from_str_radix(mode_str, 8)?;
        let namesize_str = std::str::from_utf8(&hdr[59..65])?;
        let namesize = usize::from_str_radix(namesize_str, 8)?;
        let filesize_str = std::str::from_utf8(&hdr[65..76])?;
        let filesize = usize::from_str_radix(filesize_str, 8)?;

        if namesize == 0 || namesize > 4096 {
            return Err(anyhow!("Implausible CPIO namesize: {namesize}"));
        }

        let mut name_buf = vec![0u8; namesize];
        cursor.read_exact(&mut name_buf)?;
        let raw_name = String::from_utf8_lossy(&name_buf)
            .trim_end_matches('\0')
            .to_string();

        if raw_name == TRAILER_NAME {
            break;
        }

        let mut file_data = vec![0u8; filesize];
        cursor.read_exact(&mut file_data)?;

        // Sanitize relative path (reject '..' and '/')
        let clean_rel: PathBuf = Path::new(&raw_name)
            .components()
            .filter_map(|c| match c {
                Component::Normal(s) => Some(s),
                _ => None,
            })
            .collect();

        if clean_rel.as_os_str().is_empty() {
            continue;
        }

        let is_dir = (mode & 0o170_000) == 0o040_000;
        let target_path = unique_dest_path(&dest_dir.join(&clean_rel));
        if is_dir {
            fs::create_dir_all(&target_path)?;
        } else {
            if let Some(parent) = target_path.parent() {
                fs::create_dir_all(parent)?;
            }
            let mut f = File::create(&target_path)?;
            f.write_all(&file_data)?;
            saved_files.push(target_path);
        }
    }

    Ok(saved_files)
}

fn unique_dest_path(path: &Path) -> PathBuf {
    if !path.exists() {
        return path.to_path_buf();
    }
    let parent = path.parent().unwrap_or_else(|| Path::new("."));
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("file");
    let ext = path.extension().and_then(|s| s.to_str());
    for counter in 1..10_000 {
        let candidate_name = match ext {
            Some(e) => format!("{stem} ({counter}).{e}"),
            None => format!("{stem} ({counter})"),
        };
        let candidate = parent.join(candidate_name);
        if !candidate.exists() {
            return candidate;
        }
    }
    path.to_path_buf()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpio_gz_pack_and_extract_roundtrip() {
        let tmp = std::env::temp_dir().join(format!("aethershare_cpio_test_{}", std::process::id()));
        let src_dir = tmp.join("src");
        let dst_dir = tmp.join("dst");
        fs::create_dir_all(&src_dir).unwrap();

        let f1 = src_dir.join("hello.txt");
        fs::write(&f1, b"Hello from AetherShare AirDrop!").unwrap();

        let archive = create_cpio_gz(&[f1]).unwrap();
        let extracted = extract_cpio_gz(&archive, &dst_dir).unwrap();
        assert_eq!(extracted.len(), 1);
        assert_eq!(
            fs::read_to_string(&extracted[0]).unwrap(),
            "Hello from AetherShare AirDrop!"
        );

        let _ = fs::remove_dir_all(&tmp);
    }
}
