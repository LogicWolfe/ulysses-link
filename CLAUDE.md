# CLAUDE.md

This file provides guidance to AI coding assistants when working with code in this repository.

## Repository Structure

This repository contains **ulysses-link**, a lightweight background service that extracts documentation files from code repositories and links them for Ulysses external folder importing. Written in Rust — install with `cargo install ulysses-link`.

```
ulysses-link/
├── Cargo.toml
├── ulysses-link.toml.example
├── src/
│   ├── main.rs          # Entry point, clap CLI dispatch
│   ├── lib.rs           # Library root, public module declarations
│   ├── config.rs        # TOML loading, validation, defaults
│   ├── matcher.rs       # Include/exclude filtering (ignore + globset)
│   ├── linker.rs        # Symlink creation/removal/pruning
│   ├── scanner.rs       # Full tree scan + reconciliation
│   ├── watcher.rs       # notify integration + debouncing
│   ├── engine.rs        # Core orchestrator (scan + watch lifecycle)
│   └── service.rs       # OS service install/uninstall/status
└── tests/
    └── integration.rs   # End-to-end tests with real temp dirs
```

## Core Development Principles

### CRITICAL: Assume Correct, Fail Fast Philosophy

**NEVER implement fallback strategies that mask failures with the primary approach.**

When a component expects a value to be provided:
- **DO NOT** use default values when expecting external input
- **DO NOT** attempt to coerce invalid data to meet expectations
- **DO NOT** provide alternative approaches when the primary approach fails
- **ALWAYS** assume operations will work as expected
- **ALWAYS** return explicit errors and abort execution when validation fails
- **ALWAYS** let failures be visible rather than hiding them

### Development Philosophy

- Favor simplicity over abstraction
- Use established crates rather than custom solutions
- Keep code minimal, readable, and maintainable
- Focus on correctness over premature optimization
- Sync with std threads, not async/tokio — this is a simple service
- Key crates: `notify`, `ignore`, `globset`, `toml`, `clap`, `tracing`, `anyhow`/`thiserror`

### Server Interaction Constraints

- **NEVER** start the ulysses-link service or run long-lived processes yourself
- **DO NOT** run `ulysses-link run` or `ulysses-link install` yourself
- **ALWAYS** ask the user to validate changes that require running the service
- When validation is needed, explain what changes you've made and ask the user to run the validation steps

### Commenting Best Practices

- **Code comments should reflect the current state of the code**, not its history
- **Never describe changes or modifications in comments** — use version control for tracking history
- **Write comments for first-time readers** who have no prior knowledge of the codebase
- **Focus on explaining "why" rather than "what"** — the code itself shows what it does
- **Document non-obvious behaviors, edge cases, and design decisions**
- **Remove commented-out code** — rely on version control instead

### Collaborative Problem-Solving Approach

You are an expert software engineer collaborating with an even more established and brilliant peer. When you encounter:

- Unexpected discoveries during implementation
- Ambiguity in the instructions or requirements
- Uncertainty about the best way to proceed
- Multiple viable approaches with unclear trade-offs

Seek guidance from your peer rather than making assumptions or oversimplifying the problem.

## Testing and Validation

**Commands:**
- Run all tests: `cargo test`
- Run single test: `cargo test test_name`
- Run tests in a module: `cargo test config::tests`
- Check for errors: `cargo check`
- Lint: `cargo clippy`
- Format: `cargo fmt`

**Testing Philosophy:**
- **NEVER generate ad-hoc scripts to test code**
- **ALWAYS use existing tests** — run individual tests by name
- **ALWAYS write proper tests** when validation is needed — unit tests inline with `#[cfg(test)] mod tests`, integration tests in `tests/`
- Use `tempfile::TempDir` for filesystem tests — never create files outside temp directories

## Planning and Documentation

- **NEVER** organize plans by timeline or create time-based schedules
- **NEVER** estimate how long tasks will take
- **ALWAYS** organize plans by logical components, dependencies, or functional areas
- **ALWAYS** focus on what needs to be built and how components relate to each other

## Manual Rules (Triggered explicitly)

### Pre-implementation Evaluation (`@noedit`)

When you need to evaluate before implementing, use `@noedit`:
- Evaluate the request first
- Explain your understanding and propose a solution
- Do not make any edits until the user has reviewed your proposal

### Commit Message Guidelines (`@commit`)

When generating commit messages, use `@commit`:
- Write commit messages with a clear subject line that has a capitalized first word
- Do not include tags, labels, or prefixes (like "feat:", "fix:", etc.)
- Never reference phases, steps, or parts of a plan
- State the purpose of the change clearly in the subject
- Include a short overview below the subject line
- Add point-form notes about specific changes
- **Commit messages should never be more than 15 lines and ideally will be closer to 3-5 lines**
