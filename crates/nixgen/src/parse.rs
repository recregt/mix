use std::collections::BTreeSet;

use crate::ast::{AttrSet, Expr, Key, NixStr, RelPath, StrPart};
use crate::ident::Ident;

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("{message} at byte {offset}")]
pub struct ParseError {
    pub offset: usize,
    pub message: &'static str,
}

pub fn parse(source: &str) -> Result<Expr, ParseError> {
    let mut parser = Parser { source, pos: 0 };
    let expr = parser.expr()?;
    parser.skip_trivia();
    if parser.pos != source.len() {
        return Err(parser.error("trailing input"));
    }
    Ok(expr)
}

pub fn normalize(expr: Expr) -> Expr {
    match expr {
        Expr::Str(parts) => Expr::Str(normalize_parts(parts)),
        Expr::List(items) => Expr::List(items.into_iter().map(normalize).collect()),
        Expr::Attrs(entries) => Expr::Attrs(
            entries
                .into_iter()
                .map(|(k, v)| (k, normalize(v)))
                .collect(),
        ),
        Expr::Select(base, first, rest) => match normalize(*base) {
            Expr::Select(inner, inner_first, mut inner_rest) => {
                inner_rest.push(first);
                inner_rest.extend(rest);
                Expr::Select(inner, inner_first, inner_rest)
            }
            base => Expr::Select(Box::new(base), first, rest),
        },
        Expr::Apply(function, argument) => Expr::apply(normalize(*function), normalize(*argument)),
        Expr::Lambda {
            formals,
            ellipsis,
            body,
        } => Expr::Lambda {
            formals,
            ellipsis,
            body: Box::new(normalize(*body)),
        },
        other => other,
    }
}

fn normalize_parts(parts: Vec<StrPart>) -> Vec<StrPart> {
    let mut out: Vec<StrPart> = Vec::with_capacity(parts.len());
    for part in parts {
        match part {
            StrPart::Lit(text) if text.as_str().is_empty() => {}
            StrPart::Lit(text) => match out.last_mut() {
                Some(StrPart::Lit(previous)) => {
                    let joined = format!("{}{}", previous.as_str(), text.as_str());
                    *previous = NixStr::new(joined).expect("joined literals hold no null byte");
                }
                _ => out.push(StrPart::Lit(text)),
            },
            StrPart::Interp(expr) => out.push(StrPart::Interp(normalize(expr))),
        }
    }
    out
}

struct Parser<'a> {
    source: &'a str,
    pos: usize,
}

impl<'a> Parser<'a> {
    fn error(&self, message: &'static str) -> ParseError {
        ParseError {
            offset: self.pos,
            message,
        }
    }

