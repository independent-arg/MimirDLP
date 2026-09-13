//! Turns download options into the exact arguments handed to yt-dlp.
//!
//! Flag order and defaults are pinned by `tests/engine_scenarios.rs`, which
//! rebuilds the argv of a fixed set of scenarios and compares it with what
//! each one is recorded to require. A behaviour change here must show up
//! there as a reviewed diff to those scenarios, never silently.

use std::env::consts::EXE_SUFFIX;
use std::ffi::OsString;
use std::fmt;
use std::path::Path;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AudioFormat {
    Mp3,
    Aac,
    Opus,
    Flac,
    M4a,
    Wav,
    Alac,
    Vorbis,
}

impl AudioFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            AudioFormat::Mp3 => "mp3",
            AudioFormat::Aac => "aac",
            AudioFormat::Opus => "opus",
            AudioFormat::Flac => "flac",
            AudioFormat::M4a => "m4a",
            AudioFormat::Wav => "wav",
            AudioFormat::Alac => "alac",
            AudioFormat::Vorbis => "vorbis",
        }
    }
}

impl fmt::Display for AudioFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AudioExtraction {
    pub format: AudioFormat,
    /// 0 (best) to 10 (worst). `None` leaves it to yt-dlp.
    pub quality: Option<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThumbnailFormat {
    Jpg,
    Png,
}

impl ThumbnailFormat {
    pub fn as_str(self) -> &'static str {
        match self {
            ThumbnailFormat::Jpg => "jpg",
            ThumbnailFormat::Png => "png",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Subtitles {
    /// yt-dlp `--sub-langs` syntax, e.g. `en.*` or `all,-live_chat`.
    /// Empty means all languages.
    pub languages: String,
    pub write_file: bool,
    pub embed: bool,
}

/// Remux rewraps the streams (fast, fails if the codecs don't fit), recode
/// re-encodes them (slow, always works). Only one can apply.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ContainerConversion {
    Remux(String),
    Recode(String),
}

impl ContainerConversion {
    pub fn target(&self) -> &str {
        match self {
            ContainerConversion::Remux(t) | ContainerConversion::Recode(t) => t,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SponsorBlock {
    Off,
    Mark {
        categories: String,
    },
    Remove {
        categories: String,
        force_keyframes: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Playlist {
    /// No flag at all: yt-dlp downloads a playlist only when the URL is one.
    Auto,
    Single,
    Whole {
        items: Option<String>,
        reverse: bool,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Archive {
    pub file: String,
    pub break_on_existing: bool,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Options {
    /// yt-dlp format selector, e.g. `bestvideo*+bestaudio/best`.
    pub format: String,
    /// Caps resolution through `--format-sort res:N`.
    pub max_resolution: Option<u32>,
    pub extract_audio: Option<AudioExtraction>,
    pub embed_thumbnail: bool,
    pub convert_thumbnail: Option<ThumbnailFormat>,
    pub merge_output_format: Option<String>,
    pub subtitles: Option<Subtitles>,
    pub embed_metadata: bool,
    pub embed_chapters: bool,
    pub embed_info_json: bool,
    pub container: Option<ContainerConversion>,
    pub sponsorblock: SponsorBlock,
    pub output_template: String,
    /// Replaces `output_template` with one subfolder per playlist.
    pub organize_playlist_folders: bool,
    pub verbose: bool,
    pub restrict_filenames: bool,
    pub preserve_mtime: bool,
    pub ignore_errors: bool,
    pub concurrent_fragments: u32,
    pub sleep_requests: f64,
    /// yt-dlp rate syntax, e.g. `500K` or `4.2M`.
    pub limit_rate: Option<String>,
    pub playlist: Playlist,
    pub archive: Option<Archive>,
    pub max_downloads: Option<u32>,
    pub live_from_start: bool,
    /// Seconds, or `MIN-MAX`.
    pub wait_for_video: Option<String>,
}

impl Default for Options {
    fn default() -> Self {
        Options {
            format: "bestvideo*+bestaudio/best".into(),
            max_resolution: None,
            extract_audio: None,
            embed_thumbnail: true,
            convert_thumbnail: Some(ThumbnailFormat::Jpg),
            merge_output_format: Some("mkv".into()),
            subtitles: None,
            embed_metadata: false,
            embed_chapters: false,
            embed_info_json: false,
            container: None,
            sponsorblock: SponsorBlock::Off,
            output_template: "%(title)s [%(id)s].%(ext)s".into(),
            organize_playlist_folders: false,
            verbose: true,
            restrict_filenames: true,
            preserve_mtime: false,
            ignore_errors: false,
            concurrent_fragments: 5,
            sleep_requests: 1.5,
            limit_rate: None,
            playlist: Playlist::Auto,
            archive: None,
            max_downloads: None,
            live_from_start: false,
            wait_for_video: None,
        }
    }
}

const PLAYLIST_FOLDER_TEMPLATE: &str = "%(playlist)s/%(playlist_index)02d - %(title)s.%(ext)s";

/// yt-dlp can embed a thumbnail only into these. Anything else is not a
/// warning but a postprocessing error that fails the whole download.
pub fn container_holds_thumbnail(ext: &str) -> bool {
    matches!(
        ext,
        "mp3" | "mkv" | "mka" | "ogg" | "opus" | "flac" | "m4a" | "mp4" | "mov"
    )
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThumbnailPlan {
    /// The final container is known to hold a thumbnail.
    Embed,
    /// Embed, and pass this rule to `--remux-video` so a webm result is
    /// rewrapped (no re-encode) into a container that can hold one.
    EmbedWithRewrap(&'static str),
    /// The final file will have this extension, which can't hold one.
    Skip(String),
}

pub fn thumbnail_plan(o: &Options) -> ThumbnailPlan {
    if let Some(audio) = &o.extract_audio {
        // -x aac and -x alac write .m4a and -x vorbis writes .ogg, all fine.
        return if audio.format == AudioFormat::Wav {
            ThumbnailPlan::Skip("wav".into())
        } else {
            ThumbnailPlan::Embed
        };
    }
    if let Some(conversion) = &o.container {
        let target = conversion.target();
        return if container_holds_thumbnail(target) {
            ThumbnailPlan::Embed
        } else {
            ThumbnailPlan::Skip(target.to_string())
        };
    }
    // No container chosen: a single stream keeps whatever the site serves,
    // which for YouTube is usually webm.
    if o.format == "bestaudio" || o.format.starts_with("bestaudio/") {
        ThumbnailPlan::EmbedWithRewrap("webm>opus")
    } else {
        ThumbnailPlan::EmbedWithRewrap("webm>mkv")
    }
}

/// `--embed-metadata` on its own also embeds chapters (yt-dlp's default),
/// so chapters are always stated explicitly. SponsorBlock marks only reach
/// the file as chapters, so marking forces them on.
pub fn chapters_wanted(o: &Options) -> bool {
    o.embed_chapters || matches!(o.sponsorblock, SponsorBlock::Mark { .. })
}

fn tool_path(bin_dir: &Path, name: &str) -> OsString {
    bin_dir.join(format!("{name}{EXE_SUFFIX}")).into_os_string()
}

/// The arguments for yt-dlp, not including the program itself.
pub fn build_args(
    o: &Options,
    bin_dir: &Path,
    output_dir: Option<&Path>,
    urls: &[String],
) -> Vec<OsString> {
    let mut a: Vec<OsString> = Vec::new();
    let mut push = |s: &str| a.push(s.into());

    if o.verbose {
        push("--verbose");
    }
    push("--socket-timeout");
    push("30");
    push("--sleep-requests");
    push(&o.sleep_requests.to_string());
    push("--concurrent-fragments");
    push(&o.concurrent_fragments.to_string());
    push("--ffmpeg-location");
    a.push(tool_path(bin_dir, "ffmpeg"));
    let mut js_runtime = OsString::from("deno:");
    js_runtime.push(tool_path(bin_dir, "deno"));
    a.push("--js-runtimes".into());
    a.push(js_runtime);

    let mut push = |s: &str| a.push(s.into());
    if let Some(rate) = &o.limit_rate {
        push("--limit-rate");
        push(rate);
    }
    if let Some(dir) = output_dir {
        a.push("-P".into());
        a.push(dir.as_os_str().to_owned());
    }

    let mut push = |s: &str| a.push(s.into());
    push("-f");
    push(&o.format);
    if let Some(res) = o.max_resolution {
        push("--format-sort");
        push(&format!("res:{res}"));
    }

    match &o.playlist {
        Playlist::Auto => {}
        Playlist::Single => push("--no-playlist"),
        Playlist::Whole { items, reverse } => {
            push("--yes-playlist");
            if let Some(items) = items {
                push("-I");
                push(items);
            }
            if *reverse {
                push("--playlist-reverse");
            }
        }
    }
    if o.ignore_errors {
        push("-i");
    }

    if o.live_from_start {
        push("--live-from-start");
    }
    if let Some(wait) = &o.wait_for_video {
        push("--wait-for-video");
        push(wait);
    }

    if let Some(archive) = &o.archive {
        push("--download-archive");
        push(&archive.file);
        if archive.break_on_existing {
            push("--break-on-existing");
        }
    }
    // Independent of the archive.
    if let Some(max) = o.max_downloads {
        push("--max-downloads");
        push(&max.to_string());
    }

    if o.embed_thumbnail {
        match thumbnail_plan(o) {
            ThumbnailPlan::Skip(_) => {}
            plan => {
                push("--embed-thumbnail");
                if let Some(format) = o.convert_thumbnail {
                    push("--convert-thumbnails");
                    push(format.as_str());
                }
                if let ThumbnailPlan::EmbedWithRewrap(rule) = plan {
                    push("--remux-video");
                    push(rule);
                }
            }
        }
    }

    // A merge container only matters when nothing downstream changes it.
    if o.container.is_none()
        && let Some(merge) = &o.merge_output_format
    {
        push("--merge-output-format");
        push(merge);
    }

    match &o.sponsorblock {
        SponsorBlock::Off => {}
        SponsorBlock::Mark { categories } => {
            push("--sponsorblock-mark");
            push(categories);
        }
        SponsorBlock::Remove {
            categories,
            force_keyframes,
        } => {
            push("--sponsorblock-remove");
            push(categories);
            if *force_keyframes {
                push("--force-keyframes-at-cuts");
            }
        }
    }

    if o.embed_metadata {
        push("--embed-metadata");
    }
    if chapters_wanted(o) {
        push("--embed-chapters");
    } else if o.embed_metadata {
        push("--no-embed-chapters");
    }
    if o.embed_info_json {
        push("--embed-info-json");
    }

    if let Some(subs) = &o.subtitles {
        if subs.embed {
            push("--embed-subs");
        }
        if subs.write_file {
            push("--write-subs");
        }
        push("--sub-langs");
        push(if subs.languages.is_empty() {
            "all"
        } else {
            &subs.languages
        });
    }

    if let Some(audio) = &o.extract_audio {
        push("--extract-audio");
        push("--audio-format");
        push(audio.format.as_str());
        if let Some(q) = audio.quality {
            push("--audio-quality");
            push(&q.to_string());
        }
    }

    match &o.container {
        None => {}
        Some(ContainerConversion::Remux(t)) => {
            push("--remux-video");
            push(t);
        }
        Some(ContainerConversion::Recode(t)) => {
            push("--recode-video");
            push(t);
        }
    }

    if o.restrict_filenames {
        push("--restrict-filenames");
    }
    // yt-dlp already defaults to --no-mtime; only the opposite is ever passed.
    if o.preserve_mtime {
        push("--mtime");
    }

    push("--output");
    push(if o.organize_playlist_folders {
        PLAYLIST_FOLDER_TEMPLATE
    } else {
        &o.output_template
    });

    // "--" so an ID starting with "-" is never read as an option.
    push("--");
    for url in urls {
        push(url);
    }
    a
}

/// What a yt-dlp exit code means for the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    Completed,
    /// 101, DownloadCancelled: the download limit or `--break-on-existing`
    /// was reached. This is what the user asked for, not a failure.
    StoppedAsConfigured,
    Failed(i32),
}

pub fn classify_exit(code: i32) -> Outcome {
    match code {
        0 => Outcome::Completed,
        101 => Outcome::StoppedAsConfigured,
        other => Outcome::Failed(other),
    }
}
