use regex::Regex;
use std::{env, fs, process};

fn main() {
    let args: Vec<String> = env::args().collect();
    if args.len() != 2 {
        eprintln!("Usage: commit-msg <commit-msg-file>");
        process::exit(1);
    }

    let commit_msg = fs::read_to_string(&args[1])
        .unwrap_or_else(|_| {
            eprintln!("Failed to read commit message file");
            process::exit(1);
        });

    // Simple Conventional Commits regex: type(scope?): description
    let re = Regex::new(r"^(feat|fix|docs|style|refactor|perf|test|chore)(\([^\)]+\))?: .+")
        .unwrap();

    if !re.is_match(commit_msg.trim()) {
        eprintln!("\n❌ Commit message must follow Conventional Commits format.\n");
        eprintln!("Example: `feat(parser): add support for nested rules`");
        process::exit(1);
    }

    println!("✅ Commit message passes conventional commit check.");
}
