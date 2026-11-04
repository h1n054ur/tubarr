//! Downloading, checking and swapping a binary into place without ever leaving a half-written file.

use std::{
    io::{self, Write as _},
    os::unix::fs::PermissionsExt,
    path::{Path, PathBuf},
};

use reqwest::Client;
use sha2::{Digest, Sha256};

use super::BinaryError;

/// Streams `url` into `dest`, returning the file's SHA-256 as lowercase hex.
pub async fn download(client: &Client, url: &str, dest: &Path) -> Result<String, BinaryError> {
    let mut resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(BinaryError::Status {
            url: url.to_owned(),
            status: resp.status(),
        });
    }
    let mut file = std::fs::File::create(dest)?;
    let mut hasher = Sha256::new();
    while let Some(chunk) = resp.chunk().await? {
        hasher.update(&chunk);
        file.write_all(&chunk)?;
    }
    file.sync_all()?;
    Ok(hex::encode(hasher.finalize()))
}

pub async fn fetch_text(client: &Client, url: &str) -> Result<String, BinaryError> {
    let resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(BinaryError::Status {
            url: url.to_owned(),
            status: resp.status(),
        });
    }
    Ok(resp.text().await?)
}

/// Finds the hash for `name` in a `sha256sum` style list (`<hex>  <name>`, optional `*` before binary names).
pub fn expected_sha(sums: &str, name: &str) -> Option<String> {
    sums.lines().find_map(|line| {
        let (hash, file) = line.trim().split_once(char::is_whitespace)?;
        let file = file.trim_start().trim_start_matches('*');
        (file == name && hash.len() == 64).then(|| hash.to_ascii_lowercase())
    })
}

/// Pulls the single file `entry` out of a zip into `dest`.
pub fn extract_zip_entry(zip_path: &Path, entry: &str, dest: &Path) -> Result<(), BinaryError> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    let mut src = archive.by_name(entry)?;
    let mut out = std::fs::File::create(dest)?;
    io::copy(&mut src, &mut out)?;
    out.sync_all()?;
    Ok(())
}

pub fn previous_path(target: &Path) -> PathBuf {
    let mut name = target.file_name().unwrap_or_default().to_os_string();
    name.push(".prev");
    target.with_file_name(name)
}

/// Makes `staged` executable and renames it over `target`, keeping the old `target` as `<target>.prev`.
/// The old file is hard linked first, so `target` is never missing.
pub fn swap_in(staged: &Path, target: &Path) -> io::Result<()> {
    std::fs::set_permissions(staged, std::fs::Permissions::from_mode(0o755))?;
    if target.exists() {
        let prev = previous_path(target);
        remove_if_exists(&prev)?;
        if std::fs::hard_link(target, &prev).is_err() {
            std::fs::copy(target, &prev)?;
        }
    }
    std::fs::rename(staged, target)?;
    sync_dir(target)
}

/// Puts `<target>.prev` back over `target`. Returns false when there is nothing to roll back to.
pub fn rollback(target: &Path) -> io::Result<bool> {
    let prev = previous_path(target);
    if !prev.exists() {
        return Ok(false);
    }
    std::fs::rename(&prev, target)?;
    sync_dir(target)?;
    Ok(true)
}

pub fn remove_if_exists(path: &Path) -> io::Result<()> {
    match std::fs::remove_file(path) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Persists the rename itself, so a power cut cannot bring back the old directory entry.
fn sync_dir(target: &Path) -> io::Result<()> {
    match target.parent() {
        Some(dir) => std::fs::File::open(dir)?.sync_all(),
        None => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SUMS: &str = "\
1fa6733c37ea6fb51c99ad8fe785e7b7e5f3246c9b980230329d4fb72ed8d4d6  yt-dlp
66674953fe251b89f4d08c5f0e35e0728679bd67ab3d7d05c0562af101dd3e7a  yt-dlp.exe
072aad4f2a7604e92155f61a275a4752dc64046c8f6d90df3710525d94cd37c1 *yt-dlp_linux
";

    #[test]
    fn finds_exact_name_only() {
        assert_eq!(
            expected_sha(SUMS, "yt-dlp").unwrap(),
            "1fa6733c37ea6fb51c99ad8fe785e7b7e5f3246c9b980230329d4fb72ed8d4d6"
        );
        assert_eq!(
            expected_sha(SUMS, "yt-dlp_linux").unwrap(),
            "072aad4f2a7604e92155f61a275a4752dc64046c8f6d90df3710525d94cd37c1"
        );
        assert_eq!(expected_sha(SUMS, "yt-dlp_macos"), None);
    }

    #[test]
    fn swap_keeps_previous_and_rolls_back() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("tool");
        let staged = dir.path().join(".tool.download");
        std::fs::write(&staged, "one").unwrap();
        swap_in(&staged, &target).unwrap();
        assert!(!previous_path(&target).exists());
        std::fs::write(&staged, "two").unwrap();
        swap_in(&staged, &target).unwrap();
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "two");
        assert_eq!(
            std::fs::read_to_string(previous_path(&target)).unwrap(),
            "one"
        );
        let mode = std::fs::metadata(&target).unwrap().permissions().mode();
        assert_eq!(mode & 0o111, 0o111);
        assert!(rollback(&target).unwrap());
        assert_eq!(std::fs::read_to_string(&target).unwrap(), "one");
        assert!(!rollback(&target).unwrap());
    }
}
