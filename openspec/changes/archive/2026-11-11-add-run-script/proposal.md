# Proposal

## Why

Developers need a short, repeatable way to launch the desktop app without remembering its Cargo package name or changing to the repository first. Record the completed run-script work independently of the broader desktop change's remaining validation tasks.

## What Changes

- Provide an executable `scripts/run.sh` that runs the desktop Cargo package from the repository root.
- Forward arguments unchanged to the app, including `--demo`, and propagate Cargo's exit status.
- Document normal and demo launch commands in the README.

## Capabilities

### New Capabilities

- `development-launch`: Repository launch entry point, working-directory handling, app argument forwarding, and exit-status propagation.

### Modified Capabilities

None.

## Impact

- Adds a Bash convenience script following the existing bundling-script convention and updates the README.
- Uses the existing Rust desktop package and Cargo toolchain; introduces no dependency or application-language change.
- Leaves desktop runtime, model, session, and persistence behavior under the existing desktop change.