    fn bytes(&self) -> &'a [u8] {
        self.source.as_bytes()
    }

    fn peek(&self) -> Option<u8> {
        self.bytes().get(self.pos).copied()
    }

    fn peek_at(&self, offset: usize) -> Option<u8> {
        self.bytes().get(self.pos + offset).copied()
    }

    fn skip_trivia(&mut self) {
        while let Some(byte) = self.peek() {
            match byte {
                b' ' | b'\t' | b'\n' | b'\r' => self.pos += 1,
                b'#' => {
                    while self.peek().is_some_and(|b| b != b'\n' && b != b'\r') {
                        self.pos += 1;
                    }
                }
                _ => break,
            }
        }
    }

    fn expect(&mut self, token: &str) -> Result<(), ParseError> {
        self.skip_trivia();
        if self.source[self.pos..].starts_with(token) {
            self.pos += token.len();
            Ok(())
        } else {
            Err(self.error("unexpected token"))
        }
    }

    fn expr(&mut self) -> Result<Expr, ParseError> {
        self.skip_trivia();
        if self.at_lambda() {
            self.lambda()
        } else {
            self.application()
        }
    }

    fn at_lambda(&self) -> bool {
        if self.peek() != Some(b'{') {
            return false;
        }
        let bytes = self.bytes();
        let mut p = skip_space(bytes, self.pos + 1);
        if bytes.get(p) == Some(&b'}') {
            p = skip_space(bytes, p + 1);
            return bytes.get(p) == Some(&b':');
        }
        if bytes[p..].starts_with(b"...") {
            return true;
        }
        let end = identifier_end(bytes, p);
        if end == p {
            return false;
        }
        let after = skip_space(bytes, end);
        matches!(bytes.get(after), Some(b',' | b'}'))
    }

    fn lambda(&mut self) -> Result<Expr, ParseError> {
        self.expect("{")?;
        let mut formals = BTreeSet::new();
        let mut ellipsis = false;
        loop {
            self.skip_trivia();
            if self.peek() == Some(b'}') {
                self.pos += 1;
                break;
            }
            if self.source[self.pos..].starts_with("...") {
                self.pos += 3;
                ellipsis = true;
                self.expect("}")?;
                break;
            }
            let name = self.identifier()?;
            let ident = Ident::new(name).map_err(|_| self.error("a keyword as a formal"))?;
            if !formals.insert(ident) {
                return Err(self.error("duplicate formal function argument"));
            }
            self.skip_trivia();
            if self.peek() == Some(b',') {
                self.pos += 1;
            }
        }
        self.expect(":")?;
        let body = self.expr()?;
        Ok(Expr::Lambda {
            formals,
            ellipsis,
            body: Box::new(body),
        })
    }

    fn application(&mut self) -> Result<Expr, ParseError> {
        let mut function = self.selection()?;
        loop {
            self.skip_trivia();
            if !self.at_operand() {
                return Ok(function);
            }
            let argument = self.selection()?;
            function = Expr::apply(function, argument);
        }
    }

    fn at_operand(&self) -> bool {
        match self.peek() {
            Some(b'"' | b'[' | b'{' | b'(') => true,
            Some(b'.') => self.peek_at(1) == Some(b'/'),
            Some(b) => b.is_ascii_alphabetic() || b == b'_',
            None => false,
        }
    }

    fn selection(&mut self) -> Result<Expr, ParseError> {
        let base = self.simple()?;
        let mut keys = Vec::new();
        loop {
            self.skip_trivia();
            if self.peek() == Some(b'.') && !matches!(self.peek_at(1), Some(b'.' | b'/')) {
                self.pos += 1;
                keys.push(self.key()?);
            } else {
                break;
            }
        }
        let mut keys = keys.into_iter();
        Ok(match keys.next() {
            Some(first) => Expr::Select(Box::new(base), first, keys.collect()),
            None => base,
        })
    }

    fn simple(&mut self) -> Result<Expr, ParseError> {
        self.skip_trivia();
        match self.peek() {
            Some(b'"') => Ok(Expr::Str(self.string()?)),
            Some(b'[') => {
                self.pos += 1;
                let mut items = Vec::new();
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b']') {
                        self.pos += 1;
                        return Ok(Expr::List(items));
                    }
                    items.push(self.selection()?);
                }
            }
            Some(b'{') => {
                self.pos += 1;
                let mut entries = AttrSet::new();
                loop {
                    self.skip_trivia();
                    if self.peek() == Some(b'}') {
                        self.pos += 1;
                        return Ok(Expr::Attrs(entries));
                    }
                    let key = self.key()?;
                    self.expect("=")?;
                    let value = self.expr()?;
                    self.expect(";")?;
                    if entries.insert(key, value).is_some() {
                        return Err(self.error("attribute already defined"));
                    }
                }
            }
            Some(b'(') => {
                self.pos += 1;
                let expr = self.expr()?;
                self.expect(")")?;
                Ok(expr)
            }
            Some(b'.') if self.peek_at(1) == Some(b'/') => {
                self.pos += 2;
                let start = self.pos;
                while self.peek().is_some_and(|b| {
                    b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-' | b'+')
                }) {
                    self.pos += 1;
                }
                RelPath::new(&self.source[start..self.pos])
                    .map(Expr::Path)
                    .map_err(|_| self.error("a path mix does not write"))
            }
            _ => {
                let name = self.identifier()?;
                match name {
                    "true" => Ok(Expr::Bool(true)),
                    "false" => Ok(Expr::Bool(false)),
                    _ => Ident::new(name)
                        .map(Expr::Var)
                        .map_err(|_| self.error("a keyword where a variable was expected")),
                }
            }
        }
    }

    fn key(&mut self) -> Result<Key, ParseError> {
        self.skip_trivia();
        if self.peek() == Some(b'"') {
            let parts = normalize_parts(self.string()?);
            return match parts.as_slice() {
                [] => Ok(Key::new_static("")),
                [StrPart::Lit(text)] => {
                    Key::new(text.as_str()).map_err(|_| self.error("a null byte in a key"))
                }
                _ => Err(self.error("an interpolated key")),
            };
        }
        let name = self.identifier()?;
        if Ident::new(name).is_err() {
            return Err(self.error("a keyword as a key"));
        }
        Key::new(name).map_err(|_| self.error("a null byte in a key"))
    }

    fn identifier(&mut self) -> Result<&'a str, ParseError> {
        self.skip_trivia();
        let start = self.pos;
        let end = identifier_end(self.bytes(), start);
        if end == start {
            return Err(self.error("expected an identifier"));
        }
        self.pos = end;
        Ok(&self.source[start..end])
    }

    fn string(&mut self) -> Result<Vec<StrPart>, ParseError> {
        self.expect("\"")?;
        let mut parts = Vec::new();
        let mut text = String::new();
        loop {
            let Some(c) = self.source[self.pos..].chars().next() else {
                return Err(self.error("unterminated string"));
            };
            match c {
                '"' => {
                    self.pos += 1;
                    break;
                }
                '\\' => {
                    self.pos += 1;
                    let escaped = self.next_char()?;
                    text.push(unescape(escaped));
                }
                '$' => match self.peek_at(1) {
                    Some(b'{') => {
                        self.pos += 2;
                        if !text.is_empty() {
                            parts.push(StrPart::Lit(self.lit(std::mem::take(&mut text))?));
                        }
                        let expr = self.expr()?;
                        self.expect("}")?;
                        parts.push(StrPart::Interp(expr));
                    }
                    Some(b'$') => {
                        self.pos += 2;
                        text.push_str("$$");
                    }
                    Some(b'\\') => {
                        self.pos += 2;
                        text.push('$');
                        let escaped = self.next_char()?;
                        text.push(unescape(escaped));
                    }
                    _ => {
                        self.pos += 1;
                        text.push('$');
                    }
                },
                '\r' => {
                    self.pos += 1;
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                    text.push('\n');
                }
                other => {
                    self.pos += other.len_utf8();
                    text.push(other);
                }
            }
        }
        if !text.is_empty() {
            parts.push(StrPart::Lit(self.lit(text)?));
        }
        Ok(parts)
    }

    fn next_char(&mut self) -> Result<char, ParseError> {
        let c = self.source[self.pos..]
            .chars()
            .next()
            .ok_or_else(|| self.error("unterminated escape"))?;
        self.pos += c.len_utf8();
        Ok(c)
    }

    fn lit(&self, text: String) -> Result<NixStr, ParseError> {
        NixStr::new(text).map_err(|_| self.error("a null byte in a string"))
    }
}

