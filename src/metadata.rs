//! Looks up a video before it is queued: title, thumbnail, duration and an
//! approximate size, the way a queue card needs to show something useful
//! before the download itself has even started.
//!
//! Both lookups are blocking and run on a worker thread from the GUI, same
//! as everything else in `provision` and `runner`. Neither is retried: a
//! failure here just means a plainer card (the raw URL as its title, no
//! thumbnail), not a reason to refuse queuing the link.

use std::io::Read;
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;

use crate::provision::Paths;
use crate::provision::download::agent;

/// yt-dlp itself is asked for at most one entry (`--playlist-items 1`), so a
/// playlist URL previews as its first video rather than hanging on the
/// whole list.
const TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, PartialEq)]
pub struct Metadata {
    pub title: String,
    pub thumbnail_url: Option<String>,
    /// Seconds.
    pub duration: Option<f64>,
    /// Bytes. `filesize` when yt-dlp knows it exactly, `filesize_approx`
    /// otherwise; `None` when it has no idea either (common for formats
    /// assembled from live segments).
    pub approx_size: Option<u64>,
}

#[derive(Deserialize, Default)]
struct Info {
    title: Option<String>,
    thumbnail: Option<String>,
    duration: Option<f64>,
    filesize: Option<u64>,
    filesize_approx: Option<u64>,
}

impl From<Info> for Metadata {
    fn from(info: Info) -> Self {
        Metadata {
            title: info.title.unwrap_or_default(),
            thumbnail_url: info.thumbnail,
            duration: info.duration,
            approx_size: info.filesize.or(info.filesize_approx),
        }
    }
}

/// Runs `yt-dlp -j` on `url` and parses the one line of JSON it prints.
pub fn fetch(paths: &Paths, url: &str) -> Result<Metadata, String> {
    let tool = paths.tool("yt-dlp");
    // Same JS runtime the real download uses: without it, sites that throw
    // JavaScript challenges at extractors (YouTube chief among them) can
    // fail or hang here even though the download itself works fine.
    let mut js_runtime = std::ffi::OsString::from("deno:");
    js_runtime.push(paths.tool("deno"));
    let mut child = Command::new(&tool)
        .args(["-j", "--no-warnings", "--playlist-items", "1"])
        .arg("--js-runtimes")
        .arg(js_runtime)
        .arg(url)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|e| format!("Could not start {}: {e}", tool.display()))?;

    // Drained on their own threads, concurrently with the wait below: a
    // video with many formats prints a JSON line bigger than the OS pipe
    // buffer, and the child then blocks on write() until something reads
    // it, which polling try_wait() below never does on its own. This is
    // exactly the deadlock Child::wait_with_output() avoids internally,
    // reimplemented by hand here so the timeout below can still kill the
    // child (wait_with_output() offers no way to do that mid-wait).
    let stdout_thread = child.stdout.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut buf = String::new();
            let _ = pipe.read_to_string(&mut buf);
            buf
        })
    });
    let stderr_thread = child.stderr.take().map(|mut pipe| {
        thread::spawn(move || {
            let mut buf = String::new();
            let _ = pipe.read_to_string(&mut buf);
            buf
        })
    });

    let started = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) if started.elapsed() > TIMEOUT => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("Timed out looking up this link.".into());
            }
            Ok(None) => thread::sleep(Duration::from_millis(50)),
            Err(e) => return Err(e.to_string()),
        }
    };

    let stdout = stdout_thread
        .and_then(|t| t.join().ok())
        .unwrap_or_default();
    let stderr = stderr_thread
        .and_then(|t| t.join().ok())
        .unwrap_or_default();

    if !status.success() {
        let first_line = stderr
            .lines()
            .find(|l| !l.trim().is_empty())
            .unwrap_or("yt-dlp could not look up this link.");
        return Err(first_line.trim_start_matches("ERROR: ").to_string());
    }

    let line = stdout
        .lines()
        .find(|l| !l.trim().is_empty())
        .ok_or_else(|| "yt-dlp returned no information for this link.".to_string())?;
    let info: Info =
        serde_json::from_str(line).map_err(|e| format!("Could not read yt-dlp's output: {e}"))?;
    Ok(info.into())
}

/// A one-shot, best-effort download of the thumbnail image bytes, handed
/// straight to `iced::widget::image::Handle::from_bytes` by the caller.
pub fn fetch_thumbnail(url: &str) -> Result<Vec<u8>, String> {
    agent()
        .get(url)
        .call()
        .map_err(|e| e.to_string())?
        .into_body()
        .read_to_vec()
        .map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_full_info_dict_is_mapped_to_metadata() {
        let info: Info = serde_json::from_str(
            r#"{
                "title": "Me at the zoo",
                "thumbnail": "https://i.ytimg.com/vi/jNQXAC9IVRw/maxresdefault.jpg",
                "duration": 19.0,
                "filesize": 1234567,
                "filesize_approx": 9999999
            }"#,
        )
        .unwrap();
        let meta: Metadata = info.into();
        assert_eq!(meta.title, "Me at the zoo");
        assert_eq!(
            meta.thumbnail_url.as_deref(),
            Some("https://i.ytimg.com/vi/jNQXAC9IVRw/maxresdefault.jpg")
        );
        assert_eq!(meta.duration, Some(19.0));
        // The exact size wins over the estimate when both are present.
        assert_eq!(meta.approx_size, Some(1234567));
    }

    #[test]
    fn a_missing_exact_size_falls_back_to_the_estimate() {
        let info: Info = serde_json::from_str(r#"{"filesize_approx": 555}"#).unwrap();
        let meta: Metadata = info.into();
        assert_eq!(meta.approx_size, Some(555));
    }

    #[test]
    fn fields_yt_dlp_does_not_know_are_left_out_rather_than_guessed() {
        let info: Info = serde_json::from_str("{}").unwrap();
        let meta: Metadata = info.into();
        assert_eq!(meta.title, "");
        assert_eq!(meta.thumbnail_url, None);
        assert_eq!(meta.duration, None);
        assert_eq!(meta.approx_size, None);
    }

    /// A real video's `-j` output can run past the OS pipe buffer (a video
    /// with many formats easily prints several hundred KB of JSON). Once
    /// found the hard way: reading stdout only after the child exits lets
    /// the child block on a full pipe forever, since try_wait() alone never
    /// drains it. Regression test for that deadlock, not just a parsing check.
    #[cfg(unix)]
    #[test]
    fn a_response_bigger_than_the_pipe_buffer_does_not_deadlock() {
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        let _guard = crate::testing::exec_guard();
        let dir = crate::testing::tempdir();
        let paths = Paths::new(dir.clone());
        fs::create_dir_all(&paths.bin_dir).unwrap();
        let script = paths.tool("yt-dlp");
        fs::write(
            &script,
            "#!/bin/sh\n\
             printf '{\"title\": \"big\", \"duration\": 5, \"pad\": \"'\n\
             head -c 200000 /dev/zero | tr '\\0' 'a'\n\
             printf '\"}\\n'\n",
        )
        .unwrap();
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();

        let started = Instant::now();
        let meta = fetch(&paths, "https://example.com/video").unwrap();
        assert!(
            started.elapsed() < TIMEOUT,
            "must finish well before the timeout, not by hitting it"
        );
        assert_eq!(meta.title, "big");
        assert_eq!(meta.duration, Some(5.0));
        fs::remove_dir_all(dir).unwrap();
    }
}
