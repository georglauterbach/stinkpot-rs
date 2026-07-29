# stinkpot - Agent Guide

`stinkpot-rs` is a sqlite-backed shell-history searcher. It is a much tinier [Atuin](https://atuin.sh). Its goal is to be minimal: no AI, no sync, just SQLite-backed history search.

## Repository Layout

| Path                 | Purpose                                                           |
| :------------------- | :---------------------------------------------------------------- |
| `.devcontainer/`     | [Development Container] with the complete environment             |
| `.github/`           | GitHub-related content like CI/CD, issues, etc.                   |
| `src/`               | contains all source code                                          |

## Programming Language

stinkpot is written in Rust.

### Version Information

The file [`rust-toolchain.toml`](./rust-toolchain.toml) contains information about the used toolchain version, targets, profiles, etc. Additionally, [`Cargo.toml`](./Cargo.toml) contains the used Rust edition.

### Lint & Style

The project is linted with clippy. The lint rules are very strict. Run the linter with

```console
$ cargo clippy --workspace --quiet --all-features -- -D warnings
$ cargo fmt --all -- --check
$ cargo doc --workspace --quiet --no-deps --document-private-items
```

The project also uses [EditorConfig] for general-purpose style-enforcement.

[//]: # (Links)

[Development Container]: https://containers.dev/
[EditorConfig]: https://editorconfig.org/
