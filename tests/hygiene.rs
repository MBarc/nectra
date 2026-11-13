//! The repository is public: local agent coordination files stay untracked.

#[test]
fn local_coordination_files_are_not_tracked() {
    let root = env!("CARGO_MANIFEST_DIR");
    let Ok(out) = std::process::Command::new("git")
        .args(["-C", root, "ls-files"])
        .output()
    else {
        return; // no git (a source tarball): nothing to check
    };
    if !out.status.success() {
        return;
    }
    let files = String::from_utf8_lossy(&out.stdout);
    for f in files.lines() {
        assert!(
            !(f.starts_with(".collab/")
                || matches!(
                    f,
                    "AGENT_HANDOFF.md"
                        | "REPORT.md"
                        | "RESTART-CHECKPOINT.md"
                        | ".intentionally-empty-file.o"
                )),
            "{f} is a local file and must stay untracked (see .gitignore)"
        );
    }
}
