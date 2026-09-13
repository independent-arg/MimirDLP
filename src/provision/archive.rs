//! Takes the verified download apart and installs its executables.
//!
//! Only the expected members are extracted, matched by file name and
//! written to paths chosen here, so nothing in the archive controls where
//! files land. Each executable is written to a temporary name in `bin/` and
//! renamed into place, so an interruption never leaves a half-written tool.

use std::fs::{self, File};
use std::io::{self, BufReader, Read};
use std::path::Path;

use super::platform::Packaging;
use super::{Error, Paths};

pub fn install_from(packaging: &Packaging, download: &Path, paths: &Paths) -> Result<(), Error> {
    match *packaging {
        Packaging::Raw { install_as } => {
            let mut file = File::open(download)?;
            place(&mut file, install_as, paths)?;
            Ok(())
        }
        Packaging::TarXz { members } => {
            let file = BufReader::with_capacity(1 << 20, File::open(download)?);
            let xz = lzma_rust2::XzReader::new(file, true);
            let mut tar = tar::Archive::new(xz);
            let mut found = Vec::new();
            for entry in tar.entries().map_err(archive_err)? {
                let mut entry = entry.map_err(archive_err)?;
                if !entry.header().entry_type().is_file() {
                    continue;
                }
                let path = entry.path().map_err(archive_err)?.into_owned();
                if let Some(name) = wanted(&path, members)
                    && !found.contains(&name)
                {
                    place(&mut entry, name, paths)?;
                    found.push(name);
                }
            }
            all_found(members, &found)
        }
        Packaging::Zip { members } => {
            let mut zip = zip::ZipArchive::new(File::open(download)?).map_err(archive_err)?;
            let mut found = Vec::new();
            for i in 0..zip.len() {
                let mut entry = zip.by_index(i).map_err(archive_err)?;
                if !entry.is_file() {
                    continue;
                }
                let path = Path::new(entry.name()).to_path_buf();
                if let Some(name) = wanted(&path, members)
                    && !found.contains(&name)
                {
                    place(&mut entry, name, paths)?;
                    found.push(name);
                }
            }
            all_found(members, &found)
        }
    }
}