fn unescape(c: char) -> char {
    match c {
        'n' => '\n',
        'r' => '\r',
        't' => '\t',
        other => other,
    }
}

fn skip_space(bytes: &[u8], mut p: usize) -> usize {
    while bytes
        .get(p)
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\n' | b'\r'))
    {
        p += 1;
    }
    p
}

fn identifier_end(bytes: &[u8], start: usize) -> usize {
    match bytes.get(start) {
        Some(b) if b.is_ascii_alphabetic() || *b == b'_' => {}
        _ => return start,
    }
    let mut p = start + 1;
    while bytes
        .get(p)
        .is_some_and(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'\'' | b'-'))
    {
        p += 1;
    }
    p
}

#[cfg(test)]
mod tests {
    use super::*;

    fn round_trip(expr: Expr) {
        let printed = expr.print();
        let parsed = parse(&printed).unwrap_or_else(|e| panic!("{e}\n{printed}"));
        assert_eq!(normalize(parsed), normalize(expr), "{printed}");
    }

    fn key(s: &str) -> Key {
        Key::new(s).unwrap()
    }

    #[test]
    fn round_trips_strings_with_every_escape() {
        for text in [
            "",
            "a\"b\\c",
            "${x}",
            "$${x}",
            "end $",
            "a\rb\nc\td",
            "é 你",
        ] {
            round_trip(Expr::string(text).unwrap());
        }
    }

    #[test]
    fn round_trips_an_interpolation_after_a_dollar() {
        round_trip(Expr::Str(vec![
            StrPart::Lit(NixStr::new("a$$").unwrap()),
            StrPart::Interp(Expr::Path(RelPath::new("state").unwrap())),
            StrPart::Lit(NixStr::new(" $out").unwrap()),
        ]));
    }

    #[test]
    fn round_trips_nested_attrs_lists_and_quoted_keys() {
        round_trip(Expr::attrs([
            (
                key("in"),
                Expr::List(vec![Expr::Bool(true), Expr::attrs([])]),
            ),
            (key("a b"), Expr::attrs([(key(""), Expr::Bool(false))])),
            (key("or"), Expr::List(vec![])),
        ]));
    }

    #[test]
    fn round_trips_lambdas_applications_and_selections() {
        let f = Expr::Var(Ident::new("f").unwrap());
        let lambda = Expr::Lambda {
            formals: BTreeSet::from([Ident::new("a").unwrap(), Ident::new("b").unwrap()]),
            ellipsis: true,
            body: Box::new(Expr::apply(f.clone(), Expr::Bool(true))),
        };
        round_trip(Expr::apply(
            Expr::apply(f.clone(), lambda.clone()),
            Expr::select(
                Expr::Path(RelPath::new("p").unwrap()),
                key("x"),
                [key("y z")],
            ),
        ));
        round_trip(Expr::List(vec![lambda, Expr::apply(f.clone(), f)]));
        round_trip(Expr::apply(
            Expr::attrs([]),
            Expr::Path(RelPath::new("a-b").unwrap()),
        ));
    }

    #[test]
    fn parses_a_rendered_module_with_its_header() {
        let source = "# a comment\n{ pkgs, ... }: {\n  home = {\n    packages = [\n      pkgs.git\n    ];\n  };\n}\n";
        assert!(matches!(parse(source), Ok(Expr::Lambda { .. })));
    }

    #[test]
    fn parses_the_golden_files() {
        for source in [
            include_str!("../tests/golden/flake.nix"),
            include_str!("../tests/golden/home.nix"),
        ] {
            let parsed = parse(source).unwrap();
            assert_eq!(parsed.print() + "\n", source.split_once("\n\n").unwrap().1);
        }
    }

    #[test]
    fn rejects_what_nix_rejects() {
        assert!(parse("{ a = 1; a = 2; }").is_err());
        assert!(parse("{ a = true; a = false; }").is_err());
        assert!(parse("{ a, a }: a").is_err());
        assert!(parse("{ in = true; }").is_err());
        assert!(parse("{ or = true; }").is_err());
        assert!(parse("\"unterminated").is_err());
    }
}
