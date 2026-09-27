use crate::ast::{Expr, Key, NixStr, RelPath, StrPart, Verbatim};
use crate::ident::{FileName, Ident};

const INDENT: &str = "  ";
const SPACES: &str = "                                ";

pub(crate) struct Writer<'o> {
    out: &'o mut String,
    depth: usize,
}

impl<'o> Writer<'o> {
    pub(crate) fn new(out: &'o mut String) -> Self {
        Self { out, depth: 0 }
    }

    pub(crate) fn bool(&mut self, value: bool) {
        self.out.push_str(if value { "true" } else { "false" });
    }

    pub(crate) fn var(&mut self, var: &Ident) {
        self.out.push_str(var.as_str());
    }

    pub(crate) fn path(&mut self, path: &RelPath) {
        self.out.push_str("./");
        self.out.push_str(path.as_str());
    }

    pub(crate) fn file_path(&mut self, file: &FileName) {
        self.out.push_str("./");
        self.out.push_str(file.as_str());
    }

    pub(crate) fn keys<'k>(&mut self, keys: impl IntoIterator<Item = &'k Key>) {
        for key in keys {
            self.out.push('.');
            write_key(key, self.out);
        }
    }

    pub(crate) fn select_ident(&mut self, var: &Ident, attr: &Ident) {
        self.out.push_str(var.as_str());
        self.out.push('.');
        self.out.push_str(attr.as_str());
    }

    pub(crate) fn text(&mut self, text: &NixStr) {
        self.string(|s| s.lit(text.as_str()));
    }

    pub(crate) fn string(&mut self, parts: impl FnOnce(&mut StrWriter<'_, 'o>)) {
        self.out.push('"');
        let mut writer = StrWriter {
            w: self,
            pending_dollar: false,
        };
        parts(&mut writer);
        if writer.pending_dollar {
            writer.w.out.push('$');
        }
        self.out.push('"');
    }

    pub(crate) fn parenthesized(&mut self, inner: impl FnOnce(&mut Writer<'o>)) {
        self.out.push('(');
        inner(self);
        self.out.push(')');
    }

    pub(crate) fn apply(
        &mut self,
        function: impl FnOnce(&mut Writer<'o>),
        argument: impl FnOnce(&mut Writer<'o>),
    ) {
        function(self);
        self.out.push(' ');
        argument(self);
    }

    pub(crate) fn lambda<'f>(
        &mut self,
        formals: impl IntoIterator<Item = &'f Ident>,
        ellipsis: bool,
        body: impl FnOnce(&mut Writer<'o>),
    ) {
        self.out.push('{');
        let mut previous: Option<&Ident> = None;
        for formal in formals {
            if let Some(previous) = previous {
                assert!(
                    previous < formal,
                    "lambda formals must be written in strictly increasing order"
                );
            }
            self.out
                .push_str(if previous.is_none() { " " } else { ", " });
            self.out.push_str(formal.as_str());
            previous = Some(formal);
        }
        if ellipsis {
            self.out
                .push_str(if previous.is_none() { " ..." } else { ", ..." });
        }
        self.out.push_str(if previous.is_none() && !ellipsis {
            "}: "
        } else {
            " }: "
        });
        body(self);
    }

    pub(crate) fn attrs<'k>(&mut self, entries: impl FnOnce(&mut AttrsWriter<'_, 'o, 'k>)) {
        let mut writer = AttrsWriter {
            w: self,
            last: None,
        };
        entries(&mut writer);
        if writer.last.is_some() {
            indent(self.depth, self.out);
            self.out.push('}');
        } else {
            self.out.push_str("{ }");
        }
    }

    pub(crate) fn list(&mut self, items: impl FnOnce(&mut ListWriter<'_, 'o>)) {
        let mut writer = ListWriter {
            w: self,
            opened: false,
        };
        items(&mut writer);
        if writer.opened {
            indent(self.depth, self.out);
            self.out.push(']');
        } else {
            self.out.push_str("[ ]");
        }
    }
}

pub(crate) struct AttrsWriter<'w, 'o, 'k> {
    w: &'w mut Writer<'o>,
    last: Option<&'k Key>,
}

impl<'o, 'k> AttrsWriter<'_, 'o, 'k> {
    pub(crate) fn entry(&mut self, key: &'k Key, value: impl FnOnce(&mut Writer<'o>)) {
        match self.last {
            Some(last) => assert!(
                last < key,
                "attribute keys must be written in strictly increasing order"
            ),
            None => self.w.out.push_str("{\n"),
        }
        self.last = Some(key);
        self.w.depth += 1;
        indent(self.w.depth, self.w.out);
        write_key(key, self.w.out);
        self.w.out.push_str(" = ");
        value(self.w);
        self.w.out.push_str(";\n");
        self.w.depth -= 1;
    }
}

pub(crate) struct ListWriter<'w, 'o> {
    w: &'w mut Writer<'o>,
    opened: bool,
}

