# Design

## Context

See [proposal.md](./proposal.md) for motivation. The desktop crate declares `rapid-mlx-ui` as its default executable. The existing bundling script uses Bash and resolves the repository root relative to its own location.

## Goals / Non-Goals

**Goals:** Reuse the existing Cargo launch command through a small wrapper with predictable working-directory and argument behavior.

**Non-Goals:** Add a new launcher UI, Cargo-option parser, runtime setup, or application behavior changes.

## Decisions

- Follow the existing Bash-script convention rather than adding another task runner or implementation language.
- Resolve the repository root from `BASH_SOURCE[0]` and change to it before launching Cargo, so a caller's working directory cannot select a different workspace.
- Use `exec cargo run -p rapid-mlx-ui -- "$@"` to preserve argument boundaries and propagate exit status directly. Script arguments are app arguments; Cargo profile customization remains available through direct Cargo commands.
- Use a temporary Cargo stub for wrapper verification, avoiding an unintended GUI launch or writes to real application state. No rapid-mlx interface, session history, or usage-accounting contract changes are needed.

## Risks / Trade-offs

- [Bash and Cargo are required] → Use the checkout's existing development prerequisites and retain the direct Cargo command in the README explanation.
- [Wrapper checks do not verify desktop startup] → Record their scope explicitly; the broader desktop change retains its native UI validation gates.
