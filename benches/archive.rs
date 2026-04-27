// Benchmark suite for rz archive operations.
//
// Run with: cargo bench
// Run a single benchmark: cargo bench -- info/zip
//
// Benchmarks are parameterised by entry count to expose how each format
// scales with number of entries — the key factor in operations like `info`
// and `list`.

// Allow things that are deny-level in the library but fine in benchmarks.
#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::disallowed_types,
    clippy::disallowed_methods
)]

use std::io;

use camino::{Utf8Path, Utf8PathBuf};
use criterion::{BenchmarkId, Criterion, criterion_group, criterion_main};
use globset::GlobSet;

use rz::progress::NoProgress;
use rz::{CompressOpts, DecompressOpts, seven_z, tar, tar_gz, tar_xz, tar_zst, zip};

// ── Helpers ─────────────────────────────────────────────────────────────────

fn empty_globset() -> GlobSet {
    GlobSet::empty()
}

fn compress_opts() -> CompressOpts<'static> {
    CompressOpts::new(None, empty_globset())
}

fn decompress_opts() -> DecompressOpts<'static> {
    DecompressOpts::new(false, 0, empty_globset(), empty_globset())
}

/// Create a temporary directory populated with `n` files of `size` bytes each.
/// The data is deterministic and moderately compressible.
fn create_test_dir(num_files: usize, file_size: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().unwrap();
    for i in 0..num_files {
        let path = dir.path().join(format!("file_{i:04}.dat"));
        let data: Vec<u8> = (0..file_size)
            .map(|b| ((b.wrapping_mul(37).wrapping_add(i * 13)) % 251) as u8)
            .collect();
        std::fs::write(&path, &data).unwrap();
    }
    dir
}

/// Collect sorted UTF-8 file paths from a directory.
fn collect_paths(dir: &std::path::Path) -> Vec<Utf8PathBuf> {
    let mut paths: Vec<Utf8PathBuf> = std::fs::read_dir(dir)
        .unwrap()
        .map(|e| Utf8PathBuf::from_path_buf(e.unwrap().path()).unwrap())
        .collect();
    paths.sort();
    paths
}

// ── Fixture ─────────────────────────────────────────────────────────────────

/// Holds the source files and pre-built archives so benchmark iterations don't
/// pay for setup.
struct Fixture {
    /// Source files directory (kept alive via TempDir).
    source_dir: tempfile::TempDir,
    /// Sorted paths to source files.
    sources: Vec<Utf8PathBuf>,
    /// Directory containing pre-built archives.
    archive_dir: tempfile::TempDir,
    /// UTF-8 path to archive directory.
    archive_root: Utf8PathBuf,
}

impl Fixture {
    fn new(num_files: usize, file_size: usize) -> Self {
        let source_dir = create_test_dir(num_files, file_size);
        let sources = collect_paths(source_dir.path());

        let archive_dir = tempfile::tempdir().unwrap();
        let archive_root = Utf8PathBuf::from_path_buf(archive_dir.path().to_path_buf()).unwrap();
        let opts = compress_opts();

        // Pre-build an archive in every format.
        zip::compress(&sources, &archive_root.join("test.zip"), &opts).unwrap();
        tar::compress(&sources, &archive_root.join("test.tar"), &opts).unwrap();
        tar_gz::compress(&sources, &archive_root.join("test.tar.gz"), &opts).unwrap();
        tar_zst::compress(&sources, &archive_root.join("test.tar.zst"), &opts).unwrap();
        tar_xz::compress(&sources, &archive_root.join("test.tar.xz"), &opts).unwrap();
        seven_z::compress(&sources, &archive_root.join("test.7z"), &opts).unwrap();

        Self {
            source_dir,
            sources,
            archive_dir,
            archive_root,
        }
    }

    fn archive(&self, name: &str) -> Utf8PathBuf {
        self.archive_root.join(name)
    }
}

// ── Format descriptor ───────────────────────────────────────────────────────

