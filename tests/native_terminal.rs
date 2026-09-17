#![cfg(debug_assertions)]

use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use std::{
    io::{Read, Write},
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

/// Run the complete Nectra pane inside another real PTY, as in a terminal app.
/// A local fixture supplies animation, an approval question and a structured answer.
fn exercise(resize: bool, cancel: bool, undersize: bool) {
    let pair = native_pty_system()
        .openpty(PtySize {
            rows: 28,
            cols: 100,
            pixel_width: 0,
            pixel_height: 0,
        })
        .unwrap();
    let mut cmd = CommandBuilder::new(env!("CARGO_BIN_EXE_nectra"));
    cmd.arg("__terminal-demo");
    let mut child = pair.slave.spawn_command(cmd).unwrap();
    drop(pair.slave);
    let mut input = pair.master.take_writer().unwrap();
    let mut reader = pair.master.try_clone_reader().unwrap();
    let (tx, rx) = mpsc::sync_channel(64);
    let reader_thread = thread::spawn(move || {
        let mut bytes = [0; 8192];
        loop {
            match reader.read(&mut bytes) {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    if tx.send(bytes[..n].to_vec()).is_err() {
                        break;
                    }
                }
            }
        }
    });
    let mut parser = vt100::Parser::new(28, 100, 1000);
    let mut output = Vec::new();
    let start = Instant::now();
    let mut approved = false;
    let mut inspected = false;
    let mut returned = false;
    let mut animation_frames = std::collections::BTreeSet::new();
    let mut status = None;
    #[cfg(windows)]
    let mut descendant: Option<std::os::windows::io::OwnedHandle> = None;
    while start.elapsed() < Duration::from_secs(30) {
        if let Ok(bytes) = rx.recv_timeout(Duration::from_millis(30)) {
            // On Unix the outer crossterm cursor-position query requires a reply.
            if bytes.windows(4).any(|w| w == b"\x1b[6n") {
                input.write_all(b"\x1b[28;1R").unwrap();
            }
            parser.process(&bytes);
            output.extend_from_slice(&bytes);
            assert!(output.len() < 4 * 1024 * 1024, "unbounded UI output");
            let screen = parser.screen().contents();
            if !inspected && screen.contains("Queued:") {
                input.write_all(b"\x1bOQ").unwrap();
                inspected = true;
            }
            #[cfg(windows)]
            if descendant.is_none() {
                use std::os::windows::io::FromRawHandle;
                use windows_sys::Win32::System::Threading::{OpenProcess, PROCESS_SYNCHRONIZE};
                if let Some(pid) = screen.lines().find_map(|l| {
                    l.strip_prefix("Fixture descendant: ")
                        .and_then(|s| s.trim().parse::<u32>().ok())
                }) {
                    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
                    assert!(!raw.is_null(), "test descendant should be running");
                    descendant = Some(unsafe {
                        std::os::windows::io::OwnedHandle::from_raw_handle(raw as _)
                    });
                }
            }
            // PTY rendering can coalesce frames; verify motion instead of requiring
            // one particular transient spinner glyph to reach the outer terminal.
            for frame in ['|', '/', '-', '\\'] {
                if screen.contains(&format!("Thinking {frame}")) {
                    animation_frames.insert(frame);
                }
            }
            if screen.contains("Approve writing design.md") && !approved {
                approved = true;
                if cancel {
                    input.write_all(b"\x1d\x1d").unwrap();
                    returned = true;
                } else {
                    if undersize {
                        pair.master
                            .resize(PtySize {
                                rows: 5,
                                cols: 15,
                                pixel_width: 0,
                                pixel_height: 0,
                            })
                            .unwrap();
                        thread::sleep(Duration::from_millis(400));
                        if let Some(s) = child.try_wait().unwrap() {
                            status = Some(s);
                            break;
                        }
                    }
                    if resize {
                        pair.master
                            .resize(PtySize {
                                rows: 24,
                                cols: 90,
                                pixel_width: 0,
                                pixel_height: 0,
                            })
                            .unwrap();
                        parser.set_size(24, 90);
                    }
                    // These are Nectra messages, never provider approval input.
                    input
                        .write_all(b"\x1bOQuse opuX\x1b[D\x1b[3~s and plan a shed\rthen review it\r\x1bOQy\x1bOQ")
                        .unwrap();
                }
            }
            if screen.contains("Captured clean answer:") && !returned {
                returned = true;
            }
        }
        if let Some(s) = child.try_wait().unwrap() {
            status = Some(s);
            break;
        }
    }
    if status.is_none() {
        let _ = child.kill();
        let _ = child.wait();
    }
    // Drain final output after process exit; ConPTY may still have bytes in transit.
    //
    // These bytes must go through the parser too. The loop above stops the moment
    // the child exits, so under load the last frames, the approval prompt or the
    // capture line can still be in flight -- previously they landed in `output`
    // for the failure message but were never observed, which made the assertions
    // below depend on scheduling rather than on behaviour.
    while let Ok(bytes) = rx.recv_timeout(Duration::from_millis(200)) {
        parser.process(&bytes);
        output.extend_from_slice(&bytes);
        let screen = parser.screen().contents();
        for frame in ['|', '/', '-', '\\'] {
            if screen.contains(&format!("Thinking {frame}")) {
                animation_frames.insert(frame);
            }
        }
        approved |= screen.contains("Approve writing design.md");
        // `returned` is only inferred here for the non-cancel path. A cancel run
        // proves itself by the keystrokes the live loop sends on approval; if the
        // approval only arrived after exit, cancellation was never exercised and
        // the test should fail rather than be talked into passing.
        if !cancel {
            returned |= screen.contains("Captured clean answer:");
        }
    }
    drop(rx);
    drop(input);
    drop(pair.master);
    reader_thread.join().unwrap();
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::{
            Foundation::WAIT_OBJECT_0, System::Threading::WaitForSingleObject,
        };
        let descendant = descendant.expect("fixture descendant was not observed");
        assert_eq!(
            unsafe { WaitForSingleObject(descendant.as_raw_handle() as _, 3000) },
            WAIT_OBJECT_0,
            "provider descendant survived terminal cleanup"
        );
    }
    assert!(
        animation_frames.len() >= 2 && approved && returned,
        "native animation/input was not observed: {}",
        String::from_utf8_lossy(&output)
    );
    assert_eq!(
        status
            .expect("terminal did not exit within deadline")
            .success(),
        !cancel
    );
    let output = String::from_utf8_lossy(&output);
    if cancel {
        assert!(!output.contains("Captured clean answer:"));
    } else {
        assert!(output.contains("Captured clean answer: Saved design.md"));
        assert!(output.contains("Queued main message: use opus and plan a shed"));
        assert!(output.contains("Queued main message: then review it"));
    }
}

