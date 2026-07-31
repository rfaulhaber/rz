//! Block-level tar reading for the modify paths.
//!
//! The `tar` crate cannot express two things the append/remove/update
//! rewrites need:
//!
//! - the *physical* extent of a GNU sparse entry (`Entry::size()` is the
//!   expanded logical size, extended sparse header blocks are consumed
//!   invisibly, and reading an `Entry` materialises the holes as zeros), and
//! - byte-exact re-emission (the high-level copy re-encodes headers, which
//!   drops pax records and re-derives long-name extensions).
//!
//! So the modify paths work on raw 512-byte blocks instead.  Entries are
//! handled as *groups*: any run of GNU long-name (`L`) / long-link (`K`) /
//! pax (`x`) meta headers plus the real header and its payload.  A group is
//! either copied through verbatim or skipped whole; either way the physical
//! layout — sparse maps included — is untouched.

use std::io::{Read, Write};

use crate::error::{Error, Result};

const BLOCK: usize = 512;

/// Metadata of one logical entry, resolved across its meta headers.
pub struct RawEntry {
    /// Full entry name: GNU `L` payload, else pax `path` record, else the
    /// ustar prefix+name fields.
    pub name: String,
    /// Modification time in unix seconds (pax `mtime` record wins).
    pub mtime: u64,
    /// For hard-link entries (typeflag `1`), the name of the entry linked to:
    /// GNU `K` payload, else pax `linkpath` record, else the header linkname
    /// field.  Update planning needs it — dropping a link's target entry
    /// reorders the target *after* the link, which no extractor can resolve.
    pub hardlink_target: Option<String>,
}

/// Outcome of scanning a whole archive.
pub struct Scan {
    pub entries: Vec<RawEntry>,
    /// Byte offset just past the last entry's payload padding — where the
    /// EOF terminator (or appended entries) begin.
    pub body_end: u64,
}

/// One parsed 512-byte header block.
struct Header {
    block: [u8; BLOCK],
    typeflag: u8,
    size: u64,
}

impl Header {
    /// Name from the ustar name/prefix fields.  The prefix only exists in
    /// POSIX ustar headers ("ustar\0"); GNU headers reuse those bytes for
    /// other fields.
    fn short_name(&self) -> Result<String> {
        let name = trimmed(&self.block[0..100]);
        let use_prefix = &self.block[257..263] == b"ustar\0";
        let bytes = if use_prefix {
            let prefix = trimmed(&self.block[345..500]);
            if prefix.is_empty() {
                name.to_vec()
            } else {
                let mut joined = prefix.to_vec();
                joined.push(b'/');
                joined.extend_from_slice(name);
                joined
            }
        } else {
            name.to_vec()
        };
        String::from_utf8(bytes)
            .map_err(|e| Error::InvalidUtf8Path(String::from_utf8_lossy(e.as_bytes()).into_owned()))
    }

    fn mtime(&self) -> u64 {
        parse_numeric(&self.block[136..148]).unwrap_or(0)
    }

    /// Linkname from the fixed header field.  Lossy: link targets are
    /// arbitrary bytes, and a mismatched byte only affects name matching in
    /// the update planner — never the copied bytes themselves.
    fn short_link(&self) -> String {
        String::from_utf8_lossy(trimmed(&self.block[157..257])).into_owned()
    }

    /// GNU sparse: does the base header announce extended sparse blocks?
    fn gnu_sparse_is_extended(&self) -> bool {
        self.block[482] != 0
    }
}

/// Strip trailing NULs/spaces from a fixed field.
fn trimmed(field: &[u8]) -> &[u8] {
    let end = field
        .iter()
        .position(|&b| b == 0)
        .unwrap_or(field.len());
    let mut slice = &field[..end];
    while let [rest @ .., b' '] = slice {
        slice = rest;
    }
    slice
}

