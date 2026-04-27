# CLAUDE.md

This file provides guidance to Claude Code (claude.ai/code) when working with code in this repository.

## Project

`rz` is a multi-format archive CLI tool written in Rust. It wraps archive formats (tar, tar.gz,
tar.zst, tar.xz, tar.bz2, zip, 7z) behind a unified interface with `compress`, `decompress`,
`list`, `test`, and `info` subcommands. Think `tar` and `zip/unzip` rolled into one binary with a
consistent UI. Supports stdin/stdout streaming (`-`) for tar-based formats.

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

The `Format` enum defines supported archive formats: Zip, Tar, TarGz, TarZst, TarXz, TarBz2,
SevenZ. Compression algorithms (gzip, zstd, xz, bzip2) are used as layers inside tar pipelines,
not exposed as standalone formats.

Format is inferred from file extension or magic bytes (`infer` crate) when not explicitly specified.

Each format module exports up to six public functions with uniform signatures:
`compress`, `compress_to_writer`, `decompress`, `decompress_from_reader`,
`decompress_to_writer`, `decompress_reader_to_writer`, `test`, `list`, `info`.
The `_to_writer` / `_from_reader` variants enable stdin/stdout streaming for tar-based formats.

- `filter.rs` — exclude/include patterns, path stripping, tar/zip extraction helpers,
  `should_extract()` predicate, `verify_tar_entries()`, `extract_tar_to_writer()`
- `progress.rs` — `ProgressReport` trait, `BarProgress`, `NoProgress`, `VerboseReport` decorator

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

Default (pure Rust): `flate2` (gzip), `ruzstd` (zstd), `lzma-rust2` (xz)
Archive crates: `tar`, `zip`, `sevenz-rust2`

## Environment

NixOS host. Do not use `apt`, `brew`, or `npm -g`. Use `nix run nixpkgs#<pkg>` for ephemeral
tools or `nix shell nixpkgs#<pkg>` for a session.
