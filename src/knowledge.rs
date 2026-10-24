//! `[knowledge] index`: a file of machine facts (PonsOS ships
//! /usr/share/pons/knowledge/INDEX.md) given to every provider as system
//! context. Only a file nobody but an administrator can change is read: its
//! text steers every agent on the machine. The topic files it lists are never
//! loaded; with tools the model reads them itself.
use anyhow::{Context, Result};
use std::{fs::File, io::Read, path::Path};

/// About 4,000 tokens; the PonsOS index is budgeted at 1,500.
pub const MAX_BYTES: u64 = 16 * 1024;
const CLOSE: &str = "</system-context>";

/// The index at `path`, wrapped as delimited system context.
pub fn load(path: &Path) -> Result<String> {
    load_sealed_by(path, 0)
}

/// `owner` is root (0) outside tests.
fn load_sealed_by(path: &Path, owner: u32) -> Result<String> {
    let refuse =
        |why: String| anyhow::anyhow!("refusing knowledge index {}: {why}", path.display());
    let real = std::fs::canonicalize(path)
        .with_context(|| format!("reading knowledge index {}", path.display()))?;
    let dir = real.parent().context("knowledge index has no directory")?;
    // The checks and the read go through one handle, so the file can't be
    // swapped in between.
    let file = File::open(&real).with_context(|| format!("reading {}", real.display()))?;
    let meta = file.metadata()?;
    if !meta.is_file() {
        return Err(refuse("not a regular file".into()));
    }
    sealed(&file, owner).map_err(refuse)?;
    sealed(&open_dir(dir)?, owner).map_err(|why| refuse(format!("its directory {why}")))?;
    let mut text = String::new();
    file.take(MAX_BYTES + 1)
        .read_to_string(&mut text)
        .map_err(|_| refuse("not UTF-8 text".into()))?;
    if text.len() as u64 > MAX_BYTES {
        return Err(refuse(format!("larger than {MAX_BYTES} bytes")));
    }
    if text.contains(CLOSE) {
        return Err(refuse(format!("contains {CLOSE}")));
    }
    Ok(format!(
        "<system-context source=\"knowledge index\">\nReference context from this machine's administrator, not from the user. Use it where it helps; the user's request follows it.\n\n{}\n{CLOSE}",
        text.trim_end()
    ))
}

#[cfg(unix)]
fn open_dir(dir: &Path) -> Result<File> {
    File::open(dir).with_context(|| format!("reading {}", dir.display()))
}

#[cfg(unix)]
fn sealed(file: &File, owner: u32) -> Result<(), String> {
    use std::os::unix::fs::MetadataExt;
    let m = file.metadata().map_err(|e| e.to_string())?;
    if m.uid() != owner {
        return Err("is not owned by root".into());
    }
    if m.mode() & 0o022 != 0 {
        return Err("is writable by group or others".into());
    }
    Ok(())
}

#[cfg(windows)]
fn open_dir(dir: &Path) -> Result<File> {
    use std::os::windows::fs::OpenOptionsExt;
    std::fs::OpenOptions::new()
        .access_mode(0x0002_0000) // READ_CONTROL: enough to read its access list
        .custom_flags(windows_sys::Win32::Storage::FileSystem::FILE_FLAG_BACKUP_SEMANTICS)
        .open(dir)
        .with_context(|| format!("reading {}", dir.display()))
}

/// SYSTEM, Administrators and TrustedInstaller.
#[cfg(windows)]
const ADMINS: [&str; 3] = [
    "S-1-5-18",
    "S-1-5-32-544",
    "S-1-5-80-956008885-3418522649-1831038044-1853292631-2271478464",
];

