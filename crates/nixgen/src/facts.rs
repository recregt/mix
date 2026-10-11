use crate::ast::Expr;
use crate::escape::NulByte;

const FACTS: &str = include_str!("package-facts.nix");

pub fn package_facts<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<String, NulByte> {
    let names = names
        .into_iter()
        .map(Expr::string)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!(
        "pkgs: ({}) pkgs {}",
        FACTS.trim_end(),
        Expr::List(names).print()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn arguments(applied: &str) -> &str {
        applied
            .rsplit_once(") pkgs ")
            .expect("the function is applied to the packages and a list")
            .1
    }

    #[test]
    fn the_names_follow_the_function_as_a_list_of_strings() {
        let applied = package_facts(["git", "ripgrep"]).unwrap();

        assert!(applied.starts_with("pkgs: (pkgs: names:\n"));
        assert_eq!(arguments(&applied), "[\n  \"git\"\n  \"ripgrep\"\n]");
    }

    #[test]
    fn no_names_apply_the_function_to_an_empty_list() {
        assert_eq!(arguments(&package_facts([]).unwrap()), "[ ]");
    }

    #[test]
    fn a_name_never_ends_its_string_or_opens_an_interpolation() {
        let applied = package_facts(["a\"b", "${c}"]).unwrap();

        assert_eq!(arguments(&applied), "[\n  \"a\\\"b\"\n  \"\\${c}\"\n]");
    }

    #[test]
    fn a_name_holding_a_null_byte_is_refused() {
        assert!(package_facts(["a\0b"]).is_err());
    }
}
