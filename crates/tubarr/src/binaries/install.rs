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
pub async fn download(
    client: &Client,
    url: &str,
    dest: &Path,
    limit: u64,
) -> Result<String, BinaryError> {
    let mut resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(BinaryError::Status {
            url: url.to_owned(),
            status: resp.status(),
        });
    }
    if resp.content_length().is_some_and(|size| size > limit) {
        return Err(limit_error("binary download", limit).into());
    }
    let mut file = std::fs::File::create(dest)?;
    let mut hasher = Sha256::new();
    let mut received = 0;
    while let Some(chunk) = resp.chunk().await? {
        received = checked_size(received, chunk.len(), "binary download", limit)?;
        hasher.update(&chunk);
        file.write_all(&chunk)?;
    }
    file.sync_all()?;
    Ok(hex::encode(hasher.finalize()))
}

pub async fn fetch_text(client: &Client, url: &str, limit: u64) -> Result<String, BinaryError> {
    let mut resp = client.get(url).send().await?;
    if !resp.status().is_success() {
        return Err(BinaryError::Status {
            url: url.to_owned(),
            status: resp.status(),
        });
    }
    if resp.content_length().is_some_and(|size| size > limit) {
        return Err(limit_error("checksum text", limit).into());
    }
    let mut body = Vec::new();
    let mut received = 0;
    while let Some(chunk) = resp.chunk().await? {
        received = checked_size(received, chunk.len(), "checksum text", limit)?;
        body.extend_from_slice(&chunk);
    }
    Ok(String::from_utf8_lossy(&body).into_owned())
}

fn limit_error(boundary: &str, limit: u64) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        format!("{boundary} exceeds limit of {limit} bytes"),
    )
}

