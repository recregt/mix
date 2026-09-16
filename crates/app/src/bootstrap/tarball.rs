use std::borrow::Cow;
use std::io::{Cursor, Read};
use std::path::Path;
use std::time::Duration;

use mix_core::{DownloadProgress, Error as CoreError};
use sha2::{Digest, Sha256};

use crate::bootstrap::error::{Error, Result};
use crate::bootstrap::mirror::{filter_mirror, mirror_url};
use crate::bootstrap::pins::{TarballPin, pin_for};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);
const READ_TIMEOUT: Duration = Duration::from_secs(30);
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
const MAX_DOWNLOAD_BYTES: usize = 1024 * 1024 * 1024;

#[cfg(feature = "embed-tarball")]
pub fn embedded() -> Option<&'static [u8]> {
    Some(include_bytes!(env!("MIX_NIX_TARBALL_PATH")))
}

#[cfg(not(feature = "embed-tarball"))]
pub fn embedded() -> Option<&'static [u8]> {
    None
}

pub fn host_pin() -> Result<&'static TarballPin> {
    let key = host_target_key();
    pin_for(&key).ok_or(Error::UnsupportedTarget(key))
}

fn pin_filename(pin: &TarballPin) -> &'static str {
    pin.url.rsplit('/').next().unwrap_or(pin.url)
}

fn host_target_key() -> String {
    match (
        mix_core::system::Arch::current(),
        mix_core::system::Os::current(),
    ) {
        (Some(arch), Some(os)) => format!("{arch}-{os}"),
        _ => format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS),
    }
}

pub async fn bytes(
    mirror: Option<&str>,
    progress: &dyn DownloadProgress,
) -> Result<Cow<'static, [u8]>> {
    if let Some(bytes) = embedded() {
        return Ok(Cow::Borrowed(bytes));
    }

    let pin = host_pin()?;
    let url = match filter_mirror(mirror) {
        Some(base) => mirror_url(base, pin_filename(pin)),
        None => pin.url.to_string(),
    };

    fetch_and_verify(&url, pin.sha256, progress)
        .await
        .map(Cow::Owned)
}

fn network_error(e: reqwest::Error) -> Error {
    Error::Network(Box::new(e))
}

async fn fetch_and_verify(
    url: &str,
    expected_sha256: &str,
    progress: &dyn DownloadProgress,
) -> Result<Vec<u8>> {
    tracing::info!(
        "fetching runtime archive: {url} (nix {})",
        crate::bootstrap::pins::NIX_VERSION
    );
    fetch_and_verify_with_limits(
        url,
        expected_sha256,
        CONNECT_TIMEOUT,
        READ_TIMEOUT,
        REQUEST_TIMEOUT,
        MAX_DOWNLOAD_BYTES,
        progress,
    )
    .await
}

#[tracing::instrument(
    level = "info",
    name = "download",
    skip_all,
    fields(name = "fetch the Nix runtime archive")
)]
async fn fetch_and_verify_with_limits(
    url: &str,
    expected_sha256: &str,
    connect_timeout: Duration,
    read_timeout: Duration,
    request_timeout: Duration,
    max_bytes: usize,
    progress: &dyn DownloadProgress,
) -> Result<Vec<u8>> {
    let client = reqwest::Client::builder()
        .connect_timeout(connect_timeout)
        .read_timeout(read_timeout)
        .timeout(request_timeout)
        .build()
        .map_err(network_error)?;

    let mut response = client
        .get(url)
        .send()
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(network_error)?;

    let content_length = response.content_length();
    if let Some(len) = content_length {
        progress.set_total(len);
    }

    let mut bytes = match content_length {
        Some(len) => Vec::with_capacity(len.min(max_bytes as u64) as usize),
        None => Vec::new(),
    };
    let mut hasher = Sha256::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if bytes.len() + chunk.len() > max_bytes {
            return Err(Error::Integrity {
                artifact: url.to_string(),
                detail: format!("download exceeded the {max_bytes}-byte limit"),
            });
        }
        hasher.update(&chunk);
        bytes.extend_from_slice(&chunk);
        progress.add(chunk.len() as u64);
    }

    let digest = hex(&hasher.finalize());
    if digest != expected_sha256 {
        return Err(Error::Integrity {
            artifact: url.to_string(),
            detail: format!("sha256 was {digest}, expected {expected_sha256} (pin in src/pins.rs)"),
        });
    }

    Ok(bytes)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    hex(&Sha256::digest(bytes))
}

