# Rapid MLX UI

A desktop app for managing rapid-mlx and chatting with local models downloaded from Hugging Face.

The app will use **Rust**, provide a desktop **UI**, and use **OpenSpec** to define requirements and plan changes.

## Status

This repository currently contains project documentation and an initialized OpenSpec workflow with Codex skills. The application, Rust workspace, and desktop UI framework have not been scaffolded yet. Features below describe the intended product, not functionality available today.

## Product scope

### Manage rapid-mlx

- Show whether rapid-mlx is available, running, and ready to serve requests.
- Provide controls to configure, start, and stop the managed runtime.
- Surface startup progress, runtime logs, and actionable errors in the UI.
- Distinguish a runtime launched by the app from an existing external process.

### Browse local models

- Discover models already downloaded from Hugging Face, including models in configured local directories.
- Show model identity, local location, and available metadata such as size and quantization.
- Refresh discovery and select a compatible model for chat.
- Explain missing files and incompatible models without deleting or modifying the user's model cache.

### Chat with models

- Provide a full chat interface with streamed responses, Markdown and code formatting, and cancellation.
- Support composing messages, editing prompts, retrying responses, and showing generation errors.
- Allow model selection and reasoning effort selection where the runtime and selected model support it.
- Make unsupported settings explicit; never imply that a requested effort setting was applied when it was ignored.
- Show the model and generation settings associated with a response.

### Keep sessions

- Create, list, reopen, rename, and delete persistent chat sessions.
- Preserve message history, model selection, and generation settings across app restarts.
- Keep session history locally by default.
- Define how editing or retrying a message affects subsequent history before implementing those actions.

### Show system and token statistics

- Display system load, including CPU utilization, memory usage, and GPU utilization when available.
- Show prompt and generated token counts, generation throughput, and response timing when available.
- Provide per-response and per-session token totals, with clear definitions for accumulated usage.
- Distinguish measured values, estimates, and unavailable metrics.
- Keep monitoring responsive without interrupting chat or overloading the machine.

## Engineering direction

- **Rust:** application logic, rapid-mlx integration, persistence, and telemetry collection.
- **Desktop UI:** model browser, runtime controls, chat, session history, and system statistics in one app.
- **OpenSpec:** specifications and change planning for implementation work.
- **Local data:** model discovery and session storage should work with local files; network operations must be explicit.

The UI framework, initial operating-system targets, rapid-mlx integration interface, persistence format, and telemetry sources remain to be selected after verifying runtime capabilities. Any UI tooling that requires another language must be documented as an explicit architecture decision.

## Development workflow

1. Inspect the existing code and specifications relevant to a change.
2. Use OpenSpec to capture observable behavior, acceptance criteria, design decisions, and implementation tasks for features and material behavior changes.
3. Implement changes in small, reviewable steps, keeping UI and backend behavior aligned.
4. Validate affected behavior with suitable checks and tests; visually verify UI changes when a runnable app is available.
5. Update specifications and documentation to match the delivered behavior and archive completed changes through OpenSpec.

### OpenSpec

OpenSpec 1.17.0 is initialized with the `spec-driven` schema and the core workflow for Codex. Project constraints are in [openspec/config.yaml](./openspec/config.yaml), specifications belong in [openspec/specs/](./openspec/specs/), and active changes belong in [openspec/changes/](./openspec/changes/).

With Node.js and npm available, run the CLI without a global installation:

```sh
npx --yes @fission-ai/openspec@1.17.0 list
npx --yes @fission-ai/openspec@1.17.0 list --specs
npx --yes @fission-ai/openspec@1.17.0 validate --all --strict --no-interactive
```

In Codex, invoke `$openspec-propose` with a feature description to start a change. The generated skills also cover explore, apply, update, sync, and archive workflows. Reload the agent session if newly generated skills are not discovered automatically.

No feature changes or specifications have been created yet. Application build, run, and test instructions will be added when the corresponding tooling exists.

See [AGENTS.md](./AGENTS.md) for repository instructions for coding agents.

## Initial implementation milestones

1. Verify rapid-mlx capabilities and document the desktop architecture through an OpenSpec change.
2. Scaffold the Rust application and desktop UI.
3. Implement runtime status and controls alongside local model discovery.
4. Add streaming chat with model and supported effort selection.
5. Persist and manage chat sessions.
6. Add system monitoring and token statistics, then validate the complete desktop flow.
