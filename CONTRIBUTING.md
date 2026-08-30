# Contributing

Bug reports and focused pull requests are welcome. Please search existing issues first and never attach an unsanitized diagnostic session or crash dump to a public issue.

## Development setup

Use Windows 10 or 11 x64, Rust stable with the MSVC target, and Visual Studio Build Tools with the C++ workload and Windows SDK.

```powershell
git clone --recurse-submodules https://github.com/MaharshPatelX/gpu-crash-recorder.git
cd gpu-crash-recorder
cargo fmt --all -- --check
cargo test --lib
cargo check --all-targets
```

Keep collectors observational and independently fallible. A collector failure must be recorded as source health information without ending the session. New records must use the shared UTC/QPC timestamp model, and unavailable metrics must be reported explicitly.

Do not add GPU tuning, game injection, automatic uploads, or kernel drivers without prior design discussion.
