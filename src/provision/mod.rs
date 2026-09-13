//! Downloads, verifies and installs yt-dlp, FFmpeg and Deno into `bin/`
//! next to the application, tracking what was installed in a small
//! `bin/.install_state` file (needed because FFmpeg and Deno publish the
//! checksum of their archive, which is gone after extraction; see
//! `check_update` below). Progress is reported through [`Event`]s.
//!
//! Everything here is blocking and GUI-agnostic; the GUI runs it on a worker
//! thread and forwards the events.

mod archive;
pub(crate) mod download;
mod platform;

use std::fmt;
use std::fs;
use std::io::{self, Read};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

pub use platform::supported;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Component {
    YtDlp,
    Ffmpeg,
    Deno,
}

impl Component {
    pub const ALL: [Component; 3] = [Component::YtDlp, Component::Ffmpeg, Component::Deno];

    pub fn label(self) -> &'static str {
        match self {
            Component::YtDlp => "yt-dlp",
            Component::Ffmpeg => "FFmpeg",
            Component::Deno => "Deno",
        }
    }

    /// Executables this component puts in `bin/` (without `.exe`).
    pub fn binaries(self) -> &'static [&'static str] {
        match self {
            Component::YtDlp => &["yt-dlp"],
            Component::Ffmpeg => &["ffmpeg", "ffprobe"],
            Component::Deno => &["deno"],
        }
    }
}

/// Where everything lives. Portable: all of it sits next to the executable.
#[derive(Debug, Clone)]
pub struct Paths {
    pub app_dir: PathBuf,
    pub bin_dir: PathBuf,
}

impl Paths {
    pub fn new(app_dir: PathBuf) -> Self {
        let bin_dir = app_dir.join("bin");
        Paths { app_dir, bin_dir }
    }

    /// The directory of the running executable, symlinks resolved.
    /// `YTP_APP_DIR` overrides it, which is only meant for development
    /// (`cargo run` would otherwise use `target/debug/`).
    pub fn for_current_exe() -> io::Result<Self> {
        if let Some(dir) = std::env::var_os("YTP_APP_DIR") {
            return Ok(Paths::new(PathBuf::from(dir)));
        }
        let exe = plain_path(std::env::current_exe()?.canonicalize()?);
        let dir = exe
            .parent()
            .ok_or_else(|| io::Error::other("executable has no parent directory"))?;
        Ok(Paths::new(dir.to_path_buf()))
    }

    pub fn tool(&self, name: &str) -> PathBuf {
        self.bin_dir
            .join(format!("{name}{}", std::env::consts::EXE_SUFFIX))
    }

    fn state_file(&self) -> PathBuf {
        self.bin_dir.join(".install_state")
    }
}

/// Drops the `\\?\` that Windows' `canonicalize` puts in front of a path.
///
/// It is a valid path for Rust, but it travels badly: it ends up in
/// `--ffmpeg-location`, in `-P` and in the log, where it confuses both
/// yt-dlp's own path handling and the person reading. Nothing on Linux
/// starts with it, so this needs no `cfg`.
fn plain_path(path: PathBuf) -> PathBuf {
    match path.to_str().and_then(|p| p.strip_prefix(r"\\?\")) {
        Some(rest) => PathBuf::from(rest),
        None => path,
    }
}

/// The log tags (`[INSTALL]`, `[DOWNLOAD]`, ...), shared with the downloader.
pub use crate::log::Level;

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Log(Level, String),
    /// Progress of the file currently downloading.
    Transfer {
        file: String,
        received: u64,
        total: Option<u64>,
    },
    TransferFinished,
}

pub type Emit<'a> = &'a mut dyn FnMut(Event);

#[derive(Debug)]
pub enum Error {
    Unsupported,
    Io(String),
    Download(String),
    Checksum(String),
    Archive(String),
}

impl fmt::Display for Error {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Error::Unsupported => write!(
                f,
                "This platform isn't supported yet (only Linux x86_64 for now)."
            ),
            Error::Io(m) | Error::Download(m) | Error::Checksum(m) | Error::Archive(m) => {
                f.write_str(m)
            }
        }
    }
}

impl std::error::Error for Error {}

impl From<io::Error> for Error {
    fn from(e: io::Error) -> Self {
        Error::Io(e.to_string())
    }
}

// ---------------------------------------------------------------------------
// Status
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Status {
    Missing,
    /// Present but wouldn't run (truncated, wrong architecture...).
    Broken,
    Installed {
        version: String,
    },
}

/// What is installed, based on actually running the binaries.
pub fn inspect(component: Component, paths: &Paths) -> Status {
    for name in component.binaries() {
        if !paths.tool(name).is_file() {
            return Status::Missing;
        }
    }
    let main = paths.tool(component.binaries()[0]);
    let arg = if component == Component::Ffmpeg {
        "-version"
    } else {
        "--version"
    };
    match run_for_output(&main, arg, Duration::from_secs(30)) {
        Some(out) => match parse_version(component, &out) {
            Some(version) => Status::Installed { version },
            None => Status::Broken,
        },
        None => Status::Broken,
    }
}