fn checked_size(current: u64, additional: usize, boundary: &str, limit: u64) -> io::Result<u64> {
    u64::try_from(additional)
        .ok()
        .and_then(|size| current.checked_add(size))
        .filter(|&size| size <= limit)
        .ok_or_else(|| limit_error(boundary, limit))
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
pub fn extract_zip_entry(
    zip_path: &Path,
    entry: &str,
    dest: &Path,
    limit: u64,
) -> Result<(), BinaryError> {
    let mut archive = zip::ZipArchive::new(std::fs::File::open(zip_path)?)?;
    let mut src = archive.by_name(entry)?;
    if src.size() > limit {
        return Err(limit_error("ZIP extracted entry", limit).into());
    }
    let mut out = std::fs::File::create(dest)?;
    copy_limited(&mut src, &mut out, limit)?;
    out.sync_all()?;
    Ok(())
}

/// Copies at most `limit` bytes, probing for EOF without writing any excess byte.
fn copy_limited<R: io::Read, W: io::Write>(
    src: &mut R,
    dest: &mut W,
    limit: u64,
) -> io::Result<u64> {
    let mut buffer = [0; 8192];
    let mut copied = 0;
    loop {
        let remaining = limit - copied;
        let read_limit = remaining.min(buffer.len() as u64).max(1) as usize;
        let count = match src.read(&mut buffer[..read_limit]) {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => result?,
        };
        if count == 0 {
            return Ok(copied);
        }
        copied = checked_size(copied, count, "ZIP extracted entry", limit)?;
        dest.write_all(&buffer[..count])?;
    }
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
    use axum::{Router, body::Body, http::Response, routing::get};
    use bytes::Bytes;
    use futures_util::{StreamExt as _, stream};
    use std::{convert::Infallible, io::Read as _, time::Duration};

    const SMALL_LIMIT: u64 = 32;

    fn assert_limit_error<T: std::fmt::Debug>(result: Result<T, BinaryError>) {
        let err = result.expect_err("oversized input must be rejected");
        assert!(
            matches!(&err, BinaryError::Io(e) if e.kind() == io::ErrorKind::InvalidData),
            "expected an InvalidData limit error, got {err:?}"
        );
        assert!(
            err.to_string().contains("limit") || err.to_string().contains("exceed"),
            "limit error must explain the rejection: {err}"
        );
    }

    async fn serve_response(response: Response<Body>) -> (String, tokio::task::JoinHandle<()>) {
        let response = std::sync::Arc::new(std::sync::Mutex::new(Some(response)));
        let app = Router::new().route(
            "/asset",
            get(move || {
                let response = response.lock().unwrap().take().unwrap();
                async move { response }
            }),
        );
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let url = format!("http://{}/asset", listener.local_addr().unwrap());
        let task = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        (url, task)
    }

    fn client() -> Client {
        Client::builder().no_proxy().build().unwrap()
    }

    fn chunked(bytes: Vec<u8>, stall: bool) -> Body {
        let first = stream::once(async move { Ok::<_, Infallible>(Bytes::from(bytes)) });
        if stall {
            Body::from_stream(first.chain(stream::pending()))
        } else {
            Body::from_stream(first)
        }
    }

    #[tokio::test]
    async fn oversized_content_length_rejected_before_download_file_creation() {
        let (url, server) = serve_response(
            Response::builder()
                .header("content-length", SMALL_LIMIT + 1)
                .body(Body::from(vec![b'x'; SMALL_LIMIT as usize + 1]))
                .unwrap(),
        )
        .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("asset");
        let result = download(&client(), &url, &dest, SMALL_LIMIT).await;
        server.abort();
        assert!(!dest.exists(), "declared oversize must not create a file");
        assert_limit_error(result);
    }

    #[tokio::test]
    async fn exact_download_cap_is_allowed_with_or_without_content_length() {
        for known in [false, true] {
            let bytes = vec![b'x'; SMALL_LIMIT as usize];
            let expected = hex::encode(Sha256::digest(&bytes));
            let mut response = Response::builder();
            if known {
                response = response.header("content-length", SMALL_LIMIT);
            }
            let (url, server) =
                serve_response(response.body(chunked(bytes.clone(), false)).unwrap()).await;
            let dir = tempfile::tempdir().unwrap();
            let dest = dir.path().join("asset");
            let result = download(&client(), &url, &dest, SMALL_LIMIT).await;
            server.abort();
            assert_eq!(result.unwrap(), expected);
            assert_eq!(std::fs::read(dest).unwrap(), bytes);
        }
    }

    #[tokio::test]
    async fn chunked_download_stops_at_cap_without_waiting_for_eof() {
        let (url, server) = serve_response(Response::new(chunked(
            vec![b'x'; SMALL_LIMIT as usize + 1],
            true,
        )))
        .await;
        let dir = tempfile::tempdir().unwrap();
        let dest = dir.path().join("asset");
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            download(&client(), &url, &dest, SMALL_LIMIT),
        )
        .await;
        server.abort();
        let written = std::fs::metadata(dest).map(|m| m.len()).unwrap_or(0);
        assert!(written <= SMALL_LIMIT, "wrote {written} bytes past cap");
        assert_limit_error(result.expect("must reject before the producer sends EOF"));
    }

    #[tokio::test]
    async fn checksum_content_length_is_rejected_without_waiting_for_body() {
        let (url, server) = serve_response(
            Response::builder()
                .header("content-length", SMALL_LIMIT + 1)
                .body(chunked(vec![b'x'], true))
                .unwrap(),
        )
        .await;
        let result = tokio::time::timeout(
            Duration::from_secs(2),
            fetch_text(&client(), &url, SMALL_LIMIT),
        )
        .await;
        server.abort();
        assert_limit_error(result.expect("reject declared oversize before reading the body"));
    }

    #[tokio::test]
    async fn checksum_text_exact_cap_and_chunked_oversize() {
        for oversize in [false, true] {
            let bytes = vec![b'x'; SMALL_LIMIT as usize + usize::from(oversize)];
            let (url, server) = serve_response(Response::new(chunked(bytes, oversize))).await;
            let result = tokio::time::timeout(
                Duration::from_secs(2),
                fetch_text(&client(), &url, SMALL_LIMIT),
            )
            .await;
            server.abort();
            let result = result.expect("checksum reader must not wait for oversized body EOF");
            if oversize {
                assert_limit_error(result);
            } else {
                assert_eq!(result.unwrap().len(), SMALL_LIMIT as usize);
            }
        }
    }

    struct CountedReader {
        remaining: usize,
        read: usize,
    }

    impl io::Read for CountedReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.remaining.min(buf.len()).min(7);
            buf[..n].fill(b'x');
            self.remaining -= n;
            self.read += n;
            Ok(n)
        }
    }

    #[test]
    fn actual_copy_output_is_bounded_even_when_declared_size_is_small() {
        // A lying archive's decompressor is just a Read. Keep this proof independent
        // of whether a ZIP version itself clips a forged uncompressed-size field.
        let mut src = CountedReader {
            remaining: 4096,
            read: 0,
        };
        let mut dest = Vec::new();
        let result = copy_limited(&mut src, &mut dest, SMALL_LIMIT);
        assert!(
            dest.len() <= SMALL_LIMIT as usize,
            "wrote {} bytes",
            dest.len()
        );
        assert!(
            src.read <= SMALL_LIMIT as usize + 1,
            "drained {} bytes",
            src.read
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::InvalidData);
    }

    #[test]
    fn exact_copy_cap_probes_eof_without_writing_an_extra_byte() {
        for len in [0, SMALL_LIMIT as usize] {
            let mut src = CountedReader {
                remaining: len,
                read: 0,
            };
            let mut dest = Vec::new();
            assert_eq!(
                copy_limited(&mut src, &mut dest, len as u64).unwrap(),
                len as u64
            );
            assert_eq!(dest, vec![b'x'; len]);
        }
        let mut src = io::Cursor::new(b"x");
        let mut dest = Vec::new();
        assert_eq!(
            copy_limited(&mut src, &mut dest, 0).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        assert!(dest.is_empty());
        assert_eq!(src.position(), 1);
    }

    #[test]
    fn copy_preserves_reader_and_writer_errors() {
        struct Broken;
        impl io::Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
                Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "read failed",
                ))
            }
        }
        impl io::Write for Broken {
            fn write(&mut self, _: &[u8]) -> io::Result<usize> {
                Err(io::Error::new(io::ErrorKind::WriteZero, "disk failed"))
            }
            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        assert_eq!(
            copy_limited(&mut Broken, &mut Vec::new(), SMALL_LIMIT)
                .unwrap_err()
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            copy_limited(&mut &b"x"[..], &mut Broken, SMALL_LIMIT)
                .unwrap_err()
                .kind(),
            io::ErrorKind::WriteZero
        );
    }

    fn zip_bytes(bytes: &[u8]) -> Vec<u8> {
        let mut zip = zip::ZipWriter::new(io::Cursor::new(Vec::new()));
        zip.start_file(
            "deno",
            zip::write::SimpleFileOptions::default()
                .compression_method(zip::CompressionMethod::Deflated),
        )
        .unwrap();
        zip.write_all(bytes).unwrap();
        zip.finish().unwrap().into_inner()
    }

    #[test]
    fn zip_reported_size_is_checked_before_entry_destination_creation() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("asset.zip");
        let out = dir.path().join("deno");
        std::fs::write(&zip, zip_bytes(&vec![b'x'; 4096])).unwrap();
        let result = extract_zip_entry(&zip, "deno", &out, SMALL_LIMIT);
        assert!(
            !out.exists(),
            "reported oversize must be rejected before File::create"
        );
        assert_limit_error(result);
    }

    #[test]
    fn exact_zip_entry_cap_is_allowed() {
        let dir = tempfile::tempdir().unwrap();
        let zip = dir.path().join("asset.zip");
        let out = dir.path().join("deno");
        std::fs::write(&zip, zip_bytes(&vec![b'x'; SMALL_LIMIT as usize])).unwrap();
        extract_zip_entry(&zip, "deno", &out, SMALL_LIMIT).unwrap();
        let mut bytes = Vec::new();
        std::fs::File::open(out)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, vec![b'x'; SMALL_LIMIT as usize]);
    }

    #[tokio::test]
    async fn high_expansion_archive_keeps_working_and_previous_files_and_cleans_staging() {
        use super::super::{
            BinaryManager, Tool,
            github::{Asset, Release},
        };
        use crate::config::{Binaries, Channel};

        let body = zip_bytes(&vec![b'x'; 16 * 1024]);
        assert!(body.len() < 1024, "fixture must have high expansion");
        let hash = hex::encode(Sha256::digest(&body));
        let (url, asset_server) = serve_response(Response::new(Body::from(body))).await;
        let mut spec = Tool::Deno.spec(Channel::Stable, "x86_64").unwrap();
        spec.extracted_limit = SMALL_LIMIT;
        let sums = format!("{hash}  {}\n", spec.asset);
        let (sums_url, sums_server) = serve_response(Response::new(Body::from(sums))).await;
        let release = Release {
            tag_name: "v3.0.0".to_owned(),
            assets: vec![
                Asset {
                    name: spec.asset.to_owned(),
                    browser_download_url: url,
                },
                Asset {
                    name: spec.sums.to_owned(),
                    browser_download_url: sums_url,
                },
            ],
        };
        let dir = tempfile::tempdir().unwrap();
        let manager = BinaryManager::new(Binaries {
            bin_dir: dir.path().to_path_buf(),
            ..Binaries::default()
        })
        .unwrap();
        let target = manager.path(Tool::Deno);
        std::fs::write(&target, b"working").unwrap();
        std::fs::write(previous_path(&target), b"older rollback").unwrap();
        let result = manager.install(&spec, &release, &target).await;
        asset_server.abort();
        sums_server.abort();
        assert!(
            std::fs::read(&target).unwrap() == b"working",
            "live file changed"
        );
        assert_eq!(
            std::fs::read(previous_path(&target)).unwrap(),
            b"older rollback"
        );
        assert!(std::fs::read_dir(dir.path()).unwrap().all(|entry| {
            !entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with('.')
        }));
        assert_limit_error(result);
    }

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
