use crate::ast::Expr;
use crate::escape::NulByte;

const FACTS: &str = include_str!("package-facts.nix");
const CHECK: &str = include_str!("package-check.nix");

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

pub fn derivations<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<String, NulByte> {
    let names = names
        .into_iter()
        .map(Expr::string)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!(
        "pkgs: map (name: pkgs.${{name}}.drvPath) {}",
        Expr::List(names).print()
    ))
}

pub fn package_checks<'a>(names: impl IntoIterator<Item = &'a str>) -> Result<String, NulByte> {
    let names = names
        .into_iter()
        .map(Expr::string)
        .collect::<Result<Vec<_>, _>>()?;
    Ok(format!(
        "pkgs: let names = {}; in {{ facts = ({}) pkgs names; checks = ({}) pkgs names; }}",
        Expr::List(names).print(),
        FACTS.trim_end(),
        CHECK.trim_end()
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn checks_and_facts_are_read_for_the_same_names() {
        let applied = package_checks(["git", "a\"b"]).unwrap();

        assert!(applied.starts_with("pkgs: let names = [\n  \"git\"\n  \"a\\\"b\"\n]; in {"));
        assert!(applied.contains(" facts = (pkgs: names:\n"));
        assert!(applied.contains(" checks = (pkgs: names:\n"));
        assert!(package_checks(["a\0b"]).is_err());
    }

    #[test]
    fn each_name_is_evaluated_to_its_derivation() {
        assert_eq!(
            derivations(["git", "ripgrep"]).unwrap(),
            "pkgs: map (name: pkgs.${name}.drvPath) [\n  \"git\"\n  \"ripgrep\"\n]"
        );
    }

    #[test]
    fn a_name_checked_never_ends_its_string_or_opens_an_interpolation() {
        assert_eq!(
            derivations(["a\"b", "${c}"]).unwrap(),
            "pkgs: map (name: pkgs.${name}.drvPath) [\n  \"a\\\"b\"\n  \"\\${c}\"\n]"
        );
        assert!(derivations(["a\0b"]).is_err());
    }

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