/// Only safe for output small enough to never fill the OS pipe buffer (a
/// `--version` line or two): stdout is read after the child exits, not
/// concurrently with waiting. `metadata::fetch` needed the concurrent-drain
/// version of this after hitting exactly that deadlock on real, large `-j`
/// output; copy that approach instead of this one for anything whose output
/// size isn't bounded like a version string is.
fn run_for_output(program: &Path, arg: &str, limit: Duration) -> Option<String> {
    let mut child = Command::new(program)
        .arg(arg)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::null())
        .spawn()
        .ok()?;
    let started = Instant::now();
    loop {
        match child.try_wait() {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(_)) => return None,
            Ok(None) if started.elapsed() > limit => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(_) => return None,
        }
    }
    let mut out = String::new();
    child.stdout.take()?.read_to_string(&mut out).ok()?;
    Some(out)
}

/// Extracts the version from each tool's own `--version`/`-version` output.
pub fn parse_version(component: Component, output: &str) -> Option<String> {
    let first = output.lines().next()?.trim();
    let version = match component {
        Component::YtDlp => first,
        // "ffmpeg version N-126495-g3a165c77dc-20260910 Copyright ..."
        Component::Ffmpeg => first
            .strip_prefix("ffmpeg version ")?
            .split_whitespace()
            .next()?,
        // "deno 2.9.6 (stable, release, x86_64-unknown-linux-gnu)"
        Component::Deno => first.split_whitespace().nth(1)?,
    };
    (!version.is_empty()).then(|| version.to_string())
}

// ---------------------------------------------------------------------------
// Install state (a small key=value file)
// ---------------------------------------------------------------------------

pub fn read_state(paths: &Paths, key: &str) -> Option<String> {
    let text = fs::read_to_string(paths.state_file()).ok()?;
    text.lines()
        .find_map(|l| l.strip_prefix(key)?.strip_prefix('='))
        .map(str::to_string)
        .filter(|v| !v.is_empty())
}

pub fn write_state(paths: &Paths, key: &str, value: &str) -> io::Result<()> {
    let file = paths.state_file();
    let mut lines: Vec<String> = fs::read_to_string(&file)
        .unwrap_or_default()
        .lines()
        .filter(|l| l.split_once('=').map(|(k, _)| k) != Some(key))
        .map(str::to_string)
        .collect();
    lines.push(format!("{key}={value}"));
    let tmp = file.with_extension("tmp");
    fs::write(&tmp, lines.join("\n") + "\n")?;
    fs::rename(tmp, file)
}

// ---------------------------------------------------------------------------
// Update checks
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum UpdateCheck {
    Current,
    Outdated,
    /// Installed before update tracking existed (no recorded checksum).
    Unknown,
    Failed(String),
}

pub fn check_update(component: Component, paths: &Paths) -> UpdateCheck {
    let Some(source) = platform::source(component) else {
        return UpdateCheck::Failed(Error::Unsupported.to_string());
    };
    let published = match download::fetch_text(source.checksum_url)
        .and_then(|text| source.checksum.find(&text, source.asset))
    {
        Ok(hash) => hash,
        Err(e) => return UpdateCheck::Failed(e.to_string()),
    };
    let installed = match source.state_key {
        // yt-dlp publishes the checksum of the binary itself: hash the file.
        None => match sha256_file(&paths.tool(component.binaries()[0])) {
            Ok(h) => h,
            Err(e) => return UpdateCheck::Failed(e.to_string()),
        },
        // FFmpeg and Deno publish the checksum of the archive, which is gone
        // after extraction, so compare with what was recorded at install.
        Some(key) => match read_state(paths, key) {
            Some(h) => h,
            None => return UpdateCheck::Unknown,
        },
    };
    if installed.eq_ignore_ascii_case(&published) {
        UpdateCheck::Current
    } else {
        UpdateCheck::Outdated
    }
}

