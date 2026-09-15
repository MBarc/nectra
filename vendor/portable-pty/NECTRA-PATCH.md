# Nectra patch to portable-pty 0.9.0

Copied from the crates.io source with its original license. The sole functional
change is CreatePseudoConsole flags = 0 in src/win/psuedocon.rs. Nectra creates a
new terminal emulator and should not inherit the parent cursor. The upstream
inherit-cursor negotiation stalls on the tested Windows host, and its undocumented
Win32-input/resize flags are unnecessary for Nectra's VT input transport.

Keep this change when updating the dependency; test the nested Windows PTY demo.
