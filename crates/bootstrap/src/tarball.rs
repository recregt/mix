use std::fmt::Write as _;
use std::io::Cursor;
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
    pin_for(&key).ok_or_else(|| {
        Error::Other(format!(
            "mix does not have a pinned Nix release for this target ({key})"
        ))
    })
}

fn host_target_key() -> String {
    format!("{}-{}", std::env::consts::ARCH, std::env::consts::OS)
}

pub async fn bytes() -> Result<Vec<u8>> {
    if let Some(bytes) = embedded() {
        return Ok(bytes.to_vec());
    }

    let pin = host_pin()?;
    tracing::info!(
        url = pin.url,
        version = crate::pins::NIX_VERSION,
        "fetching Nix tarball"
    );
    let response = reqwest::get(pin.url)
        .await
        .map_err(|e| Error::Network(e.to_string()))?;
    let bytes = response
        .bytes()
        .await
        .map_err(|e| Error::Network(e.to_string()))?
        .to_vec();

    let digest = sha256_hex(&bytes);
    if digest != pin.sha256 {
        return Err(Error::Integrity {
            artifact: pin.url.to_string(),
            detail: format!(
                "sha256 was {digest}, expected {} (pin in src/pins.rs)",
                pin.sha256
            ),
        });
    }

    Ok(bytes)
}

fn sha256_hex(bytes: &[u8]) -> String {
    let digest = Sha256::digest(bytes);
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        write!(out, "{byte:02x}").expect("writing to a String never fails");
    }
    out
}

pub fn unpack(tarball: &[u8], dest: &Path) -> Result<()> {
    let mut decompressed = Vec::new();
    lzma_rs::xz_decompress(&mut Cursor::new(tarball), &mut decompressed)
        .map_err(|e| Error::Other(format!("decompressing Nix tarball: {e}")))?;

    let mut archive = tar::Archive::new(Cursor::new(decompressed));
    archive.set_preserve_permissions(true);
    archive.set_preserve_mtime(true);
    archive.unpack(dest).map_err(|e| Error::Io {
        path: dest.to_path_buf(),
        source: e,
    })?;

    Ok(())
}
