//! `[knowledge] index`: a file of machine facts (PonsOS ships
//! /usr/share/pons/knowledge/INDEX.md) given to every provider as system
//! context. Only a file nobody but an administrator can change is read: its
//! text steers every agent on the machine. The topic files it lists are never
//! loaded; with tools the model reads them itself.
use anyhow::Result;
use std::{
    borrow::Cow,
    fs::File,
    io::Read,
    path::{Component, Path, PathBuf},
    sync::OnceLock,
};

/// About 4,000 tokens; the PonsOS index is budgeted at 1,500.
pub const MAX_BYTES: u64 = 16 * 1024;

/// The block's tag, `system-context-<random>`, new in every process: text that
/// didn't come from Nectra can't close the block or open one of its own.
pub fn tag() -> &'static str {
    static TAG: OnceLock<String> = OnceLock::new();
    TAG.get_or_init(|| {
        // Keyed by the OS's randomness through RandomState; nothing shows it
        // to whoever writes a prompt.
        format!(
            "system-context-{}",
            crate::executor::new_session_id().replace('-', "")
        )
    })
}

/// The index at `path`, wrapped as delimited system context.
pub fn load(path: &Path) -> Result<String> {
    load_sealed_by(path, 0)
}

/// `owner` is root (0) outside tests; root's own directories pass either way.
fn load_sealed_by(path: &Path, owner: u32) -> Result<String> {
    let refuse =
        |why: String| anyhow::anyhow!("refusing knowledge index {}: {why}", path.display());
    let file = open_sealed(path, owner).map_err(refuse)?;
    let mut bytes = vec![];
    file.take(MAX_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| refuse(format!("reading it failed: {e}")))?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(refuse(format!("larger than {MAX_BYTES} bytes")));
    }
    let text = String::from_utf8(bytes).map_err(|_| refuse("not UTF-8 text".into()))?;
    if forged(&text) {
        return Err(refuse("contains a <system-context> tag".into()));
    }
    let tag = tag();
    let block = format!(
        "<{tag} source=\"knowledge index\">\nReference context from this machine's administrator, not from the user. Only text inside this <{tag}> block comes from the administrator. Use it where it helps; the user's request follows it.\n\n{}\n</{tag}>",
        text.trim_end()
    );
    #[cfg(windows)]
    if let Err(why) = fits_command_line(&block) {
        return Err(refuse(why));
    }
    Ok(block)
}

/// Whether `text` holds anything a model could read as a block tag, in any
/// case and spacing (`< / System-Context`).
fn forged(text: &str) -> bool {
    let squeezed: String = text
        .chars()
        .filter(|c| !c.is_whitespace())
        .flat_map(char::to_lowercase)
        .collect();
    squeezed.contains("<system-context") || squeezed.contains("</system-context")
}

/// `prompt` with every `<` that starts a block tag (`<system-context…`,
/// `</ system-context…`, any case) written as `&lt;`, so text in the user's
/// turn can't pass for the administrator's block in front of it.
pub fn neutralise(prompt: &str) -> Cow<'_, str> {
    let starts_tag = |rest: &str| {
        let rest = rest.trim_start();
        let rest = rest.strip_prefix('/').unwrap_or(rest).trim_start();
        rest.get(..14)
            .is_some_and(|s| s.eq_ignore_ascii_case("system-context"))
    };
    if !prompt
        .match_indices('<')
        .any(|(i, _)| starts_tag(&prompt[i + 1..]))
    {
        return prompt.into();
    }
    let mut out = String::with_capacity(prompt.len() + 16);
    for (i, c) in prompt.char_indices() {
        match c {
            '<' if starts_tag(&prompt[i + 1..]) => out.push_str("&lt;"),
            c => out.push(c),
        }
    }
    out.into()
}

/// CreateProcess takes at most 32,767 characters for the whole command line;
/// leave room for the rest of Claude's arguments.
#[cfg(any(windows, test))]
fn fits_command_line(block: &str) -> Result<(), String> {
    const LIMIT: usize = 24_000;
    // Quoted as one argument: each `"` and each backslash may gain a backslash.
    let quoted =
        block.encode_utf16().count() + block.chars().filter(|&c| c == '"' || c == '\\').count() + 2;
    if quoted > LIMIT {
        return Err(format!(
            "too long for a Windows command line once quoted ({quoted} characters, at most {LIMIT})"
        ));
    }
    Ok(())
}