pub fn sha256_file(path: &Path) -> io::Result<String> {
    let mut file = fs::File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hex(&hasher.finalize()))
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Removes leftovers of an interrupted run (partial downloads, half-written
/// binaries). Only files this module creates, identified by their prefix.
pub fn clean_leftovers(paths: &Paths) {
    let Ok(entries) = fs::read_dir(&paths.bin_dir) else {
        return;
    };
    for entry in entries.flatten() {
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name.starts_with(".download-") || name.starts_with(".tmp-") {
            let _ = fs::remove_file(entry.path());
        }
    }
}

/// Logs an error before passing it on, so every failure shows up in the log.
fn logged<T>(emit: Emit<'_>, result: Result<T, Error>) -> Result<T, Error> {
    if let Err(e) = &result {
        emit(Event::Log(Level::Error, e.to_string()));
    }
    result
}

/// Downloads, verifies and installs one component.
/// Every failure is already in the log when this returns an error.
pub fn install(component: Component, paths: &Paths, emit: Emit<'_>) -> Result<(), Error> {
    let source = logged(emit, platform::source(component).ok_or(Error::Unsupported))?;
    let created = fs::create_dir_all(&paths.bin_dir).map_err(|e| {
        Error::Io(format!(
            "Cannot create directory: {} ({e})",
            paths.bin_dir.display()
        ))
    });
    logged(emit, created)?;

    emit(Event::Log(
        Level::Install,
        format!("{}...", component.label()),
    ));

    let sums = download::fetch_text_logged(source.checksum_url, emit)?;
    let expected = logged(emit, source.checksum.find(&sums, source.asset))?;

    let part = paths.bin_dir.join(format!(".download-{}", source.asset));
    let actual = download::download_to_file(source.url, source.asset, &part, emit)?;

    emit(Event::Log(
        Level::Verify,
        format!("Checking SHA256 for {}...", source.asset),
    ));
    if !actual.eq_ignore_ascii_case(&expected) {
        let _ = fs::remove_file(&part);
        emit(Event::Log(
            Level::Error,
            format!("Hash mismatch for {}!", source.asset),
        ));
        emit(Event::Log(Level::Info, format!("Expected: {expected}")));
        emit(Event::Log(Level::Info, format!("Actual:   {actual}")));
        return Err(Error::Checksum(format!(
            "The download of {} didn't match its published checksum.",
            source.asset
        )));
    }
    emit(Event::Log(Level::Ok, "Hash verification passed".into()));

    if !matches!(source.packaging, platform::Packaging::Raw { .. }) {
        emit(Event::Log(Level::Info, "Extracting...".into()));
    }
    let result = archive::install_from(&source.packaging, &part, paths);
    let _ = fs::remove_file(&part);
    logged(emit, result)?;

    if let Some(key) = source.state_key {
        logged(
            emit,
            write_state(paths, key, &expected).map_err(Error::from),
        )?;
    }
    emit(Event::Log(
        Level::Ok,
        format!("{} installed successfully", component.label()),
    ));
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tempdir;

    #[test]
    fn versions_are_parsed_from_each_tools_own_output() {
        assert_eq!(
            parse_version(Component::YtDlp, "2026.08.30.232658\n").as_deref(),
            Some("2026.08.30.232658")
        );
        assert_eq!(
            parse_version(
                Component::Ffmpeg,
                "ffmpeg version N-126495-g3a165c77dc-20260910 Copyright (c) 2000-2026\nbuilt with gcc"
            )
            .as_deref(),
            Some("N-126495-g3a165c77dc-20260910")
        );
        assert_eq!(
            parse_version(
                Component::Deno,
                "deno 2.9.6 (stable, release, x86_64-unknown-linux-gnu)\nv8 15.0"
            )
            .as_deref(),
            Some("2.9.6")
        );
        assert_eq!(parse_version(Component::Ffmpeg, "garbage"), None);
        assert_eq!(parse_version(Component::Deno, ""), None);
    }

    #[test]
    fn state_file_reads_and_writes_the_documented_key_value_format() {
        let dir = tempdir();
        let paths = Paths::new(dir.clone());
        fs::create_dir_all(&paths.bin_dir).unwrap();
        fs::write(
            paths.state_file(),
            "ffmpeg_archive_sha256=806d1dd\ndeno_zip_sha256=394f07f\n",
        )
        .unwrap();
        assert_eq!(
            read_state(&paths, "ffmpeg_archive_sha256").as_deref(),
            Some("806d1dd")
        );
        assert_eq!(
            read_state(&paths, "deno_zip_sha256").as_deref(),
            Some("394f07f")
        );
        assert_eq!(read_state(&paths, "deno_zip"), None);

        write_state(&paths, "deno_zip_sha256", "abc").unwrap();
        let text = fs::read_to_string(paths.state_file()).unwrap();
        assert_eq!(text, "ffmpeg_archive_sha256=806d1dd\ndeno_zip_sha256=abc\n");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_windows_extended_length_path_is_handed_over_as_a_plain_one() {
        assert_eq!(
            plain_path(PathBuf::from(r"\\?\C:\Apps\MimirDLP")),
            PathBuf::from(r"C:\Apps\MimirDLP")
        );
        // Everything else is left exactly as it is.
        assert_eq!(
            plain_path(PathBuf::from("/home/user/apps")),
            PathBuf::from("/home/user/apps")
        );
        assert_eq!(
            plain_path(PathBuf::from(r"C:\Apps")),
            PathBuf::from(r"C:\Apps")
        );
    }

    #[test]
    fn leftovers_are_cleaned_but_nothing_else() {
        let dir = tempdir();
        let paths = Paths::new(dir.clone());
        fs::create_dir_all(&paths.bin_dir).unwrap();
        for f in [".download-x.zip", ".tmp-ffmpeg", "ffmpeg", ".install_state"] {
            fs::write(paths.bin_dir.join(f), "x").unwrap();
        }
        clean_leftovers(&paths);
        let mut left: Vec<String> = fs::read_dir(&paths.bin_dir)
            .unwrap()
            .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        left.sort();
        assert_eq!(left, [".install_state", "ffmpeg"]);
        fs::remove_dir_all(dir).unwrap();
    }
}
