# Project Instructions

## Git Commits
- On any reasonable amount of code / meaningful change, create a meaningful commit without waiting for explicit user instruction.
- Commits must have clear, conventional messages (e.g., `feat:`, `fix:`, `chore:`, `refactor:`).
- Before committing, run `git status` and `git diff` to stage only intended files. Never commit secrets or `target/` artifacts.
- Keep commits atomic and focused.

## Project: reko
- Rust CLI, cross-platform (Windows/Linux/macOS), `clap` derive.
- Build: `cargo build`, `cargo test`, `cargo build --release`