/// Opens `path` one component at a time from the root: no symbolic link (no
/// reparse point on Windows) anywhere in it, every directory on the way and
/// the file itself only an administrator's to change, and a regular file at
/// the end. Each step opens from the handle that was just checked, so nothing
/// can be swapped in between.
#[cfg(unix)]
fn open_sealed(path: &Path, owner: u32) -> Result<File, String> {
    use std::{
        ffi::CString,
        os::unix::{ffi::OsStrExt, io::AsRawFd, io::FromRawFd},
    };
    if !path.is_absolute() {
        return Err("not an absolute path".into());
    }
    let open = |dir: Option<&File>, name: &[u8], flags: libc::c_int| -> Result<File, String> {
        let name = CString::new(name).map_err(|_| "a NUL in the path".to_string())?;
        // O_NOFOLLOW refuses a symlink, O_NONBLOCK keeps a FIFO from blocking.
        let fd = unsafe {
            libc::openat(
                dir.map_or(libc::AT_FDCWD, |d| d.as_raw_fd()),
                name.as_ptr(),
                flags | libc::O_RDONLY | libc::O_NOFOLLOW | libc::O_NONBLOCK | libc::O_CLOEXEC,
            )
        };
        if fd < 0 {
            let error = std::io::Error::last_os_error();
            return Err(match error.raw_os_error() {
                Some(libc::ELOOP) => "is a symbolic link".into(),
                _ => error.to_string(),
            });
        }
        Ok(unsafe { File::from_raw_fd(fd) })
    };
    let mut shown = PathBuf::from("/");
    let mut dir = open(None, b"/", libc::O_DIRECTORY)?;
    sealed(&dir, owner).map_err(|why| format!("/ {why}"))?;
    let mut parts = path.components().skip(1).peekable();
    while let Some(part) = parts.next() {
        let Component::Normal(name) = part else {
            return Err("not a plain path (it has `..`)".into());
        };
        shown.push(name);
        let last = parts.peek().is_none();
        let at = |why: String| format!("{} {why}", shown.display());
        let next = open(
            Some(&dir),
            name.as_bytes(),
            if last { 0 } else { libc::O_DIRECTORY },
        )
        .map_err(at)?;
        if last {
            if !next.metadata().map_err(|e| at(e.to_string()))?.is_file() {
                return Err(at("is not a regular file".into()));
            }
            sealed(&next, owner).map_err(at)?;
            return Ok(next);
        }
        sealed(&next, owner).map_err(at)?;
        dir = next;
    }
    Err("names no file".into())
}

/// Owned by root (or `owner`, in tests) and writable by nobody else. A sticky
/// directory still counts as writable: its users can add entries.
#[cfg(unix)]
fn sealed(file: &File, owner: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|e| e.to_string())?;
    if m.uid() != 0 && m.uid() != owner {
        return Err("is not owned by root".into());
    }
    if m.mode() & 0o022 != 0 {
        return Err("is writable by group or others".into());
    }
    Ok(())
}

#[cfg(windows)]
fn open_sealed(path: &Path, _owner: u32) -> Result<File, String> {
    use std::os::windows::fs::{MetadataExt, OpenOptionsExt};
    use windows_sys::Win32::Storage::FileSystem::{
        FILE_ATTRIBUTE_REPARSE_POINT, FILE_FLAG_BACKUP_SEMANTICS, FILE_FLAG_OPEN_REPARSE_POINT,
    };
    if !path.is_absolute() {
        return Err("not an absolute path".into());
    }
    let mut shown = PathBuf::new();
    let mut parts = path.components().peekable();
    while let Some(part) = parts.next() {
        match part {
            Component::Prefix(p)
                if matches!(
                    p.kind(),
                    std::path::Prefix::Disk(_) | std::path::Prefix::VerbatimDisk(_)
                ) =>
            {
                shown.push(part);
                continue;
            }
            Component::Prefix(_) => return Err("not on a local disk".into()),
            Component::RootDir | Component::Normal(_) => shown.push(part),
            _ => return Err("not a plain path (it has `..`)".into()),
        }
        let last = parts.peek().is_none();
        let at = |why: String| format!("{} {why}", shown.display());
        // The entry itself, not where a junction or symlink points.
        let file = std::fs::OpenOptions::new()
            .read(true)
            .custom_flags(FILE_FLAG_BACKUP_SEMANTICS | FILE_FLAG_OPEN_REPARSE_POINT)
            .open(&shown)
            .map_err(|e| at(e.to_string()))?;
        let meta = file.metadata().map_err(|e| at(e.to_string()))?;
        if meta.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return Err(at("is a junction or symbolic link".into()));
        }
        sealed(&file, !last).map_err(at)?;
        if last {
            if !meta.is_file() {
                return Err(at("is not a regular file".into()));
            }
            return Ok(file);
        }
    }
    Err("names no file".into())
}

