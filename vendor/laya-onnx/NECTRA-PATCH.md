# Nectra patch to laya-onnx 0.1.0

Copied from https://github.com/NandhaKishorM/laya (laya-rust, rev d74e8be46ef5241e4ba2b16cd25e896f24f71373, Apache-2.0,
see LICENSE) without the samples, tests and tools. The only change is the new `LayaOptions::disable_prepacking`, which
calls `SessionBuilder::with_prepacking(false)`; measurements in docs/design/LAYA-MEMORY.md. Drop this directory and go
back to the git dependency once upstream exposes the option.
