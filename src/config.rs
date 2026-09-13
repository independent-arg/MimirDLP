//! What the Download screen remembers, stored next to the application.
//!
//! Portable on purpose: the file lives in the application folder, not in
//! `~/.config`, so copying the folder copies the settings with it. The
//! format is the plain `key=value` of `bin/.install_state`, parsed by hand,
//! and unknown keys are ignored so an older build can read a newer file.
//!
//! This is the GUI's own model. It is deliberately narrower than
//! [`crate::engine::Options`]: combinations that make no sense (audio only
//! with a video container, say) can't be expressed here at all, and
//! [`Settings::to_options`] is the single place that turns it into engine
//! options. Numbers the user types stay text and are checked by the
//! `valid_*` functions, so a half typed value never reaches yt-dlp.

use std::fmt;
use std::fs;
use std::path::PathBuf;

use crate::engine::{
    Archive, AudioExtraction, AudioFormat, ContainerConversion, Options, Playlist, SponsorBlock,
    Subtitles, ThumbnailFormat,
};
use crate::provision::Paths;

pub const FILE_NAME: &str = "mimirdlp.config";
pub const ARCHIVE_NAME: &str = "download_archive.txt";

macro_rules! keyed {
    ($name:ident { $($variant:ident => $key:literal, $label:literal),+ $(,)? }) => {
        impl $name {
            pub const ALL: &'static [$name] = &[$($name::$variant),+];

            fn key(self) -> &'static str {
                match self { $($name::$variant => $key),+ }
            }

            fn from_key(key: &str) -> Option<Self> {
                match key { $($key => Some($name::$variant),)+ _ => None }
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(match self { $($name::$variant => $label),+ })
            }
        }
    };
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    VideoAudio,
    AudioOnly,
}
keyed!(Kind {
    VideoAudio => "video", "Video and audio",
    AudioOnly => "audio", "Audio only",
});

