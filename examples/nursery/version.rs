// Shared by the firmware's and the simulator's build scripts (included with `include!`).

/// Export `NURSERY_GIT`: the short commit hash, with "-dirty" for uncommitted changes.
fn export_git_version() {
    let git = |args: &[&str]| {
        std::process::Command::new("git")
            .args(args)
            .output()
            .ok()
            .filter(|o| o.status.success())
            .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    };
    let hash = git(&["rev-parse", "--short", "HEAD"]).unwrap_or_else(|| "unknown".into());
    let dirty = git(&["status", "--porcelain"]).is_some_and(|s| !s.is_empty());
    println!("cargo:rustc-env=NURSERY_GIT={hash}{}", if dirty { "-dirty" } else { "" });
}
