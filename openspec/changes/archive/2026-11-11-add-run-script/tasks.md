# Tasks

## 1. Development launch

- [x] 1.1 Add executable `scripts/run.sh`; verify Bash syntax, Cargo invocation from the repository root and another directory, unchanged demo/spaced/empty arguments, and nonzero exit-status propagation with a temporary Cargo stub.
- [x] 1.2 Document normal and demo script usage in the README; verify the documented command shapes against the script and check the diff for whitespace errors.
- [x] 1.3 Record launch acceptance scenarios and validate the change with strict OpenSpec checks.

## Validation record

- `bash -n scripts/run.sh` passed.
- Temporary Cargo-stub checks passed for argument forwarding, invocation outside the checkout, and propagation of exit status 23. These checks do not launch the desktop app or write application state.
- `git diff --check` passed after adding the script and updating the README.
- `npx --yes @fission-ai/openspec@1.17.0 validate add-run-script --strict --no-interactive` passed.
- `cargo fmt --check` and `cargo clippy --workspace --all-targets -- -D warnings` passed.
- `cargo test --workspace` passed: 142 tests passed and 4 were ignored by the existing suite configuration.
- Strict OpenSpec validation passed for the change, the synced main spec, and the active desktop change before archiving.

## Workflow follow-up

- Sync the development-launch spec and archive this completed change.
- Commit and push the run script, documentation, specification, and archive.
