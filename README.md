# `stinkpot-rs`

`stinkpot-rs` is a sqlite-backed shell-history searcher. It is a much tinier [Atuin](https://atuin.sh). Its goal is to be minimal: no AI, no sync, just SQLite-backed history search.

> [!note]
>
> This is a fork [`tangled.org/oppi.li/stinkpot`](https://tangled.org/oppi.li/stinkpot).
>
>The original author had been using [Atuin], but without actually employing "most of its features: the sync server, Atuin AI, dotfiles manager, script manager or the KV store. \[The\] only use case for atuin was the session agnostic history management, and the searcher TUI. stinkpot provides these while being a small Go binary."
>
> stinkpot is a tiny turtle species apparently, hence the
name.

## Usage

Call eval the init script in your shell setup:

```bash
eval "$(stinkpot init bash)"
```

Start by importing your existing bash history into stinkpot:

```bash
stinkpot import
```

Hit `ctrl+r` in your shell to trigger a search. Hit tab or enter to accept the selection. The history database is stored in `~/.local/share/stinkpot`.

## Development

To build `stinkpot-rs`, run `cargo build` for debug mode or `cargo build --release` for release mode.

[//]: # (Links)

[Atuin]: https://atuin.sh
