use std::collections::HashMap;
use std::path::Path;

const PATH: &str = "/etc/os-release";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Distro {
    Ubuntu,
    Debian,
    Other(String),
}

pub fn detect() -> Distro {
    detect_at(Path::new(PATH))
}

fn detect_at(path: &Path) -> Distro {
    let Ok(contents) = std::fs::read_to_string(path) else {
        return Distro::Other("unknown (no /etc/os-release)".into());
    };
    detect_from(&contents)
}

fn detect_from(contents: &str) -> Distro {
    let fields = parse(contents);

    match fields.get("ID").map(String::as_str) {
        Some("ubuntu") => Distro::Ubuntu,
        Some("debian") => Distro::Debian,
        Some(other) => Distro::Other(other.to_string()),
        None => Distro::Other("unknown".into()),
    }
}

fn parse(contents: &str) -> HashMap<String, String> {
    contents
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let (key, value) = line.split_once('=')?;
            let value = value.trim().trim_matches(['"', '\'']);
            Some((key.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_quoted_and_unquoted_values() {
        let sample = "ID=ubuntu\nVERSION_ID=\"24.04\"\nNAME=Ubuntu\n";
        let fields = parse(sample);
        assert_eq!(fields.get("ID").map(String::as_str), Some("ubuntu"));
        assert_eq!(fields.get("VERSION_ID").map(String::as_str), Some("24.04"));
    }

    #[test]
    fn parses_single_quoted_values() {
        let sample = "ID='ubuntu'\n";
        let fields = parse(sample);
        assert_eq!(fields.get("ID").map(String::as_str), Some("ubuntu"));
    }

    #[test]
    fn detect_from_single_quoted_ubuntu() {
        assert_eq!(detect_from("ID='ubuntu'\n"), Distro::Ubuntu);
    }

    #[test]
    fn detect_from_ubuntu() {
        assert_eq!(detect_from("ID=ubuntu\n"), Distro::Ubuntu);
    }

    #[test]
    fn detect_from_debian() {
        assert_eq!(detect_from("ID=debian\n"), Distro::Debian);
    }

    #[test]
    fn detect_from_unknown_distro() {
        assert_eq!(detect_from("ID=arch\n"), Distro::Other("arch".into()));
    }

    #[test]
    fn detect_from_missing_id_field() {
        assert_eq!(
            detect_from("NAME=Whatever\n"),
            Distro::Other("unknown".into())
        );
    }

    #[test]
    fn detect_at_missing_file() {
        assert_eq!(
            detect_at(Path::new("/does/not/exist/os-release")),
            Distro::Other("unknown (no /etc/os-release)".into())
        );
    }
}
