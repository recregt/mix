use std::path::PathBuf;

use arbitrary::{Arbitrary, Unstructured};
use mix_nixgen::ast::Expr;
use mix_nixgen_fuzz::generate::{ArbExpr, NAMES, is_data, model};
use mix_nixgen_fuzz::nix;

const BATCH: usize = 500;

fn syntax_file(exprs: &[&Expr]) -> String {
    let mut out = format!("{{ {}, ... }}: [\n", NAMES.join(", "));
    for expr in exprs {
        out.push('(');
        out.push_str(&expr.print());
        out.push_str(")\n");
    }
    out.push(']');
    out
}

fn data_file(exprs: &[&Expr]) -> String {
    let mut out = String::from("builtins.toJSON [\n");
    for expr in exprs {
        out.push('(');
        out.push_str(&expr.print());
        out.push_str(")\n");
    }
    out.push(']');
    out
}

fn parses(exprs: &[&Expr]) -> bool {
    nix::parse(&syntax_file(exprs)).status.success()
}

fn evaluates_to_its_model(exprs: &[&Expr]) -> bool {
    let output = nix::eval_raw_file(&data_file(exprs));
    if !output.status.success() {
        return false;
    }
    let Ok(values) = serde_json::from_slice::<Vec<serde_json::Value>>(&output.stdout) else {
        return false;
    };
    values.len() == exprs.len() && values.iter().zip(exprs).all(|(v, e)| *v == model(e))
}

fn failures<'a>(exprs: &[&'a Expr], check: fn(&[&Expr]) -> bool) -> Vec<&'a Expr> {
    if exprs.is_empty() || check(exprs) {
        return Vec::new();
    }
    if exprs.len() == 1 {
        return vec![exprs[0]];
    }
    let (left, right) = exprs.split_at(exprs.len() / 2);
    let mut found = failures(left, check);
    found.extend(failures(right, check));
    found
}

fn main() {
    let corpus_dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("corpus/printer"));

    let mut exprs = Vec::new();
    for entry in std::fs::read_dir(&corpus_dir).expect("reading the corpus directory") {
        let Ok(entry) = entry else { continue };
        let Ok(bytes) = std::fs::read(entry.path()) else {
            continue;
        };
        if let Ok(input) = ArbExpr::arbitrary_take_rest(Unstructured::new(&bytes)) {
            exprs.push(input.build());
        }
    }

    let all: Vec<&Expr> = exprs.iter().collect();
    let data: Vec<&Expr> = exprs.iter().filter(|e| is_data(e)).collect();
    let mut bad_syntax = Vec::new();
    for batch in all.chunks(BATCH) {
        bad_syntax.extend(failures(batch, parses));
    }
    let mut bad_data = Vec::new();
    for batch in data.chunks(BATCH) {
        bad_data.extend(failures(batch, evaluates_to_its_model));
    }

    for expr in &bad_syntax {
        eprintln!("=== NIX DID NOT PARSE ===\n{}", expr.print());
    }
    for expr in &bad_data {
        eprintln!("=== NIX EVALUATED SOMETHING ELSE ===\n{}", expr.print());
    }
    println!(
        "checked {} expressions: {} failed to parse; {} data expressions: {} evaluated differently",
        all.len(),
        bad_syntax.len(),
        data.len(),
        bad_data.len()
    );
    if !bad_syntax.is_empty() || !bad_data.is_empty() {
        std::process::exit(1);
    }
}
