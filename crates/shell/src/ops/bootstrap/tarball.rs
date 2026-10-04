#![allow(clippy::disallowed_methods)]

use mix_core::{DownloadProgress, Error as CoreError};
use mix_exec::Scope;
use sha2::{Digest, Sha256};
use std::borrow::Cow;
use std::io::{Cursor, Read};
use std::path::Path;

use crate::effect::mirror::{filter_mirror, mirror_url};
use crate::ops::bootstrap::error::{Error, Result};
use mix_pins::{TarballPin, pin_for};

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
    scope: &Scope,
) -> Result<Cow<'static, [u8]>> {
    if let Some(bytes) = embedded() {
        return Ok(Cow::Borrowed(bytes));
    }

    let pin = host_pin()?;
    let url = match filter_mirror(mirror) {
        Some(base) => mirror_url(base, pin_filename(pin)),
        None => pin.url.to_string(),
    };

    fetch_and_verify(&url, pin.sha256, pin.size, progress, scope)
        .await
        .map(Cow::Owned)
}

fn network_error(e: reqwest::Error) -> Error {
    Error::Network(Box::new(e))
}

pub(crate) async fn fetch_and_verify(
    url: &str,
    expected_sha256: &str,
    size: u64,
    progress: &dyn DownloadProgress,
    scope: &Scope,
) -> Result<Vec<u8>> {
    progress.fetching(url);
    scope
        .guard(download(url, expected_sha256, size, progress))
        .await
        .map_err(|_| Error::Interrupted)?
}

