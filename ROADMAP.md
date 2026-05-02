# rz — Feature Roadmap

A running list of features that would bring `rz` closer to feature-parity with
established archive tools (`tar`, `bsdtar`, `zip`/`unzip`, `7z`, `pigz`,
`zstd`, etc.). Items are ranked by expected user impact.

## Tier 1 — genuine parity gaps users will notice

1. ~~**Preserve symlinks on compress**~~ — **DONE.** `CompressOpts::follow_symlinks`
   defaults to `false` and the tar builder stores symlinks by default; zip
   writes POSIX symlink entries (`zip.rs::write_symlink_entry`). Integration
   coverage in `tests/symlinks.rs`.

2. ~~**`append` / `update` / `delete` subcommands** *(tar -r/-u/--delete, zip -u/-d)*~~ **DONE.**
   Archives are currently immutable once written. Natural fit:

   ```sh
   rz append archive.tar.gz newfile
   rz update archive.tar.gz src/   # only changed mtimes
   rz remove archive.zip '*.log'
   ```

   Compressed-tar streams can't be appended in place without re-encoding the
   compression layer — implementation is read-then-rewrite. Uncompressed tar
   and zip support in-place append.

3. **Encryption** *(zip AES-256, 7z AES-256)*
   `--password` / `--password-file` / `--password-stdin`. The `zip` crate
   supports AES and ZipCrypto; `sevenz-rust2` has encryption. Default to
   `--password-stdin` for safety.

4. **Ownership/permission preservation symmetry** — **DONE.**
   - Compress: `--mtime` / `--owner` / `--group` / `--mode` override the
     corresponding tar header fields. `--mode` accepts `644`, `0644`, or
     `0o644` and is range-checked against the 12-bit permission mask.
   - Decompress: `--same-owner` (alias `--numeric-owner`) restores uid/gid
     via `tar::Archive::set_preserve_ownerships`, which silently no-ops for
     non-root — matching GNU tar's behavior under CAP_CHOWN.
   Tar-family only on both sides. Zip lacks portable UID/GID fields and
   `sevenz-rust2` doesn't expose per-entry overrides, so all six flags are
   rejected up front on those formats rather than silently no-oping.

6. ~~**Parallel compression** *(pigz, `zstd -T N`, `xz -T N`)*~~ — **DONE.**
   `--threads N` global flag configures rayon's global thread pool before any
   archive operation. gzip and zstd block compression, zip parallel
   decompression, and any other rayon-backed path all pick up the setting
   automatically. `0` (or omitted) lets rayon auto-detect from physical cores.

## Tier 2 — ergonomic and scripting wins

7. ~~**Archive conversion / repack**~~ — **DONE.** `rz convert a.tar.gz -o a.tar.zst`
   Implemented via extract-to-tempdir + re-compress path, which handles all
   format combinations (tar-family ↔ zip ↔ 7z).  Supports `--to <FORMAT>`
   to derive the output path from the input stem, `--level`, and `-F/--force`.
   Integration coverage in `tests/convert.rs`.
   The entry-stream optimization for pure compression-layer swaps (`.gz` →
   `.zst` without decoding to tar level) is deferred as a future enhancement.

9. ~~**Shell completions + man pages**~~ — **DONE.** `rz completions <shell>`
   and `rz man` emit clap-generated output. Packaging them into `nix build`
   outputs (man page, completions bundle) is still a todo.

10. **Time-based filters** — **DONE.** `--newer-than DATE` / `--older-than
    DATE` on both compress and decompress. Accepts RFC 3339
    (`2024-01-02T03:04:05Z`), date-only `YYYY-MM-DD` (midnight UTC), and
    `@<unix-seconds>`. Bounds are exclusive, matching GNU tar's
    `--newer`/`--newer-mtime` semantics. Tar-family only on both sides:
    compress reads filesystem mtime in the walker; decompress reads the tar
    header. Zip and 7z reject the flags up-front because their entries lack
    reliable per-entry mtime through `zip` / `sevenz-rust2`.

11. **Transform / rename rules** — ~~`--rename OLD=NEW`~~ and ~~`--prefix PATH`~~
    **DONE (partial).** `--rename OLD=NEW` (repeatable) and `--prefix PATH` are
    implemented and wired into tar-family, zip, and 7z extraction paths.
    Path safety is re-validated after rewriting so hostile rules can't inject
    `..` or absolute paths.  `--transform 's/foo/bar/'` (sed-style regex) is
    deferred — higher complexity, not yet implemented.

## Tier 3 — niche but notable

14. **Split / multi-volume archives** — `zip -s 100m`, `7z -v100m`. Uncommon.

15. **Hard-link dedup** in tar compress — two hard links to the same inode are
    stored twice today. Requires an inode-tracking pass.

