//! Which provider CLI sees which skill, and linking them so every CLI does.
//!
//! Observed on Claude Code 2.1.302, Codex 0.162.1 and Gemini CLI 0.26.0: no CLI
//! reads another's own skill folder, but Codex and Gemini share `.agents/skills`.
//! Routing moves a task between CLIs, so a skill only one of them sees comes and
//! goes from turn to turn.
use anyhow::{bail, Context, Result};
use std::{
    collections::BTreeMap,
    fs,
    path::{Path, PathBuf},
};

/// Skill folders under a home or project directory, and the CLIs that read each.
const FOLDERS: [(&str, &[&str]); 4] = [
    (".claude/skills", &["claude"]),
    (".agents/skills", &["codex", "gemini"]),
    (".codex/skills", &["codex"]),
    (".gemini/skills", &["gemini"]),
];
pub const CLIS: [&str; 3] = ["claude", "codex", "gemini"];

#[derive(Debug, Default)]
pub struct Skill {
    pub seen_by: Vec<&'static str>,
    pub folders: Vec<PathBuf>,
}

/// Skills under `root`, by name. A skill is a directory holding SKILL.md;
/// dot-directories (Codex's bundled `.system`) are not the user's.
pub fn scan(root: &Path) -> Result<BTreeMap<String, Skill>> {
    let mut skills = BTreeMap::<String, Skill>::new();
    for (folder, clis) in FOLDERS {
        // Joined per component: `mklink` rejects mixed separators.
        let dir = folder.split('/').fold(root.to_path_buf(), |p, c| p.join(c));
        for path in entries(&dir)? {
            let name = path.file_name().unwrap_or_default().to_string_lossy();
            if name.starts_with('.') || !has_skill(&path)? {
                continue;
            }
            let skill = skills.entry(name.into_owned()).or_default();
            for cli in clis {
                if !skill.seen_by.contains(cli) {
                    skill.seen_by.push(cli);
                }
            }
            skill.folders.push(dir.clone());
        }
    }
    Ok(skills)
}

pub fn render(scopes: &[(&str, BTreeMap<String, Skill>)]) -> String {
    let mut out = format!(
        "{:<32} {:<8} {:<7} {:<7} {:<7}\n",
        "SKILL", "SCOPE", "CLAUDE", "CODEX", "GEMINI"
    );
    for (scope, skills) in scopes {
        for (name, skill) in skills {
            let seen = |cli| {
                if skill.seen_by.contains(&cli) {
                    "yes"
                } else {
                    "-"
                }
            };
            out += &format!(
                "{name:<32} {scope:<8} {:<7} {:<7} {:<7}\n",
                seen("claude"),
                seen("codex"),
                seen("gemini")
            );
        }
    }
    out
}

/// Links each user skill between `~/.claude/skills` and `~/.agents/skills`, so
/// Claude, Codex and Gemini all see it. Never replaces anything already there.
pub fn link(home: &Path) -> Result<Vec<String>> {
    let claude = home.join(".claude").join("skills");
    let shared = home.join(".agents").join("skills");
    let mut done = vec![];
    for (from, to) in [(&claude, &shared), (&shared, &claude)] {
        for target in entries(from)? {
            let name = target.file_name().unwrap_or_default();
            if name.to_string_lossy().starts_with('.') || !has_skill(&target)? {
                continue;
            }
            let link = to.join(name);
            if fs::symlink_metadata(&link).is_ok() {
                continue;
            }
            fs::create_dir_all(to)?;
            link_dir(&link, &target)?;
            done.push(format!("{} -> {}", link.display(), target.display()));
        }
    }
    Ok(done)
}

/// The entries of a skills folder; a folder that doesn't exist has none, but
/// any other failure (permissions, say) is reported rather than read as empty.
fn entries(dir: &Path) -> Result<Vec<PathBuf>> {
    match fs::read_dir(dir) {
        Ok(entries) => entries
            .map(|e| Ok(e?.path()))
            .collect::<Result<_>>()
            .with_context(|| format!("reading {}", dir.display())),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(vec![]),
        Err(e) => Err(e).with_context(|| format!("reading {}", dir.display())),
    }
}

