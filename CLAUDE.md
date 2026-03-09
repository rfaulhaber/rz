# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`rz` is a multi-format archive CLI tool written in Rust. It wraps archive formats (tar, tar.gz,
tar.zst, tar.xz, tar.bz2, zip) behind a unified interface with `compress`, `decompress`, `list`,
and `info` subcommands. Think `tar` and `zip/unzip` rolled into one binary with a consistent UI.

## Commands

```sh
cargo build                    # build
cargo clippy                   # lint (strict — see below)
cargo nextest run              # test (nextest is in the dev shell)
cargo nextest run test_name    # run a single test
cargo run -- compress -h       # run with args
nix build                      # reproducible build via flake
nix develop                    # enter dev shell (rust toolchain + rust-analyzer + nextest)
```

## Architecture

Flat layout — no `src/` directory. Source files live at the repo root.

- `main.rs` — binary entry point, parses CLI args
- `lib.rs` — library root, re-exports modules
- `cmd.rs` — CLI definition (clap derive): `Cli`, `Command` enum, `Format` enum

The `Format` enum defines supported archive formats: Zip, Tar, TarGz, TarZst, TarXz, TarBz2.
Compression algorithms (gzip, zstd, xz, bzip2) are used as layers inside tar pipelines, not
exposed as standalone formats.

Format is inferred from file extension or magic bytes (`infer` crate) when not explicitly specified.

## Enforced Conventions (clippy + clippy.toml)

These are **deny**-level lints — the build will fail if violated:

- **No `std::path`**: use `camino::Utf8PathBuf` / `Utf8Path`
- **No `std::fs`**: use `fs_err` equivalents for all filesystem operations
- **No `unwrap()`, `expect()`, `panic!()`, `todo!()`, `unimplemented!()`**
- **No `dbg!()`, `println!()`, `eprintln!()`**

## Dependencies

Pure Rust implementations preferred. C-binding crates are behind optional features:

- `xz2` feature → `xz2` crate (C bindings to liblzma)
- `bzip2` feature → `bzip2` crate (C bindings to libbz2)

Default (pure Rust): `flate2` (gzip), `ruzstd` (zstd), `lzma-rs` (xz)
Archive crates: `tar`, `zip`

## Environment

NixOS host. Do not use `apt`, `brew`, or `npm -g`. Use `nix run nixpkgs#<pkg>` for ephemeral
tools or `nix shell nixpkgs#<pkg>` for a session.
