//! Deterministic regression coverage for the "final flush discarded" bug in
//! `compress_to_writer`: writing entries through successfully but then losing
//! the very last flush error because it only runs inside a dropped
//! `BufWriter`'s `Drop` impl (which discards its `Result`).
//!
//! The `/dev/full`-backed tests in `compress_stdout_flush.rs` exercise the
//! real stdout path end to end, but whether the bug actually reaches an
//! observable OS-level write failure depends on incidental buffering (a
//! coincidental byte in the compressed output can trigger an early flush that
//! masks the bug). This file isolates the exact invariant with a writer that
//! always succeeds at `write` and always fails at `flush`, so there's no
//! byte-content luck involved: any code path that never calls the final
//! `flush` will report success anyway.

mod helpers;

use std::io;

use helpers::{TestResult, build_file_tree, default_compress_opts, temp_utf8_dir};

/// Accepts every write but always fails to flush. Isolates the specific bug
/// where a discarded `BufWriter`'s `Drop` impl retries buffered *writes*, but
/// never calls the inner writer's `flush`.
struct FlushFails;

impl io::Write for FlushFails {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Err(io::Error::other("flush always fails"))
    }
}

#[test]
fn tar_compress_to_writer_propagates_flush_error() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let result = rz_archive::tar::compress_to_writer(
        std::slice::from_ref(&tree),
        FlushFails,
        &default_compress_opts(None),
    );
    assert!(result.is_err(), "expected the flush error to propagate");
    Ok(())
}

#[test]
#[cfg(feature = "bzip2")]
fn tar_bz2_compress_to_writer_propagates_flush_error() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let result = rz_archive::tar_bz2::compress_to_writer(
        std::slice::from_ref(&tree),
        FlushFails,
        &default_compress_opts(None),
    );
    assert!(result.is_err(), "expected the flush error to propagate");
    Ok(())
}

#[test]
fn tar_xz_compress_to_writer_propagates_flush_error() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let tree = tmp.join("tree");
    build_file_tree(&tree)?;

    let result = rz_archive::tar_xz::compress_to_writer(
        std::slice::from_ref(&tree),
        FlushFails,
        &default_compress_opts(None),
    );
    assert!(result.is_err(), "expected the flush error to propagate");
    Ok(())
}