struct FormatDesc {
    name: &'static str,
    filename: &'static str,
    compress: fn(&[Utf8PathBuf], &Utf8Path, &CompressOpts<'_>) -> rz::error::Result<()>,
    decompress: fn(&Utf8Path, &Utf8Path, &DecompressOpts<'_>) -> rz::error::Result<()>,
    list: fn(&Utf8Path) -> rz::error::Result<Vec<rz::Entry>>,
    info: fn(&Utf8Path) -> rz::error::Result<rz::ArchiveInfo>,
    test: fn(&Utf8Path, &dyn rz::progress::ProgressReport) -> rz::error::Result<()>,
}

const FORMATS: &[FormatDesc] = &[
    FormatDesc {
        name: "zip",
        filename: "test.zip",
        compress: zip::compress,
        decompress: zip::decompress,
        list: zip::list,
        info: zip::info,
        test: zip::test,
    },
    FormatDesc {
        name: "tar",
        filename: "test.tar",
        compress: tar::compress,
        decompress: tar::decompress,
        list: tar::list,
        info: tar::info,
        test: tar::test,
    },
    FormatDesc {
        name: "tar.gz",
        filename: "test.tar.gz",
        compress: tar_gz::compress,
        decompress: tar_gz::decompress,
        list: tar_gz::list,
        info: tar_gz::info,
        test: tar_gz::test,
    },
    FormatDesc {
        name: "tar.zst",
        filename: "test.tar.zst",
        compress: tar_zst::compress,
        decompress: tar_zst::decompress,
        list: tar_zst::list,
        info: tar_zst::info,
        test: tar_zst::test,
    },
    FormatDesc {
        name: "tar.xz",
        filename: "test.tar.xz",
        compress: tar_xz::compress,
        decompress: tar_xz::decompress,
        list: tar_xz::list,
        info: tar_xz::info,
        test: tar_xz::test,
    },
    FormatDesc {
        name: "7z",
        filename: "test.7z",
        compress: seven_z::compress,
        decompress: seven_z::decompress,
        list: seven_z::list,
        info: seven_z::info,
        test: seven_z::test,
    },
];

// ── Entry-count parameter sets ──────────────────────────────────────────────

/// (label, num_files, file_size_bytes)
///
/// `10x4KB` and `500x4KB` vary entry count at fixed payload to expose per-entry
/// overhead. `1x1MB` flips this — a single large file isolates codec throughput
/// from archive bookkeeping. 1 MB (not 10 MB) keeps the slowest format (7z
/// compress at ~480 MB/s) under ~500 ms/iter so criterion can still draw a
/// reasonable sample.
const SIZES: &[(&str, usize, usize)] = &[
    ("10x4KB", 10, 4096),
    ("500x4KB", 500, 4096),
    ("1x1MB", 1, 1_048_576),
];

// ── Benchmarks ──────────────────────────────────────────────────────────────

fn bench_info(c: &mut Criterion) {
    let mut group = c.benchmark_group("info");

    for &(label, num_files, file_size) in SIZES {
        let fixture = Fixture::new(num_files, file_size);

        for fmt in FORMATS {
            let archive = fixture.archive(fmt.filename);
            group.bench_with_input(BenchmarkId::new(fmt.name, label), &archive, |b, archive| {
                b.iter(|| {
                    (fmt.info)(archive).unwrap();
                });
            });
        }
    }

    group.finish();
}

fn bench_list(c: &mut Criterion) {
    let mut group = c.benchmark_group("list");

    for &(label, num_files, file_size) in SIZES {
        let fixture = Fixture::new(num_files, file_size);

        for fmt in FORMATS {
            let archive = fixture.archive(fmt.filename);
            group.bench_with_input(BenchmarkId::new(fmt.name, label), &archive, |b, archive| {
                b.iter(|| {
                    (fmt.list)(archive).unwrap();
                });
            });
        }
    }

    group.finish();
}

fn bench_test(c: &mut Criterion) {
    let mut group = c.benchmark_group("test");

    for &(label, num_files, file_size) in SIZES {
        let fixture = Fixture::new(num_files, file_size);

        for fmt in FORMATS {
            let archive = fixture.archive(fmt.filename);
            group.bench_with_input(BenchmarkId::new(fmt.name, label), &archive, |b, archive| {
                b.iter(|| {
                    (fmt.test)(archive, &NoProgress).unwrap();
                });
            });
        }
    }

    group.finish();
}

fn bench_compress(c: &mut Criterion) {
    let mut group = c.benchmark_group("compress");

    for &(label, num_files, file_size) in SIZES {
        let fixture = Fixture::new(num_files, file_size);

        for fmt in FORMATS {
            group.bench_function(BenchmarkId::new(fmt.name, label), |b| {
                b.iter_with_large_drop(|| {
                    let out_dir = tempfile::tempdir().unwrap();
                    let out_path =
                        Utf8PathBuf::from_path_buf(out_dir.path().join(fmt.filename)).unwrap();
                    (fmt.compress)(&fixture.sources, &out_path, &compress_opts()).unwrap();
                    out_dir // returned so drop (cleanup) isn't timed
                });
            });
        }
    }

    group.finish();
}

fn bench_decompress(c: &mut Criterion) {
    let mut group = c.benchmark_group("decompress");

    for &(label, num_files, file_size) in SIZES {
        let fixture = Fixture::new(num_files, file_size);

        for fmt in FORMATS {
            let archive = fixture.archive(fmt.filename);
            group.bench_with_input(BenchmarkId::new(fmt.name, label), &archive, |b, archive| {
                b.iter_with_large_drop(|| {
                    let out_dir = tempfile::tempdir().unwrap();
                    let out_path =
                        Utf8PathBuf::from_path_buf(out_dir.path().to_path_buf()).unwrap();
                    (fmt.decompress)(archive, &out_path, &decompress_opts()).unwrap();
                    out_dir
                });
            });
        }
    }

    group.finish();
}

fn bench_decompress_to_sink(c: &mut Criterion) {
    let mut group = c.benchmark_group("decompress_to_sink");

    // Only zip supports decompress_to_writer from file path directly.
    // For tar-based formats we use their decompress_to_writer too.
    // This benchmark isolates decompression speed from filesystem write overhead.

    for &(label, num_files, file_size) in SIZES {
        let fixture = Fixture::new(num_files, file_size);

        // zip
        {
            let archive = fixture.archive("test.zip");
            group.bench_with_input(BenchmarkId::new("zip", label), &archive, |b, archive| {
                b.iter(|| {
                    zip::decompress_to_writer(archive, &mut io::sink(), &decompress_opts())
                        .unwrap();
                });
            });
        }

        // tar
        {
            let archive = fixture.archive("test.tar");
            group.bench_with_input(BenchmarkId::new("tar", label), &archive, |b, archive| {
                b.iter(|| {
                    tar::decompress_to_writer(archive, &mut io::sink(), &decompress_opts())
                        .unwrap();
                });
            });
        }

        // tar.gz
        {
            let archive = fixture.archive("test.tar.gz");
            group.bench_with_input(BenchmarkId::new("tar.gz", label), &archive, |b, archive| {
                b.iter(|| {
                    tar_gz::decompress_to_writer(archive, &mut io::sink(), &decompress_opts())
                        .unwrap();
                });
            });
        }

        // tar.zst
        {
            let archive = fixture.archive("test.tar.zst");
            group.bench_with_input(
                BenchmarkId::new("tar.zst", label),
                &archive,
                |b, archive| {
                    b.iter(|| {
                        tar_zst::decompress_to_writer(archive, &mut io::sink(), &decompress_opts())
                            .unwrap();
                    });
                },
            );
        }

        // tar.xz
        {
            let archive = fixture.archive("test.tar.xz");
            group.bench_with_input(BenchmarkId::new("tar.xz", label), &archive, |b, archive| {
                b.iter(|| {
                    tar_xz::decompress_to_writer(archive, &mut io::sink(), &decompress_opts())
                        .unwrap();
                });
            });
        }

        // 7z
        {
            let archive = fixture.archive("test.7z");
            group.bench_with_input(BenchmarkId::new("7z", label), &archive, |b, archive| {
                b.iter(|| {
                    seven_z::decompress_to_writer(archive, &mut io::sink(), &decompress_opts())
                        .unwrap();
                });
            });
        }
    }

    group.finish();
}

criterion_group!(
    benches,
    bench_info,
    bench_list,
    bench_test,
    bench_compress,
    bench_decompress,
    bench_decompress_to_sink,
);
criterion_main!(benches);
