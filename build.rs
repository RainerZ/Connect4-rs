//! Embed build metadata for the About popup: date and git hash become
//! compile-time env vars (env!("BUILD_DATE") / env!("GIT_HASH")).
use std::process::Command;

fn run(cmd: &str, args: &[&str]) -> String {
    Command::new(cmd)
        .args(args)
        .output()
        .ok()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default()
}

fn main() {
    println!("cargo:rustc-env=BUILD_DATE={}", run("date", &["+%Y-%m-%d %H:%M"]));
    println!("cargo:rustc-env=GIT_HASH={}", run("git", &["rev-parse", "--short", "HEAD"]));
    println!("cargo:rerun-if-changed=.git/HEAD");
}
