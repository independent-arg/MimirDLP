//! Runs yt-dlp and turns its output into events the GUI can show.
//!
//! The arguments come from [`crate::engine::build_args`], which is pinned by
//! the reference harness. Only the presentation flags added here are extra,
//! and they are prepended so the engine's argv stays comparable with the
//! goldens. They were checked against real downloads: progress lines arrive
//! on stdout, postprocessor lines on stderr, and `--print` implies `--quiet`,
//! so `--progress` and `--no-quiet` are needed to keep the rest of the
//! output flowing.

use std::ffi::OsString;
use std::io::{BufRead, BufReader, Read};
use std::path::Path;
use std::process::{Child, Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::Duration;

use crate::engine::{self, Options, Outcome};
use crate::log::Level;
use crate::provision::Paths;

/// Prefixes chosen so they can't be confused with yt-dlp's own output.
const DOWNLOAD: &str = "[ytp] ";
const POSTPROCESS: &str = "[ytp-pp] ";
const FILE: &str = "[ytp-file] ";
/// How often the reading loop checks whether the run was cancelled.
const POLL: Duration = Duration::from_millis(200);

#[derive(Debug, Clone, PartialEq)]
pub enum Event {
    Log(Level, String),
    Progress(Progress),
    /// A finished file, as reported by `--print after_move:%(filepath)s`.
    File(String),
    Finished(Finish),
}

#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct Progress {
    /// Which parallel download this is. yt-dlp numbers them when more than
    /// one runs at a time; 0 when it doesn't say.
    pub stream: u32,
    pub downloaded: u64,
    pub total: Option<u64>,
    /// Bytes per second.
    pub speed: Option<f64>,
    /// Seconds left.
    pub eta: Option<u64>,
    pub done: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Finish {
    Completed,
    /// Exit 101: the download limit or `--break-on-existing` was reached.
    StoppedAsConfigured,
    Cancelled,
    Failed(String),
}

/// Lets the GUI stop a running download from another thread.
#[derive(Debug, Default)]
pub struct Cancel {
    requested: AtomicBool,
    child: Mutex<Option<Child>>,
}

impl Cancel {
    pub fn cancel(&self) {
        self.requested.store(true, Ordering::SeqCst);
        if let Ok(mut slot) = self.child.lock()
            && let Some(child) = slot.as_mut()
        {
            // Abrupt on purpose: yt-dlp leaves its .part file behind and
            // resumes it on the next run, which is what a user expects of a
            // cancelled download.
            let _ = child.kill();
        }
    }

    pub fn is_cancelled(&self) -> bool {
        self.requested.load(Ordering::SeqCst)
    }

    fn hold(&self, child: Child) {
        if let Ok(mut slot) = self.child.lock() {
            *slot = Some(child);
        }
    }

    fn take(&self) -> Option<Child> {
        self.child.lock().ok().and_then(|mut slot| slot.take())
    }
}

/// Flags that shape the output so it can be parsed. Prepended to the
/// engine's arguments, never mixed into them.
fn presentation_args() -> Vec<OsString> {
    [
        "--newline",
        "--progress",
        // --print implies --quiet, which would hide everything else.
        "--no-quiet",
        "--color",
        "never",
        "--progress-delta",
        "0.3",
        "--progress-template",
        "download:[ytp] %(progress.status)s|%(progress.downloaded_bytes)s|%(progress.total_bytes)s|%(progress.total_bytes_estimate)s|%(progress.speed)s|%(progress.eta)s",
        "--progress-template",
        "postprocess:[ytp-pp] %(progress.status)s|%(progress.postprocessor)s",
        "--print",
        "after_move:[ytp-file] %(filepath)s",
    ]
    .into_iter()
    .map(OsString::from)
    .collect()
}

/// Runs yt-dlp to completion on the calling thread, reporting as it goes.
/// The caller is expected to run this on a thread of its own.
pub fn run(
    paths: &Paths,
    options: &Options,
    output_dir: Option<&Path>,
    urls: &[String],
    cancel: &Cancel,
    emit: &mut dyn FnMut(Event),
) {
    let mut args = presentation_args();
    args.extend(engine::build_args(
        options,
        &paths.bin_dir,
        output_dir,
        urls,
    ));

    let tool = paths.tool("yt-dlp");
    let spawned = Command::new(&tool)
        .args(&args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn();
    let mut child = match spawned {
        Ok(child) => child,
        Err(e) => {
            emit(Event::Log(
                Level::Error,
                format!("Could not start {}: {e}", tool.display()),
            ));
            emit(Event::Finished(Finish::Failed(
                "yt-dlp could not be started. Reinstall it from Setup.".into(),
            )));
            return;
        }
    };

    let (tx, rx) = mpsc::channel();
    let readers: Vec<_> = [
        child
            .stdout
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
        child
            .stderr
            .take()
            .map(|s| Box::new(s) as Box<dyn Read + Send>),
    ]
    .into_iter()
    .flatten()
    .map(|stream| {
        let tx = tx.clone();
        thread::spawn(move || {
            for line in BufReader::new(stream).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })
    })
    .collect();
    drop(tx);

    cancel.hold(child);

    let mut last_error = None;
    loop {
        let line = match rx.recv_timeout(POLL) {
            Ok(line) => line,
            // Killing yt-dlp does not kill what yt-dlp started: ffmpeg keeps
            // the pipes open until it notices. A cancelled run therefore
            // stops reading instead of waiting for the last writer to let
            // go, which is what makes Stop feel immediate.
            Err(mpsc::RecvTimeoutError::Timeout) if cancel.is_cancelled() => break,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };
        if let Some(event) = parse_line(line.trim_end()) {
            if let Event::Log(Level::Error, message) = &event {
                last_error = Some(message.clone());
            }
            emit(event);
        }
    }
    if !cancel.is_cancelled() {
        for reader in readers {
            let _ = reader.join();
        }
    }

    let code = cancel
        .take()
        .and_then(|mut child| child.wait().ok())
        .and_then(|status| status.code());
    let finish = if cancel.is_cancelled() {
        Finish::Cancelled
    } else {
        match code.map(engine::classify_exit) {
            Some(Outcome::Completed) => Finish::Completed,
            Some(Outcome::StoppedAsConfigured) => Finish::StoppedAsConfigured,
            Some(Outcome::Failed(code)) => Finish::Failed(
                last_error.unwrap_or_else(|| format!("yt-dlp exited with code {code}.")),
            ),
            // Killed by a signal, or the process vanished.
            None => {
                Finish::Failed(last_error.unwrap_or_else(|| "yt-dlp stopped unexpectedly.".into()))
            }
        }
    };
    emit(Event::Finished(finish));
}

/// One line of yt-dlp output. `None` for lines worth dropping.
fn parse_line(line: &str) -> Option<Event> {
    let (stream, rest) = split_stream(line);
    if let Some(fields) = rest.strip_prefix(DOWNLOAD) {
        return parse_progress(stream, fields).map(Event::Progress);
    }
    if let Some(fields) = rest.strip_prefix(POSTPROCESS) {
        return parse_postprocess(fields);
    }
    if let Some(path) = rest.strip_prefix(FILE) {
        return Some(Event::File(path.to_string()));
    }
    if line.is_empty() {
        return None;
    }
    if let Some(rest) = line.strip_prefix("ERROR:") {
        return Some(Event::Log(Level::Error, rest.trim().to_string()));
    }
    if let Some(rest) = line.strip_prefix("WARNING:") {
        return Some(Event::Log(Level::Warn, rest.trim().to_string()));
    }
    Some(Event::Log(Level::Info, line.to_string()))
}

/// yt-dlp numbers its progress lines (`2: [ytp] ...`) when several
/// downloads run at once, which is what a live stream does: it fetches
/// video and audio in parallel. Checked against a real live stream, where
/// the numbering shows up with and without `--verbose`. Anything that
/// isn't `<number>: ` is left alone, so ordinary lines such as
/// `[download] Destination: ...` keep their text.
fn split_stream(line: &str) -> (u32, &str) {
    if let Some((slot, rest)) = line.split_once(": ")
        && let Ok(slot) = slot.trim().parse::<u32>()
    {
        return (slot, rest);
    }
    (0, line)
}

/// `status|downloaded|total|total_estimate|speed|eta`, in the order of the
/// progress template above. Fields yt-dlp doesn't know yet arrive as `NA`.
fn parse_progress(stream: u32, rest: &str) -> Option<Progress> {
    let mut fields = rest.split('|');
    let status = fields.next()?;
    let mut next = || number(fields.next().unwrap_or("NA"));
    let downloaded = next().unwrap_or(0.0);
    let total = next();
    let estimate = next();
    let speed = next();
    let eta = next();
    Some(Progress {
        stream,
        downloaded: downloaded.max(0.0) as u64,
        // The estimate is all there is for streams of unknown length.
        total: total.or(estimate).map(|t| t.max(0.0) as u64),
        speed: speed.filter(|s| *s > 0.0),
        eta: eta.map(|e| e.max(0.0) as u64),
        done: status == "finished",
    })
}

fn number(field: &str) -> Option<f64> {
    match field {
        "NA" | "None" | "" => None,
        value => value.parse().ok(),
    }
}

fn parse_postprocess(rest: &str) -> Option<Event> {
    let (status, name) = rest.split_once('|')?;
    if status != "started" {
        return None;
    }
    // yt-dlp's postprocessor class names, turned into something readable.
    let label = match name {
        "ExtractAudio" => "Extracting audio...",
        "VideoRemuxer" => "Rewrapping the container...",
        "VideoConvertor" => "Re-encoding the video...",
        "Merger" => "Merging video and audio...",
        "ThumbnailsConvertor" => "Converting the thumbnail...",
        "EmbedThumbnail" => "Embedding the thumbnail...",
        "FFmpegMetadata" => "Embedding metadata...",
        "EmbedSubtitle" => "Embedding subtitles...",
        "SponsorBlock" => "Looking up SponsorBlock segments...",
        "ModifyChapters" => "Applying SponsorBlock...",
        "MoveFiles" | "MoveFilesAfterDownload" => "Moving the file into place...",
        other => return Some(Event::Log(Level::Info, format!("{other}..."))),
    };
    Some(Event::Log(Level::Info, label.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines recorded from real runs of the bundled yt-dlp.
    #[test]
    fn progress_lines_are_parsed_as_yt_dlp_prints_them() {
        let start = parse_line("[ytp] downloading|1024|991017|NA|NA|NA").unwrap();
        assert_eq!(
            start,
            Event::Progress(Progress {
                stream: 0,
                downloaded: 1024,
                total: Some(991017),
                speed: None,
                eta: None,
                done: false,
            })
        );
        let end = parse_line("[ytp] finished|991017|991017|NA|19476822.36014507|NA").unwrap();
        let Event::Progress(end) = end else {
            panic!("expected progress")
        };
        assert!(end.done);
        assert_eq!(end.speed, Some(19476822.36014507));
    }

    #[test]
    fn an_estimate_is_used_when_the_total_is_unknown() {
        let event = parse_line("[ytp] downloading|2048|NA|4096.5|1000.0|12").unwrap();
        assert_eq!(
            event,
            Event::Progress(Progress {
                stream: 0,
                downloaded: 2048,
                total: Some(4096),
                speed: Some(1000.0),
                eta: Some(12),
                done: false,
            })
        );
    }

    /// Recorded from a real live stream with --live-from-start.
    #[test]
    fn numbered_progress_lines_of_parallel_downloads_are_recognised() {
        let first = parse_line("1: [ytp] downloading|20870|NA|NA|NA|NA").unwrap();
        let second = parse_line("2: [ytp] downloading|311917|NA|NA|771326.89|0.0").unwrap();
        let Event::Progress(first) = first else {
            panic!("expected progress")
        };
        let Event::Progress(second) = second else {
            panic!("expected progress")
        };
        assert_eq!((first.stream, first.downloaded), (1, 20870));
        assert_eq!((second.stream, second.downloaded), (2, 311917));
        assert_eq!(second.speed, Some(771326.89));
    }

    #[test]
    fn a_colon_in_an_ordinary_line_is_not_a_slot_number() {
        assert_eq!(
            parse_line("[download] Destination: /tmp/video.mkv"),
            Some(Event::Log(
                Level::Info,
                "[download] Destination: /tmp/video.mkv".into()
            ))
        );
    }

    #[test]
    fn postprocessors_become_readable_lines_once() {
        assert_eq!(
            parse_line("[ytp-pp] started|Merger"),
            Some(Event::Log(Level::Info, "Merging video and audio...".into()))
        );
        // Only the start is reported, so the log doesn't say everything twice.
        assert_eq!(parse_line("[ytp-pp] finished|Merger"), None);
        assert_eq!(
            parse_line("[ytp-pp] started|SomethingNew"),
            Some(Event::Log(Level::Info, "SomethingNew...".into()))
        );
    }

    #[test]
    fn errors_warnings_and_plain_lines_keep_their_level() {
        assert_eq!(
            parse_line("ERROR: [youtube] abc: This video is unavailable"),
            Some(Event::Log(
                Level::Error,
                "[youtube] abc: This video is unavailable".into()
            ))
        );
        assert_eq!(
            parse_line("WARNING: something is off"),
            Some(Event::Log(Level::Warn, "something is off".into()))
        );
        assert_eq!(
            parse_line("[download] Destination: /tmp/video.mkv"),
            Some(Event::Log(
                Level::Info,
                "[download] Destination: /tmp/video.mkv".into()
            ))
        );
        assert_eq!(parse_line(""), None);
    }

    #[test]
    fn the_final_path_is_reported() {
        assert_eq!(
            parse_line("[ytp-file] /home/u/Videos/Me at the zoo [jNQXAC9IVRw].mkv"),
            Some(Event::File(
                "/home/u/Videos/Me at the zoo [jNQXAC9IVRw].mkv".into()
            ))
        );
    }

    #[test]
    fn presentation_flags_come_before_the_engine_arguments() {
        // The engine's argv ends with "--" and the URLs, so anything added
        // afterwards would be read as a URL.
        let flags = presentation_args();
        assert_eq!(flags.first().unwrap(), "--newline");
        assert!(flags.iter().any(|f| f == "--no-quiet"));
    }

    #[cfg(unix)]
    mod process {
        use super::*;
        use std::fs;
        use std::os::unix::fs::PermissionsExt;

        /// A stand-in for yt-dlp that prints the given script and exits with
        /// the given code.
        fn fake_ytdlp(body: &str, exit: i32) -> (std::path::PathBuf, Paths) {
            let dir = crate::testing::tempdir();
            let paths = Paths::new(dir.clone());
            fs::create_dir_all(&paths.bin_dir).unwrap();
            let script = paths.tool("yt-dlp");
            fs::write(&script, format!("#!/bin/sh\n{body}\nexit {exit}\n")).unwrap();
            fs::set_permissions(&script, fs::Permissions::from_mode(0o755)).unwrap();
            (dir, paths)
        }

        fn collect(paths: &Paths, cancel: &Cancel) -> Vec<Event> {
            let _writing = crate::testing::exec_guard();
            let mut events = Vec::new();
            run(
                paths,
                &Options::default(),
                None,
                &["https://example.com/v".into()],
                cancel,
                &mut |e| events.push(e),
            );
            events
        }

        #[test]
        fn a_successful_run_reports_progress_the_file_and_completion() {
            let (dir, paths) = fake_ytdlp(
                "echo '[ytp] downloading|10|100|NA|NA|NA'\n\
                 echo '[ytp-pp] started|Merger' >&2\n\
                 echo '[ytp-file] /tmp/out.mkv'\n\
                 echo 'ERROR: not fatal here' >&2",
                0,
            );
            let events = collect(&paths, &Cancel::default());
            assert!(events.iter().any(|e| matches!(e, Event::Progress(_))));
            assert!(events.contains(&Event::File("/tmp/out.mkv".into())));
            assert_eq!(events.last(), Some(&Event::Finished(Finish::Completed)));
            fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn a_failure_carries_the_last_error_line() {
            let (dir, paths) = fake_ytdlp("echo 'ERROR: this video is unavailable' >&2", 1);
            let events = collect(&paths, &Cancel::default());
            assert_eq!(
                events.last(),
                Some(&Event::Finished(Finish::Failed(
                    "this video is unavailable".into()
                )))
            );
            fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn exit_101_is_not_a_failure() {
            let (dir, paths) = fake_ytdlp("echo '[download] limit reached'", 101);
            let events = collect(&paths, &Cancel::default());
            assert_eq!(
                events.last(),
                Some(&Event::Finished(Finish::StoppedAsConfigured)),
                "{events:?}"
            );
            fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn cancelling_stops_the_child_and_reports_it_as_cancelled() {
            let (dir, paths) = fake_ytdlp("echo 'started'\nsleep 60", 0);
            let cancel = std::sync::Arc::new(Cancel::default());
            let stopper = cancel.clone();
            thread::spawn(move || {
                thread::sleep(std::time::Duration::from_millis(300));
                stopper.cancel();
            });
            let started = std::time::Instant::now();
            let events = collect(&paths, &cancel);
            assert!(started.elapsed() < std::time::Duration::from_secs(10));
            assert_eq!(events.last(), Some(&Event::Finished(Finish::Cancelled)));
            fs::remove_dir_all(dir).unwrap();
        }

        #[test]
        fn a_missing_yt_dlp_is_reported_instead_of_panicking() {
            let dir = crate::testing::tempdir();
            let paths = Paths::new(dir.clone());
            let events = collect(&paths, &Cancel::default());
            assert!(matches!(
                events.last(),
                Some(Event::Finished(Finish::Failed(_)))
            ));
            fs::remove_dir_all(dir).unwrap();
        }
    }
}
