//! Where each component comes from on each platform.
//!
//! Every table is compiled everywhere and only *chosen* by `cfg`, so the
//! Windows sources are type checked and unit tested while building on
//! Linux. A mistake here otherwise only shows up on someone else's machine,
//! at install time.

use super::{Component, Error};

/// How the published checksum file is laid out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChecksumFormat {
    /// `<sha256>  <file name>` lines, one per release asset.
    SumsFile,
    /// A file describing one asset. On Linux it is `<sha256>  <name>`, but
    /// Deno's Windows file is PowerShell `Get-FileHash` output
    /// (`Hash : 15E5...`), so the first 64-hex-digit token is taken.
    SingleHash,
}

impl ChecksumFormat {
    pub fn find(self, text: &str, asset: &str) -> Result<String, Error> {
        let found = match self {
            ChecksumFormat::SumsFile => text.lines().find_map(|line| {
                let mut parts = line.split_whitespace();
                let hash = parts.next()?;
                // sha256sum marks binary mode with a leading '*'.
                let name = parts.next()?.trim_start_matches('*');
                (name == asset && is_sha256(hash)).then(|| hash.to_ascii_lowercase())
            }),
            ChecksumFormat::SingleHash => text
                .split(|c: char| c.is_whitespace() || c == ':')
                .find(|t| is_sha256(t))
                .map(str::to_ascii_lowercase),
        };
        found.ok_or_else(|| Error::Checksum(format!("Could not find the checksum for {asset}.")))
    }
}

fn is_sha256(token: &str) -> bool {
    token.len() == 64 && token.bytes().all(|b| b.is_ascii_hexdigit())
}

/// What the downloaded asset is and which files to take out of it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Packaging {
    /// The asset is the executable itself, installed under this name.
    Raw { install_as: &'static str },
    /// Take these executables, by file name, from a `.tar.xz`.
    TarXz { members: &'static [&'static str] },
    /// Take these executables, by file name, from a `.zip`.
    Zip { members: &'static [&'static str] },
}

#[derive(Debug, Clone, Copy)]
pub struct Source {
    pub url: &'static str,
    pub asset: &'static str,
    pub checksum_url: &'static str,
    pub checksum: ChecksumFormat,
    pub packaging: Packaging,
    /// Key in `bin/.install_state` for components whose published checksum
    /// is of the archive rather than the installed binary.
    pub state_key: Option<&'static str>,
}

/// The three sources for one platform.
#[derive(Debug, Clone, Copy)]
pub struct Table {
    pub yt_dlp: Source,
    pub ffmpeg: Source,
    pub deno: Source,
}

impl Table {
    fn get(&self, component: Component) -> Source {
        match component {
            Component::YtDlp => self.yt_dlp,
            Component::Ffmpeg => self.ffmpeg,
            Component::Deno => self.deno,
        }
    }
}

// Nightly is deliberate: it is the channel yt-dlp itself recommends.
const YTDLP_SUMS: &str =
    "https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download/SHA2-256SUMS";
const FFMPEG_SUMS: &str =
    "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/checksums.sha256";

/// Only one table is used per build; the other is kept for the type checker
/// and the tests.
#[allow(dead_code)]
const LINUX_X86_64: Table = Table {
    yt_dlp: Source {
        url: "https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download/yt-dlp_linux",
        asset: "yt-dlp_linux",
        checksum_url: YTDLP_SUMS,
        checksum: ChecksumFormat::SumsFile,
        packaging: Packaging::Raw {
            install_as: "yt-dlp",
        },
        state_key: None,
    },
    ffmpeg: Source {
        url: "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-linux64-gpl.tar.xz",
        asset: "ffmpeg-master-latest-linux64-gpl.tar.xz",
        checksum_url: FFMPEG_SUMS,
        checksum: ChecksumFormat::SumsFile,
        packaging: Packaging::TarXz {
            members: &["ffmpeg", "ffprobe"],
        },
        state_key: Some("ffmpeg_archive_sha256"),
    },
    deno: Source {
        url: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip",
        asset: "deno-x86_64-unknown-linux-gnu.zip",
        checksum_url: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-unknown-linux-gnu.zip.sha256sum",
        checksum: ChecksumFormat::SingleHash,
        packaging: Packaging::Zip { members: &["deno"] },
        state_key: Some("deno_zip_sha256"),
    },
};