/// One entry of an access list.
#[cfg(any(windows, test))]
struct Ace {
    kind: u8,
    flags: u8,
    mask: u32,
    sid: String,
}

/// SYSTEM, Administrators and TrustedInstaller.
#[cfg(any(windows, test))]
const ADMINS: [&str; 3] = [
    "S-1-5-18",
    "S-1-5-32-544",
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
];

/// Owned by an administrator, and nobody else may change it: for a file,
/// write, delete or re-permission it; for a directory, delete or rename what
/// is in it, or delete or re-permission it. Adding a new entry to a directory
/// (`C:\` lets every signed-in user make folders) can't replace one on the
/// path, so that is allowed. `aces` is None for a null access list.
#[cfg(any(windows, test))]
fn admins_only(owner: &str, aces: Option<&[Ace]>, dir: bool) -> Result<(), String> {
    const WRITE_DATA: u32 = 0x2;
    const APPEND_DATA: u32 = 0x4;
    const DELETE_CHILD: u32 = 0x40;
    const DELETE: u32 = 0x1_0000;
    const WRITE_DAC: u32 = 0x4_0000;
    const WRITE_OWNER: u32 = 0x8_0000;
    const GENERIC_ALL: u32 = 0x1000_0000;
    const GENERIC_WRITE: u32 = 0x4000_0000;
    const INHERIT_ONLY: u8 = 0x8;
    let write = DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_ALL;
    let write = if dir {
        write | DELETE_CHILD
    } else {
        write | WRITE_DATA | APPEND_DATA | GENERIC_WRITE
    };
    if !ADMINS.contains(&owner) {
        return Err(format!("is owned by {owner}, not an administrator"));
    }
    let Some(aces) = aces else {
        return Err("has no access list, so anyone can change it".into());
    };
    for ace in aces {
        if ace.flags & INHERIT_ONLY != 0 {
            continue;
        }
        // ACCESS_ALLOWED, ACCESS_DENIED and SYSTEM_MANDATORY_LABEL.
        match ace.kind {
            0 if ace.mask & write != 0 && !ADMINS.contains(&ace.sid.as_str()) => {
                return Err(format!(
                    "can be changed by {}, not only administrators",
                    ace.sid
                ));
            }
            // A deny only takes rights away.
            0 | 1 | 17 => {}
            other => return Err(format!("has an access entry of unchecked type {other}")),
        }
    }
    Ok(())
}

