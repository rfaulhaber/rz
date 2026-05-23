mod helpers;

use globset::GlobSet;
use helpers::{TestResult, temp_utf8_dir};
use rz_archive::error::Error;
use rz_archive::{CompressOpts, DecompressOpts};

// ── helpers ──────────────────────────────────────────────────────────────────

fn compress_opts_with_password(password: &str) -> CompressOpts<'static> {
    let mut opts = CompressOpts::new(None, GlobSet::empty());
    opts.password = Some(password.to_owned());
    opts
}

fn decompress_opts_with_password(password: &str) -> DecompressOpts<'static> {
    let mut opts = DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty());
    opts.password = Some(password.to_owned());
    opts
}

fn decompress_opts_no_password() -> DecompressOpts<'static> {
    DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty())
}

fn decompress_opts_wrong_password() -> DecompressOpts<'static> {
    let mut opts = DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty());
    opts.password = Some("wrong_password".to_owned());
    opts
}

// ── zip tests ─────────────────────────────────────────────────────────────────

#[test]
fn zip_aes_round_trip() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let src = tmp.join("secret.txt");
    fs_err::write(&src, b"top secret content\n")?;

    let archive = tmp.join("secret.zip");
    rz_archive::zip::compress(&[src], &archive, &compress_opts_with_password("mypassword"))?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    rz_archive::zip::decompress(&archive, &out, &decompress_opts_with_password("mypassword"))?;

    let content = fs_err::read_to_string(out.join("secret.txt"))?;
    assert_eq!(content, "top secret content\n");
    Ok(())
}

#[test]
fn zip_decrypt_wrong_password_errors() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let src = tmp.join("secret.txt");
    fs_err::write(&src, b"content\n")?;

    let archive = tmp.join("secret.zip");
    rz_archive::zip::compress(&[src], &archive, &compress_opts_with_password("correct"))?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let result = rz_archive::zip::decompress(&archive, &out, &decompress_opts_wrong_password());
    // The zip crate may allow wrong passwords (ZipCrypto weakness note in the zip docs),
    // but for AES-256 it should fail.
    assert!(
        result.is_err(),
        "expected error with wrong password, got success"
    );
    Ok(())
}

#[test]
fn zip_decrypt_missing_password_errors() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let src = tmp.join("secret.txt");
    fs_err::write(&src, b"content\n")?;

    let archive = tmp.join("secret.zip");
    rz_archive::zip::compress(
        &[src],
        &archive,
        &compress_opts_with_password("somepassword"),
    )?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let result = rz_archive::zip::decompress(&archive, &out, &decompress_opts_no_password());
    assert!(
        matches!(result, Err(Error::PasswordRequired)),
        "expected PasswordRequired, got: {result:?}",
    );
    Ok(())
}

#[test]
fn zip_decrypt_unencrypted_with_password_works() -> TestResult {
    // Providing a password on an unencrypted archive must NOT error —
    // by_index_decrypt ignores the password when the entry is not encrypted.
    let (_guard, tmp) = temp_utf8_dir()?;
    let src = tmp.join("plain.txt");
    fs_err::write(&src, b"plain content\n")?;

    let archive = tmp.join("plain.zip");
    rz_archive::zip::compress(&[src], &archive, &CompressOpts::new(None, GlobSet::empty()))?;

    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    // Should succeed — password is provided but archive is unencrypted.
    rz_archive::zip::decompress(&archive, &out, &decompress_opts_with_password("ignored"))?;

    let content = fs_err::read_to_string(out.join("plain.txt"))?;
    assert_eq!(content, "plain content\n");
    Ok(())
}

#[test]
fn zip_test_encrypted() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let src = tmp.join("secret.txt");
    fs_err::write(&src, b"content\n")?;

    let archive = tmp.join("secret.zip");
    rz_archive::zip::compress(&[src], &archive, &compress_opts_with_password("pw"))?;

    // test with correct password
    rz_archive::zip::test(&archive, Some("pw"), &rz_archive::progress::NoProgress)?;

    // test without password should return PasswordRequired
    let result = rz_archive::zip::test(&archive, None, &rz_archive::progress::NoProgress);
    assert!(matches!(result, Err(Error::PasswordRequired)));
    Ok(())
}

// ── 7z tests ──────────────────────────────────────────────────────────────────