/// Checked against the real releases: `yt-dlp.exe` is listed in
/// `SHA2-256SUMS`, the FFmpeg zip (186 MB, larger than the Linux tarball)
/// holds `ffmpeg-master-latest-win64-gpl/bin/ffmpeg.exe` and `ffprobe.exe`,
/// and Deno's zip holds a single `deno.exe` with a PowerShell style
/// `.sha256sum` next to it. Members are matched by file name with the
/// `.exe` removed, so the folder inside the zip doesn't matter.
#[allow(dead_code)]
const WINDOWS_X86_64: Table = Table {
    yt_dlp: Source {
        url: "https://github.com/yt-dlp/yt-dlp-nightly-builds/releases/latest/download/yt-dlp.exe",
        asset: "yt-dlp.exe",
        checksum_url: YTDLP_SUMS,
        checksum: ChecksumFormat::SumsFile,
        packaging: Packaging::Raw {
            install_as: "yt-dlp",
        },
        state_key: None,
    },
    ffmpeg: Source {
        url: "https://github.com/yt-dlp/FFmpeg-Builds/releases/download/latest/ffmpeg-master-latest-win64-gpl.zip",
        asset: "ffmpeg-master-latest-win64-gpl.zip",
        checksum_url: FFMPEG_SUMS,
        checksum: ChecksumFormat::SumsFile,
        packaging: Packaging::Zip {
            members: &["ffmpeg", "ffprobe"],
        },
        state_key: Some("ffmpeg_archive_sha256"),
    },
    deno: Source {
        url: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip",
        asset: "deno-x86_64-pc-windows-msvc.zip",
        checksum_url: "https://github.com/denoland/deno/releases/latest/download/deno-x86_64-pc-windows-msvc.zip.sha256sum",
        checksum: ChecksumFormat::SingleHash,
        packaging: Packaging::Zip { members: &["deno"] },
        state_key: Some("deno_zip_sha256"),
    },
};

pub fn supported() -> bool {
    source(Component::YtDlp).is_some()
}

pub fn source(component: Component) -> Option<Source> {
    #[cfg(all(target_os = "linux", target_arch = "x86_64"))]
    let table = Some(LINUX_X86_64);
    #[cfg(all(target_os = "windows", target_arch = "x86_64"))]
    let table = Some(WINDOWS_X86_64);
    #[cfg(not(any(
        all(target_os = "linux", target_arch = "x86_64"),
        all(target_os = "windows", target_arch = "x86_64")
    )))]
    let table: Option<Table> = None;

    table.map(|table| table.get(component))
}

#[cfg(test)]
mod tests {
    use super::*;

    const H1: &str = "39973a5d585e84ca2bf715b029e7ca2b57a63ee428b219b4fe485b2f7ab58e40";
    const H2: &str = "3f1b267b4488f3aed3731a9e84a44011ca5569901868532e10ee11fd07d69707";

    #[test]
    fn every_table_is_self_consistent() {
        for (name, table) in [("linux", LINUX_X86_64), ("windows", WINDOWS_X86_64)] {
            for source in [table.yt_dlp, table.ffmpeg, table.deno] {
                assert!(source.url.starts_with("https://"), "{name}: {}", source.url);
                assert!(
                    source.checksum_url.starts_with("https://"),
                    "{name}: {}",
                    source.checksum_url
                );
                // The asset name is what the checksum file is searched for,
                // so it has to be the file the URL actually fetches.
                assert!(
                    source.url.ends_with(source.asset),
                    "{name}: {} does not end with {}",
                    source.url,
                    source.asset
                );
            }
        }
    }

