use std::collections::{BTreeMap, BTreeSet};

use arbitrary::Arbitrary;
use mix_nixgen::Ident;
use mix_nixgen::ast::{Expr, Key, NixStr, RelPath, StrPart};

pub const NAMES: &[&str] = &["a", "b", "f", "pkgs", "home-manager", "x'", "_y"];
const PATHS: &[&str] = &["state", "home.nix", "a-b", "x.y.z"];
const MAX_DEPTH: usize = 8;

#[derive(Debug, Arbitrary)]
pub enum ArbExpr {
    Str(Vec<ArbPart>),
    Bool(bool),
    List(Vec<ArbExpr>),
    Attrs(Vec<(String, ArbExpr)>),
    Var(u8),
    Select(Box<ArbExpr>, String, Vec<String>),
    Apply(Box<ArbExpr>, Box<ArbExpr>),
    Lambda(Vec<u8>, bool, Box<ArbExpr>),
    Path(u8),
}

#[derive(Debug, Arbitrary)]
pub enum ArbPart {
    Lit(String),
    Interp(ArbExpr),
}

fn text(raw: String) -> String {
    raw.chars().filter(|c| *c != '\0').collect()
}

fn name(index: u8) -> Ident {
    Ident::new(NAMES[usize::from(index) % NAMES.len()]).expect("every name is an identifier")
}

impl ArbExpr {
    pub fn build(self) -> Expr {
        self.build_at(0)
    }

    fn build_at(self, depth: usize) -> Expr {
        if depth > MAX_DEPTH {
            return Expr::Bool(false);
        }
        let deeper = depth + 1;
        match self {
            ArbExpr::Str(parts) => Expr::Str(
                parts
                    .into_iter()
                    .map(|part| match part {
                        ArbPart::Lit(raw) => StrPart::Lit(NixStr::new(text(raw)).unwrap()),
                        ArbPart::Interp(inner) => StrPart::Interp(inner.build_at(deeper)),
                    })
                    .collect(),
            ),
            ArbExpr::Bool(value) => Expr::Bool(value),
            ArbExpr::List(items) => {
                Expr::List(items.into_iter().map(|i| i.build_at(deeper)).collect())
            }
            ArbExpr::Attrs(entries) => Expr::Attrs(
                entries
                    .into_iter()
                    .map(|(k, v)| (Key::new(text(k)).unwrap(), v.build_at(deeper)))
                    .collect::<BTreeMap<_, _>>(),
            ),
            ArbExpr::Var(index) => Expr::Var(name(index)),
            ArbExpr::Select(base, first, rest) => Expr::select(
                base.build_at(deeper),
                Key::new(text(first)).unwrap(),
                rest.into_iter().map(|k| Key::new(text(k)).unwrap()),
            ),
            ArbExpr::Apply(function, argument) => {
                Expr::apply(function.build_at(deeper), argument.build_at(deeper))
            }
            ArbExpr::Lambda(formals, ellipsis, body) => Expr::Lambda {
                formals: formals.into_iter().map(name).collect::<BTreeSet<_>>(),
                ellipsis,
                body: Box::new(body.build_at(deeper)),
            },
            ArbExpr::Path(index) => Expr::Path(
                RelPath::new(PATHS[usize::from(index) % PATHS.len()])
                    .expect("every path is a relative path"),
            ),
        }
    }
}

pub fn is_data(expr: &Expr) -> bool {
    match expr {
        Expr::Str(parts) => parts.iter().all(|part| match part {
            StrPart::Lit(_) => true,
            StrPart::Interp(inner) => matches!(inner, Expr::Str(_)) && is_data(inner),
        }),
        Expr::Bool(_) => true,
        Expr::List(items) => items.iter().all(is_data),
        Expr::Attrs(entries) => entries.values().all(is_data),
        _ => false,
    }
}

pub fn model(expr: &Expr) -> serde_json::Value {
    match expr {
        Expr::Str(parts) => serde_json::Value::String(concatenate(parts)),
        Expr::Bool(value) => serde_json::Value::Bool(*value),
        Expr::List(items) => serde_json::Value::Array(items.iter().map(model).collect()),
        Expr::Attrs(entries) => serde_json::Value::Object(
            entries
                .iter()
                .map(|(k, v)| (k.as_str().to_owned(), model(v)))
                .collect(),
        ),
        _ => unreachable!("only data has a model"),
    }
}

fn concatenate(parts: &[StrPart]) -> String {
    parts
        .iter()
        .map(|part| match part {
            StrPart::Lit(text) => text.as_str().to_owned(),
            StrPart::Interp(Expr::Str(inner)) => concatenate(inner),
            StrPart::Interp(_) => unreachable!("only strings are interpolated in data"),
        })
        .collect()
}
