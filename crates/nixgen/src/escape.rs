#[derive(Debug, thiserror::Error)]
#[error("string value contains a null byte, which Nix cannot represent")]
pub struct NulByte;

pub fn nix_string_literal(raw: &str) -> Result<String, NulByte> {
    if raw.contains('\0') {
        return Err(NulByte);
    }

    let mut out = String::with_capacity(raw.len() + 2);
    out.push('"');

    let mut chars = raw.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '\\' => out.push_str("\\\\"),
            '"' => out.push_str("\\\""),
            '$' if chars.peek() == Some(&'{') => out.push_str("\\$"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            other => out.push(other),
        }
    }

    out.push('"');
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_plain_text_in_quotes() {
        assert_eq!(nix_string_literal("firefox").unwrap(), "\"firefox\"");
    }

    #[test]
    fn escapes_backslashes_before_quotes() {
        assert_eq!(nix_string_literal(r#"a\"b"#).unwrap(), r#""a\\\"b""#);
    }

    #[test]
    fn escapes_double_quotes() {
        assert_eq!(
            nix_string_literal(r#"say "hi""#).unwrap(),
            r#""say \"hi\"""#
        );
    }

    #[test]
    fn escapes_interpolation_but_not_a_lone_dollar() {
        assert_eq!(nix_string_literal("${foo}").unwrap(), r#""\${foo}""#);
        assert_eq!(nix_string_literal("$5").unwrap(), "\"$5\"");
    }

    #[test]
    fn escapes_control_characters() {
        assert_eq!(nix_string_literal("a\nb\tc\rd").unwrap(), r#""a\nb\tc\rd""#);
    }

    #[test]
    fn rejects_a_null_byte() {
        assert!(nix_string_literal("a\0b").is_err());
    }
}
