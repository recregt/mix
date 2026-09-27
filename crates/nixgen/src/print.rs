use crate::ast::{Expr, Key, StrPart};
use crate::ident::Ident;

const INDENT: &str = "  ";
const SPACES: &str = "                                ";

pub(crate) fn print(expr: &Expr) -> String {
    let mut out = String::with_capacity(1024);
    write(expr, 0, &mut out);
    out
}

pub(crate) fn print_into(expr: &Expr, out: &mut String) {
    write(expr, 0, out);
}

pub(crate) fn print_function(formals: &[Ident], ellipsis: bool, body: &Expr, out: &mut String) {
    write_lambda(formals, ellipsis, body, 0, out);
}

fn write(expr: &Expr, depth: usize, out: &mut String) {
    match expr {
        Expr::Str(parts) => write_str(parts, depth, out),
        Expr::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
        Expr::List(items) if items.is_empty() => out.push_str("[ ]"),
        Expr::List(items) => {
            out.push_str("[\n");
            for item in items {
                indent(depth + 1, out);
                write_operand(item, depth + 1, out);
                out.push('\n');
            }
            indent(depth, out);
            out.push(']');
        }
        Expr::Attrs(entries) if entries.is_empty() => out.push_str("{ }"),
        Expr::Attrs(entries) => {
            out.push_str("{\n");
            for (key, value) in entries {
                indent(depth + 1, out);
                write_key(key, out);
                out.push_str(" = ");
                write(value, depth + 1, out);
                out.push_str(";\n");
            }
            indent(depth, out);
            out.push('}');
        }
        Expr::Var(ident) => out.push_str(ident.as_str()),
        Expr::Select(base, first, rest) => {
            write_operand(base, depth, out);
            for key in std::iter::once(first).chain(rest) {
                out.push('.');
                write_key(key, out);
            }
        }
        Expr::Apply(function, argument) => {
            if matches!(**function, Expr::Lambda { .. }) {
                write_parenthesized(function, depth, out);
            } else {
                write(function, depth, out);
            }
            out.push(' ');
            write_operand(argument, depth, out);
        }
        Expr::Lambda {
            formals,
            ellipsis,
            body,
        } => write_lambda(formals, *ellipsis, body, depth, out),
        Expr::Path(path) => {
            out.push_str("./");
            out.push_str(path.as_str());
        }
    }
}

fn write_lambda(formals: &[Ident], ellipsis: bool, body: &Expr, depth: usize, out: &mut String) {
    out.push('{');
    let mut first = true;
    for formal in formals {
        out.push_str(if first { " " } else { ", " });
        out.push_str(formal.as_str());
        first = false;
    }
    if ellipsis {
        out.push_str(if first { " ..." } else { ", ..." });
        first = false;
    }
    out.push_str(if first { "}: " } else { " }: " });
    write(body, depth, out);
}

fn is_operand(expr: &Expr) -> bool {
    matches!(
        expr,
        Expr::Str(_)
            | Expr::Bool(_)
            | Expr::List(_)
            | Expr::Attrs(_)
            | Expr::Var(_)
            | Expr::Select(..)
            | Expr::Path(_)
    )
}

fn write_operand(expr: &Expr, depth: usize, out: &mut String) {
    if is_operand(expr) {
        write(expr, depth, out);
    } else {
        write_parenthesized(expr, depth, out);
    }
}

fn write_parenthesized(expr: &Expr, depth: usize, out: &mut String) {
    out.push('(');
    write(expr, depth, out);
    out.push(')');
}

fn write_key(key: &Key, out: &mut String) {
    if key.is_bare() {
        out.push_str(key.as_str());
    } else {
        out.push('"');
        write_escaped(key.as_str(), false, out);
        out.push('"');
    }
}

fn write_str(parts: &[StrPart], depth: usize, out: &mut String) {
    out.push('"');
    for (index, part) in parts.iter().enumerate() {
        match part {
            StrPart::Lit(text) => {
                let before_interpolation = matches!(parts.get(index + 1), Some(StrPart::Interp(_)));
                write_escaped(text.as_str(), before_interpolation, out);
            }
            StrPart::Interp(expr) => {
                out.push_str("${");
                write(expr, depth, out);
                out.push('}');
            }
        }
    }
    out.push('"');
}

fn write_escaped(text: &str, before_interpolation: bool, out: &mut String) {
    let bytes = text.as_bytes();
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        let escape = match byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            b'\r' => "\\r",
            b'$' if bytes.get(index + 1) == Some(&b'{')
                || (before_interpolation && index + 1 == bytes.len()) =>
            {
                "\\$"
            }
            _ => continue,
        };
        out.push_str(&text[start..index]);
        out.push_str(escape);
        start = index + 1;
    }
    out.push_str(&text[start..]);
}

