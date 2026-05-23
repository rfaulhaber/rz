use std::io::Write;

use indicatif::{ProgressBar, ProgressStyle};

/// Trait for reporting progress during archive operations.
///
/// Two implementations exist: [`BarProgress`] (real progress bar on stderr)
/// and [`NoProgress`] (silent no-op).  Using a trait lets the format modules
/// remain completely unaware of the progress UI.
pub trait ProgressReport: Send + Sync {
    /// Set the total expected byte count (enables percentage + ETA).
    fn set_length(&self, len: u64);

    /// Report that `n` additional bytes have been processed.
    fn inc(&self, n: u64);

    /// Return the accumulated byte count so far.
    fn position(&self) -> u64;

    /// Report that a named entry is being processed (shown as the bar message).
    fn set_entry(&self, name: &str);

    /// Mark the operation as complete and remove the progress bar.
    fn finish(&self);
}

// ── No-op implementation ─────────────────────────────────────────────────────

/// Silent progress reporter — used when `--progress` is not passed.
pub struct NoProgress;

impl ProgressReport for NoProgress {
    fn set_length(&self, _len: u64) {}
    fn inc(&self, _n: u64) {}
    fn position(&self) -> u64 {
        0
    }
    fn set_entry(&self, _name: &str) {}
    fn finish(&self) {}
}

// ── indicatif-backed implementation ──────────────────────────────────────────

/// Real progress bar that renders on stderr via `indicatif`.
pub struct BarProgress {
    bar: ProgressBar,
}

impl BarProgress {
    /// Create a byte-counting progress bar with a known total (for decompress).
    pub fn bytes(total: u64) -> Self {
        let bar = ProgressBar::new(total);
        bar.set_style(
            ProgressStyle::default_bar()
                .template("{bar:40.cyan/blue} {bytes}/{total_bytes} ({eta}) {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_bar()),
        );
        Self { bar }
    }

    /// Create a byte-counting progress bar without a known total (for compress).
    pub fn spinner() -> Self {
        let bar = ProgressBar::new_spinner();
        bar.set_style(
            ProgressStyle::default_spinner()
                .template("{spinner:.green} {bytes} ({bytes_per_sec}) {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        bar.enable_steady_tick(std::time::Duration::from_millis(120));
        Self { bar }
    }

    /// Create a hidden progress bar that tracks bytes but renders nothing.
    /// Useful for `--totals` without `--progress`.
    pub fn hidden() -> Self {
        Self {
            bar: ProgressBar::hidden(),
        }
    }
}

impl ProgressReport for BarProgress {
    fn set_length(&self, len: u64) {
        self.bar.set_length(len);
    }

    fn inc(&self, n: u64) {
        self.bar.inc(n);
    }

    fn position(&self) -> u64 {
        self.bar.position()
    }

    fn set_entry(&self, name: &str) {
        self.bar.set_message(name.to_owned());
    }

    fn finish(&self) {
        self.bar.finish_and_clear();
    }
}

// ── Verbose decorator ───────────────────────────────────────────────────────

/// Decorator that prints each entry name to stderr before delegating to an
/// inner [`ProgressReport`].  Used when `--verbose` is passed.
pub struct VerboseReport<'a> {
    inner: &'a dyn ProgressReport,
}

impl<'a> VerboseReport<'a> {
    pub fn new(inner: &'a dyn ProgressReport) -> Self {
        Self { inner }
    }
}

impl ProgressReport for VerboseReport<'_> {
    fn set_length(&self, len: u64) {
        self.inner.set_length(len);
    }

    fn inc(&self, n: u64) {
        self.inner.inc(n);
    }

    fn position(&self) -> u64 {
        self.inner.position()
    }

    fn set_entry(&self, name: &str) {
        let mut stderr = std::io::stderr().lock();
        let _ = writeln!(stderr, "{name}");
        self.inner.set_entry(name);
    }

    fn finish(&self) {
        self.inner.finish();
    }
}
