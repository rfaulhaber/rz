use std::borrow::Cow;
use std::io::Write;

use indicatif::{ProgressBar, ProgressStyle};

/// Escape control characters in an untrusted archive entry name for terminal
/// display.
///
/// Entry names are attacker-controlled bytes, and `rz list` is the natural
/// "inspect before you extract" step: a raw ESC lets CSI sequences (erase-
/// line, cursor-up) hide entries from the listing.  C0 controls, DEL, and C1
/// controls are rendered as `\xNN` (with `\n`/`\t`/`\r` kept mnemonic), and
/// backslash itself is doubled so the output stays unambiguous — the same
/// defanging GNU tar applies to `tar t` output.  JSON output needs none of
/// this; serde_json escapes controls itself.
pub fn escape_entry_name(name: &str) -> Cow<'_, str> {
    // char::is_control covers exactly C0, DEL, and C1.
    let needs_escape = |c: char| c.is_control() || c == '\\';
    if !name.chars().any(needs_escape) {
        return Cow::Borrowed(name);
    }
    use std::fmt::Write as _;
    let mut out = String::with_capacity(name.len() + 8);
    for c in name.chars() {
        match c {
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            c if needs_escape(c) => {
                let _ = write!(out, "\\x{:02x}", c as u32);
            }
            c => out.push(c),
        }
    }
    Cow::Owned(out)
}

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
        // The bar renders on the user's terminal, so the message needs the
        // same defanging as printed listings.
        self.bar.set_message(escape_entry_name(name).into_owned());
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
        let _ = writeln!(stderr, "{}", escape_entry_name(name));
        // The raw name goes on to the inner reporter, which escapes at its
        // own display boundary.
        self.inner.set_entry(name);
    }

    fn finish(&self) {
        self.inner.finish();
    }
}

#[cfg(test)]
mod tests {
    use super::escape_entry_name;

    #[test]
    fn plain_names_borrow_unchanged() {
        assert!(matches!(
            escape_entry_name("src/main.rs"),
            std::borrow::Cow::Borrowed("src/main.rs"),
        ));
    }

    #[test]
    fn esc_sequences_are_defanged() {
        assert_eq!(
            escape_entry_name("a\x1b[2Kb").as_ref(),
            "a\\x1b[2Kb",
        );
    }

    #[test]
    fn common_controls_stay_mnemonic() {
        assert_eq!(escape_entry_name("a\nb\tc\rd").as_ref(), "a\\nb\\tc\\rd");
    }

    #[test]
    fn del_and_c1_are_hex_escaped() {
        assert_eq!(escape_entry_name("a\x7fb").as_ref(), "a\\x7fb");
        assert_eq!(escape_entry_name("a\u{85}b").as_ref(), "a\\x85b");
    }

    #[test]
    fn backslash_is_doubled() {
        assert_eq!(escape_entry_name("a\\x1b").as_ref(), "a\\\\x1b");
    }
}
