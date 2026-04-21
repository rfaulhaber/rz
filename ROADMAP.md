# rz — Feature Roadmap

A running list of features that would bring `rz` closer to feature-parity with
established archive tools (`tar`, `bsdtar`, `zip`/`unzip`, `7z`, `pigz`,
`zstd`, etc.). Items are ranked by expected user impact.

## Tier 1 — genuine parity gaps users will notice

1. **Preserve symlinks on compress** *(tar, bsdtar default)*
   Today the tool always dereferences symlinks; `--follow-symlinks` / `-H`
   (see `cmd.rs`) is the only mode. Standard `tar` stores symlinks by default
   and dereferences only with `-h`. Inverting the default (storing symlinks)
   and keeping `-H` as the opt-in to follow is the single biggest fidelity win.
   The `tar` crate's `Builder::follow_symlinks(false)` handles it.

2. **`append` / `update` / `delete` subcommands** *(tar -r/-u/--delete, zip -u/-d)*
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

4. **Ownership/permission preservation symmetry**
   `-P` / `--preserve-permissions` only restores mode bits today. Worth adding:
   - `--same-owner` / `--numeric-owner` (restore uid/gid)
   - `--owner=NAME`, `--group=NAME`, `--mode=M` override flags on compress for
     reproducible archives.

6. **Parallel compression** *(pigz, `zstd -T N`, `xz -T N`)*
   `rayon` is already a dependency. A global `-T N` / `--threads N` that maps
   to each backend's thread knob (zstd has it natively; gzip needs a
   pigz-style block strategy or a parallel-gzip crate).

## Tier 2 — ergonomic and scripting wins

7. **Archive conversion / repack** — `rz convert a.tar.gz -o a.tar.zst`
   Today the same effect requires `rz d | rz c` through stdin/stdout. A
   dedicated subcommand can skip redundant CRC verification and, for pure
   compression-layer swaps (`.gz` → `.zst`), stream entries through without
   decoding to tar level.

9. **Shell completions + man pages** *(`clap_complete`, `clap_mangen`)*
   Zero behavior change, instant discoverability.

   ```sh
   rz completions bash > /etc/bash_completion.d/rz
   rz man > rz.1
   ```

10. **Time-based filters** — `--newer-than DATE`, `--older-than DATE`
    GNU tar's `--newer` / `--newer-mtime`. Fits the existing filter pipeline.

11. **Transform / rename rules** — `--transform 's/foo/bar/'` on extract
    Simpler variants: `--rename OLD=NEW`, `--prefix PATH`.

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

## Notes on scope

Adding more formats (`.rar`, `.lz4`, `.br`) is tempting but doesn't advance
the core premise of *unifying* the common formats. Depth (encryption, parallel,
reproducibility) over breadth.