async fn download(
    url: &str,
    expected_sha256: &str,
    size: u64,
    progress: &dyn DownloadProgress,
) -> Result<Vec<u8>> {
    let mut response = reqwest::get(url)
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(network_error)?;

    let size_mismatch = |found: String| Error::Integrity {
        artifact: url.to_string(),
        detail: format!("{found} bytes, expected exactly {size} (pin in crates/pins)"),
    };
    if let Some(len) = response.content_length()
        && len != size
    {
        return Err(size_mismatch(len.to_string()));
    }
    progress.set_total(size);

    let mut bytes = Vec::with_capacity(usize::try_from(size).unwrap_or(0));
    let mut hasher = Sha256::new();
    while let Some(chunk) = response.chunk().await.map_err(network_error)? {
        if (bytes.len() + chunk.len()) as u64 > size {
            return Err(size_mismatch("more than that".to_string()));
        }
        hasher.update(&chunk);
        bytes.extend_from_slice(&chunk);
        progress.add(chunk.len() as u64);
    }

    if bytes.len() as u64 != size {
        return Err(size_mismatch(bytes.len().to_string()));
    }
    let digest = hex(&hasher.finalize());
    if digest != expected_sha256 {
        return Err(Error::Integrity {
            artifact: url.to_string(),
            detail: format!("sha256 was {digest}, expected {expected_sha256} (pin in crates/pins)"),
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
#[allow(clippy::disallowed_methods)]
mod tests {
    use super::*;
    use mockito::Server;

    fn make_xz_tarball(files: &[(&str, &[u8])]) -> Vec<u8> {
        let src = tempfile::tempdir().unwrap();
        for (name, contents) in files {
            std::fs::write(src.path().join(name), contents).unwrap();
        }

        tar_of(src.path())
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

        let bytes = fetch_and_verify(
            &server.url(),
            &digest,
            body.len() as u64,
            &mix_core::NoopProgress,
            &Scope::root(),
        )
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

        let err = fetch_and_verify(
            &server.url(),
            &"0".repeat(64),
            9,
            &mix_core::NoopProgress,
            &Scope::root(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Integrity { .. }));
    }

    async fn serve(body: Vec<u8>, content_length: bool) -> std::net::SocketAddr {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            use std::io::{BufRead as _, Write as _};
            if let Ok((mut conn, _)) = listener.accept() {
                let mut request = std::io::BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                while request.read_line(&mut line).is_ok_and(|read| read > 2) {
                    line.clear();
                }
                let head = if content_length {
                    format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len())
                } else {
                    "HTTP/1.1 200 OK\r\nConnection: close\r\n\r\n".to_string()
                };
                let _ = conn.write_all(head.as_bytes());
                let _ = conn.write_all(&body);
            }
        });
        addr
    }

    #[tokio::test]
    async fn a_download_announcing_another_size_than_its_pin_is_refused_at_once() {
        let body = vec![0u8; 64];
        let addr = serve(body.clone(), true).await;

        let err = fetch_and_verify(
            &format!("http://{addr}/"),
            &sha256_hex(&body),
            8,
            &mix_core::NoopProgress,
            &Scope::root(),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, Error::Integrity { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_download_longer_than_its_pin_is_cut_off() {
        let body = vec![0u8; 64];
        let addr = serve(body.clone(), false).await;

        let err = fetch_and_verify(
            &format!("http://{addr}/"),
            &sha256_hex(&body),
            8,
            &mix_core::NoopProgress,
            &Scope::root(),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, Error::Integrity { .. }), "{err:?}");
    }

    #[tokio::test]
    async fn a_download_shorter_than_its_pin_is_refused() {
        let body = vec![0u8; 8];
        let addr = serve(body.clone(), false).await;

        let err = fetch_and_verify(
            &format!("http://{addr}/"),
            &sha256_hex(&body),
            64,
            &mix_core::NoopProgress,
            &Scope::root(),
        )
        .await
        .unwrap_err();

        assert!(matches!(err, Error::Integrity { .. }), "{err:?}");
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

        let err = fetch_and_verify(
            &server.url(),
            &"0".repeat(64),
            16,
            &mix_core::NoopProgress,
            &Scope::root(),
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Network(_)));
    }

    #[tokio::test(start_paused = true)]
    async fn fetch_and_verify_waits_out_a_server_silent_for_an_hour() {
        let body = b"slow but steady".to_vec();
        let expected = sha256_hex(&body);
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (fell_silent, silent) = tokio::sync::oneshot::channel();
        let (resume, resumed) = std::sync::mpsc::channel::<()>();
        std::thread::spawn(move || {
            use std::io::{BufRead as _, Write as _};
            if let Ok((mut conn, _)) = listener.accept() {
                let mut request = std::io::BufReader::new(conn.try_clone().unwrap());
                let mut line = String::new();
                while request.read_line(&mut line).is_ok_and(|read| read > 2) {
                    line.clear();
                }
                let head = format!("HTTP/1.1 200 OK\r\nContent-Length: {}\r\n\r\n", body.len());
                let (first, rest) = body.split_at(5);
                let _ = conn.write_all(head.as_bytes());
                let _ = conn.write_all(first);
                let _ = fell_silent.send(());
                if resumed.recv().is_ok() {
                    let _ = conn.write_all(rest);
                }
            }
        });
        let an_hour_later = async {
            silent.await.unwrap();
            tokio::time::sleep(std::time::Duration::from_secs(60 * 60)).await;
            resume.send(()).unwrap();
        };

        let url = format!("http://{addr}/");
        let scope = Scope::root();
        let (bytes, ()) = tokio::join!(
            fetch_and_verify(&url, &expected, 15, &mix_core::NoopProgress, &scope),
            an_hour_later
        );

        assert_eq!(bytes.unwrap(), b"slow but steady");
    }

    #[tokio::test]
    async fn a_cancelled_request_stops_waiting_for_a_silent_server() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        let (accepted, wait_for_accept) = tokio::sync::oneshot::channel();
        std::thread::spawn(move || {
            let conn = listener.accept();
            let _ = accepted.send(());
            std::thread::park();
            drop(conn);
        });
        let scope = Scope::root();
        let cancelling = scope.clone();
        tokio::spawn(async move {
            if wait_for_accept.await.is_ok() {
                cancelling.cancel(mix_exec::Reason::Interrupted);
            }
        });

        let err = fetch_and_verify(
            &format!("http://{addr}/"),
            &"0".repeat(64),
            1,
            &mix_core::NoopProgress,
            &scope,
        )
        .await
        .unwrap_err();
        assert!(matches!(err, Error::Interrupted));
    }

    #[tokio::test]
    async fn fetch_and_verify_reports_a_connection_failure() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let err = fetch_and_verify(
            &format!("http://{addr}/"),
            &"0".repeat(64),
            1,
            &mix_core::NoopProgress,
            &Scope::root(),
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

    fn tar_of(dir: &std::path::Path) -> Vec<u8> {
        let mut archive = tar::Builder::new(Vec::new());
        archive.append_dir_all(".", dir).unwrap();
        xz_compress(&archive.into_inner().unwrap())
    }

    fn xz_compress(bytes: &[u8]) -> Vec<u8> {
        use std::io::Write as _;
        let mut encoder = liblzma::write::XzEncoder::new(Vec::new(), 6);
        encoder.write_all(bytes).unwrap();
        encoder.finish().unwrap()
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

        let tarball = tar_of(src.path());

        let dest = tempfile::tempdir().unwrap();
        unpack(&tarball, dest.path()).unwrap();

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