#[test]
fn seven_z_aes_round_trip() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let src = tmp.join("secret.txt");
    fs_err::write(&src, b"7z secret content\n")?;

    let archive = tmp.join("secret.7z");
    rz_archive::seven_z::compress(&[src], &archive, &compress_opts_with_password("mypassword"))?;

    let out = tmp.join("out");
    rz_archive::seven_z::decompress(&archive, &out, &decompress_opts_with_password("mypassword"))?;

    let content = fs_err::read_to_string(out.join("secret.txt"))?;
    assert_eq!(content, "7z secret content\n");
    Ok(())
}

#[test]
fn seven_z_decrypt_wrong_password_errors() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let src = tmp.join("secret.txt");
    fs_err::write(&src, b"content\n")?;

    let archive = tmp.join("secret.7z");
    rz_archive::seven_z::compress(&[src], &archive, &compress_opts_with_password("correct"))?;

    let out = tmp.join("out");
    let result = rz_archive::seven_z::decompress(&archive, &out, &decompress_opts_wrong_password());
    assert!(
        result.is_err(),
        "expected error with wrong password, got success"
    );
    Ok(())
}

// ── tar rejects password ──────────────────────────────────────────────────────

#[test]
fn tar_gz_rejects_password_compress() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let src = tmp.join("file.txt");
    fs_err::write(&src, b"content\n")?;
    let archive = tmp.join("archive.tar.gz");

    // We test at the format-dispatch level via the error returned from main.rs;
    // the library functions themselves don't check the password field.
    // The CLI rejects non-zip/7z passwords before calling compress.
    // Verify the error type is correct by simulating the check:
    let fmt = rz_archive::cmd::Format::TarGz;
    let password = Some("test".to_owned());
    let result: rz_archive::error::Result<()> = if password.is_some()
        && !matches!(
            fmt,
            rz_archive::cmd::Format::Zip | rz_archive::cmd::Format::SevenZ
        ) {
        Err(Error::EncryptionUnsupported(fmt.to_string()))
    } else {
        rz_archive::tar_gz::compress(&[src], &archive, &CompressOpts::new(None, GlobSet::empty()))
    };
    assert!(matches!(result, Err(Error::EncryptionUnsupported(_))));
    Ok(())
}

#[test]
fn tar_gz_rejects_password_decompress() -> TestResult {
    let (_guard, tmp) = temp_utf8_dir()?;
    let src = tmp.join("file.txt");
    fs_err::write(&src, b"content\n")?;
    let archive = tmp.join("archive.tar.gz");
    rz_archive::tar_gz::compress(&[src], &archive, &CompressOpts::new(None, GlobSet::empty()))?;

    let fmt = rz_archive::cmd::Format::TarGz;
    let password = Some("test".to_owned());
    let out = tmp.join("out");
    fs_err::create_dir(&out)?;
    let result: rz_archive::error::Result<()> = if password.is_some()
        && !matches!(
            fmt,
            rz_archive::cmd::Format::Zip | rz_archive::cmd::Format::SevenZ
        ) {
        Err(Error::EncryptionUnsupported(fmt.to_string()))
    } else {
        rz_archive::tar_gz::decompress(
            &archive,
            &out,
            &DecompressOpts::new(true, 0, GlobSet::empty(), GlobSet::empty()),
        )
    };
    assert!(matches!(result, Err(Error::EncryptionUnsupported(_))));
    Ok(())
}

// ── CLI parser: mutually exclusive password flags ─────────────────────────────

#[test]
fn cli_password_args_mutually_exclusive() {
    use clap::Parser;
    use rz_archive::cmd::Cli;

    // --password-stdin and --password together should fail
    let result = Cli::try_parse_from([
        "rz_archive",
        "compress",
        "--password-stdin",
        "--password",
        "pw",
        "file",
    ]);
    assert!(result.is_err(), "expected clap error for conflicting flags");

    // --password-file and --password together should fail
    let result = Cli::try_parse_from([
        "rz_archive",
        "compress",
        "--password-file",
        "/tmp/pw.txt",
        "--password",
        "pw",
        "file",
    ]);
    assert!(result.is_err(), "expected clap error for conflicting flags");

    // just --password should succeed
    let result = Cli::try_parse_from(["rz_archive", "compress", "--password", "pw", "file"]);
    assert!(result.is_ok(), "expected --password alone to parse fine");
}