/// Parse a tar numeric field: octal, or GNU base-256 when the top bit of the
/// first byte is set.
fn parse_numeric(field: &[u8]) -> Result<u64> {
    let Some((&first, _)) = field.split_first() else {
        return Ok(0);
    };
    if first & 0x80 != 0 {
        // Base-256: big-endian two's complement with the marker bit masked
        // off.  Reject values that don't fit u64 rather than truncating.
        let mut value: u64 = u64::from(first & 0x7f);
        for &b in &field[1..] {
            if value > (u64::MAX >> 8) {
                return Err(corrupt("base-256 numeric field overflows u64"));
            }
            value = (value << 8) | u64::from(b);
        }
        return Ok(value);
    }
    // Historic producers pad octal fields with *leading* spaces; strip them
    // so a modify path never refuses an archive the read paths accept.
    let mut text = trimmed(field);
    while let [b' ', rest @ ..] = text {
        text = rest;
    }
    if text.is_empty() {
        return Ok(0);
    }
    let mut value: u64 = 0;
    for &b in text {
        if !(b'0'..=b'7').contains(&b) {
            return Err(corrupt("invalid octal digit in header numeric field"));
        }
        value = value
            .checked_mul(8)
            .and_then(|v| v.checked_add(u64::from(b - b'0')))
            .ok_or_else(|| corrupt("octal numeric field overflows u64"))?;
    }
    Ok(value)
}

fn corrupt(what: &str) -> Error {
    Error::Io(std::io::Error::other(format!("corrupt tar archive: {what}")))
}

/// Verify the header checksum the way tar implementations do: sum of all
/// bytes with the checksum field read as spaces, accepting both the unsigned
/// and the historic signed variants.
fn verify_checksum(block: &[u8; BLOCK]) -> Result<()> {
    let stored = parse_numeric(&block[148..156])?;
    let mut unsigned: u64 = 0;
    let mut signed: i64 = 0;
    for (i, &b) in block.iter().enumerate() {
        let byte = if (148..156).contains(&i) { b' ' } else { b };
        unsigned += u64::from(byte);
        signed += i64::from(byte as i8);
    }
    if stored == unsigned || i64::try_from(stored) == Ok(signed) {
        Ok(())
    } else {
        Err(corrupt("header checksum mismatch"))
    }
}

/// Round a payload size up to whole blocks.  Checked: a hostile base-256
/// size near `u64::MAX` parses fine, and the unchecked multiply either
/// panicked (debug) or wrapped the payload length to zero (release) —
/// which would reinterpret entry data as headers.
fn round_up(n: u64) -> Result<u64> {
    n.div_ceil(BLOCK as u64)
        .checked_mul(BLOCK as u64)
        .ok_or_else(|| corrupt("entry size field overflows the archive"))
}

/// Upper bound for a meta header's payload (long names, pax records).  Real
/// ones are at most a few KiB; the size field is attacker-controlled and
/// used to allocate, so an absurd value must be an error, not an OOM abort.
const MAX_META_PAYLOAD: u64 = 16 * 1024 * 1024;

/// Fill `buf` from `reader`, erroring on a short read.
fn read_block<R: Read + ?Sized>(reader: &mut R, buf: &mut [u8]) -> Result<()> {
    reader
        .read_exact(buf)
        .map_err(|_| corrupt("unexpected end of archive"))?;
    Ok(())
}

/// Pax override values parsed from an `x` header's records.
#[derive(Default)]
struct PaxOverrides {
    path: Option<String>,
    size: Option<u64>,
    mtime: Option<u64>,
    linkpath: Option<String>,
    /// GNU pax sparse (0.x/1.0): the entry's *real* name.  The ustar header
    /// (and any `path` record for long names) carries the mangled
    /// `GNUSparseFile.<pid>/<name>` spelling, so this must win — matching on
    /// the mangled name made `update` re-append forever without ever
    /// superseding the stale copy.
    sparse_name: Option<String>,
}

fn parse_pax_records(payload: &[u8]) -> Result<PaxOverrides> {
    let mut out = PaxOverrides::default();
    let mut rest = payload;
    while !rest.is_empty() {
        // "%d key=value\n" — the decimal length covers the whole record.
        let space = rest
            .iter()
            .position(|&b| b == b' ')
            .ok_or_else(|| corrupt("malformed pax record"))?;
        let len: usize = std::str::from_utf8(&rest[..space])
            .ok()
            .and_then(|s| s.parse().ok())
            .ok_or_else(|| corrupt("malformed pax record length"))?;
        if len <= space + 1 || len > rest.len() {
            return Err(corrupt("pax record length out of bounds"));
        }
        let record = &rest[space + 1..len];
        rest = &rest[len..];
        let record = record.strip_suffix(b"\n").unwrap_or(record);
        let Some(eq) = record.iter().position(|&b| b == b'=') else {
            continue;
        };
        let (key, value) = (&record[..eq], &record[eq + 1..]);
        match key {
            b"path" => {
                out.path = Some(String::from_utf8(value.to_vec()).map_err(|e| {
                    Error::InvalidUtf8Path(String::from_utf8_lossy(e.as_bytes()).into_owned())
                })?);
            }
            b"size" => {
                out.size = std::str::from_utf8(value).ok().and_then(|s| s.parse().ok());
            }
            b"mtime" => {
                // May be fractional ("1234.5678"); the integer part is all
                // the update gate needs.
                out.mtime = std::str::from_utf8(value)
                    .ok()
                    .and_then(|s| s.split('.').next()?.parse().ok());
            }
            b"linkpath" => {
                out.linkpath = Some(String::from_utf8_lossy(value).into_owned());
            }
            b"GNU.sparse.name" => {
                out.sparse_name = Some(String::from_utf8_lossy(value).into_owned());
            }
            _ => {}
        }
    }
    Ok(out)
}