fn hex(digest: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";

    let mut out = Vec::with_capacity(digest.len() * 2);
    for &byte in digest {
        out.push(HEX[usize::from(byte >> 4)]);
        out.push(HEX[usize::from(byte & 0x0f)]);
    }
    String::from_utf8(out).expect("hex digits are always valid UTF-8")
}

const SKIP_BUF_BYTES: usize = 32 * 1024;

struct XzSource<'a> {
    reader: liblzma::bufread::XzDecoder<Cursor<&'a [u8]>>,
    error: Option<String>,
    pos: u64,
    skip_buf: Vec<u8>,
}

impl<'a> XzSource<'a> {
    fn new(tarball: &'a [u8]) -> Self {
        Self {
            reader: liblzma::bufread::XzDecoder::new_stream(Cursor::new(tarball), decoder_stream()),
            error: None,
            pos: 0,
            skip_buf: Vec::new(),
        }
    }

    fn discard(&mut self, mut amt: u64) -> std::io::Result<()> {
        if amt == 0 {
            return Ok(());
        }
        if self.skip_buf.is_empty() {
            self.skip_buf = vec![0; SKIP_BUF_BYTES];
        }
        while amt > 0 {
            let want = amt.min(self.skip_buf.len() as u64) as usize;
            let read = match self.reader.read(&mut self.skip_buf[..want]) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::UnexpectedEof,
                        "unexpected end of archive while skipping entry padding",
                    ));
                }
                Ok(read) => read,
                Err(e) => {
                    self.error = Some(e.to_string());
                    return Err(e);
                }
            };
            self.pos += read as u64;
            amt -= read as u64;
        }
        Ok(())
    }
}

impl Read for XzSource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.reader.read(buf) {
            Ok(read) => {
                self.pos += read as u64;
                Ok(read)
            }
            Err(e) => {
                self.error = Some(e.to_string());
                Err(e)
            }
        }
    }
}

// `tar` only skips forward, and only ever to the start of the next header. Implementing that as a
// `Seek` lets `entries_with_seek` take over the skipping: `tar`'s read-based path zeroes a fresh
// 32 KiB stack buffer for every entry, whether or not there is anything to skip.
impl std::io::Seek for XzSource<'_> {
    fn seek(&mut self, pos: std::io::SeekFrom) -> std::io::Result<u64> {
        let target = match pos {
            std::io::SeekFrom::Current(delta) => self.pos.checked_add_signed(delta),
            std::io::SeekFrom::Start(offset) => Some(offset),
            std::io::SeekFrom::End(_) => None,
        };
        let target = target.filter(|target| *target >= self.pos).ok_or_else(|| {
            std::io::Error::new(
                std::io::ErrorKind::Unsupported,
                "the archive stream can only be advanced forward",
            )
        })?;

        self.discard(target - self.pos)?;
        Ok(self.pos)
    }
}

// Every archive reaching `unpack` is already verified byte for byte: downloads against the pinned
// sha256, the embedded tarball at build time. `IGNORE_CHECK` drops xz's own per-block integrity
// check, which would hash the same bytes a second time; stream and index headers are still checked.
fn decoder_stream() -> liblzma::stream::Stream {
    liblzma::stream::Stream::new_auto_decoder(
        u64::MAX,
        liblzma::stream::CONCATENATED | liblzma::stream::IGNORE_CHECK,
    )
    .expect("the xz decoder flags are valid")
}

