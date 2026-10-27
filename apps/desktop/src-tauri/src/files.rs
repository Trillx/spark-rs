//! Attachments on disk: where opened ones go, safe file names, and what
//! spark-rs refuses to open.

use std::fs;
use std::path::{Path, PathBuf};

use serde::Serialize;

/// A file the user picked or dropped to attach.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LocalFile {
    pub path: String,
    pub name: String,
    pub size: u64,
}

pub fn describe(path: &Path) -> Option<LocalFile> {
    let meta = fs::metadata(path).ok()?;
    if !meta.is_file() {
        return None;
    }
    Some(LocalFile {
        path: path.to_string_lossy().into_owned(),
        name: path.file_name()?.to_string_lossy().into_owned(),
        size: meta.len(),
    })
}

/// Where opened attachments are written, one folder per attachment. It's
/// emptied when the app starts.
pub fn opened_dir() -> PathBuf {
    std::env::temp_dir().join("spark-rs").join("opened")
}

pub fn clear_opened() {
    // Files a viewer still holds open stay; that's fine.
    let _ = fs::remove_dir_all(opened_dir());
}

/// Extensions that run code when opened. Those attachments open only after
/// the user says Open Anyway.
const RISKY: &[&str] = &[
    "exe",
    "com",
    "scr",
    "pif",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "hta",
    "msi",
    "msp",
    "msc",
    "cpl",
    "jar",
    "reg",
    "lnk",
    "url",
    "scf",
    "inf",
    "application",
    "appref-ms",
    "gadget",
    "iso",
    "img",
    "vhd",
    "vhdx",
];

pub fn is_risky(name: &str) -> bool {
    let ext = name
        .trim_end_matches(['.', ' '])
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .unwrap_or_default();
    RISKY.contains(&ext.as_str())
}

/// A name that's safe as one file name on Windows: no folders, reserved
/// characters or device names.
pub fn safe_file_name(name: &str) -> String {
    let cleaned: String = name
        .chars()
        .map(|c| match c {
            '<' | '>' | ':' | '"' | '/' | '\\' | '|' | '?' | '*' => '_',
            c if c.is_control() => '_',
            c => c,
        })
        .collect();
    let cleaned = cleaned.trim().trim_end_matches(['.', ' ']).to_owned();
    let stem = cleaned
        .split('.')
        .next()
        .unwrap_or_default()
        .to_ascii_uppercase();
    const DEVICES: &[&str] = &[
        "CON", "PRN", "AUX", "NUL", "COM1", "COM2", "COM3", "COM4", "COM5", "COM6", "COM7", "COM8",
        "COM9", "LPT1", "LPT2", "LPT3", "LPT4", "LPT5", "LPT6", "LPT7", "LPT8", "LPT9",
    ];
    if cleaned.is_empty() || cleaned.chars().all(|c| c == '.') {
        "attachment".into()
    } else if DEVICES.contains(&stem.as_str()) {
        format!("_{cleaned}")
    } else {
        cleaned
    }
}

/// `dir/name`, or `dir/name (2).ext` and so on if that's taken.
pub fn unique_path(dir: &Path, name: &str) -> PathBuf {
    let first = dir.join(name);
    if !first.exists() {
        return first;
    }
    let (stem, ext) = match name.rsplit_once('.') {
        Some((stem, ext)) if !stem.is_empty() => (stem, format!(".{ext}")),
        _ => (name, String::new()),
    };
    (2..)
        .map(|n| dir.join(format!("{stem} ({n}){ext}")))
        .find(|p| !p.exists())
        .expect("some name is free")
}

/// Writes a downloaded file and marks it as coming from the internet, so
/// Windows and Office treat it with the usual care (Protected View).
pub fn write_downloaded(path: &Path, bytes: &[u8]) -> Result<(), String> {
    fs::write(path, bytes).map_err(|e| format!("Couldn't save {}: {e}", path.display()))?;
    mark_from_internet(path);
    Ok(())
}

#[cfg(windows)]
fn mark_from_internet(path: &Path) {
    let mut stream = path.as_os_str().to_owned();
    stream.push(":Zone.Identifier");
    // Only NTFS has alternate streams; elsewhere the file stays unmarked.
    let _ = fs::write(stream, "[ZoneTransfer]\r\nZoneId=3\r\n");
}

#[cfg(not(windows))]
fn mark_from_internet(_path: &Path) {}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_lose_folders_reserved_characters_and_devices() {
        assert_eq!(safe_file_name("..\\..\\evil.pdf"), ".._.._evil.pdf");
        assert_eq!(safe_file_name("a/b:c?.pdf"), "a_b_c_.pdf");
        assert_eq!(safe_file_name("report.pdf. . "), "report.pdf");
        assert_eq!(safe_file_name("CON.txt"), "_CON.txt");
        assert_eq!(safe_file_name("..."), "attachment");
        assert_eq!(safe_file_name("Quote 42 [v2].pdf"), "Quote 42 [v2].pdf");
    }

    #[test]
    fn programs_are_risky_documents_are_not() {
        assert!(is_risky("invoice.pdf.exe"));
        assert!(is_risky("RUN.JS"));
        assert!(is_risky("setup.msi. "));
        assert!(!is_risky("invoice.pdf"));
        assert!(!is_risky("drawing.dxf"));
        assert!(!is_risky("exe"));
    }

    #[test]
    fn taken_names_get_a_number() {
        let dir = std::env::temp_dir().join(format!("spark-files-test-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        assert_eq!(unique_path(&dir, "a.pdf"), dir.join("a.pdf"));
        fs::write(dir.join("a.pdf"), b"1").unwrap();
        fs::write(dir.join("a (2).pdf"), b"2").unwrap();
        assert_eq!(unique_path(&dir, "a.pdf"), dir.join("a (3).pdf"));
        fs::write(dir.join("README"), b"").unwrap();
        assert_eq!(unique_path(&dir, "README"), dir.join("README (2)"));
        fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn downloads_are_written_and_described() {
        let dir = std::env::temp_dir().join(format!("spark-files-write-{}", std::process::id()));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("x.pdf");
        write_downloaded(&path, b"%PDF").unwrap();
        let described = describe(&path).unwrap();
        assert_eq!((described.name.as_str(), described.size), ("x.pdf", 4));
        assert!(describe(&dir).is_none(), "folders can't be attached");
        fs::remove_dir_all(&dir).unwrap();
    }
}
