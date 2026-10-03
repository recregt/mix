use crate::v1::Code;

pub fn parse(name: &str) -> Option<Code> {
    let upper = name.trim().to_ascii_uppercase().replace('-', "_");
    let full = if upper.starts_with("CODE_") {
        upper
    } else {
        format!("CODE_{upper}")
    };
    Code::from_str_name(&full).filter(|code| *code != Code::Unspecified)
}

pub fn name(code: Code) -> &'static str {
    code.as_str_name().trim_start_matches("CODE_")
}

pub fn kebab(code: Code) -> String {
    name(code).to_ascii_lowercase().replace('_', "-")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn every_code() -> impl Iterator<Item = Code> {
        Code::DEFINED
            .iter()
            .filter_map(|value| Code::try_from(*value).ok())
    }

    #[test]
    fn every_name_a_code_is_shown_by_parses_back() {
        for code in every_code() {
            assert_eq!(parse(name(code)), Some(code), "{code:?}");
            assert_eq!(parse(&name(code).to_lowercase()), Some(code), "{code:?}");
            assert_eq!(parse(code.as_str_name()), Some(code), "{code:?}");
            assert_eq!(parse(&kebab(code)), Some(code), "{code:?}");
        }
        assert_eq!(kebab(Code::GitRecordFailed), "git-record-failed");
    }

    #[test]
    fn a_name_mix_does_not_use_is_not_a_code() {
        assert_eq!(parse("NOPE"), None);
        assert_eq!(parse("UNSPECIFIED"), None);
        assert_eq!(parse(""), None);
    }
}