// `tar::Archive::unpack`, except that it iterates with `entries_with_seek` so that entry padding is
// skipped through `XzSource`'s `Seek` rather than through `tar`'s zero-a-32-KiB-buffer path.
fn unpack_entries(archive: &mut tar::Archive<XzSource<'_>>, dest: &Path) -> std::io::Result<()> {
    if dest.symlink_metadata().is_err() {
        std::fs::create_dir_all(dest)?;
    }
    let dest = dest.canonicalize().unwrap_or_else(|_| dest.to_path_buf());

    // Directories are applied last, deepest first, so that restrictive permissions on a directory
    // cannot stop its own descendants from being created.
    let mut directories = Vec::new();
    for entry in archive.entries_with_seek()? {
        let mut entry = entry?;
        if entry.header().entry_type() == tar::EntryType::Directory {
            directories.push(entry);
        } else {
            entry.unpack_in(&dest)?;
        }
    }

    directories.sort_by(|a, b| b.path_bytes().cmp(&a.path_bytes()));
    for mut directory in directories {
        directory.unpack_in(&dest)?;
    }

    Ok(())
}

pub fn unpack(tarball: &[u8], dest: &Path) -> Result<()> {
    tracing::debug!("unpacking archive into {}", dest.display());
    let mut archive = tar::Archive::new(XzSource::new(tarball));
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    archive.set_unpack_xattrs(true);
    let unpacked = unpack_entries(&mut archive, dest);

    let mut source = archive.into_inner();
    if unpacked.is_ok() {
        let _ = std::io::copy(&mut source, &mut std::io::sink());
    }

    if let Some(detail) = source.error {
        return Err(Error::Decompression(detail));
    }

    unpacked.map_err(|e| CoreError::Io {
        path: dest.to_path_buf(),
        source: e,
    })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use mockito::Server;

    fn make_xz_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        let src = tempfile::tempdir().unwrap();
        for (name, contents) in files {
            std::fs::write(src.path().join(name), contents).unwrap();
        }

        let output = std::process::Command::new("tar")
            .args(["cJf", "-", "-C"])
            .arg(src.path())
            .arg(".")
            .output()
            .expect("tar must be on PATH to build test fixtures");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        output.stdout
    }

    #[test]
    fn unpack_extracts_files_to_dest() {
        let tarball = make_xz_tarball(&[("hello.txt", b"world")]);
        let dest = tempfile::tempdir().unwrap();

        unpack(&tarball, dest.path()).unwrap();

        assert_eq!(
            std::fs::read(dest.path().join("hello.txt")).unwrap(),
            b"world"
        );
    }

    #[test]
    fn unpack_rejects_a_non_xz_payload() {
        let dest = tempfile::tempdir().unwrap();
        let err = unpack(b"not an xz stream", dest.path()).unwrap_err();
        assert!(matches!(err, Error::Decompression(_)));
    }

    #[test]
    fn unpack_rejects_a_corrupt_xz_stream_tail() {
        let mut tarball = make_xz_tarball(&[("hello.txt", b"world")]);
        let last = tarball.len() - 1;
        tarball[last] ^= 0xff;
        let dest = tempfile::tempdir().unwrap();

        let err = unpack(&tarball, dest.path()).unwrap_err();
        assert!(matches!(err, Error::Decompression(_)));
    }

    #[test]
    fn unpack_rejects_a_truncated_xz_stream() {
        let tarball = make_xz_tarball(&[("hello.txt", b"world")]);
        let dest = tempfile::tempdir().unwrap();

        let err = unpack(&tarball[..tarball.len() / 2], dest.path()).unwrap_err();
        assert!(matches!(err, Error::Decompression(_)));
    }

    #[test]
    fn sha256_hex_matches_a_known_digest() {
        assert_eq!(
            sha256_hex(b""),
            "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
        );
    }

    #[tokio::test]
    async fn fetch_and_verify_succeeds_when_digest_matches() {
        let mut server = Server::new_async().await;
        let body = b"hello world".to_vec();
        let digest = sha256_hex(&body);
        let _mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(body.clone())
            .create_async()
            .await;

        let bytes = fetch_and_verify(&server.url(), &digest, &mix_core::NoopProgress)
            .await
            .unwrap();
        assert_eq!(bytes, body);
    }

    #[tokio::test]
    async fn fetch_and_verify_rejects_a_sha256_mismatch() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(b"corrupted")
            .create_async()
            .await;

        let err = fetch_and_verify(&server.url(), &"0".repeat(64), &mix_core::NoopProgress)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Integrity { .. }));
    }

    #[tokio::test]
    async fn fetch_and_verify_aborts_a_download_past_the_byte_limit() {
        let mut server = Server::new_async().await;
        let body = vec![0u8; 64];
        let _mock = server
            .mock("GET", "/")
            .with_status(200)
            .with_body(body.clone())
            .create_async()
            .await;

        let err = fetch_and_verify_with_limits(
            &server.url(),
            &sha256_hex(&body),
            CONNECT_TIMEOUT,
            READ_TIMEOUT,
            REQUEST_TIMEOUT,
            8,
            &mix_core::NoopProgress,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Integrity { .. }));
    }

    #[tokio::test]
    async fn fetch_and_verify_rejects_a_404_instead_of_hashing_the_body() {
        let mut server = Server::new_async().await;
        let _mock = server
            .mock("GET", "/")
            .with_status(404)
            .with_body(b"<html>404</html>")
            .create_async()
            .await;

        let err = fetch_and_verify(&server.url(), &"0".repeat(64), &mix_core::NoopProgress)
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Network(_)));
    }

    #[tokio::test]
    async fn fetch_and_verify_times_out_instead_of_hanging_on_a_silent_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let _conn = listener.accept();
            std::thread::sleep(Duration::from_secs(5));
        });

        let err = fetch_and_verify_with_limits(
            &format!("http://{addr}/"),
            &"0".repeat(64),
            Duration::from_millis(500),
            Duration::from_millis(200),
            Duration::from_secs(10),
            MAX_DOWNLOAD_BYTES,
            &mix_core::NoopProgress,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Network(_)));
    }

    #[tokio::test]
    async fn fetch_and_verify_read_timeout_catches_a_drip_fed_stall() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::Write as _;
            if let Ok((mut conn, _)) = listener.accept() {
                let _ = conn.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\nx");
                std::thread::sleep(Duration::from_secs(5));
            }
        });

        let started = std::time::Instant::now();
        let err = fetch_and_verify_with_limits(
            &format!("http://{addr}/"),
            &"0".repeat(64),
            Duration::from_secs(1),
            Duration::from_millis(200),
            Duration::from_secs(10),
            MAX_DOWNLOAD_BYTES,
            &mix_core::NoopProgress,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Network(_)));
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "read_timeout should catch the stall long before the request timeout"
        );
    }

    #[tokio::test]
    async fn fetch_and_verify_reports_a_connection_failure() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let err = fetch_and_verify(
            &format!("http://{addr}/"),
            &"0".repeat(64),
            &mix_core::NoopProgress,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Network(_)));
    }

    #[test]
    fn pin_filename_extracts_the_last_url_segment() {
        let pin = pin_for("x86_64-linux").unwrap();
        assert_eq!(pin_filename(pin), "nix-2.35.2-x86_64-linux.tar.xz");
    }

    #[test]
    fn pin_for_finds_a_known_target() {
        assert!(pin_for("x86_64-linux").is_some());
    }

    #[test]
    fn host_target_key_matches_a_known_pin_on_this_platform() {
        assert!(pin_for(&host_target_key()).is_some());
    }

    #[test]
    fn pin_for_returns_none_for_an_unknown_target() {
        assert!(pin_for("sparc64-solaris").is_none());
    }

    #[test]
    fn every_pin_has_a_well_formed_url_and_digest() {
        for pin in crate::bootstrap::pins::NIX_TARBALLS {
            assert!(
                pin.url.starts_with("https://"),
                "{}: url {:?} is not https",
                pin.target,
                pin.url
            );
            assert_eq!(
                pin.sha256.len(),
                64,
                "{}: sha256 {:?} is not 64 hex characters",
                pin.target,
                pin.sha256
            );
            assert!(
                pin.sha256
                    .chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{}: sha256 {:?} is not lowercase hex",
                pin.target,
                pin.sha256
            );
        }
    }

    #[test]
    fn the_pinned_flake_inputs_are_full_commit_revisions() {
        for (name, rev) in [
            ("nixpkgs", crate::bootstrap::pins::NIXPKGS_REV),
            ("home-manager", crate::bootstrap::pins::HOME_MANAGER_REV),
        ] {
            assert_eq!(
                rev.len(),
                40,
                "{name}: revision {rev:?} is not a 40 character commit hash"
            );
            assert!(
                rev.chars()
                    .all(|c| c.is_ascii_hexdigit() && !c.is_ascii_uppercase()),
                "{name}: revision {rev:?} is not lowercase hex"
            );
        }
    }

    fn xz_compress(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut child = std::process::Command::new("xz")
            .args(["-z", "-c"])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()
            .expect("xz must be on PATH to build test fixtures");
        child.stdin.take().unwrap().write_all(bytes).unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(output.status.success());
        output.stdout
    }

    #[test]
    fn unpack_rejects_a_valid_xz_stream_with_corrupt_tar_data() {
        let compressed = xz_compress(b"this is not a tar archive");
        let dest = tempfile::tempdir().unwrap();
        let err = unpack(&compressed, dest.path()).unwrap_err();
        assert!(matches!(err, Error::Core(mix_core::Error::Io { .. })));
    }

    #[test]
    fn unpack_restores_unaligned_entries_under_a_read_only_directory() {
        use std::os::unix::fs::PermissionsExt as _;

        let src = tempfile::tempdir().unwrap();
        std::fs::create_dir(src.path().join("locked")).unwrap();
        // Sizes that are not multiples of 512, so every entry is followed by padding that the
        // extractor has to skip before it can read the next header.
        std::fs::write(src.path().join("locked/first"), vec![1u8; 700]).unwrap();
        std::fs::write(src.path().join("locked/second"), vec![2u8; 3]).unwrap();
        std::fs::set_permissions(
            src.path().join("locked"),
            std::fs::Permissions::from_mode(0o500),
        )
        .unwrap();

        let output = std::process::Command::new("tar")
            .args(["cJf", "-", "-C"])
            .arg(src.path())
            .arg(".")
            .output()
            .expect("tar must be on PATH to build test fixtures");
        assert!(output.status.success());

        let dest = tempfile::tempdir().unwrap();
        unpack(&output.stdout, dest.path()).unwrap();

        assert_eq!(
            std::fs::read(dest.path().join("locked/first")).unwrap(),
            vec![1u8; 700]
        );
        assert_eq!(
            std::fs::read(dest.path().join("locked/second")).unwrap(),
            vec![2u8; 3]
        );
        assert_eq!(
            std::fs::metadata(dest.path().join("locked"))
                .unwrap()
                .permissions()
                .mode()
                & 0o777,
            0o500
        );
    }

    #[test]
    fn unpack_does_not_escape_dest_for_a_path_traversal_entry() {
        let mut header = tar::Header::new_gnu();
        {
            let gnu = header.as_gnu_mut().unwrap();
            let name = b"../escape.txt\0";
            gnu.name[..name.len()].copy_from_slice(name);
        }
        let data: &[u8] = b"pwned";
        header.set_size(data.len() as u64);
        header.set_mode(0o644);
        header.set_entry_type(tar::EntryType::Regular);
        header.set_cksum();

        let mut tar_bytes = Vec::new();
        tar_bytes.extend_from_slice(header.as_bytes());
        tar_bytes.extend_from_slice(data);
        let pad = (512 - (tar_bytes.len() % 512)) % 512;
        tar_bytes.extend(std::iter::repeat_n(0u8, pad));
        tar_bytes.extend(std::iter::repeat_n(0u8, 1024));

        let compressed = xz_compress(&tar_bytes);
        let dest = tempfile::tempdir().unwrap();
        let escaped = dest.path().parent().unwrap().join("escape.txt");
        let _ = std::fs::remove_file(&escaped);

        unpack(&compressed, dest.path()).unwrap();

        assert!(!escaped.exists(), "traversal entry must not escape dest");
    }
}
