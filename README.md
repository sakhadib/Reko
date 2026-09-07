# reko

Cross-platform CLI (Windows / Linux / macOS) built with Rust + `clap`.

## Quick start

```bash
cargo run -- --help
cargo run -- hello --help
cargo run -- hello Alice
```

## Development

```bash
cargo build
cargo test
cargo run -- -v hello world   # with verbosity
```

## Release build

```bash
cargo build --release
./target/release/reko --help
```

### Cross-compilation

Install targets:

```bash
rustup target add x86_64-pc-windows-gnu x86_64-unknown-linux-gnu aarch64-apple-darwin x86_64-apple-darwin
cargo build --release --target x86_64-unknown-linux-gnu
```

Or use CI (`.github/workflows/ci.yml`) which builds on ubuntu/windows/macos runners and publishes artifacts on tags `v*`.

## Project layout

```
src/main.rs  # CLI entry (clap derive)
Cargo.toml   # package + release profile (LTO, stripped)
```