#[test]
fn native_terminal_animation_approval_resize_and_context_handoff() {
    exercise(true, false, false);
}

#[test]
fn completed_reply_returns_without_any_exit_keystroke() {
    exercise(false, false, false);
}

#[test]
fn cancelling_native_work_does_not_save_a_fake_answer() {
    exercise(false, true, false);
}

#[test]
fn undersized_resize_keeps_provider_work_alive() {
    exercise(true, false, true);
}

#[test]
fn hook_helper_captures_only_structured_answer_and_returns_no_approval() {
    use std::process::{Command, Stdio};
    let root = tempfile::tempdir().unwrap();
    let mut child = Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args(["__terminal-hook", root.path().to_str().unwrap(), "claude"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(br#"{"hook_event_name":"Stop","last_assistant_message":"Saved design.md","tool_input":"not conversation history"}"#).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(result.status.success());
    assert_eq!(String::from_utf8(result.stdout).unwrap().trim(), "{}");
    let files: Vec<_> = std::fs::read_dir(root.path())
        .unwrap()
        .map(|e| e.unwrap().path())
        .collect();
    assert_eq!(files.len(), 1);
    let record = std::fs::read_to_string(&files[0]).unwrap();
    assert!(record.contains("Saved design.md"));
    assert!(!record.contains("not conversation history"));
    let codex_root = tempfile::tempdir().unwrap();
    std::fs::write(
        codex_root.path().join("notify.config"),
        "invalid mutable configuration",
    )
    .unwrap();
    let result = Command::new(env!("CARGO_BIN_EXE_nectra"))
        .args([
            "__terminal-hook",
            codex_root.path().to_str().unwrap(),
            "codex",
            "[]",
            r#"{"type":"agent-turn-complete","last-assistant-message":"Codex completed"}"#,
        ])
        .output()
        .unwrap();
    assert!(result.status.success(), "{:?}", result);
    assert!(std::fs::read_dir(codex_root.path()).unwrap().any(|entry| {
        let path = entry.unwrap().path();
        path.extension().is_some_and(|ext| ext == "json")
            && std::fs::read_to_string(path)
                .unwrap()
                .contains("Codex completed")
    }));
}

#[test]
fn actual_user_prompt_receives_context_through_hook_only() {
    use std::process::{Command, Stdio};
    let root = tempfile::tempdir().unwrap();
    let context = "Continue silently.\n{\"conversation\":[],\"current_user_message\":\"which model am I speaking to?\"}";
    std::fs::write(root.path().join("context.txt"), context).unwrap();
    for (runtime, event) in [("claude", "UserPromptSubmit"), ("gemini", "BeforeAgent")] {
        let mut child = Command::new(env!("CARGO_BIN_EXE_nectra"))
            .args(["__terminal-hook", root.path().to_str().unwrap(), runtime])
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .spawn()
            .unwrap();
        child.stdin.take().unwrap().write_all(serde_json::to_string(&serde_json::json!({"hook_event_name":event,"prompt":"which model am I speaking to?"})).unwrap().as_bytes()).unwrap();
        let result = child.wait_with_output().unwrap();
        assert!(result.status.success());
        let value: serde_json::Value = serde_json::from_slice(&result.stdout).unwrap();
        assert_eq!(value["hookSpecificOutput"]["additionalContext"], context);
    }
}