/// What to ask yt-dlp for when downloading video.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VideoFormat {
    Best,
    PreMerged,
    VideoOnly,
    Custom,
}
keyed!(VideoFormat {
    Best => "best", "Best quality (video plus audio)",
    PreMerged => "premerged", "Best single file (faster, no merging)",
    VideoOnly => "videoonly", "Video only, without audio",
    Custom => "custom", "A format string of my own",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Quality {
    Best,
    P2160,
    P1440,
    P1080,
    P720,
    P480,
}
keyed!(Quality {
    Best => "best", "Best available",
    P2160 => "2160", "Up to 2160p (4K)",
    P1440 => "1440", "Up to 1440p",
    P1080 => "1080", "Up to 1080p",
    P720 => "720", "Up to 720p",
    P480 => "480", "Up to 480p",
});

impl Quality {
    /// `--format-sort res:N`, or nothing for the best available.
    fn cap(self) -> Option<u32> {
        match self {
            Quality::Best => None,
            Quality::P2160 => Some(2160),
            Quality::P1440 => Some(1440),
            Quality::P1080 => Some(1080),
            Quality::P720 => Some(720),
            Quality::P480 => Some(480),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Audio {
    Mp3,
    Aac,
    M4a,
    Opus,
    Vorbis,
    Flac,
    Alac,
    Wav,
}
keyed!(Audio {
    Mp3 => "mp3", "MP3 (most compatible)",
    Aac => "aac", "AAC",
    M4a => "m4a", "M4A",
    Opus => "opus", "Opus (smallest)",
    Vorbis => "vorbis", "Vorbis",
    Flac => "flac", "FLAC (lossless)",
    Alac => "alac", "ALAC (lossless)",
    Wav => "wav", "WAV (uncompressed)",
});

impl Audio {
    fn format(self) -> AudioFormat {
        match self {
            Audio::Mp3 => AudioFormat::Mp3,
            Audio::Aac => AudioFormat::Aac,
            Audio::M4a => AudioFormat::M4a,
            Audio::Opus => AudioFormat::Opus,
            Audio::Vorbis => AudioFormat::Vorbis,
            Audio::Flac => AudioFormat::Flac,
            Audio::Alac => AudioFormat::Alac,
            Audio::Wav => AudioFormat::Wav,
        }
    }

    /// Lossless formats ignore the quality scale, so it is pinned to 0 and
    /// not asked about.
    pub fn is_lossless(self) -> bool {
        matches!(self, Audio::Flac | Audio::Alac | Audio::Wav)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Container {
    Mkv,
    Mp4,
}
keyed!(Container {
    Mkv => "mkv", "MKV (recommended)",
    Mp4 => "mp4", "MP4 (most compatible)",
});

/// Rewrapping keeps the streams, re-encoding replaces them. Two ways to the
/// same container, so only one can apply (the engine enforces that too).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Convert {
    Keep,
    Remux,
    Recode,
}
keyed!(Convert {
    Keep => "keep", "Leave it alone",
    Remux => "remux", "Rewrap (fast, keeps the codecs)",
    Recode => "recode", "Re-encode (slow, always works)",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Thumb {
    Jpg,
    Original,
    Png,
}
keyed!(Thumb {
    Jpg => "jpg", "As JPG (recommended)",
    Original => "original", "As it comes",
    Png => "png", "As PNG",
});

impl Thumb {
    fn format(self) -> Option<ThumbnailFormat> {
        match self {
            Thumb::Jpg => Some(ThumbnailFormat::Jpg),
            Thumb::Png => Some(ThumbnailFormat::Png),
            Thumb::Original => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Subs {
    Off,
    File,
    Embed,
    Both,
}
keyed!(Subs {
    Off => "off", "Don't download",
    File => "file", "Save as .srt file",
    Embed => "embed", "Embed in the video",
    Both => "both", "Save and embed",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Sponsor {
    Off,
    Mark,
    Remove,
}
keyed!(Sponsor {
    Off => "off", "Off",
    Mark => "mark", "Mark as chapters",
    Remove => "remove", "Cut out of the file",
});

/// `default` is yt-dlp's own preset: everything for marking, everything but
/// filler for removing, and it never includes the two categories that can
/// only be marked.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cats {
    SponsorsOnly,
    Promos,
    Everything,
    Custom,
}
keyed!(Cats {
    SponsorsOnly => "sponsor", "Sponsors only",
    Promos => "promos", "Sponsors, self-promotion, interaction",
    Everything => "default", "Everything SponsorBlock covers",
    Custom => "custom", "A list of my own",
});

/// The window's colors. An app-wide preference, kept in the same file as
/// everything else since the app is portable and has nowhere else to put it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThemePref {
    System,
    Light,
    Dark,
}
keyed!(ThemePref {
    System => "system", "Follow system",
    Light => "light", "Light",
    Dark => "dark", "Dark",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlaylistChoice {
    Auto,
    Single,
    Whole,
}
keyed!(PlaylistChoice {
    Auto => "auto", "Follow the link",
    Single => "single", "Just this video",
    Whole => "whole", "The whole playlist",
});

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Template {
    TitleId,
    Title,
    Id,
    TitleChannel,
    Custom,
}
keyed!(Template {
    TitleId => "title_id", "Title [ID].ext",
    Title => "title", "Title.ext",
    Id => "id", "ID.ext",
    TitleChannel => "title_channel", "Title - Channel [ID].ext",
    Custom => "custom", "A template of my own",
});

impl Template {
    fn preset(self) -> &'static str {
        match self {
            Template::TitleId | Template::Custom => "%(title)s [%(id)s].%(ext)s",
            Template::Title => "%(title)s.%(ext)s",
            Template::Id => "%(id)s.%(ext)s",
            Template::TitleChannel => "%(title)s - %(uploader)s [%(id)s].%(ext)s",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub theme: ThemePref,
    /// Empty means the `downloads` folder next to the application.
    pub output_dir: String,
    pub kind: Kind,
    pub video_format: VideoFormat,
    /// yt-dlp `-f` syntax, used by [`VideoFormat::Custom`].
    pub custom_format: String,
    pub quality: Quality,
    pub audio: Audio,
    /// Off keeps the audio stream exactly as the site serves it: no
    /// `--extract-audio` at all, not even to the site's own format.
    pub audio_convert: bool,
    /// yt-dlp's `--audio-quality` scale: 0 best, 10 smallest.
    pub audio_quality: u8,
    pub container: Container,
    pub convert: Convert,
    /// Target container for [`Convert`], for example `mp4`.
    pub convert_target: String,
    pub thumbnail: bool,
    pub thumbnail_format: Thumb,
    pub metadata: bool,
    pub chapters: bool,
    pub subs: Subs,
    /// yt-dlp `--sub-langs` syntax. Empty means all languages.
    pub sub_langs: String,
    pub sponsor: Sponsor,
    pub cats: Cats,
    /// Used when [`Cats::Custom`] is selected.
    pub custom_cats: String,
    pub keyframes: bool,
    pub playlist: PlaylistChoice,
    /// yt-dlp `-I` syntax, for example `1-5,8`. Empty means every item.
    pub playlist_items: String,
    pub playlist_reverse: bool,
    pub organize: bool,
    pub archive: bool,
    /// Empty means `download_archive.txt` next to the application. A bare
    /// name is kept there too; an absolute path is used as given.
    pub archive_file: String,
    pub archive_stop: bool,
    /// Empty means no limit.
    pub max_downloads: String,
    pub template: Template,
    pub custom_template: String,
    /// yt-dlp rate syntax (`500K`, `4.2M`). Empty means no limit.
    pub limit_rate: String,
    pub fragments: String,
    pub sleep: String,
    pub live_from_start: bool,
    /// Seconds or `MIN-MAX`. Empty means don't wait.
    pub wait_for_video: String,
    pub verbose: bool,
    pub restrict_filenames: bool,
    pub preserve_mtime: bool,
    pub ignore_errors: bool,
}

impl Default for Settings {
    fn default() -> Self {
        let engine = Options::default();
        Settings {
            theme: ThemePref::System,
            output_dir: String::new(),
            kind: Kind::VideoAudio,
            video_format: VideoFormat::Best,
            custom_format: String::new(),
            quality: Quality::Best,
            audio: Audio::Mp3,
            audio_convert: true,
            audio_quality: 5,
            container: Container::Mkv,
            convert: Convert::Keep,
            convert_target: String::new(),
            thumbnail: engine.embed_thumbnail,
            thumbnail_format: Thumb::Jpg,
            metadata: engine.embed_metadata,
            chapters: engine.embed_chapters,
            subs: Subs::Off,
            sub_langs: String::new(),
            sponsor: Sponsor::Off,
            cats: Cats::SponsorsOnly,
            custom_cats: String::new(),
            keyframes: false,
            playlist: PlaylistChoice::Auto,
            playlist_items: String::new(),
            playlist_reverse: false,
            organize: false,
            archive: false,
            archive_file: String::new(),
            archive_stop: false,
            max_downloads: String::new(),
            template: Template::TitleId,
            custom_template: String::new(),
            limit_rate: String::new(),
            fragments: engine.concurrent_fragments.to_string(),
            sleep: engine.sleep_requests.to_string(),
            live_from_start: engine.live_from_start,
            wait_for_video: String::new(),
            // yt-dlp's verbose output is noise in a window with no
            // terminal to scroll back through, so it starts off.
            verbose: false,
            restrict_filenames: engine.restrict_filenames,
            preserve_mtime: engine.preserve_mtime,
            ignore_errors: engine.ignore_errors,
        }
    }
}

impl Settings {
    /// Where files land: the configured folder, or `downloads` next to the
    /// application. A leading `~` is expanded by hand, since nothing else
    /// in this text field's path does it for us.
    pub fn output_dir(&self, paths: &Paths) -> PathBuf {
        let dir = self.output_dir.trim();
        if dir.is_empty() {
            return paths.app_dir.join("downloads");
        }
        if let Some(rest) = dir.strip_prefix('~')
            && (rest.is_empty() || rest.starts_with('/'))
            && let Some(home) = home_dir()
        {
            return PathBuf::from(home).join(rest.trim_start_matches('/'));
        }
        PathBuf::from(dir)
    }

    /// The history file: the name given, next to the application unless it
    /// is an absolute path, or `download_archive.txt` when nothing is set.
    pub fn archive_path(&self, paths: &Paths) -> PathBuf {
        let name = self.archive_file.trim();
        if name.is_empty() {
            return paths.app_dir.join(ARCHIVE_NAME);
        }
        let path = PathBuf::from(name);
        if path.is_absolute() {
            path
        } else {
            paths.app_dir.join(path)
        }
    }

    /// Every video the history holds, one entry per line as yt-dlp wrote
    /// them (`<extractor> <id>`), if the file exists.
    pub fn history_lines(&self, paths: &Paths) -> Option<Vec<String>> {
        let text = fs::read_to_string(self.archive_path(paths)).ok()?;
        Some(
            text.lines()
                .filter(|line| !line.trim().is_empty())
                .map(str::to_string)
                .collect(),
        )
    }

    /// How many videos the history already holds, if it exists.
    pub fn history_entries(&self, paths: &Paths) -> Option<usize> {
        self.history_lines(paths).map(|lines| lines.len())
    }

    /// The categories that will be sent to yt-dlp.
    pub fn categories(&self) -> &str {
        match self.cats {
            Cats::SponsorsOnly => "sponsor",
            Cats::Promos => "sponsor,selfpromo,interaction",
            Cats::Everything => "default",
            Cats::Custom => self.custom_cats.trim(),
        }
    }

    /// Every reason this configuration can't be handed to yt-dlp, worded for
    /// the person who has to fix it. The screen shows the first one.
    pub fn problems(&self) -> Vec<String> {
        let mut problems = Vec::new();
        if !valid_rate(&self.limit_rate) {
            problems.push("The speed limit should look like 500K or 4.2M, or be empty.".into());
        }
        if !valid_fragments(&self.fragments) {
            problems.push("Concurrent fragments should be a whole number of 1 or more.".into());
        }
        if !valid_sleep(&self.sleep) {
            problems
                .push("The sleep between requests should be a number such as 0, 1.5 or 2.".into());
        }
        if !valid_wait(&self.wait_for_video) {
            problems.push(
                "The wait for a stream should be seconds or a range, such as 60 or 60-3600.".into(),
            );
        }
        if !valid_limit(&self.max_downloads) {
            problems.push("The download limit should be a whole number, or empty.".into());
        }
        if self.playlist == PlaylistChoice::Whole && !valid_items(&self.playlist_items) {
            problems.push("Playlist items should look like 1-5, 8 or 2,4,6.".into());
        }
        if self.kind == Kind::VideoAudio
            && self.convert != Convert::Keep
            && !valid_container(&self.convert_target)
        {
            problems
                .push("The container to convert to should be a name such as mp4 or mkv.".into());
        }
        if self.sponsor != Sponsor::Off
            && let Err(reason) = validate_categories(self.categories(), self.sponsor)
        {
            problems.push(reason);
        }
        if self.kind == Kind::VideoAudio
            && self.video_format == VideoFormat::Custom
            && self.custom_format.trim().is_empty()
        {
            problems.push("The custom format string can't be empty.".into());
        }
        if self.template == Template::Custom && self.custom_template.trim().is_empty() {
            problems.push("The custom name template can't be empty.".into());
        }
        problems
    }

    /// The single place where the screen's model becomes engine options.
    pub fn to_options(&self, paths: &Paths) -> Options {
        let mut o = Options::default();
        match self.kind {
            Kind::VideoAudio => {
                o.format = match self.video_format {
                    VideoFormat::Best => o.format,
                    VideoFormat::PreMerged => "best".into(),
                    VideoFormat::VideoOnly => "bestvideo".into(),
                    // problems() refuses an empty one, so this is only a
                    // guard against ever handing yt-dlp nothing at all.
                    VideoFormat::Custom => some_text(&self.custom_format).unwrap_or(o.format),
                };
                o.max_resolution = match self.video_format {
                    // A format string of your own picks its own quality.
                    VideoFormat::Custom => None,
                    _ => self.quality.cap(),
                };
                o.merge_output_format = Some(match self.container {
                    Container::Mkv => "mkv".into(),
                    Container::Mp4 => "mp4".into(),
                });
                let target = self.convert_target.trim().to_ascii_lowercase();
                if !target.is_empty() {
                    o.container = match self.convert {
                        Convert::Keep => None,
                        Convert::Remux => Some(ContainerConversion::Remux(target)),
                        Convert::Recode => Some(ContainerConversion::Recode(target)),
                    };
                }
            }
            Kind::AudioOnly => {
                // Fetch the audio stream only: downloading a video stream
                // just to throw it away would be waste.
                o.format = "bestaudio/best".into();
                // Off means "leave it as the site serves it", so there is
                // no extraction at all and the engine rewraps webm audio
                // into opus so a thumbnail still fits.
                o.extract_audio = self.audio_convert.then(|| AudioExtraction {
                    format: self.audio.format(),
                    quality: Some(if self.audio.is_lossless() {
                        0
                    } else {
                        self.audio_quality.min(10)
                    }),
                });
                o.merge_output_format = None;
            }
        }
        o.embed_thumbnail = self.thumbnail;
        o.convert_thumbnail = self.thumbnail_format.format();
        o.embed_metadata = self.metadata;
        o.embed_chapters = self.chapters;
        o.subtitles = match self.subs {
            Subs::Off => None,
            choice => Some(Subtitles {
                languages: self.sub_langs.trim().to_string(),
                write_file: matches!(choice, Subs::File | Subs::Both),
                embed: matches!(choice, Subs::Embed | Subs::Both),
            }),
        };
        o.sponsorblock = match self.sponsor {
            Sponsor::Off => SponsorBlock::Off,
            Sponsor::Mark => SponsorBlock::Mark {
                categories: self.categories().into(),
            },
            Sponsor::Remove => SponsorBlock::Remove {
                categories: self.categories().into(),
                force_keyframes: self.keyframes,
            },
        };
        o.playlist = match self.playlist {
            PlaylistChoice::Auto => Playlist::Auto,
            PlaylistChoice::Single => Playlist::Single,
            PlaylistChoice::Whole => Playlist::Whole {
                items: some_text(&self.playlist_items),
                reverse: self.playlist_reverse,
            },
        };
        o.organize_playlist_folders = self.organize;
        o.archive = self.archive.then(|| Archive {
            // Portable: the history lives with the application.
            file: self.archive_path(paths).to_string_lossy().into(),
            break_on_existing: self.archive_stop,
        });
        // Independent of the archive: it works with or without one.
        o.max_downloads = self.max_downloads.trim().parse().ok().filter(|n| *n > 0);
        o.output_template = match self.template {
            Template::Custom => self.custom_template.trim().to_string(),
            preset => preset.preset().to_string(),
        };
        o.limit_rate = some_text(&self.limit_rate);
        if let Ok(fragments) = self.fragments.trim().parse::<u32>()
            && fragments >= 1
        {
            o.concurrent_fragments = fragments;
        }
        if let Ok(sleep) = self.sleep.trim().parse::<f64>()
            && sleep >= 0.0
        {
            o.sleep_requests = sleep;
        }
        o.live_from_start = self.live_from_start;
        o.wait_for_video = some_text(&self.wait_for_video);
        o.verbose = self.verbose;
        o.restrict_filenames = self.restrict_filenames;
        o.preserve_mtime = self.preserve_mtime;
        o.ignore_errors = self.ignore_errors;
        o
    }

    pub fn load(paths: &Paths) -> Settings {
        Settings::parse(&fs::read_to_string(Settings::file(paths)).unwrap_or_default())
    }

    pub fn save(&self, paths: &Paths) -> std::io::Result<()> {
        let file = Settings::file(paths);
        if let Some(parent) = file.parent() {
            fs::create_dir_all(parent)?;
        }
        // Written through a temporary file so an interrupted save can't
        // leave half a line behind.
        let tmp = file.with_extension("tmp");
        fs::write(&tmp, self.serialize())?;
        fs::rename(&tmp, &file)
    }

    fn file(paths: &Paths) -> PathBuf {
        paths.app_dir.join(FILE_NAME)
    }

    fn serialize(&self) -> String {
        let yes = |v: bool| if v { "yes" } else { "no" };
        [
            ("theme", self.theme.key().into()),
            ("output_dir", self.output_dir.clone()),
            ("kind", self.kind.key().into()),
            ("video_format", self.video_format.key().into()),
            ("custom_format", self.custom_format.clone()),
            ("quality", self.quality.key().into()),
            ("audio_format", self.audio.key().into()),
            ("audio_convert", yes(self.audio_convert).into()),
            ("audio_quality", self.audio_quality.to_string()),
            ("container", self.container.key().into()),
            ("convert", self.convert.key().into()),
            ("convert_target", self.convert_target.clone()),
            ("thumbnail", yes(self.thumbnail).into()),
            ("thumbnail_format", self.thumbnail_format.key().into()),
            ("metadata", yes(self.metadata).into()),
            ("chapters", yes(self.chapters).into()),
            ("subtitles", self.subs.key().into()),
            ("subtitle_langs", self.sub_langs.clone()),
            ("sponsorblock", self.sponsor.key().into()),
            ("sponsorblock_cats", self.cats.key().into()),
            ("sponsorblock_custom", self.custom_cats.clone()),
            ("sponsorblock_keyframes", yes(self.keyframes).into()),
            ("playlist", self.playlist.key().into()),
            ("playlist_items", self.playlist_items.clone()),
            ("playlist_reverse", yes(self.playlist_reverse).into()),
            ("organize", yes(self.organize).into()),
            ("archive", yes(self.archive).into()),
            ("archive_file", self.archive_file.clone()),
            ("archive_stop", yes(self.archive_stop).into()),
            ("max_downloads", self.max_downloads.clone()),
            ("template", self.template.key().into()),
            ("custom_template", self.custom_template.clone()),
            ("limit_rate", self.limit_rate.clone()),
            ("fragments", self.fragments.clone()),
            ("sleep", self.sleep.clone()),
            ("live_from_start", yes(self.live_from_start).into()),
            ("wait_for_video", self.wait_for_video.clone()),
            ("verbose", yes(self.verbose).into()),
            ("restrict_filenames", yes(self.restrict_filenames).into()),
            ("preserve_mtime", yes(self.preserve_mtime).into()),
            ("ignore_errors", yes(self.ignore_errors).into()),
        ]
        .into_iter()
        .map(|(key, value)| format!("{key}={value}\n"))
        .collect()
    }

    fn parse(text: &str) -> Settings {
        let mut s = Settings::default();
        for (key, value) in text.lines().filter_map(|line| line.split_once('=')) {
            let value = value.trim();
            let flag = |current: bool| match value {
                "yes" => true,
                "no" => false,
                _ => current,
            };
            match key.trim() {
                "theme" => s.theme = ThemePref::from_key(value).unwrap_or(s.theme),
                "output_dir" => s.output_dir = value.to_string(),
                "kind" => s.kind = Kind::from_key(value).unwrap_or(s.kind),
                "video_format" => {
                    s.video_format = VideoFormat::from_key(value).unwrap_or(s.video_format)
                }
                "custom_format" => s.custom_format = value.to_string(),
                "quality" => s.quality = Quality::from_key(value).unwrap_or(s.quality),
                "audio_format" => s.audio = Audio::from_key(value).unwrap_or(s.audio),
                "audio_convert" => s.audio_convert = flag(s.audio_convert),
                "audio_quality" => {
                    s.audio_quality = value
                        .parse()
                        .map(|q: u8| q.min(10))
                        .unwrap_or(s.audio_quality)
                }
                "container" => s.container = Container::from_key(value).unwrap_or(s.container),
                "convert" => s.convert = Convert::from_key(value).unwrap_or(s.convert),
                "convert_target" => s.convert_target = value.to_string(),
                "thumbnail" => s.thumbnail = flag(s.thumbnail),
                "thumbnail_format" => {
                    s.thumbnail_format = Thumb::from_key(value).unwrap_or(s.thumbnail_format)
                }
                "metadata" => s.metadata = flag(s.metadata),
                "chapters" => s.chapters = flag(s.chapters),
                "subtitles" => s.subs = Subs::from_key(value).unwrap_or(s.subs),
                "subtitle_langs" => s.sub_langs = value.to_string(),
                "sponsorblock" => s.sponsor = Sponsor::from_key(value).unwrap_or(s.sponsor),
                "sponsorblock_cats" => s.cats = Cats::from_key(value).unwrap_or(s.cats),
                "sponsorblock_custom" => s.custom_cats = value.to_string(),
                "sponsorblock_keyframes" => s.keyframes = flag(s.keyframes),
                "playlist" => s.playlist = PlaylistChoice::from_key(value).unwrap_or(s.playlist),
                "playlist_items" => s.playlist_items = value.to_string(),
                "playlist_reverse" => s.playlist_reverse = flag(s.playlist_reverse),
                "organize" => s.organize = flag(s.organize),
                "archive" => s.archive = flag(s.archive),
                "archive_file" => s.archive_file = value.to_string(),
                "archive_stop" => s.archive_stop = flag(s.archive_stop),
                "max_downloads" => s.max_downloads = value.to_string(),
                "template" => s.template = Template::from_key(value).unwrap_or(s.template),
                "custom_template" => s.custom_template = value.to_string(),
                "limit_rate" => s.limit_rate = value.to_string(),
                "fragments" => s.fragments = value.to_string(),
                "sleep" => s.sleep = value.to_string(),
                "live_from_start" => s.live_from_start = flag(s.live_from_start),
                "wait_for_video" => s.wait_for_video = value.to_string(),
                "verbose" => s.verbose = flag(s.verbose),
                "restrict_filenames" => s.restrict_filenames = flag(s.restrict_filenames),
                "preserve_mtime" => s.preserve_mtime = flag(s.preserve_mtime),
                "ignore_errors" => s.ignore_errors = flag(s.ignore_errors),
                _ => {}
            }
        }
        s
    }
}

/// Windows has no `HOME`, it has `USERPROFILE`.
fn home_dir() -> Option<std::ffi::OsString> {
    std::env::var_os("HOME").or_else(|| std::env::var_os("USERPROFILE"))
}

fn some_text(value: &str) -> Option<String> {
    let value = value.trim();
    (!value.is_empty()).then(|| value.to_string())
}

/// A rate like `500K` or `4.2M`, the syntax yt-dlp documents for
/// `--limit-rate`. Empty is valid: no limit.
pub fn valid_rate(rate: &str) -> bool {
    let rate = rate.trim();
    if rate.is_empty() {
        return true;
    }
    let digits = rate.strip_suffix(['K', 'M', 'G']).unwrap_or(rate);
    !digits.starts_with('-') && matches!(digits.parse::<f64>(), Ok(n) if n.is_finite())
}

/// yt-dlp refuses 0 fragments and anything that isn't a plain number.
pub fn valid_fragments(value: &str) -> bool {
    matches!(value.trim().parse::<u32>(), Ok(n) if n >= 1)
}

pub fn valid_sleep(value: &str) -> bool {
    matches!(value.trim().parse::<f64>(), Ok(n) if n >= 0.0)
}

/// Seconds, or `MIN-MAX`. Empty means don't wait.
pub fn valid_wait(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    match value.split_once('-') {
        Some((min, max)) => is_number(min) && is_number(max),
        None => is_number(value),
    }
}

pub fn valid_limit(value: &str) -> bool {
    let value = value.trim();
    value.is_empty() || matches!(value.parse::<u32>(), Ok(n) if n >= 1)
}

/// yt-dlp's `-I` syntax, kept to the ranges and lists the menu documents.
pub fn valid_items(value: &str) -> bool {
    let value = value.trim();
    if value.is_empty() {
        return true;
    }
    value.split(',').all(|part| {
        let part = part.trim();
        match part.split_once('-') {
            Some((start, end)) => {
                (start.trim().is_empty() || is_number(start))
                    && (end.trim().is_empty() || is_number(end))
                    && !(start.trim().is_empty() && end.trim().is_empty())
            }
            None => is_number(part),
        }
    })
}

/// A container name: lowercase letters and digits only.
pub fn valid_container(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty()
        && value
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
}

fn is_number(value: &str) -> bool {
    let value = value.trim();
    !value.is_empty() && value.chars().all(|c| c.is_ascii_digit())
}

/// The two modes accept different category sets, and yt-dlp only rejects a
/// bad list when it parses its arguments, well after this screen could have
/// said so instead.
pub fn validate_categories(categories: &str, mode: Sponsor) -> Result<(), String> {
    const VALID: &[&str] = &[
        "sponsor",
        "intro",
        "outro",
        "selfpromo",
        "preview",
        "filler",
        "interaction",
        "music_offtopic",
        "hook",
        "poi_highlight",
        "chapter",
        "all",
        "default",
    ];
    let categories = categories.trim();
    if categories.is_empty() {
        return Err("Pick at least one SponsorBlock category.".into());
    }
    for part in categories.split(',') {
        let bare = part.trim().trim_start_matches('-');
        if bare.is_empty() {
            return Err("There is an empty category in the SponsorBlock list.".into());
        }
        if !VALID.contains(&bare) {
            return Err(format!("Unknown SponsorBlock category: {bare}"));
        }
        if mode == Sponsor::Remove && matches!(bare, "poi_highlight" | "chapter") {
            return Err(format!("'{bare}' can only be marked, not removed."));
        }
    }
    Ok(())
}

/// A URL yt-dlp could plausibly take. Anything stricter would reject the
/// shortened and regional forms it supports.
pub fn valid_url(url: &str) -> bool {
    let url = url.trim();
    (url.starts_with("http://") || url.starts_with("https://")) && url.len() > 10
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::engine::{ThumbnailPlan, thumbnail_plan};
    use crate::testing::tempdir;

    fn paths() -> Paths {
        Paths::new(PathBuf::from("/app"))
    }

    #[test]
    fn settings_survive_a_save_and_load_round_trip() {
        let dir = tempdir();
        let paths = Paths::new(dir.clone());
        let settings = Settings {
            theme: ThemePref::Light,
            output_dir: "/videos".into(),
            kind: Kind::AudioOnly,
            video_format: VideoFormat::Custom,
            custom_format: "bestvideo+bestaudio".into(),
            audio: Audio::Flac,
            audio_convert: false,
            audio_quality: 3,
            convert: Convert::Recode,
            convert_target: "mp4".into(),
            sponsor: Sponsor::Remove,
            cats: Cats::Custom,
            custom_cats: "sponsor,intro".into(),
            keyframes: true,
            playlist: PlaylistChoice::Whole,
            playlist_items: "1-5".into(),
            playlist_reverse: true,
            organize: true,
            archive: true,
            archive_file: "seen.txt".into(),
            archive_stop: true,
            max_downloads: "3".into(),
            template: Template::Custom,
            custom_template: "%(id)s.%(ext)s".into(),
            sub_langs: "en.*,es".into(),
            limit_rate: "2M".into(),
            fragments: "8".into(),
            sleep: "0".into(),
            live_from_start: true,
            wait_for_video: "60-3600".into(),
            ..Settings::default()
        };
        settings.save(&paths).unwrap();
        assert_eq!(Settings::load(&paths), settings);
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn unknown_keys_and_values_fall_back_to_the_defaults() {
        let settings = Settings::parse("kind=spaceship\nsomething=else\nthumbnail=maybe\n");
        assert_eq!(settings, Settings::default());
        assert_eq!(
            Settings::load(&Paths::new("/nowhere".into())),
            Settings::default()
        );
    }

    #[test]
    fn video_settings_become_the_expected_engine_options() {
        let settings = Settings {
            quality: Quality::P1080,
            container: Container::Mp4,
            metadata: true,
            ..Settings::default()
        };
        let options = settings.to_options(&paths());
        assert_eq!(options.format, Options::default().format);
        assert_eq!(options.max_resolution, Some(1080));
        assert_eq!(options.merge_output_format.as_deref(), Some("mp4"));
        assert_eq!(options.container, None);
        assert!(options.embed_metadata);
        assert!(options.extract_audio.is_none());
    }

    #[test]
    fn the_format_base_follows_the_chosen_video_format() {
        let default_format = Options::default().format;
        let mut settings = Settings::default();
        assert_eq!(settings.to_options(&paths()).format, default_format);
        settings.video_format = VideoFormat::PreMerged;
        assert_eq!(settings.to_options(&paths()).format, "best");
        settings.video_format = VideoFormat::VideoOnly;
        assert_eq!(settings.to_options(&paths()).format, "bestvideo");
        settings.video_format = VideoFormat::Custom;
        settings.custom_format = " bestvideo[vcodec^=av01]+bestaudio ".into();
        assert_eq!(
            settings.to_options(&paths()).format,
            "bestvideo[vcodec^=av01]+bestaudio"
        );
        // The resolution cap is left to the format string.
        assert_eq!(
            Settings {
                quality: Quality::P720,
                ..settings.clone()
            }
            .to_options(&paths())
            .max_resolution,
            None
        );
        // An empty one is refused rather than sent as nothing.
        settings.custom_format = String::new();
        assert_eq!(settings.to_options(&paths()).format, default_format);
        assert!(
            settings
                .problems()
                .iter()
                .any(|p| p.contains("format string can't be empty"))
        );
    }

    #[test]
    fn audio_can_be_kept_exactly_as_the_site_serves_it() {
        let settings = Settings {
            kind: Kind::AudioOnly,
            audio_convert: false,
            ..Settings::default()
        };
        let options = settings.to_options(&paths());
        assert_eq!(options.format, "bestaudio/best");
        assert!(options.extract_audio.is_none());
        // webm audio is rewrapped so the thumbnail still fits.
        assert_eq!(
            thumbnail_plan(&options),
            ThumbnailPlan::EmbedWithRewrap("webm>opus")
        );
    }

    #[test]
    fn the_history_file_lists_its_entries_in_order() {
        let dir = tempdir();
        let paths = Paths::new(dir.clone());
        let settings = Settings {
            archive: true,
            ..Settings::default()
        };
        assert_eq!(settings.history_lines(&paths), None, "no file yet");
        fs::write(
            settings.archive_path(&paths),
            "youtube a\n\nyoutube b\nyoutube c\n",
        )
        .unwrap();
        assert_eq!(
            settings.history_lines(&paths),
            Some(vec![
                "youtube a".to_string(),
                "youtube b".to_string(),
                "youtube c".to_string(),
            ])
        );
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_history_file_can_be_named_and_counted() {
        let paths = paths();
        let mut settings = Settings {
            archive: true,
            ..Settings::default()
        };
        assert_eq!(
            settings.archive_path(&paths),
            PathBuf::from("/app/download_archive.txt")
        );
        settings.archive_file = "seen.txt".into();
        assert_eq!(
            settings.archive_path(&paths),
            PathBuf::from("/app/seen.txt")
        );
        settings.archive_file = "/tmp/shared-history.txt".into();
        assert_eq!(
            settings.archive_path(&paths),
            PathBuf::from("/tmp/shared-history.txt")
        );
        assert_eq!(
            settings.to_options(&paths).archive.unwrap().file,
            "/tmp/shared-history.txt"
        );

        // Counting what is already in it, blank lines aside.
        let dir = tempdir();
        let paths = Paths::new(dir.clone());
        let settings = Settings {
            archive: true,
            ..Settings::default()
        };
        assert_eq!(settings.history_entries(&paths), None, "no file yet");
        fs::write(
            settings.archive_path(&paths),
            "youtube a
youtube b

",
        )
        .unwrap();
        assert_eq!(settings.history_entries(&paths), Some(2));
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_container_conversion_only_applies_with_a_target() {
        let mut settings = Settings {
            convert: Convert::Remux,
            ..Settings::default()
        };
        assert_eq!(settings.to_options(&paths()).container, None);
        settings.convert_target = "MP4".into();
        assert_eq!(
            settings.to_options(&paths()).container,
            Some(ContainerConversion::Remux("mp4".into()))
        );
        settings.convert = Convert::Recode;
        assert_eq!(
            settings.to_options(&paths()).container,
            Some(ContainerConversion::Recode("mp4".into()))
        );
        // Audio only discards the video stream a conversion would apply to.
        settings.kind = Kind::AudioOnly;
        assert_eq!(settings.to_options(&paths()).container, None);
    }

    #[test]
    fn audio_only_fetches_only_audio_and_converts_it() {
        let settings = Settings {
            kind: Kind::AudioOnly,
            audio: Audio::Opus,
            audio_quality: 2,
            ..Settings::default()
        };
        let options = settings.to_options(&paths());
        assert_eq!(options.format, "bestaudio/best");
        let extraction = options.extract_audio.as_ref().unwrap();
        assert_eq!(extraction.format, AudioFormat::Opus);
        assert_eq!(extraction.quality, Some(2));
        // No merge container: there is nothing to merge.
        assert_eq!(options.merge_output_format, None);
        assert_eq!(thumbnail_plan(&options), ThumbnailPlan::Embed);
    }

    #[test]
    fn lossless_formats_ignore_the_quality_scale() {
        let settings = Settings {
            kind: Kind::AudioOnly,
            audio: Audio::Flac,
            audio_quality: 9,
            ..Settings::default()
        };
        let extraction = settings.to_options(&paths()).extract_audio.unwrap();
        assert_eq!(extraction.quality, Some(0));
    }

    #[test]
    fn wav_is_the_one_audio_format_without_a_thumbnail() {
        let settings = Settings {
            kind: Kind::AudioOnly,
            audio: Audio::Wav,
            ..Settings::default()
        };
        let options = settings.to_options(&paths());
        assert_eq!(thumbnail_plan(&options), ThumbnailPlan::Skip("wav".into()));
    }

    #[test]
    fn the_thumbnail_format_can_be_left_as_it_comes() {
        let mut settings = Settings::default();
        assert_eq!(
            settings.to_options(&paths()).convert_thumbnail,
            Some(ThumbnailFormat::Jpg)
        );
        settings.thumbnail_format = Thumb::Original;
        assert_eq!(settings.to_options(&paths()).convert_thumbnail, None);
        settings.thumbnail_format = Thumb::Png;
        assert_eq!(
            settings.to_options(&paths()).convert_thumbnail,
            Some(ThumbnailFormat::Png)
        );
    }

    #[test]
    fn sponsorblock_and_subtitles_map_to_the_documented_category_lists() {
        let settings = Settings {
            sponsor: Sponsor::Mark,
            cats: Cats::Promos,
            subs: Subs::Both,
            sub_langs: " en.* ".into(),
            ..Settings::default()
        };
        let options = settings.to_options(&paths());
        assert_eq!(
            options.sponsorblock,
            SponsorBlock::Mark {
                categories: "sponsor,selfpromo,interaction".into()
            }
        );
        let subs = options.subtitles.unwrap();
        assert_eq!(subs.languages, "en.*");
        assert!(subs.write_file && subs.embed);
    }

    #[test]
    fn a_custom_category_list_is_checked_against_yt_dlps_own_list() {
        assert!(validate_categories("sponsor,intro", Sponsor::Mark).is_ok());
        assert!(validate_categories("all,-preview", Sponsor::Mark).is_ok());
        assert!(validate_categories("poi_highlight", Sponsor::Mark).is_ok());
        // Marking only, never removing.
        let error = validate_categories("poi_highlight", Sponsor::Remove).unwrap_err();
        assert!(error.contains("can only be marked"), "{error}");
        assert!(validate_categories("chapter", Sponsor::Remove).is_err());
        assert!(validate_categories("sponsr", Sponsor::Mark).is_err());
        assert!(validate_categories("", Sponsor::Mark).is_err());
        assert!(validate_categories("sponsor,,intro", Sponsor::Mark).is_err());

        let settings = Settings {
            sponsor: Sponsor::Remove,
            cats: Cats::Custom,
            custom_cats: "chapter".into(),
            ..Settings::default()
        };
        assert!(settings.problems().iter().any(|p| p.contains("marked")));
    }

    #[test]
    fn playlists_archives_and_templates_reach_the_engine() {
        let settings = Settings {
            playlist: PlaylistChoice::Whole,
            playlist_items: " 1-5 ".into(),
            playlist_reverse: true,
            organize: true,
            archive: true,
            archive_stop: true,
            max_downloads: "3".into(),
            template: Template::TitleChannel,
            ..Settings::default()
        };
        let options = settings.to_options(&paths());
        assert_eq!(
            options.playlist,
            Playlist::Whole {
                items: Some("1-5".into()),
                reverse: true
            }
        );
        assert!(options.organize_playlist_folders);
        let archive = options.archive.unwrap();
        assert_eq!(
            archive.file,
            paths()
                .app_dir
                .join(ARCHIVE_NAME)
                .to_string_lossy()
                .to_string()
        );
        assert!(archive.break_on_existing);
        assert_eq!(options.max_downloads, Some(3));
        assert_eq!(
            options.output_template,
            "%(title)s - %(uploader)s [%(id)s].%(ext)s"
        );
    }

    #[test]
    fn the_download_limit_does_not_depend_on_the_archive() {
        let settings = Settings {
            max_downloads: "2".into(),
            ..Settings::default()
        };
        assert_eq!(settings.to_options(&paths()).max_downloads, Some(2));
    }

    #[test]
    fn a_custom_template_replaces_the_preset() {
        let settings = Settings {
            template: Template::Custom,
            custom_template: " %(id)s.%(ext)s ".into(),
            ..Settings::default()
        };
        assert_eq!(
            settings.to_options(&paths()).output_template,
            "%(id)s.%(ext)s"
        );
        let empty = Settings {
            template: Template::Custom,
            ..Settings::default()
        };
        assert!(
            empty
                .problems()
                .iter()
                .any(|p| p.contains("can't be empty"))
        );
    }

    #[test]
    fn engine_knobs_keep_their_defaults_when_the_text_is_not_a_number() {
        let defaults = Options::default();
        let settings = Settings {
            fragments: "abc".into(),
            sleep: String::new(),
            ..Settings::default()
        };
        let options = settings.to_options(&paths());
        assert_eq!(options.concurrent_fragments, defaults.concurrent_fragments);
        assert_eq!(options.sleep_requests, defaults.sleep_requests);
        assert_eq!(settings.problems().len(), 2, "{:?}", settings.problems());
    }

    #[test]
    fn live_stream_settings_reach_the_engine() {
        let settings = Settings {
            live_from_start: true,
            wait_for_video: "60-3600".into(),
            ..Settings::default()
        };
        let options = settings.to_options(&paths());
        assert!(options.live_from_start);
        assert_eq!(options.wait_for_video.as_deref(), Some("60-3600"));
    }

    #[test]
    fn the_output_folder_defaults_next_to_the_application_and_expands_a_tilde() {
        let paths = paths();
        let mut settings = Settings::default();
        assert_eq!(settings.output_dir(&paths), PathBuf::from("/app/downloads"));
        settings.output_dir = "~/Videos".into();
        // HOME on Unix, USERPROFILE on Windows, whichever the app itself
        // would use (see home_dir() above).
        let home = home_dir().expect("HOME or USERPROFILE must be set to run this test");
        assert_eq!(
            settings.output_dir(&paths),
            PathBuf::from(home).join("Videos")
        );
        settings.output_dir = "~sometool/x".into();
        assert_eq!(settings.output_dir(&paths), PathBuf::from("~sometool/x"));
    }

    #[test]
    fn typed_values_are_checked_before_the_download_starts() {
        assert!(valid_rate("") && valid_rate("500K") && valid_rate("4.2M"));
        assert!(!valid_rate("fast") && !valid_rate("K") && !valid_rate("1.5MiB"));
        assert!(valid_fragments("1") && valid_fragments(" 16 "));
        assert!(!valid_fragments("0") && !valid_fragments("") && !valid_fragments("2.5"));
        assert!(valid_sleep("0") && valid_sleep("1.5"));
        assert!(!valid_sleep("-1") && !valid_sleep("soon"));
        assert!(valid_wait("") && valid_wait("60") && valid_wait("60-3600"));
        assert!(!valid_wait("60-") && !valid_wait("a-b"));
        assert!(valid_limit("") && valid_limit("5") && !valid_limit("0") && !valid_limit("x"));
        assert!(valid_items("") && valid_items("1-5,8") && valid_items("2,4,6"));
        assert!(!valid_items("one") && !valid_items("1-5,"));
        assert!(valid_container("mp4") && !valid_container("") && !valid_container("MP4!"));
        assert!(valid_url("https://www.youtube.com/watch?v=abc"));
        assert!(!valid_url("youtube.com/watch?v=abc") && !valid_url("https://"));
    }
}
