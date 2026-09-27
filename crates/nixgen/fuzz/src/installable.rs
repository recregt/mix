use arbitrary::Arbitrary;
use mix_nixgen::{AttrPath, FlakeRef, Installable, PublicKey};

const BASE64: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";

#[derive(Debug, Arbitrary)]
pub struct InstallableInput {
    pub dir: Vec<u8>,
    pub git: bool,
    pub key: Option<Vec<u8>>,
    pub segments: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Parts {
    pub git: bool,
    pub dir: Vec<u8>,
    pub key: Option<String>,
    pub segments: Vec<String>,
}

impl InstallableInput {
    pub fn build(self) -> Option<(Installable, Parts)> {
        let mut dir = b"/".to_vec();
        dir.extend(self.dir.into_iter().filter(|b| *b != 0));
        let path = std::path::PathBuf::from(
            <std::ffi::OsStr as std::os::unix::ffi::OsStrExt>::from_bytes(&dir),
        );
        let segments: Vec<String> = self
            .segments
            .into_iter()
            .filter(|s| !s.is_empty() && !s.contains('"'))
            .collect();
        let attr_path = AttrPath::new(segments.clone()).ok()?;
        let key = self.key.filter(|k| !k.is_empty()).map(|bytes| {
            let mut text: String = bytes
                .iter()
                .map(|b| BASE64[usize::from(b % 64)] as char)
                .collect();
            text.push('=');
            text
        });
        let flake = if self.git {
            let signer = key
                .clone()
                .map(|k| PublicKey::new(k).expect("base64 by construction"));
            FlakeRef::git_file(path, signer).ok()?
        } else {
            FlakeRef::path(path).ok()?
        };
        let parts = Parts {
            git: self.git,
            dir,
            key: if self.git { key } else { None },
            segments,
        };
        Some((Installable::new(flake, attr_path), parts))
    }
}

pub fn decode(rendered: &str) -> Result<Parts, &'static str> {
    if rendered.contains('^') {
        return Err("a raw ^ would be read as an outputs spec");
    }
    let (git, rest) = if let Some(rest) = rendered.strip_prefix("git+file://") {
        (true, rest)
    } else if let Some(rest) = rendered.strip_prefix("path:") {
        (false, rest)
    } else {
        return Err("unknown scheme");
    };
    let (before_fragment, fragment) = rest.split_once('#').ok_or("no fragment")?;
    let (path, query) = match before_fragment.split_once('?') {
        Some((path, query)) => (path, Some(query)),
        None => (before_fragment, None),
    };
    let mut key = None;
    if let Some(query) = query {
        for pair in query.split('&') {
            let (name, value) = pair.split_once('=').ok_or("a query pair without =")?;
            if name == "publicKey" {
                key = Some(String::from_utf8(percent_decode(value)?).map_err(|_| "key not utf8")?);
            }
        }
    }
    let fragment = String::from_utf8(percent_decode(fragment)?).map_err(|_| "fragment not utf8")?;
    Ok(Parts {
        git,
        dir: percent_decode(path)?,
        key,
        segments: parse_attr_path(&fragment)?,
    })
}

fn percent_decode(text: &str) -> Result<Vec<u8>, &'static str> {
    let bytes = text.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = bytes.get(i + 1..i + 3).ok_or("a truncated escape")?;
            let hex = std::str::from_utf8(hex).map_err(|_| "a bad escape")?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| "a bad escape")?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    Ok(out)
}

fn parse_attr_path(text: &str) -> Result<Vec<String>, &'static str> {
    let mut segments = Vec::new();
    let mut current = String::new();
    let mut chars = text.chars();
    while let Some(c) = chars.next() {
        match c {
            '.' => segments.push(std::mem::take(&mut current)),
            '"' => loop {
                match chars.next() {
                    Some('"') => break,
                    Some(inner) => current.push(inner),
                    None => return Err("missing closing quote"),
                }
            },
            other => current.push(other),
        }
    }
    if !current.is_empty() {
        segments.push(current);
    }
    Ok(segments)
}
