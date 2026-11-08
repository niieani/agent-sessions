use std::process::Command;

fn git(args: &[&str]) -> String {
    let output = Command::new("git")
        .args(args)
        .output()
        .expect("git build identity");
    assert!(output.status.success(), "git build identity failed");
    String::from_utf8(output.stdout)
        .expect("git UTF-8 output")
        .trim()
        .to_owned()
}

fn main() {
    for path in ["src", "Cargo.toml", "Cargo.lock", "build.rs"] {
        println!("cargo:rerun-if-changed={path}");
    }
    for path in [
        "HEAD".to_owned(),
        git(&["rev-parse", "--symbolic-full-name", "HEAD"]),
    ] {
        println!(
            "cargo:rerun-if-changed={}",
            git(&["rev-parse", "--git-path", &path])
        );
    }
    let mut identity = git(&["rev-parse", "--short=12", "HEAD"]);
    let diff = git(&[
        "diff",
        "HEAD",
        "--",
        "src",
        "Cargo.toml",
        "Cargo.lock",
        "build.rs",
    ]);
    if !diff.is_empty() {
        let fingerprint = diff.bytes().fold(0xcbf29ce484222325_u64, |h, b| {
            (h ^ u64::from(b)).wrapping_mul(0x100000001b3)
        });
        identity.push_str(&format!("-dirty-{fingerprint:016x}"));
    }
    println!("cargo:rustc-env=ASESSIONS_BUILD_HASH={identity}");
    let target = std::env::var("TARGET").expect("cargo sets TARGET for build scripts");
    println!("cargo:rustc-env=ASESSIONS_BUILD_TARGET={target}");
}