fn indent(depth: usize, out: &mut String) {
    let width = depth * INDENT.len();
    if let Some(spaces) = SPACES.get(..width) {
        out.push_str(spaces);
    } else {
        for _ in 0..depth {
            out.push_str(INDENT);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ast::{NixStr, RelPath};

    fn key(s: &str) -> Key {
        Key::new(s).unwrap()
    }

    fn lit(s: &str) -> StrPart {
        StrPart::Lit(NixStr::new(s).unwrap())
    }

    fn string(s: &str) -> String {
        Expr::string(s).unwrap().print()
    }

    #[test]
    fn escapes_quotes_backslashes_and_carriage_returns() {
        assert_eq!(string(r#"a"b\c"#), r#""a\"b\\c""#);
        assert_eq!(string("a\rb"), r#""a\rb""#);
    }

    #[test]
    fn keeps_newlines_and_tabs_raw() {
        assert_eq!(string("a\nb\tc"), "\"a\nb\tc\"");
    }

    #[test]
    fn escapes_an_interpolation_but_not_a_lone_dollar() {
        assert_eq!(string("${x}"), r#""\${x}""#);
        assert_eq!(string("$5 and $"), r#""$5 and $""#);
        assert_eq!(string("$${x}"), r#""$\${x}""#);
    }

    #[test]
    fn escapes_a_dollar_that_would_join_an_interpolation() {
        let expr = Expr::Str(vec![
            lit("a$"),
            StrPart::Interp(Expr::Path(RelPath::new("state").unwrap())),
        ]);
        assert_eq!(expr.print(), r#""a\$${./state}""#);
    }

    #[test]
    fn prints_a_bare_key_and_quotes_every_other_key() {
        let expr = Expr::attrs([
            (key("home"), Expr::Bool(true)),
            (key("in"), Expr::Bool(true)),
            (key("a.b"), Expr::Bool(true)),
            (key("${x}"), Expr::Bool(true)),
        ]);
        assert_eq!(
            expr.print(),
            "{\n  \"\\${x}\" = true;\n  \"a.b\" = true;\n  home = true;\n  \"in\" = true;\n}"
        );
    }

    #[test]
    fn sorts_keys_by_their_bytes() {
        let expr = Expr::attrs([(key("b"), Expr::Bool(true)), (key("a"), Expr::Bool(false))]);
        assert_eq!(expr.print(), "{\n  a = false;\n  b = true;\n}");
    }

    #[test]
    fn prints_empty_collections_on_one_line() {
        assert_eq!(Expr::List(vec![]).print(), "[ ]");
        assert_eq!(Expr::attrs([]).print(), "{ }");
    }

    #[test]
    fn prints_one_list_element_per_line() {
        let pkgs = Expr::Var(Ident::new("pkgs").unwrap());
        let expr = Expr::List(vec![
            Expr::select(pkgs.clone(), key("git"), []),
            Expr::select(pkgs, key("node-sass"), []),
        ]);
        assert_eq!(expr.print(), "[\n  pkgs.git\n  pkgs.node-sass\n]");
    }

    #[test]
    fn wraps_an_application_inside_a_list_or_an_argument() {
        let f = Expr::Var(Ident::new("f").unwrap());
        let call = Expr::apply(f.clone(), Expr::Bool(true));
        assert_eq!(Expr::List(vec![call.clone()]).print(), "[\n  (f true)\n]");
        assert_eq!(Expr::apply(f, call).print(), "f (f true)");
    }

    #[test]
    fn wraps_a_lambda_in_function_position() {
        let lambda = Expr::Lambda {
            formals: vec![],
            ellipsis: true,
            body: Box::new(Expr::Bool(true)),
        };
        assert_eq!(
            Expr::apply(lambda, Expr::attrs([])).print(),
            "({ ... }: true) { }"
        );
    }

    #[test]
    fn prints_a_formal_set_lambda() {
        let lambda = Expr::Lambda {
            formals: vec![Ident::new("pkgs").unwrap()],
            ellipsis: true,
            body: Box::new(Expr::attrs([])),
        };
        assert_eq!(lambda.print(), "{ pkgs, ... }: { }");
        let bare = Expr::Lambda {
            formals: vec![],
            ellipsis: false,
            body: Box::new(Expr::Bool(true)),
        };
        assert_eq!(bare.print(), "{}: true");
    }

    #[test]
    fn selects_through_a_quoted_key() {
        let expr = Expr::select(
            Expr::Var(Ident::new("nixpkgs").unwrap()),
            key("legacyPackages"),
            [key("x86_64-linux")],
        );
        assert_eq!(expr.print(), "nixpkgs.legacyPackages.x86_64-linux");
        let quoted = Expr::select(Expr::Var(Ident::new("a").unwrap()), key("b c"), []);
        assert_eq!(quoted.print(), "a.\"b c\"");
    }

    #[test]
    fn indents_nested_attrs_and_lists() {
        let expr = Expr::attrs([(
            key("home"),
            Expr::attrs([(
                key("packages"),
                Expr::List(vec![Expr::Path(RelPath::new("home.nix").unwrap())]),
            )]),
        )]);
        assert_eq!(
            expr.print(),
            "{\n  home = {\n    packages = [\n      ./home.nix\n    ];\n  };\n}"
        );
    }
}