fn has_skill(dir: &Path) -> Result<bool> {
    let file = dir.join("SKILL.md");
    match fs::metadata(&file) {
        Ok(m) => Ok(m.is_file()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e).with_context(|| format!("reading {}", file.display())),
    }
}

fn link_dir(link: &Path, target: &Path) -> Result<()> {
    #[cfg(windows)]
    {
        // A junction needs neither admin rights nor developer mode; a symlink does.
        // The paths travel as environment variables, which PowerShell never parses
        // as code, so a folder named `x&del ...` stays a name (`cmd /c mklink` ran it).
        let status = std::process::Command::new("powershell")
            .args([
                "-NoProfile",
                "-NonInteractive",
                "-Command",
                "New-Item -ItemType Junction -Path $env:NECTRA_LINK -Value $env:NECTRA_TARGET -ErrorAction Stop | Out-Null",
            ])
            .env("NECTRA_LINK", link)
            .env("NECTRA_TARGET", target)
            .stdout(std::process::Stdio::null())
            .status()?;
        if !status.success() {
            bail!("could not link {} to {}", link.display(), target.display());
        }
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link)
        .with_context(|| format!("linking {} to {}", link.display(), target.display()))?;
    #[cfg(not(any(windows, unix)))]
    bail!("linking skills is not supported on this platform");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn skill(root: &Path, folder: &str, name: &str) {
        let dir = root.join(folder).join(name);
        fs::create_dir_all(&dir).unwrap();
        fs::write(dir.join("SKILL.md"), "---\nname: x\ndescription: y\n---\n").unwrap();
    }

    #[test]
    fn linking_makes_every_user_skill_visible_to_every_cli() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        skill(home, ".claude/skills", "review");
        skill(home, ".agents/skills", "deploy");
        skill(home, ".codex/skills/.system", "bundled");
        fs::create_dir_all(home.join(".agents/skills/notes")).unwrap(); // no SKILL.md
        let before = scan(home).unwrap();
        assert_eq!(before.keys().collect::<Vec<_>>(), ["deploy", "review"]);
        assert_eq!(before["review"].seen_by, ["claude"]);
        assert_eq!(before["deploy"].seen_by, ["codex", "gemini"]);

        assert_eq!(link(home).unwrap().len(), 2);
        let after = scan(home).unwrap();
        for name in ["deploy", "review"] {
            assert_eq!(after[name].seen_by, ["claude", "codex", "gemini"], "{name}");
        }
        // Idempotent, and an existing entry is never replaced.
        assert!(link(home).unwrap().is_empty());
        assert!(home.join(".agents/skills/review/SKILL.md").is_file());
    }

    #[test]
    fn a_skill_name_is_never_run_as_a_command() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        // Under `cmd /c mklink` this created `pwned` and expanded %OS%.
        let name = "x&mkdir pwned&rem %OS%";
        skill(home, ".claude/skills", name);
        assert_eq!(link(home).unwrap().len(), 1);
        assert!(home
            .join(".agents/skills")
            .join(name)
            .join("SKILL.md")
            .is_file());
        assert!(!home.join("pwned").exists() && !Path::new("pwned").exists());
    }

    #[test]
    fn an_existing_name_is_left_alone() {
        let home = tempfile::tempdir().unwrap();
        let home = home.path();
        skill(home, ".claude/skills", "same");
        fs::create_dir_all(home.join(".agents/skills/same")).unwrap();
        fs::write(home.join(".agents/skills/same/keep.txt"), "mine").unwrap();
        assert!(link(home).unwrap().is_empty());
        assert_eq!(
            fs::read_to_string(home.join(".agents/skills/same/keep.txt")).unwrap(),
            "mine"
        );
    }
}
