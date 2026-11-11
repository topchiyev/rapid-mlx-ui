# Development Launch

## Purpose

Provide a documented repository entry point for launching the desktop app consistently from a development checkout.

## ADDED Requirements

### Requirement: Repository run entry point
The repository SHALL provide an executable `scripts/run.sh` that runs `cargo run -p rapid-mlx-ui` from the repository root, independently of the caller's working directory.

#### Scenario: Launch from the checkout
- **WHEN** a developer invokes `./scripts/run.sh` from the repository root
- **THEN** the script invokes Cargo to run the desktop package using the default development profile

#### Scenario: Launch from another directory
- **WHEN** a developer invokes the script by its path while in another directory
- **THEN** Cargo runs from the repository root

### Requirement: App argument forwarding
The run script SHALL forward every supplied argument unchanged to the desktop app after Cargo's `--` separator.

#### Scenario: Launch demo mode
- **WHEN** a developer invokes `./scripts/run.sh --demo`
- **THEN** the desktop app receives `--demo`

#### Scenario: Preserve argument boundaries
- **WHEN** the caller supplies an argument containing spaces or an empty argument
- **THEN** Cargo receives each as a separate unchanged app argument

### Requirement: Launch exit status
The run script SHALL return Cargo's exit status to the caller.

#### Scenario: Cargo fails
- **WHEN** Cargo exits with a nonzero status
- **THEN** the script exits with that same status

### Requirement: Documented launch commands
The README SHALL document normal and demo launches using the run script and explain how to invoke it from another directory.

#### Scenario: Find the launch command
- **WHEN** a developer reads the README's build and run instructions
- **THEN** normal and demo run-script commands and argument-forwarding behavior are described