impl<'o> ListWriter<'_, 'o> {
    pub(crate) fn item(&mut self, value: impl FnOnce(&mut Writer<'o>)) {
        if !self.opened {
            self.w.out.push_str("[\n");
            self.opened = true;
        }
        self.w.depth += 1;
        indent(self.w.depth, self.w.out);
        value(self.w);
        self.w.out.push('\n');
        self.w.depth -= 1;
    }
}

pub(crate) struct StrWriter<'w, 'o> {
    w: &'w mut Writer<'o>,
    pending_dollar: bool,
}

impl<'o> StrWriter<'_, 'o> {
    pub(crate) fn lit(&mut self, text: &str) {
        if text.is_empty() {
            return;
        }
        if self.pending_dollar {
            self.w
                .out
                .push_str(if text.starts_with('{') { "\\$" } else { "$" });
            self.pending_dollar = false;
        }
        let body = match text.strip_suffix('$') {
            Some(body) => {
                self.pending_dollar = true;
                body
            }
            None => text,
        };
        write_escaped(body, self.w.out);
    }

    pub(crate) fn verbatim(&mut self, text: Verbatim<'_>) {
        let text = text.as_str();
        if text.is_empty() {
            return;
        }
        if self.pending_dollar {
            self.w
                .out
                .push_str(if text.starts_with('{') { "\\$" } else { "$" });
            self.pending_dollar = false;
        }
        self.w.out.push_str(text);
    }

    pub(crate) fn interp(&mut self, value: impl FnOnce(&mut Writer<'o>)) {
        if self.pending_dollar {
            self.w.out.push_str("\\$");
            self.pending_dollar = false;
        }
        self.w.out.push_str("${");
        value(self.w);
        self.w.out.push('}');
    }
}

pub(crate) fn write_key(key: &Key, out: &mut String) {
    if key.is_bare() {
        out.push_str(key.as_str());
    } else {
        out.push('"');
        write_escaped(key.as_str(), out);
        out.push('"');
    }
}

fn write_escaped(text: &str, out: &mut String) {
    let bytes = text.as_bytes();
    let mut start = 0;
    for (index, &byte) in bytes.iter().enumerate() {
        let escape = match byte {
            b'"' => "\\\"",
            b'\\' => "\\\\",
            b'\r' => "\\r",
            b'$' if bytes.get(index + 1) == Some(&b'{') => "\\$",
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

pub(crate) fn print(expr: &Expr) -> String {
    let mut out = String::with_capacity(1024);
    write_expr(&mut Writer::new(&mut out), expr);
    out
}

fn write_expr(w: &mut Writer<'_>, expr: &Expr) {
    match expr {
        Expr::Str(parts) => w.string(|s| {
            for part in parts {
                match part {
                    StrPart::Lit(text) => s.lit(text.as_str()),
                    StrPart::Interp(inner) => s.interp(|w| write_expr(w, inner)),
                }
            }
        }),
        Expr::Bool(value) => w.bool(*value),
        Expr::List(items) => w.list(|l| {
            for item in items {
                l.item(|w| write_operand(w, item));
            }
        }),
        Expr::Attrs(entries) => w.attrs(|a| {
            for (key, value) in entries.iter() {
                a.entry(key, |w| write_expr(w, value));
            }
        }),
        Expr::Var(ident) => w.var(ident),
        Expr::Select(base, first, rest) => {
            if matches!(**base, Expr::Path(_)) {
                w.parenthesized(|w| write_expr(w, base));
            } else {
                write_operand(w, base);
            }
            w.keys(std::iter::once(first).chain(rest));
        }
        Expr::Apply(function, argument) => w.apply(
            |w| {
                if matches!(**function, Expr::Lambda { .. }) {
                    w.parenthesized(|w| write_expr(w, function));
                } else {
                    write_expr(w, function);
                }
            },
            |w| write_operand(w, argument),
        ),
        Expr::Lambda {
            formals,
            ellipsis,
            body,
        } => w.lambda(formals, *ellipsis, |w| write_expr(w, body)),
        Expr::Path(path) => w.path(path),
    }
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

fn write_operand(w: &mut Writer<'_>, expr: &Expr) {
    if is_operand(expr) {
        write_expr(w, expr);
    } else {
        w.parenthesized(|w| write_expr(w, expr));
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeSet;

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
    fn escapes_a_dollar_across_literal_pieces() {
        let empty_between = Expr::Str(vec![
            lit("|$"),
            lit(""),
            StrPart::Interp(Expr::string("").unwrap()),
        ]);
        assert_eq!(empty_between.print(), r#""|\$${""}""#);
        let brace_after = Expr::Str(vec![lit("a$"), lit("{x}")]);
        assert_eq!(brace_after.print(), r#""a\${x}""#);
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
            formals: BTreeSet::new(),
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
            formals: BTreeSet::from([Ident::new("pkgs").unwrap()]),
            ellipsis: true,
            body: Box::new(Expr::attrs([])),
        };
        assert_eq!(lambda.print(), "{ pkgs, ... }: { }");
        let bare = Expr::Lambda {
            formals: BTreeSet::new(),
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
    fn parenthesizes_a_path_before_a_selection() {
        let expr = Expr::select(Expr::Path(RelPath::new("a").unwrap()), key("b"), []);
        assert_eq!(expr.print(), "(./a).b");
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