16. **Extended attributes / ACLs** — tar `--xattrs`. Cross-platform support in
    the `tar` crate is patchy.

17. **Streaming reads from URLs** — `bsdtar -xf https://…`. Adds an HTTP dep.

18. **Checksums on list** — `rz list --checksum sha256` emits
    `sha256sum`-compatible output.

19. **Sparse file support** *(tar -S)* — tricky in Rust, probably skip.

## Code quality & technical debt

Items surfaced during the April 2026 codebase review. Most of the batch has
landed on branch `robot-fixes` (10 commits); the three items below are
deferred because they're substantial and warrant dedicated design work.

### Deferred

**P1. Streaming parallel compression** *(tar.gz, tar.zst)*
Today `parallel_gz_compress` / `parallel_zst_compress` buffer the entire
uncompressed tar in RAM before splitting into 1 MiB blocks. Peak memory is
at least the uncompressed archive size — documented as deliberate in
`tar_gz.rs:14` / `tar_zst.rs:12`, but a multi-GB archive is still a problem.
A redesign would tee the tar builder's output into a bounded channel of
1 MiB blocks, with worker threads compressing in parallel and a writer
thread draining compressed frames in order. Needs care to preserve the
current throughput win; worth benchmarking against the current path.

**R1. Extract per-subcommand handlers from `main.rs::run()`**
`run()` is ~500 lines with six match arms (Compress, Decompress, List, Test,
Info, Formats) plus Completions/Man. Shared locals (`progress`,
`verbose_progress`, `base_progress`, `excludes`) complicate a naive
extraction — the progress trait object's lifetime is the trickiest part.
Likely shape: one `fn run_compress(args, cli) -> Result<()>` per subcommand,
with a small helper that builds the progress chain from `cli.progress` /
`cli.verbose` / `totals`.

**R2. `ArchiveBackend` trait for format dispatch**
Every subcommand currently has a large `match fmt { Format::Zip => …,
Format::TarGz => …, … }` block that calls module-level free functions. A
trait per backend (`trait ArchiveBackend { fn compress(...); fn
decompress(...); fn list(...); ... }`) with a `fn backend(fmt: &Format) ->
&'static dyn ArchiveBackend` resolver would collapse seven match arms to
one call site per subcommand. Best tackled *after* R1, since the handlers
become the natural consumers of the trait.

### Completed (April 2026)

Tracked on branch `robot-fixes`; see commit messages for rationale and
mechanical details.

- **B1/B6.** Zip decompress now recreates symlinks (was writing the target
  string as a file) and masks stored mode to `0o7777`.
- **B2/B3.** Tar decompress rejects symlink/hardlink targets that are
  absolute or contain `..` — closes a path-traversal hole.
- **B4.** 7z `--keep-newer` now errors explicitly instead of silently
  degrading to "skip existing" (7z entries lack reliable mtime).
- **B5.** Zip/7z compress rejects `--mtime` / `--owner` / `--group` with a
  clear pointer to tar-family formats (was silently no-op).
- **B7.** Info-path size arithmetic uses `saturating_add` to guard against
  corrupt/adversarial archives overflowing `u64`.
- **B8.** `Format::default_output` falls back to `archive` for degenerate
  inputs (`/`, `.`, `..`, empty) instead of emitting `..tar.gz` et al.
- **R3.** Collapsed `build_include_set` / `build_exclude_set` wrappers into
  a single `build_glob_set`.
- **R4.** Moved `can_fast_path` off `DecompressOpts` into a 7z-local helper
  (backend-specific, doesn't belong on the shared type).
- **R5.** Added unit tests for the `MultiFrameDecoder` state machine.
- **R8.** `walk_dir_simple` uses `sort_by_cached_key` to avoid re-allocating
  `OsString` keys on every comparison.
- **P2.** `tar.gz` / `tar.zst` compress now skips rayon for inputs ≤ one
  block (rayon dispatch + `Vec<Vec<u8>>` was pure overhead in that case).
- **P3.** `Format::from_path` uses a byte-level case-insensitive suffix
  check instead of lowercasing the whole path.
- **C1.** Added `Display for Format` rendering the kebab-case clap value
  (error messages no longer leak `TarGz`-style variant names).
- **C2.** Reworded `ZstdLevelUnsupported` to tell the user what to do
  instead of implying only uncompressed output is possible.
- **C4.** Merged duplicate `impl BarProgress` blocks.
- **C7.** Added unit tests for `safe_entry_path`, `safe_link_target`, and
  `should_extract` precedence.

## Notes on scope

Adding more formats (`.rar`, `.lz4`, `.br`) is tempting but doesn't advance
the core premise of *unifying* the common formats. Depth (encryption, parallel,
reproducibility) over breadth.