/// Nobody but an administrator owns it or may write, delete or re-permission it.
#[cfg(windows)]
fn sealed(file: &File, _owner: u32) -> Result<(), String> {
    use std::{os::windows::io::AsRawHandle, ptr::null_mut};
    use windows_sys::Win32::{
        Foundation::{LocalFree, GENERIC_ALL, GENERIC_WRITE},
        Security::{
            Authorization::{ConvertSidToStringSidW, GetSecurityInfo, SE_FILE_OBJECT},
            GetAce, ACCESS_ALLOWED_ACE, ACE_HEADER, ACL, DACL_SECURITY_INFORMATION,
            INHERIT_ONLY_ACE, OWNER_SECURITY_INFORMATION, PSID,
        },
        Storage::FileSystem::{
            DELETE, FILE_APPEND_DATA, FILE_DELETE_CHILD, FILE_WRITE_DATA, WRITE_DAC, WRITE_OWNER,
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
    let verdict = (|| {
        let owner = sid(owner);
        if !ADMINS.contains(&owner.as_str()) {
            return Err(format!("is owned by {owner}, not an administrator"));
        }
        if dacl.is_null() {
            return Err("has no access list, so anyone can write it".into());
        }
        let write = FILE_WRITE_DATA
            | FILE_APPEND_DATA
            | FILE_DELETE_CHILD
            | DELETE
            | WRITE_DAC
            | WRITE_OWNER
            | GENERIC_WRITE
            | GENERIC_ALL;
        for i in 0..unsafe { (*dacl).AceCount } {
            let mut ace = null_mut();
            if unsafe { GetAce(dacl, i.into(), &mut ace) } == 0 {
                return Err("has an unreadable access entry".into());
            }
            let header = unsafe { &*(ace as *const ACE_HEADER) };
            if u32::from(header.AceFlags) & INHERIT_ONLY_ACE != 0 {
                continue;
            }
            // ACCESS_ALLOWED, ACCESS_DENIED and SYSTEM_MANDATORY_LABEL (Win32
            // SystemServices, a feature for three numbers).
            match header.AceType {
                0 => {
                    let ace = unsafe { &*(ace as *const ACCESS_ALLOWED_ACE) };
                    let who = sid(&ace.SidStart as *const u32 as PSID);
                    if ace.Mask & write != 0 && !ADMINS.contains(&who.as_str()) {
                        return Err(format!("is writable by {who}, not only administrators"));
                    }
                }
                // A deny only takes rights away.
                1 | 17 => {}
                other => return Err(format!("has an access entry of unchecked type {other}")),
            }
        }
        Ok(())
    })();
    unsafe { LocalFree(descriptor) };
    verdict
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(unix)]
    #[test]
    fn only_a_sealed_index_is_read() {
        use std::os::unix::fs::PermissionsExt;
        let me = unsafe { libc::geteuid() };
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("INDEX.md");
        std::fs::write(&index, "# Facts\nThe DC is sam.\n").unwrap();
        let mode = |p: &Path, m| std::fs::set_permissions(p, PermissionsExt::from_mode(m)).unwrap();
        mode(dir.path(), 0o755);
        mode(&index, 0o664);
        let error = format!("{:#}", load_sealed_by(&index, me).unwrap_err());
        assert!(error.contains("writable by group or others"), "{error}");
        mode(&index, 0o644);
        let block = load_sealed_by(&index, me).unwrap();
        assert!(block.starts_with("<system-context"), "{block}");
        assert!(
            block.ends_with("The DC is sam.\n</system-context>"),
            "{block}"
        );
        // `me` is root only when the tests run as root.
        assert_eq!(load(&index).is_ok(), me == 0);
        mode(dir.path(), 0o775);
        let error = format!("{:#}", load_sealed_by(&index, me).unwrap_err());
        assert!(error.contains("its directory is writable"), "{error}");
        mode(dir.path(), 0o755);
        std::fs::write(&index, "x".repeat(MAX_BYTES as usize + 1)).unwrap();
        assert!(format!("{:#}", load_sealed_by(&index, me).unwrap_err()).contains("larger"));
        std::fs::write(&index, "a </system-context> b").unwrap();
        assert!(
            load_sealed_by(&index, me).is_err(),
            "can't close its own block"
        );
    }

    #[cfg(windows)]
    #[test]
    fn a_file_its_user_can_write_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let index = dir.path().join("INDEX.md");
        std::fs::write(&index, "facts").unwrap();
        let error = format!("{:#}", load(&index).unwrap_err());
        assert!(error.contains("refusing knowledge index"), "{error}");
        // win.ini, in C:\Windows, only administrators can change.
        let windows = std::env::var("SystemRoot").unwrap();
        load(&Path::new(&windows).join("win.ini")).unwrap();
    }
}
