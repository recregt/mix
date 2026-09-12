use std::io::{Cursor, Read};
use std::path::Path;

use mix_core::{Error, Result};
use sha2::{Digest, Sha256};

use crate::pins::{TarballPin, pin_for};

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
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

pub async fn bytes(mirror: Option<&str>) -> Result<Vec<u8>> {
    if let Some(bytes) = embedded() {
        return Ok(bytes.to_vec());
    }

    let pin = host_pin()?;
    let url = match filter_mirror(mirror) {
        Some(base) => mirror_url(base, pin_filename(pin)),
        None => pin.url.to_string(),
    };

    fetch_and_verify(&url, pin.sha256).await
}

fn filter_mirror(value: Option<&str>) -> Option<&str> {
    value.map(str::trim).filter(|s| !s.is_empty())
}

fn mirror_url(base: &str, filename: &str) -> String {
    format!("{}/{filename}", base.trim().trim_end_matches('/'))
}

async fn fetch_and_verify(url: &str, expected_sha256: &str) -> Result<Vec<u8>> {
    tracing::info!(
        "fetching runtime archive: {url} (nix {})",
        crate::pins::NIX_VERSION
    );
    let response = reqwest::get(url)
        .await
        .and_then(reqwest::Response::error_for_status)
        .map_err(|e| Error::Network(e.to_string()))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Network(e.to_string()))?
        .to_vec();

    let digest = sha256_hex(&bytes);
    if digest != expected_sha256 {
        return Err(Error::Integrity {
            artifact: url.to_string(),
            detail: format!("sha256 was {digest}, expected {expected_sha256} (pin in src/pins.rs)"),
        });
    }

    Ok(bytes)
}

pub fn sha256_hex(bytes: &[u8]) -> String {
    const HEX: [u8; 16] = *b"0123456789abcdef";

    let digest = Sha256::digest(bytes);
    let mut out = Vec::with_capacity(digest.len() * 2);
    for byte in digest {
        out.push(HEX[usize::from(byte >> 4)]);
        out.push(HEX[usize::from(byte & 0x0f)]);
    }
    String::from_utf8(out).expect("hex digits are always valid UTF-8")
}

struct XzSource<'a> {
    reader: liblzma::bufread::XzDecoder<Cursor<&'a [u8]>>,
    error: Option<String>,
}

impl Read for XzSource<'_> {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        match self.reader.read(buf) {
            Ok(read) => Ok(read),
            Err(e) => {
                self.error = Some(e.to_string());
                Err(e)
            }
        }
    }
}

pub fn unpack(tarball: &[u8], dest: &Path) -> Result<()> {
    tracing::debug!("unpacking archive into {}", dest.display());
    let source = XzSource {
        reader: liblzma::bufread::XzDecoder::new_multi_decoder(Cursor::new(tarball)),
        error: None,
    };

    let mut archive = tar::Archive::new(source);
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    archive.set_unpack_xattrs(true);
    let unpacked = archive.unpack(dest);

    let mut source = archive.into_inner();
    if unpacked.is_ok() {
        let _ = std::io::copy(&mut source, &mut std::io::sink());
    }

    if let Some(detail) = source.error {
        return Err(Error::Decompression(detail));
    }

    unpacked.map_err(|e| Error::Io {
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

        let bytes = fetch_and_verify(&server.url(), &digest).await.unwrap();
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

        let err = fetch_and_verify(&server.url(), &"0".repeat(64))
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

        let err = fetch_and_verify(&server.url(), &"0".repeat(64))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Network(_)));
    }

    #[tokio::test]
    async fn fetch_and_verify_reports_a_connection_failure() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        drop(listener);

        let err = fetch_and_verify(&format!("http://{addr}/"), &"0".repeat(64))
            .await
            .unwrap_err();
        assert!(matches!(err, Error::Network(_)));
    }

    #[test]
    fn mirror_url_joins_base_and_filename() {
        assert_eq!(
            mirror_url("http://mirror.internal", "nix-2.35.2-x86_64-linux.tar.xz"),
            "http://mirror.internal/nix-2.35.2-x86_64-linux.tar.xz"
        );
    }

    #[test]
    fn mirror_url_trims_a_trailing_slash_on_the_base() {
        assert_eq!(
            mirror_url("http://mirror.internal/", "nix-2.35.2-x86_64-linux.tar.xz"),
            "http://mirror.internal/nix-2.35.2-x86_64-linux.tar.xz"
        );
    }

    #[test]
    fn mirror_url_trims_surrounding_whitespace_on_the_base() {
        assert_eq!(
            mirror_url(
                " http://mirror.internal/ ",
                "nix-2.35.2-x86_64-linux.tar.xz"
            ),
            "http://mirror.internal/nix-2.35.2-x86_64-linux.tar.xz"
        );
    }

    #[test]
    fn filter_mirror_none_when_unset() {
        assert_eq!(filter_mirror(None), None);
    }

    #[test]
    fn filter_mirror_none_when_empty_string() {
        assert_eq!(filter_mirror(Some("")), None);
    }

    #[test]
    fn filter_mirror_none_when_whitespace_only() {
        assert_eq!(filter_mirror(Some("   ")), None);
    }

    #[test]
    fn filter_mirror_some_when_a_real_url_is_set() {
        assert_eq!(
            filter_mirror(Some("http://mirror.internal")),
            Some("http://mirror.internal")
        );
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
    fn pin_for_returns_none_for_an_unknown_target() {
        assert!(pin_for("sparc64-solaris").is_none());
    }

    #[test]
    fn every_pin_has_a_well_formed_url_and_digest() {
        for pin in crate::pins::NIX_TARBALLS {
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
        assert!(matches!(err, Error::Io { .. }));
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
