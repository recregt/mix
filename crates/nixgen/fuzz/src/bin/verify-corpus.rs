use std::io::Write as _;
use std::path::PathBuf;

use mix_nixgen::ast::Expr;
use mix_nixgen::parse::parse;
use mix_nixgen_fuzz::nix;

fn main() {
    let corpus_dir: PathBuf = std::env::args()
        .nth(1)
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("corpus/parse"));

    if !corpus_dir.exists() {
        eprintln!("Corpus directory does not exist: {}", corpus_dir.display());
        std::process::exit(1);
    }

    let mut exprs: Vec<(PathBuf, Expr)> = Vec::new();
    let mut total_files = 0;
    let mut parsed_by_nixgen = 0;

    print!("Loading corpus files from {}... ", corpus_dir.display());
    std::io::stdout().flush().unwrap();

    for entry in std::fs::read_dir(&corpus_dir).expect("reading the corpus directory") {
        let Ok(entry) = entry else { continue };
        let path = entry.path();
        if !path.is_file() {
            continue;
        }
        total_files += 1;
        let Ok(content) = std::fs::read_to_string(&path) else {
            continue;
        };
        if let Ok(ast) = parse(&content) {
            parsed_by_nixgen += 1;
            exprs.push((path, ast));
        }
    }

    println!(
        "done.\nTotal files: {}, Parsed by mix-nixgen: {}",
        total_files, parsed_by_nixgen
    );

    if exprs.is_empty() {
        println!("No expressions to verify.");
        return;
    }

    println!(
        "Verifying {} expressions against official Nix evaluator...",
        exprs.len()
    );

    let mut failed = 0;
    let mut passed = 0;
    let total = exprs.len();

    for (i, (path, expr)) in exprs.iter().enumerate() {
        let printed = expr.print();
        let output = nix::parse(&printed);
        let stderr = String::from_utf8_lossy(&output.stderr);

        // Nix parses syntax successfully if it succeeds OR only complains about undefined variables.
        // A true grammar failure is "syntax error".
        let is_valid_syntax = output.status.success() || stderr.contains("undefined variable");

        if is_valid_syntax {
            passed += 1;
        } else {
            failed += 1;
            if failed <= 5 {
                eprintln!(
                    "\n❌ [REAL SYNTAX ERROR #{}] File: {}",
                    failed,
                    path.file_name().unwrap().to_string_lossy()
                );
                eprintln!("--- Nix Error ---");
                eprintln!("{}", stderr.trim());
                eprintln!("--- Printed Nix Code ---");
                eprintln!("{}", printed.trim());
                eprintln!("-----------------");
            }
        }

        if (i + 1) % 100 == 0 || i + 1 == total {
            print!(
                "\rProgress: [{}/{}] (Passed: {}, Failed: {})",
                i + 1,
                total,
                passed,
                failed
            );
            std::io::stdout().flush().unwrap();
        }
    }

    println!(
        "\n\nFinished verification: {} passed, {} failed out of {}.",
        passed, failed, total
    );

    if failed > 5 {
        println!("(Only first 5 failures shown above to avoid flooding terminal)");
    }

    if failed > 0 {
        std::process::exit(1);
    }
}