/// The member name if this archive path's file name is one we want.
fn wanted(path: &Path, members: &[&'static str]) -> Option<&'static str> {
    let file_name = path.file_name()?.to_str()?;
    let stem = file_name
        .strip_suffix(std::env::consts::EXE_SUFFIX)
        .filter(|_| !std::env::consts::EXE_SUFFIX.is_empty())
        .unwrap_or(file_name);
    members.iter().copied().find(|m| *m == stem)
}

fn all_found(members: &[&str], found: &[&str]) -> Result<(), Error> {
    let missing: Vec<&str> = members
        .iter()
        .copied()
        .filter(|m| !found.contains(m))
        .collect();
    if missing.is_empty() {
        Ok(())
    } else {
        Err(Error::Archive(format!(
            "The archive didn't contain {} (unexpected layout).",
            missing.join(", ")
        )))
    }
}

fn place(reader: &mut dyn Read, name: &str, paths: &Paths) -> Result<(), Error> {
    let target = paths.tool(name);
    let tmp = paths.bin_dir.join(format!(".tmp-{name}"));
    let result = (|| -> io::Result<()> {
        let mut out = File::create(&tmp)?;
        io::copy(reader, &mut out)?;
        out.sync_all()?;
        make_executable(&tmp)?;
        fs::rename(&tmp, &target)
    })();
    if result.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    result.map_err(|e| Error::Io(format!("Could not install {name}: {e}")))
}

#[cfg(unix)]
fn make_executable(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(0o755))
}

#[cfg(not(unix))]
fn make_executable(_path: &Path) -> io::Result<()> {
    Ok(())
}

fn archive_err(e: impl std::fmt::Display) -> Error {
    Error::Archive(format!("Could not read the archive: {e}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tempdir;
    use std::io::Write;

    fn setup() -> (std::path::PathBuf, Paths) {
        let dir = tempdir();
        let paths = Paths::new(dir.clone());
        fs::create_dir_all(&paths.bin_dir).unwrap();
        (dir, paths)
    }

    fn tar_xz(entries: &[(&str, &[u8])]) -> Vec<u8> {
        let mut tar = tar::Builder::new(Vec::new());
        for (path, data) in entries {
            let mut header = tar::Header::new_gnu();
            header.set_size(data.len() as u64);
            header.set_mode(0o644);
            header.set_cksum();
            tar.append_data(&mut header, path, *data).unwrap();
        }
        let raw = tar.into_inner().unwrap();
        let mut xz =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(6)).unwrap();
        xz.write_all(&raw).unwrap();
        xz.finish().unwrap()
    }

    #[test]
    fn tar_xz_members_are_taken_by_name_and_made_executable() {
        let (dir, paths) = setup();
        let archive = dir.join("a.tar.xz");
        fs::write(
            &archive,
            tar_xz(&[
                ("ffmpeg-master/LICENSE.txt", b"license"),
                ("ffmpeg-master/bin/ffmpeg", b"FFMPEG"),
                ("ffmpeg-master/bin/ffprobe", b"FFPROBE"),
                ("ffmpeg-master/bin/ffplay", b"FFPLAY"),
            ]),
        )
        .unwrap();
        install_from(
            &Packaging::TarXz {
                members: &["ffmpeg", "ffprobe"],
            },
            &archive,
            &paths,
        )
        .unwrap();
        assert_eq!(fs::read(paths.tool("ffmpeg")).unwrap(), b"FFMPEG");
        assert_eq!(fs::read(paths.tool("ffprobe")).unwrap(), b"FFPROBE");
        assert!(
            !paths.tool("ffplay").exists(),
            "only the wanted members are extracted"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(paths.tool("ffmpeg"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o755);
        }
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_missing_member_is_an_error_not_a_partial_install() {
        let (dir, paths) = setup();
        let archive = dir.join("a.tar.xz");
        fs::write(&archive, tar_xz(&[("x/bin/ffmpeg", b"FFMPEG")])).unwrap();
        let err = install_from(
            &Packaging::TarXz {
                members: &["ffmpeg", "ffprobe"],
            },
            &archive,
            &paths,
        )
        .unwrap_err();
        assert!(err.to_string().contains("ffprobe"), "{err}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_path_traversal_entry_cannot_escape_bin() {
        let (dir, paths) = setup();
        // Built by hand: tar::Builder refuses to write ".." itself.
        let mut raw = Vec::new();
        let mut header = tar::Header::new_gnu();
        header.as_old_mut().name[..16].copy_from_slice(b"../../evil/ffmpg");
        header.set_size(4);
        header.set_mode(0o644);
        header.set_cksum();
        raw.extend_from_slice(header.as_bytes());
        raw.extend_from_slice(b"EVIL");
        raw.resize(raw.len() + 508 + 1024, 0);
        let mut xz =
            lzma_rust2::XzWriter::new(Vec::new(), lzma_rust2::XzOptions::with_preset(6)).unwrap();
        xz.write_all(&raw).unwrap();
        let archive = dir.join("a.tar.xz");
        fs::write(&archive, xz.finish().unwrap()).unwrap();
        // The entry's file name is wanted, but it is still written to bin/
        // under the name chosen here, never to the path inside the archive.
        install_from(
            &Packaging::TarXz {
                members: &["ffmpg"],
            },
            &archive,
            &paths,
        )
        .unwrap();
        assert!(!dir.join("../evil").exists());
        assert_eq!(fs::read(paths.tool("ffmpg")).unwrap(), b"EVIL");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn zip_members_are_taken_by_name() {
        let (dir, paths) = setup();
        let archive = dir.join("a.zip");
        let mut zip = zip::ZipWriter::new(File::create(&archive).unwrap());
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Deflated);
        zip.start_file("deno", options).unwrap();
        zip.write_all(b"DENO").unwrap();
        zip.start_file("README.md", options).unwrap();
        zip.write_all(b"readme").unwrap();
        zip.finish().unwrap();
        install_from(&Packaging::Zip { members: &["deno"] }, &archive, &paths).unwrap();
        assert_eq!(fs::read(paths.tool("deno")).unwrap(), b"DENO");
        assert!(!paths.bin_dir.join("README.md").exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn raw_downloads_are_installed_under_their_final_name() {
        let (dir, paths) = setup();
        let download = dir.join("yt-dlp_linux");
        fs::write(&download, b"YTDLP").unwrap();
        install_from(
            &Packaging::Raw {
                install_as: "yt-dlp",
            },
            &download,
            &paths,
        )
        .unwrap();
        assert_eq!(fs::read(paths.tool("yt-dlp")).unwrap(), b"YTDLP");
        assert!(!paths.bin_dir.join(".tmp-yt-dlp").exists());
        fs::remove_dir_all(dir).unwrap();
    }
}