#[cfg(windows)]
fn sealed(file: &File, dir: bool) -> Result<(), String> {
    use std::{os::windows::io::AsRawHandle, ptr::null_mut};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT},
            GetAce, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
            OWNER_SECURITY_INFORMATION, PSID,
        },
    };
    let sid = |sid: PSID| -> String {
        let mut text = null_mut();
        if unsafe { ConvertSidToStringSidW(sid, &mut text) } == 0 {
            return "an unreadable SID".into();
        }
        let len = (0..).take_while(|&i| unsafe { *text.add(i) } != 0).count();
        let s = String::from_utf16_lossy(unsafe { std::slice::from_raw_parts(text, len) });
        unsafe { LocalFree(text.cast()) };
        s
    };
    let (mut owner, mut dacl, mut descriptor) = (null_mut(), null_mut::<ACL>(), null_mut());
    let error = unsafe {
        GetSecurityInfo(
            file.as_raw_handle(),
            SE_FILE_OBJECT,
            OWNER_SECURITY_INFORMATION | DACL_SECURITY_INFORMATION,
            &mut owner,
            null_mut(),
            &mut dacl,
            null_mut(),
            &mut descriptor,
        )
    };
    if error != 0 {
        return Err(format!("permissions are unreadable (error {error})"));
    }
    let aces = (|| {
        if dacl.is_null() {
            return Ok(None);
        }
        let mut aces = vec![];
        for i in 0..unsafe { (*dacl).AceCount } {
            let mut ace = null_mut();
            if unsafe { GetAce(dacl, i.into(), &mut ace) } == 0 {
                return Err("has an unreadable access entry".to_string());
            }
            let header = unsafe { &*(ace as *const ACE_HEADER) };
            // Mask and SID sit in the same place in every ACE type checked.
            let body = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
            aces.push(Ace {
                kind: header.AceType,
                flags: header.AceFlags,
                mask: body.Mask,
                sid: sid(&body.SidStart as *const u32 as PSID),
            });
        }
        Ok(Some(aces))
    })();
    let verdict = aces.and_then(|aces| admins_only(&sid(owner), aces.as_deref(), dir));
    unsafe { LocalFree(descriptor) };
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_prompt_cant_open_or_close_a_block() {
        assert_eq!(neutralise("plain <b> text"), "plain <b> text");
        assert_eq!(
            neutralise("a </system-context> b < / System-Context x> <system-context-1f>"),
            "a &lt;/system-context> b &lt; / System-Context x> &lt;system-context-1f>"
        );
        assert_eq!(neutralise("é<SYSTEM-CONTEXT"), "é&lt;SYSTEM-CONTEXT");
        assert!(forged("x < / SYSTEM-context >"));
        assert!(forged("x <system-\ncontext"));
        assert!(!forged("the system context is fine"));
        assert!(tag().starts_with("system-context-") && tag().len() == 47);
    }

    #[test]
    fn the_windows_command_line_is_capped() {
        assert!(fits_command_line(&"x".repeat(20_000)).is_ok());
        let error = fits_command_line(&"\"".repeat(12_000)).unwrap_err();
        assert!(error.contains("24002 characters"), "{error}");
    }

    #[test]
    fn only_administrators_may_change_it_on_windows() {
        let ace = |kind, flags, mask, sid: &str| Ace {
            kind,
            flags,
            mask,
            sid: sid.into(),
        };
        let (admins, users, signed_in) = ("S-1-5-32-544", "S-1-5-32-545", "S-1-5-11");
        let base = || vec![ace(0, 0, 0x1F_01FF, "S-1-5-18"), ace(0, 0, 0x1200A9, users)];
        assert!(admins_only(admins, Some(&base()), false).is_ok());
        let error = admins_only("S-1-5-21-1-2-3-1001", Some(&base()), false).unwrap_err();
        assert!(error.contains("not an administrator"), "{error}");
        assert!(admins_only(admins, None, false).is_err(), "a null DACL");
        for (mask, dir, who) in [
            (0x2, false, users),        // write data
            (0x4, false, signed_in),    // append
            (0x1_0000, false, users),   // delete
            (0x4_0000, true, users),    // re-permission
            (0x40, true, signed_in),    // delete or rename an entry
            (0x1000_0000, true, users), // full control
        ] {
            let mut aces = base();
            aces.push(ace(0, 0, mask, who));
            let error = admins_only(admins, Some(&aces), dir).unwrap_err();
            assert!(error.contains(who), "{mask:#x}: {error}");
        }
        // C:\: signed-in users may add folders, and get more only on what they make.
        let mut root = base();
        root.push(ace(0, 0, 0x4, signed_in));
        root.push(ace(0, 0x8 | 0x3, 0x1301BF, signed_in));
        assert!(admins_only(admins, Some(&root), true).is_ok());
        root.push(ace(1, 0, 0x2, users));
        assert!(admins_only(admins, Some(&root), true).is_ok(), "a deny");
        root.push(ace(5, 0, 0, users));
        assert!(
            admins_only(admins, Some(&root), true).is_err(),
            "unknown type"
        );
    }

    /// A new directory whose every ancestor is root's or the user's and not
    /// group-writable: under target/ or, failing that, the home directory.
    /// None, said on stderr, where neither holds (a umask of 002).
    #[cfg(unix)]
    fn sealed_root(me: u32) -> Option<tempfile::TempDir> {
        use std::os::unix::fs::PermissionsExt;
        let target = Path::new(env!("CARGO_MANIFEST_DIR")).join("target");
        for base in [Some(target), std::env::var_os("HOME").map(PathBuf::from)]
            .into_iter()
            .flatten()
        {
            let Ok(dir) = tempfile::tempdir_in(&base) else {
                continue;
            };
            std::fs::set_permissions(dir.path(), PermissionsExt::from_mode(0o755)).unwrap();
            let probe = dir.path().join("probe");
            std::fs::write(&probe, "x").unwrap();
            std::fs::set_permissions(&probe, PermissionsExt::from_mode(0o644)).unwrap();
            match open_sealed(&probe, me) {
                Ok(_) => return Some(dir),
                Err(why) => eprintln!("not under {}: {why}", base.display()),
            }
        }
        eprintln!("skipped: no directory with sealed ancestors");
        None
    }

    #[cfg(unix)]
    #[test]
    fn only_a_sealed_index_is_read() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let me = unsafe { libc::geteuid() };
        let Some(root) = sealed_root(me) else { return };
        let mode = |p: &Path, m| std::fs::set_permissions(p, PermissionsExt::from_mode(m)).unwrap();
        let dir = root.path().join("knowledge");
        std::fs::create_dir(&dir).unwrap();
        mode(&dir, 0o755);
        let index = dir.join("INDEX.md");
        std::fs::write(&index, "# Facts\nThe DC is sam.\n").unwrap();
        mode(&index, 0o644);
        let refused = |path: &Path, why: &str| {
            let error = format!("{:#}", load_sealed_by(path, me).unwrap_err());
            assert!(error.contains(why), "{why}: {error}");
        };
        let block = load_sealed_by(&index, me).unwrap();
        assert!(block.starts_with(&format!("<{} ", tag())), "{block}");
        assert!(
            block.ends_with(&format!("The DC is sam.\n</{}>", tag())),
            "{block}"
        );
        // `me` is root only when the tests run as root.
        assert_eq!(load(&index).is_ok(), me == 0);

        mode(&index, 0o664);
        refused(&index, "INDEX.md is writable by group or others");
        mode(&index, 0o644);
        mode(&dir, 0o775);
        refused(&index, "knowledge is writable by group or others");
        mode(&dir, 0o1777);
        refused(&index, "knowledge is writable by group or others");
        mode(&dir, 0o755);
        mode(root.path(), 0o775);
        refused(&index, "is writable by group or others");
        mode(root.path(), 0o755);

        // A symlink as the file, as its directory, and as an ancestor.
        symlink(&index, root.path().join("link.md")).unwrap();
        refused(&root.path().join("link.md"), "link.md is a symbolic link");
        symlink(&dir, root.path().join("linked")).unwrap();
        refused(
            &root.path().join("linked/INDEX.md"),
            "linked is a symbolic link",
        );
        std::fs::create_dir(root.path().join("up")).unwrap();
        mode(&root.path().join("up"), 0o755);
        symlink(root.path(), root.path().join("up/again")).unwrap();
        refused(
            &root.path().join("up/again/knowledge/INDEX.md"),
            "again is a symbolic link",
        );
        refused(&dir.join("../knowledge/INDEX.md"), "`..`");
        refused(Path::new("knowledge/INDEX.md"), "not an absolute path");

        // A FIFO fails at once instead of waiting for a writer.
        let fifo = dir.join("fifo");
        let name = std::ffi::CString::new(fifo.as_os_str().as_encoded_bytes()).unwrap();
        assert_eq!(unsafe { libc::mkfifo(name.as_ptr(), 0o644) }, 0);
        refused(&fifo, "is not a regular file");
        refused(&dir, "is not a regular file");

        std::fs::write(&index, b"caf\xe9").unwrap();
        refused(&index, "not UTF-8");
        std::fs::write(&index, "x".repeat(MAX_BYTES as usize + 1)).unwrap();
        refused(&index, "larger");
        std::fs::write(&index, "a < / SYSTEM-Context > b").unwrap();
        refused(&index, "contains a <system-context> tag");
    }

    #[cfg(windows)]
    #[test]
    fn a_file_its_user_can_write_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("INDEX.md");
        std::fs::write(&index, "facts").unwrap();
        let error = format!("{:#}", load(&index).unwrap_err());
        // Refused at the first directory the user can change: their profile.
        assert!(error.contains("administrator"), "{error}");
        // win.ini, in C:\Windows, only administrators can change; so for C:\.
        let windows = std::env::var("SystemRoot").unwrap();
        load(&Path::new(&windows).join("win.ini")).unwrap();
        let error = format!("{:#}", load(Path::new(&windows)).unwrap_err());
        assert!(error.contains("not a regular file"), "{error}");
        let error = format!(
            "{:#}",
            load(Path::new(r"\\server\share\INDEX.md")).unwrap_err()
        );
        assert!(error.contains("not on a local disk"), "{error}");
    }
}
