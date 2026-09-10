use std::collections::HashMap;

const PATH: &str = "/etc/os-release";

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Distro {
    Ubuntu,
    Debian,
    Other(String),
}

pub fn detect() -> Distro {
    let Ok(contents) = std::fs::read_to_string(PATH) else {
        return Distro::Other("unknown (no /etc/os-release)".into());
    };
    let fields = parse(&contents);

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
            let value = value.trim().trim_matches('"');
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
}