    #[test]
    fn the_windows_table_matches_what_upstream_publishes() {
        let windows = WINDOWS_X86_64;
        assert_eq!(windows.yt_dlp.asset, "yt-dlp.exe");
        assert_eq!(
            windows.yt_dlp.packaging,
            Packaging::Raw {
                install_as: "yt-dlp"
            },
            "the .exe is added by Paths::tool, not stored in the name"
        );
        // Windows ships a zip where Linux ships a tar.xz.
        assert_eq!(windows.ffmpeg.asset, "ffmpeg-master-latest-win64-gpl.zip");
        assert_eq!(
            windows.ffmpeg.packaging,
            Packaging::Zip {
                members: &["ffmpeg", "ffprobe"]
            }
        );
        assert_eq!(windows.deno.checksum, ChecksumFormat::SingleHash);
        // A bin/ folder copied between systems has to stay readable, so the
        // state keys are the same on both.
        assert_eq!(windows.ffmpeg.state_key, LINUX_X86_64.ffmpeg.state_key);
        assert_eq!(windows.deno.state_key, LINUX_X86_64.deno.state_key);
        assert_eq!(
            windows.yt_dlp.checksum_url,
            LINUX_X86_64.yt_dlp.checksum_url
        );
    }

    /// Not part of the normal run: asks upstream whether the assets this
    /// platform's table names still exist. Run it when installs start
    /// failing, with `cargo test -- --ignored upstream`.
    #[test]
    #[ignore = "hits the network"]
    fn upstream_still_publishes_every_asset_in_this_table() {
        for component in Component::ALL {
            let source = source(component).expect("this platform has a table");
            let text = crate::provision::download::fetch_text(source.checksum_url)
                .unwrap_or_else(|e| panic!("{}: {e}", source.checksum_url));
            let hash = source
                .checksum
                .find(&text, source.asset)
                .unwrap_or_else(|e| panic!("{}: {e}", source.asset));
            assert_eq!(hash.len(), 64, "{}", source.asset);
        }
    }

    #[test]
    fn sums_file_matches_the_exact_asset_name() {
        let sums = format!("{H2}  yt-dlp\n{H1}  yt-dlp_linux\n{H2}  yt-dlp_linux_aarch64\n");
        assert_eq!(
            ChecksumFormat::SumsFile
                .find(&sums, "yt-dlp_linux")
                .unwrap(),
            H1
        );
        assert_eq!(ChecksumFormat::SumsFile.find(&sums, "yt-dlp").unwrap(), H2);
        assert!(
            ChecksumFormat::SumsFile
                .find(&sums, "yt-dlp_macos")
                .is_err()
        );
    }

    #[test]
    fn sums_file_accepts_binary_mode_marker_and_uppercase() {
        let sums = format!("{}  *ffmpeg.tar.xz\n", H1.to_uppercase());
        assert_eq!(
            ChecksumFormat::SumsFile
                .find(&sums, "ffmpeg.tar.xz")
                .unwrap(),
            H1
        );
    }

    #[test]
    fn single_hash_reads_the_linux_and_the_windows_formats() {
        let linux = format!("{H1}  deno-x86_64-unknown-linux-gnu.zip\n");
        assert_eq!(
            ChecksumFormat::SingleHash.find(&linux, "deno.zip").unwrap(),
            H1
        );
        // Deno's Windows .sha256sum, as actually published (PowerShell).
        let windows = format!(
            "\r\nAlgorithm : SHA256\r\nHash      : {}\r\nPath      : C:\\a\\deno\\deno\\target\\release\\deno-x86_64-pc-windows-msvc.zip\r\n",
            H1.to_uppercase()
        );
        assert_eq!(
            ChecksumFormat::SingleHash
                .find(&windows, "deno.zip")
                .unwrap(),
            H1
        );
        assert!(
            ChecksumFormat::SingleHash
                .find("Algorithm : SHA256", "deno.zip")
                .is_err()
        );
    }
}