/// What to do with each entry group during [`process`].
enum Sink<'a, W: Write> {
    /// Copy kept groups through verbatim; drop the rest.
    Copy {
        writer: &'a mut W,
        keep: &'a mut dyn FnMut(&str) -> bool,
    },
    /// Metadata only.
    ScanOnly,
}

/// Drive the block-level walk.  Shared by [`scan`] and [`copy_entries`], so
/// the offset math that decides where an entry physically ends exists exactly
/// once.
fn process<R: Read + ?Sized, W: Write>(reader: &mut R, mut sink: Sink<'_, W>) -> Result<Scan> {
    let mut entries = Vec::new();
    let mut pos: u64 = 0;

    // Meta headers (with payloads) buffered for the current group.
    let mut pending: Vec<Vec<u8>> = Vec::new();
    let mut long_name: Option<Vec<u8>> = None;
    let mut long_link: Option<Vec<u8>> = None;
    let mut pax: Option<PaxOverrides> = None;
    // body_end trails `pos`, marking the end of the last *complete* group so
    // a truncated trailing group is not counted.
    let mut body_end: u64 = 0;

    let mut block = [0u8; BLOCK];
    loop {
        match reader.read_exact(&mut block) {
            Ok(()) => {}
            // EOF exactly at a block boundary with no terminator: GNU tar
            // tolerates this, so we do too.
            Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => break,
            Err(e) => return Err(e.into()),
        }
        if block.iter().all(|&b| b == 0) {
            break;
        }
        verify_checksum(&block)?;
        pos += BLOCK as u64;

        let header = Header {
            typeflag: block[156],
            size: parse_numeric(&block[124..136])?,
            block,
        };

        match header.typeflag {
            // GNU long name / long link: payload names the next real entry.
            b'L' | b'K' => {
                if header.size > MAX_META_PAYLOAD {
                    return Err(corrupt("long-name header declares an absurd size"));
                }
                let padded = round_up(header.size)?;
                let mut payload = vec![0u8; padded as usize];
                read_block(reader, &mut payload)?;
                pos += padded;
                let mut value = payload[..header.size as usize].to_vec();
                while value.last() == Some(&0) {
                    value.pop();
                }
                if header.typeflag == b'L' {
                    long_name = Some(value);
                } else {
                    long_link = Some(value);
                }
                let mut group_part = header.block.to_vec();
                group_part.extend_from_slice(&payload);
                pending.push(group_part);
            }
            // Pax extended header for the next entry.
            b'x' => {
                if header.size > MAX_META_PAYLOAD {
                    return Err(corrupt("pax header declares an absurd size"));
                }
                let padded = round_up(header.size)?;
                let mut payload = vec![0u8; padded as usize];
                read_block(reader, &mut payload)?;
                pos += padded;
                pax = Some(parse_pax_records(&payload[..header.size as usize])?);
                let mut group_part = header.block.to_vec();
                group_part.extend_from_slice(&payload);
                pending.push(group_part);
            }
            // Pax global header: applies to everything after it, so it is
            // always carried through, independent of keep decisions.
            b'g' => {
                if header.size > MAX_META_PAYLOAD {
                    return Err(corrupt("pax global header declares an absurd size"));
                }
                let padded = round_up(header.size)?;
                let mut payload = vec![0u8; padded as usize];
                read_block(reader, &mut payload)?;
                pos += padded;
                if let Sink::Copy { writer, .. } = &mut sink {
                    writer.write_all(&header.block)?;
                    writer.write_all(&payload)?;
                }
                body_end = pos;
            }
            // A real entry.
            _ => {
                // GNU sparse: extended sparse header blocks sit between the
                // base header and the data; they belong to the group.
                let mut sparse_ext: Vec<u8> = Vec::new();
                if header.typeflag == b'S' && header.gnu_sparse_is_extended() {
                    loop {
                        let mut ext = [0u8; BLOCK];
                        read_block(reader, &mut ext)?;
                        pos += BLOCK as u64;
                        let more = ext[504] != 0;
                        sparse_ext.extend_from_slice(&ext);
                        if !more {
                            break;
                        }
                    }
                }

                let pax_now = pax.take().unwrap_or_default();
                let long_link_now = long_link.take();
                let long_name_now = long_name.take();
                // `GNU.sparse.name` outranks everything: for pax sparse
                // entries both the header field and any `path` record hold
                // the mangled `GNUSparseFile.<pid>/` spelling.
                let name = match pax_now.sparse_name {
                    Some(n) => n,
                    None => match long_name_now {
                        Some(bytes) => String::from_utf8(bytes).map_err(|e| {
                            Error::InvalidUtf8Path(
                                String::from_utf8_lossy(e.as_bytes()).into_owned(),
                            )
                        })?,
                        None => match pax_now.path {
                            Some(p) => p,
                            None => header.short_name()?,
                        },
                    },
                };
                let hardlink_target = (header.typeflag == b'1').then(|| {
                    match long_link_now {
                        Some(bytes) => String::from_utf8_lossy(&bytes).into_owned(),
                        None => pax_now.linkpath.unwrap_or_else(|| header.short_link()),
                    }
                });
                // Physical payload length.  For GNU sparse entries the size
                // field already holds the stored (compacted) byte count —
                // the logical size lives in `realsize`, which we never need.
                let data_len = round_up(pax_now.size.unwrap_or(header.size))?;
                let mtime = pax_now.mtime.unwrap_or_else(|| header.mtime());

                entries.push(RawEntry {
                    name: name.clone(),
                    mtime,
                    hardlink_target,
                });

                match &mut sink {
                    Sink::Copy { writer, keep } => {
                        if keep(&name) {
                            for part in &pending {
                                writer.write_all(part)?;
                            }
                            writer.write_all(&header.block)?;
                            writer.write_all(&sparse_ext)?;
                            let mut limited = reader.take(data_len);
                            let copied = std::io::copy(&mut limited, *writer)?;
                            if copied != data_len {
                                return Err(corrupt("unexpected end of entry data"));
                            }
                        } else {
                            let drained =
                                std::io::copy(&mut reader.take(data_len), &mut std::io::sink())?;
                            if drained != data_len {
                                return Err(corrupt("unexpected end of entry data"));
                            }
                        }
                    }
                    Sink::ScanOnly => {
                        let drained =
                            std::io::copy(&mut reader.take(data_len), &mut std::io::sink())?;
                        if drained != data_len {
                            return Err(corrupt("unexpected end of entry data"));
                        }
                    }
                }
                pos += data_len;
                pending.clear();
                body_end = pos;
            }
        }
    }

    Ok(Scan { entries, body_end })
}

/// Scan an archive's entries and physical end without writing anything.
pub fn scan<R: Read + ?Sized>(reader: &mut R) -> Result<Scan> {
    process::<R, std::io::Sink>(reader, Sink::ScanOnly)
}

/// Copy every entry group `keep` approves from `reader` to `writer`,
/// byte-for-byte.  Pax global headers are always carried through.  The writer
/// receives no EOF terminator — callers append further entries and finish via
/// `tar::Builder`, which writes it.
pub fn copy_entries<R: Read + ?Sized, W: Write>(
    reader: &mut R,
    writer: &mut W,
    keep: &mut dyn FnMut(&str) -> bool,
) -> Result<()> {
    process(reader, Sink::Copy { writer, keep })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn header_of(name: &str, size: u64, typeflag: u8) -> [u8; BLOCK] {
        let mut h = tar::Header::new_gnu();
        h.set_path(name).map_err(|_| ()).ok();
        h.set_size(size);
        h.set_mtime(1_000_000);
        h.set_mode(0o644);
        h.set_entry_type(tar::EntryType::new(typeflag));
        h.set_cksum();
        let mut block = [0u8; BLOCK];
        block.copy_from_slice(h.as_bytes());
        block
    }

    #[test]
    fn scan_finds_body_end_before_terminator() -> Result<()> {
        let mut archive = Vec::new();
        archive.extend_from_slice(&header_of("a.txt", 5, b'0'));
        archive.extend_from_slice(b"hello");
        archive.extend_from_slice(&[0u8; BLOCK - 5]);
        archive.extend_from_slice(&[0u8; 2 * BLOCK]);

        let scan = scan(&mut archive.as_slice())?;
        assert_eq!(scan.body_end, 2 * BLOCK as u64);
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].name, "a.txt");
        assert_eq!(scan.entries[0].mtime, 1_000_000);
        Ok(())
    }

    #[test]
    fn octal_and_base256_sizes_parse() -> Result<()> {
        assert_eq!(parse_numeric(b"0000644\0")?, 0o644);
        // Base-256 encoding of 1 << 33.
        let mut field = [0u8; 12];
        field[0] = 0x80;
        field[7] = 0x02;
        assert_eq!(parse_numeric(&field)?, 1 << 33);
        Ok(())
    }

    #[test]
    fn corrupt_checksum_is_rejected() {
        let mut block = header_of("a.txt", 0, b'0');
        block[0] ^= 0xff;
        assert!(verify_checksum(&block).is_err());
    }

    #[test]
    fn base256_size_near_u64_max_errors_instead_of_panicking() {
        let mut h = tar::Header::new_gnu();
        h.set_path("big").map_err(|_| ()).ok();
        h.set_mtime(0);
        h.set_mode(0o644);
        // Base-256 u64::MAX: marker byte, three zeros, eight 0xFF.
        let bytes = h.as_mut_bytes();
        bytes[124] = 0x80;
        for b in &mut bytes[125..128] {
            *b = 0;
        }
        for b in &mut bytes[128..136] {
            *b = 0xff;
        }
        h.set_cksum();
        let mut archive = h.as_bytes().to_vec();
        archive.extend_from_slice(&[0u8; 2 * BLOCK]);
        assert!(scan(&mut archive.as_slice()).is_err());
    }

    #[test]
    fn leading_space_padded_octal_fields_parse() -> Result<()> {
        let mut h = tar::Header::new_gnu();
        h.set_path("sp.txt").map_err(|_| ()).ok();
        h.set_mtime(1_000_000);
        h.set_mode(0o644);
        h.as_mut_bytes()[124..136].copy_from_slice(b"          5 ");
        h.set_cksum();
        let mut archive = h.as_bytes().to_vec();
        archive.extend_from_slice(b"hello");
        archive.extend_from_slice(&[0u8; BLOCK - 5]);
        archive.extend_from_slice(&[0u8; 2 * BLOCK]);

        let scan = scan(&mut archive.as_slice())?;
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].name, "sp.txt");
        assert_eq!(scan.body_end, 2 * BLOCK as u64);
        Ok(())
    }

    #[test]
    fn pax_sparse_name_wins_over_the_mangled_header_name() -> Result<()> {
        // The shape bsdtar and `gnutar --sparse-version=1.0` emit: an `x`
        // header whose GNU.sparse.name holds the real name, and a ustar
        // header spelled GNUSparseFile.<pid>/<name>.
        let records =
            b"22 GNU.sparse.major=1\n22 GNU.sparse.minor=0\n28 GNU.sparse.name=real.bin\n";
        let mut x = tar::Header::new_ustar();
        x.set_entry_type(tar::EntryType::XHeader);
        x.set_size(records.len() as u64);
        x.set_mode(0o644);
        x.set_mtime(0);
        x.set_path("paxheader").map_err(|_| ()).ok();
        x.set_cksum();
        let mut archive = x.as_bytes().to_vec();
        archive.extend_from_slice(records);
        archive.resize(archive.len().next_multiple_of(BLOCK), 0);
        archive.extend_from_slice(&header_of("GNUSparseFile.0/real.bin", 5, b'0'));
        archive.extend_from_slice(b"MAP\0\0");
        archive.resize(archive.len().next_multiple_of(BLOCK), 0);
        archive.extend_from_slice(&[0u8; 2 * BLOCK]);

        let scan = scan(&mut archive.as_slice())?;
        assert_eq!(scan.entries.len(), 1);
        assert_eq!(scan.entries[0].name, "real.bin");
        Ok(())
    }
}
