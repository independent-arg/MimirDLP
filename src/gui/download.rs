//! The Download screen: a queue of links, each previewed with real metadata
//! before it downloads.
//!
//! Adding a link queues it immediately and kicks off a background lookup
//! (`crate::metadata::fetch`, then a thumbnail fetch) that fills the card in
//! once it resolves; a lookup failure never blocks queuing, it just leaves a
//! plainer card. `Settings` (edited on the Settings page) supplies the
//! defaults a new item is seeded with and everything not shown per item
//! (subtitles, SponsorBlock, template, and so on); turning one item's
//! configuration into yt-dlp arguments is [`Settings::to_options`] plus
//! [`crate::engine::build_args`], and running it is [`crate::runner`]. Items
//! download one at a time, in order; every step yt-dlp reports lands in the
//! tagged activity log shared with the Setup screen.

use std::collections::BTreeMap;
use std::future::Future;
use std::path::Path;
use std::sync::Arc;
use std::thread;

use iced::futures::channel::{mpsc, oneshot};
use iced::widget::{
    Column, button, column, container, image, pick_list, progress_bar, row, rule, scrollable,
    space, text, text_input,
};
use iced::{Alignment, Element, Length, Task, Theme};

use super::{log as log_panel, style};
use crate::config::{Kind, Quality, Settings, valid_url};
use crate::engine::{ThumbnailPlan, thumbnail_plan};
use crate::log::Level;
use crate::metadata::{self, Metadata};
use crate::provision::Paths;
use crate::runner::{self, Cancel, Event, Finish, Progress};

/// How many activity lines are kept. Enough to see a whole run, capped so
/// an hours long recording can't grow without end.
const MAX_LOG: usize = 2000;

#[derive(Debug, Clone)]
pub enum Message {
    UrlInput(String),
    AddUrl,
    RemoveItem(u64),
    ItemKind(u64, Kind),
    ItemQuality(u64, Quality),
    MetadataFetched(u64, Result<Metadata, String>),
    ThumbnailFetched(u64, Result<Vec<u8>, String>),
    // Settings-page messages. `Settings`'s own fields are mutated directly;
    // see gui/settings.rs for where these are emitted.
    OutputDir(String),
    Kind(Kind),
    VideoFormat(crate::config::VideoFormat),
    CustomFormat(String),
    Quality(Quality),
    Audio(crate::config::Audio),
    AudioConvert(bool),
    AudioQuality(u8),
    Container(crate::config::Container),
    Convert(crate::config::Convert),
    ConvertTarget(String),
    Thumbnail(bool),
    ThumbFormat(crate::config::Thumb),
    Metadata(bool),
    Chapters(bool),
    Subs(crate::config::Subs),
    SubLangs(String),
    Sponsor(crate::config::Sponsor),
    Cats(crate::config::Cats),
    CustomCats(String),
    Keyframes(bool),
    Playlist(crate::config::PlaylistChoice),
    PlaylistItems(String),
    PlaylistReverse(bool),
    Organize(bool),
    Archive(bool),
    ArchiveFile(String),
    ArchiveStop(bool),
    ClearHistory,
    ConfirmClearHistory,
    CancelClearHistory,
    ToggleHistoryList,
    MaxDownloads(String),
    Template(crate::config::Template),
    CustomTemplate(String),
    LimitRate(String),
    Fragments(String),
    Sleep(String),
    LiveFromStart(bool),
    WaitForVideo(String),
    Verbose(bool),
    Restrict(bool),
    Mtime(bool),
    IgnoreErrors(bool),
    Start,
    Stop,
    OpenFolder,
    OpenItemFolder(String),
    Report(Event),
}

#[derive(Debug, Clone, PartialEq)]
enum MetaState {
    Fetching,
    Ready(Metadata),
    Failed(String),
}

#[derive(Debug, Clone, PartialEq)]
enum ItemStatus {
    Queued,
    Downloading(Progress),
    Completed { path: Option<String> },
    Failed(String),
}

struct QueueItem {
    id: u64,
    url: String,
    /// Per-item quick picks, seeded from `Settings` when the item is added.
    kind: Kind,
    quality: Quality,
    meta: MetaState,
    thumbnail: Option<image::Handle>,
    status: ItemStatus,
}

pub struct Download {
    paths: Paths,
    settings: Settings,
    url_input: String,
    items: Vec<QueueItem>,
    next_id: u64,
    log: Vec<(Level, String)>,
    /// One entry per parallel download of the *current* item, keyed by
    /// yt-dlp's slot number (a live stream fetches video and audio at once).
    progress: BTreeMap<u32, Progress>,
    /// Files yt-dlp reported finished for the current item.
    files: Vec<String>,
    /// The item currently downloading, if any.
    current: Option<u64>,
    /// Present while a download runs, and what stops it.
    job: Option<Arc<Cancel>>,
    /// Shown next to the button when the run can't start.
    problem: Option<String>,
    /// Waiting for the history file to be cleared on purpose.
    clearing_history: bool,
    /// How many videos the history holds, when it has been looked at.
    history: Option<usize>,
    /// The history's own entries, shown one by one while this is `Some`.
    history_list: Option<Vec<String>>,
    /// The last line as it arrived, and how often it has repeated, so a
    /// line yt-dlp keeps printing is collapsed instead of filling the log.
    last: Option<(Level, String)>,
    repeats: u32,
}

impl Download {
    pub fn new(paths: Paths, settings: Settings) -> Self {
        Download {
            paths,
            settings,
            url_input: String::new(),
            items: Vec::new(),
            next_id: 0,
            log: Vec::new(),
            progress: BTreeMap::new(),
            files: Vec::new(),
            current: None,
            job: None,
            problem: None,
            clearing_history: false,
            history: None,
            history_list: None,
            last: None,
            repeats: 0,
        }
    }

    pub fn running(&self) -> bool {
        self.job.is_some()
    }

    pub fn settings(&self) -> &Settings {
        &self.settings
    }

    pub fn paths(&self) -> &Paths {
        &self.paths
    }

    /// An app-wide preference kept in the same settings file for simplicity;
    /// read and set from the top bar, not this screen.
    pub fn theme(&self) -> crate::config::ThemePref {
        self.settings.theme
    }

    pub fn set_theme(&mut self, pref: crate::config::ThemePref) {
        self.settings.theme = pref;
        self.save();
    }

    /// The extra context `gui::settings`'s history section needs, that
    /// isn't part of `Settings` itself.
    pub fn history_view(&self) -> super::settings::History<'_> {
        super::settings::History {
            count: self.history,
            entries: self.history_list.as_deref(),
            confirming_clear: self.clearing_history,
        }
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        // Settings are remembered as soon as they change, except while
        // typing, which is saved when the download starts.
        let mut remember = true;
        let settings = &mut self.settings;
        match message {
            Message::UrlInput(value) => {
                self.url_input = value;
                remember = false;
            }
            Message::AddUrl => return self.add_url(),
            Message::RemoveItem(id) => {
                remember = false;
                self.items
                    .retain(|i| i.id != id || matches!(i.status, ItemStatus::Downloading(_)));
            }
            Message::ItemKind(id, value) => {
                remember = false;
                if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
                    item.kind = value;
                }
            }
            Message::ItemQuality(id, value) => {
                remember = false;
                if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
                    item.quality = value;
                }
            }
            Message::MetadataFetched(id, result) => {
                remember = false;
                let mut thumbnail_url = None;
                if let Some(item) = self.items.iter_mut().find(|i| i.id == id) {
                    item.meta = match result {
                        Ok(meta) => {
                            thumbnail_url = meta.thumbnail_url.clone();
                            MetaState::Ready(meta)
                        }
                        Err(e) => MetaState::Failed(e),
                    };
                }
                if let Some(url) = thumbnail_url {
                    return Task::perform(fetch_thumbnail(url), move |result| {
                        Message::ThumbnailFetched(id, result)
                    });
                }
            }
            Message::ThumbnailFetched(id, result) => {
                remember = false;
                if let Ok(bytes) = result
                    && let Some(item) = self.items.iter_mut().find(|i| i.id == id)
                {
                    item.thumbnail = Some(image::Handle::from_bytes(bytes));
                }
            }
            // Typed values: saved when the download starts.
            Message::OutputDir(value) => {
                settings.output_dir = value;
                remember = false;
            }
            Message::ConvertTarget(value) => {
                settings.convert_target = value;
                remember = false;
            }
            Message::SubLangs(value) => {
                settings.sub_langs = value;
                remember = false;
            }
            Message::CustomCats(value) => {
                settings.custom_cats = value;
                remember = false;
            }
            Message::PlaylistItems(value) => {
                settings.playlist_items = value;
                remember = false;
            }
            Message::MaxDownloads(value) => {
                settings.max_downloads = value;
                remember = false;
            }
            Message::CustomTemplate(value) => {
                settings.custom_template = value;
                remember = false;
            }
            Message::LimitRate(value) => {
                settings.limit_rate = value;
                remember = false;
            }
            Message::Fragments(value) => {
                settings.fragments = value;
                remember = false;
            }
            Message::Sleep(value) => {
                settings.sleep = value;
                remember = false;
            }
            Message::WaitForVideo(value) => {
                settings.wait_for_video = value;
                remember = false;
            }
            Message::CustomFormat(value) => {
                settings.custom_format = value;
                remember = false;
            }
            Message::ArchiveFile(value) => {
                settings.archive_file = value;
                remember = false;
            }
            Message::Kind(value) => settings.kind = value,
            Message::VideoFormat(value) => settings.video_format = value,
            Message::AudioConvert(value) => settings.audio_convert = value,
            Message::ClearHistory => {
                remember = false;
                self.clearing_history = true;
            }
            Message::CancelClearHistory => {
                remember = false;
                self.clearing_history = false;
            }
            Message::ConfirmClearHistory => {
                remember = false;
                self.clearing_history = false;
                let entries = self.settings.history_entries(&self.paths).unwrap_or(0);
                let path = self.settings.archive_path(&self.paths);
                let note = match std::fs::remove_file(&path) {
                    Ok(()) => (Level::Ok, format!("History cleared ({entries} entries).")),
                    Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                        (Level::Info, "There is no history file yet.".into())
                    }
                    Err(e) => (Level::Warn, format!("Could not clear the history: {e}")),
                };
                self.note(note);
                self.history = None;
                self.history_list = None;
            }
            Message::ToggleHistoryList => {
                remember = false;
                self.history_list = if self.history_list.is_some() {
                    None
                } else {
                    self.settings.history_lines(&self.paths)
                };
            }
            Message::Quality(value) => settings.quality = value,
            Message::Audio(value) => settings.audio = value,
            Message::AudioQuality(value) => settings.audio_quality = value,
            Message::Container(value) => settings.container = value,
            Message::Convert(value) => settings.convert = value,
            Message::Thumbnail(value) => settings.thumbnail = value,
            Message::ThumbFormat(value) => settings.thumbnail_format = value,
            Message::Metadata(value) => settings.metadata = value,
            Message::Chapters(value) => settings.chapters = value,
            Message::Subs(value) => settings.subs = value,
            Message::Sponsor(value) => settings.sponsor = value,
            Message::Cats(value) => settings.cats = value,
            Message::Keyframes(value) => settings.keyframes = value,
            Message::Playlist(value) => settings.playlist = value,
            Message::PlaylistReverse(value) => settings.playlist_reverse = value,
            Message::Organize(value) => settings.organize = value,
            Message::Archive(value) => settings.archive = value,
            Message::ArchiveStop(value) => settings.archive_stop = value,
            Message::Template(value) => settings.template = value,
            Message::LiveFromStart(value) => settings.live_from_start = value,
            Message::Verbose(value) => settings.verbose = value,
            Message::Restrict(value) => settings.restrict_filenames = value,
            Message::Mtime(value) => settings.preserve_mtime = value,
            Message::IgnoreErrors(value) => settings.ignore_errors = value,
            Message::Start => return self.start(),
            Message::Stop => {
                if let Some(cancel) = self.job.clone() {
                    self.note((Level::Warn, "Stopping the download...".into()));
                    cancel.cancel();
                }
                return Task::none();
            }
            Message::OpenFolder => {
                remember = false;
                let dir = self.settings.output_dir(&self.paths);
                if !dir.is_dir() {
                    self.note((Level::Info, "The output folder doesn't exist yet.".into()));
                } else if let Err(e) = open_folder(&dir) {
                    self.note((Level::Warn, format!("Could not open the folder: {e}")));
                }
            }
            Message::OpenItemFolder(path) => {
                remember = false;
                if let Err(e) = reveal_file(Path::new(&path)) {
                    self.note((Level::Warn, format!("Could not open the folder: {e}")));
                }
            }
            Message::Report(event) => return self.report(event),
        }
        if remember {
            // Cheap, and it keeps the history count (and the open entry
            // list, if any) honest after the file name or the toggle changes.
            self.history = self
                .settings
                .archive
                .then(|| self.settings.history_entries(&self.paths))
                .flatten();
            if self.history_list.is_some() {
                self.history_list = self.settings.history_lines(&self.paths);
            }
            self.save();
        }
        Task::none()
    }

    /// Adds one line to the activity log, keeping only the last
    /// [`MAX_LOG`] of them. A live recording can run for hours, and an
    /// unbounded log would grow (and slow the window down) with it.
    fn note(&mut self, entry: (Level, String)) {
        // A live recording repeats the same line over and over (yt-dlp
        // sleeps between requests and says so every time), so repeats are
        // counted on one line instead of pushing the rest out of view.
        if self.last.as_ref() == Some(&entry)
            && let Some(last) = self.log.last_mut()
        {
            self.repeats += 1;
            *last = (entry.0, format!("{}  (x{})", entry.1, self.repeats + 1));
            return;
        }
        self.last = Some(entry.clone());
        self.repeats = 0;
        self.log.push(entry);
        if self.log.len() > MAX_LOG {
            self.log.drain(..self.log.len() - MAX_LOG);
        }
    }

    fn save(&mut self) {
        if let Err(e) = self.settings.save(&self.paths) {
            self.note((Level::Warn, format!("Could not save the settings: {e}")));
        }
    }

    /// Validates and queues the pasted link, then kicks off its metadata
    /// lookup. A lookup failure (handled in `MetadataFetched`) never removes
    /// the item; it just leaves a plainer card.
    fn add_url(&mut self) -> Task<Message> {
        let url = self.url_input.trim().to_string();
        if !valid_url(&url) {
            self.problem =
                Some("That doesn't look like a link (it should start with http).".into());
            return Task::none();
        }
        self.problem = None;
        self.url_input.clear();
        if self.items.iter().any(|i| i.url == url) {
            return Task::none();
        }

        let id = self.next_id;
        self.next_id += 1;
        self.items.push(QueueItem {
            id,
            url: url.clone(),
            kind: self.settings.kind,
            quality: self.settings.quality,
            meta: MetaState::Fetching,
            thumbnail: None,
            status: ItemStatus::Queued,
        });

        let paths = self.paths.clone();
        Task::perform(fetch_metadata(paths, url), move |result| {
            Message::MetadataFetched(id, result)
        })
    }

    fn start(&mut self) -> Task<Message> {
        if self.running() {
            return Task::none();
        }
        if !self.items.iter().any(|i| i.status == ItemStatus::Queued) {
            self.problem = Some("Add at least one link first.".into());
            return Task::none();
        }
        self.problem = None;
        self.save();
        self.run_next()
    }

    /// Starts the next `Queued` item. An item whose per-item configuration
    /// turns out to be invalid (checked against *its own* kind/quality, not
    /// just the global defaults) is marked `Failed` and skipped, so one bad
    /// item doesn't block the rest of the queue. Does nothing if a download
    /// is already running or nothing is left to start.
    fn run_next(&mut self) -> Task<Message> {
        if self.running() {
            return Task::none();
        }
        loop {
            let Some(index) = self
                .items
                .iter()
                .position(|i| i.status == ItemStatus::Queued)
            else {
                return Task::none();
            };

            let mut item_settings = self.settings.clone();
            item_settings.kind = self.items[index].kind;
            item_settings.quality = self.items[index].quality;
            if let Some(problem) = item_settings.problems().into_iter().next() {
                self.items[index].status = ItemStatus::Failed(problem.clone());
                let url = self.items[index].url.clone();
                self.note((Level::Error, format!("{url}: {problem}")));
                continue;
            }

            let id = self.items[index].id;
            let url = self.items[index].url.clone();
            let options = item_settings.to_options(&self.paths);
            let output_dir = self.settings.output_dir(&self.paths);
            let paths = self.paths.clone();
            let cancel = Arc::new(Cancel::default());
            self.job = Some(cancel.clone());
            self.current = Some(id);
            self.items[index].status = ItemStatus::Downloading(Progress::default());
            self.progress.clear();
            self.files.clear();

            if !self.log.is_empty() {
                self.note((Level::Info, String::new()));
            }
            self.note((Level::Download, url.clone()));
            self.note((
                Level::Info,
                format!("Saving into: {}", output_dir.display()),
            ));
            if let ThumbnailPlan::Skip(ext) = thumbnail_plan(&options)
                && options.embed_thumbnail
            {
                self.note((
                    Level::Info,
                    format!("No thumbnail: a .{ext} file can't hold one."),
                ));
            }

            let (sender, receiver) = mpsc::unbounded();
            let urls = vec![url];
            thread::spawn(move || {
                runner::run(
                    &paths,
                    &options,
                    Some(&output_dir),
                    &urls,
                    &cancel,
                    &mut |event| {
                        let _ = sender.unbounded_send(event);
                    },
                );
            });
            return Task::run(receiver, Message::Report);
        }
    }

    fn report(&mut self, event: Event) -> Task<Message> {
        match event {
            Event::Log(level, line) => self.note((level, line)),
            Event::Progress(progress) => {
                self.progress.insert(progress.stream, progress);
                let combined = combined(&self.progress);
                if let Some(id) = self.current
                    && let Some(item) = self.items.iter_mut().find(|i| i.id == id)
                {
                    item.status = ItemStatus::Downloading(combined);
                }
            }
            Event::File(path) => {
                self.note((Level::Ok, format!("Saved: {path}")));
                self.files.push(path.clone());
                if let Some(id) = self.current
                    && let Some(item) = self.items.iter_mut().find(|i| i.id == id)
                {
                    item.status = ItemStatus::Completed { path: Some(path) };
                }
            }
            Event::Finished(finish) => {
                self.job = None;
                self.progress.clear();
                // A finished run may have added to the archive.
                self.history = self
                    .settings
                    .archive
                    .then(|| self.settings.history_entries(&self.paths))
                    .flatten();
                if self.history_list.is_some() {
                    self.history_list = self.settings.history_lines(&self.paths);
                }
                let id = self.current.take();
                let (level, message) = match &finish {
                    Finish::Completed => (
                        Level::Success,
                        match self.files.len() {
                            0 => "Finished, with nothing new to download".to_string(),
                            1 => "Download completed".to_string(),
                            n => format!("Download completed ({n} files)"),
                        },
                    ),
                    Finish::StoppedAsConfigured => (
                        Level::Success,
                        "Stopped as configured (the limit or the history said so)".into(),
                    ),
                    Finish::Cancelled => (
                        Level::Warn,
                        "Download cancelled. A partial file was kept, so starting again resumes it."
                            .into(),
                    ),
                    Finish::Failed(reason) => (Level::Error, reason.clone()),
                };
                self.note((level, message));
                if let Some(id) = id
                    && let Some(item) = self.items.iter_mut().find(|i| i.id == id)
                {
                    match &finish {
                        // The .part file is kept, so the item goes back to
                        // Queued: starting again resumes it.
                        Finish::Cancelled => item.status = ItemStatus::Queued,
                        Finish::Failed(reason) => item.status = ItemStatus::Failed(reason.clone()),
                        _ if !matches!(item.status, ItemStatus::Completed { .. }) => {
                            item.status = ItemStatus::Completed { path: None };
                        }
                        _ => {}
                    }
                }
                if !matches!(finish, Finish::Cancelled) {
                    return self.run_next();
                }
            }
        }
        Task::none()
    }

    pub fn view(&self) -> Element<'_, Message> {
        let top_bar = row![
            text_input("Paste a video or playlist link...", &self.url_input)
                .on_input(Message::UrlInput)
                .on_submit(Message::AddUrl)
                .padding(10),
            button(text("Add"))
                .padding([10, 22])
                .style(style::pill)
                .on_press(Message::AddUrl),
        ]
        .spacing(10)
        .align_y(Alignment::Center);

        let queue: Element<'_, Message> = if self.items.is_empty() {
            container(
                column![
                    text("Your queue is empty.").size(16),
                    text("Paste a link above to get started.")
                        .size(13)
                        .style(|t: &Theme| text::Style {
                            color: Some(style::muted(t))
                        }),
                ]
                .spacing(6)
                .align_x(Alignment::Center),
            )
            .center(Length::Fill)
            .into()
        } else {
            scrollable(
                Column::with_children(self.items.iter().map(item_card))
                    .spacing(10)
                    .width(Length::Fill),
            )
            .height(Length::Fill)
            .into()
        };

        let mut footer = column![self.actions(), self.queue_progress()].spacing(12);
        footer = footer.push(
            column![
                text("Activity").size(15),
                log_panel::panel(
                    &self.log,
                    "Nothing yet. Every step of the download will be listed here.",
                    Length::Fixed(150.0),
                ),
            ]
            .spacing(8),
        );

        column![
            container(top_bar).padding([16, 24]),
            container(queue)
                .padding(iced::Padding {
                    top: 0.0,
                    right: 24.0,
                    bottom: 12.0,
                    left: 24.0,
                })
                .height(Length::Fill),
            rule::horizontal(1),
            container(container(footer.max_width(900)).center_x(Length::Fill)).padding([12, 24]),
        ]
        .into()
    }

    /// A thin aggregate bar across the whole queue, the way a batch of
    /// downloads reports overall progress once each item has its own.
    fn queue_progress(&self) -> Element<'_, Message> {
        let total = self.items.len();
        if total == 0 {
            return space::vertical().height(0).into();
        }
        let done = self
            .items
            .iter()
            .filter(|i| {
                matches!(
                    i.status,
                    ItemStatus::Completed { .. } | ItemStatus::Failed(_)
                )
            })
            .count();
        let summary = if self.running() {
            format!("Downloading queue: {done} of {total} done")
        } else if done == total {
            "All done.".to_string()
        } else {
            format!("Ready: {done} of {total} done")
        };
        column![
            text(summary).size(13).style(|t: &Theme| text::Style {
                color: Some(style::muted(t))
            }),
            progress_bar(0.0..=1.0, done as f32 / total as f32)
                .girth(6)
                .style(style::progress),
        ]
        .spacing(6)
        .into()
    }

    fn actions(&self) -> Element<'_, Message> {
        let button = if self.running() {
            button(text("Stop"))
                .padding([10, 28])
                .style(style::pill_danger)
                .on_press(Message::Stop)
        } else {
            button(text("Download"))
                .padding([10, 28])
                .style(style::pill)
                .on_press(Message::Start)
        };
        let open_folder = iced::widget::button(text("Open folder"))
            .padding([10, 20])
            .style(style::pill_secondary)
            .on_press(Message::OpenFolder);
        let mut actions = row![button, open_folder]
            .spacing(12)
            .align_y(Alignment::Center);
        if let Some(problem) = &self.problem {
            actions = actions.push(text(problem).style(text::danger));
        } else if self.running() {
            actions = actions.push(text("Working...").style(|t: &Theme| text::Style {
                color: Some(style::muted(t)),
            }));
        }
        actions.into()
    }
}

/// One card in the queue: a thumbnail (or a placeholder while it loads or
/// on a failed lookup), the title, the two quick per-item pickers, whatever
/// metadata resolved, and this item's own status/progress.
fn item_card(item: &QueueItem) -> Element<'_, Message> {
    let id = item.id;
    let thumb: Element<'_, Message> = match &item.thumbnail {
        Some(handle) => image(handle.clone())
            .width(120)
            .height(68)
            .content_fit(iced::ContentFit::Cover)
            .into(),
        None => {
            let glyph = if matches!(item.meta, MetaState::Fetching) {
                "..."
            } else {
                "?"
            };
            container(text(glyph).size(20).style(|t: &Theme| text::Style {
                color: Some(style::muted(t)),
            }))
            .width(120)
            .height(68)
            .center(Length::Fill)
            .style(style::card)
            .into()
        }
    };

    let title = match &item.meta {
        MetaState::Ready(meta) if !meta.title.is_empty() => meta.title.clone(),
        _ => item.url.clone(),
    };

    let details: Element<'_, Message> = match &item.meta {
        MetaState::Fetching => text("Looking up this link...")
            .size(13)
            .style(|t: &Theme| text::Style {
                color: Some(style::muted(t)),
            })
            .into(),
        MetaState::Failed(e) => text(format!("Could not look this link up ({e})"))
            .size(13)
            .style(text::warning)
            .into(),
        MetaState::Ready(meta) => {
            let mut parts = Vec::new();
            if let Some(seconds) = meta.duration {
                parts.push(duration(seconds.max(0.0).round() as u64));
            }
            if let Some(bytes) = meta.approx_size {
                parts.push(format!("~{:.1} MB", bytes as f64 / 1_000_000.0));
            }
            if parts.is_empty() {
                space::vertical().height(0).into()
            } else {
                text(parts.join("   "))
                    .size(13)
                    .style(|t: &Theme| text::Style {
                        color: Some(style::muted(t)),
                    })
                    .into()
            }
        }
    };

    let picks = row![
        pick_list(Kind::ALL, Some(item.kind), move |k| Message::ItemKind(
            id, k
        )),
        pick_list(Quality::ALL, Some(item.quality), move |q| {
            Message::ItemQuality(id, q)
        }),
    ]
    .spacing(8);

    let status_row: Element<'_, Message> = match &item.status {
        ItemStatus::Queued => text("Queued")
            .size(13)
            .style(|t: &Theme| text::Style {
                color: Some(style::muted(t)),
            })
            .into(),
        ItemStatus::Downloading(progress) => progress_view(*progress),
        ItemStatus::Completed { path } => {
            let mut row = row![text("Completed").size(13).style(text::success)]
                .spacing(10)
                .align_y(Alignment::Center);
            if let Some(path) = path {
                row = row.push(
                    button(text("Open folder").size(13))
                        .padding([4, 12])
                        .style(style::pill_secondary)
                        .on_press(Message::OpenItemFolder(path.clone())),
                );
            }
            row.into()
        }
        ItemStatus::Failed(reason) => text(format!("Failed: {reason}"))
            .size(13)
            .style(text::danger)
            .into(),
    };

    let removable = !matches!(item.status, ItemStatus::Downloading(_));
    let remove = button(text("Remove").size(13))
        .padding([4, 12])
        .style(style::pill_secondary)
        .on_press_maybe(removable.then_some(Message::RemoveItem(id)));

    container(
        row![
            thumb,
            column![text(title).size(14), picks, details, status_row].spacing(6),
            space::horizontal(),
            remove,
        ]
        .spacing(14)
        .align_y(Alignment::Center),
    )
    .padding(12)
    .width(Length::Fill)
    .style(style::card)
    .into()
}

/// Runs `metadata::fetch` on a worker thread and resolves once it returns,
/// the same one-shot idiom `gui::wait_on_a_thread` uses for a plain delay:
/// a blocking call has no business running on the iced executor.
fn fetch_metadata(paths: Paths, url: String) -> impl Future<Output = Result<Metadata, String>> {
    let (tx, rx) = oneshot::channel();
    thread::spawn(move || {
        let _ = tx.send(metadata::fetch(&paths, &url));
    });
    async move {
        rx.await
            .unwrap_or_else(|_| Err("the lookup was interrupted".into()))
    }
}

fn fetch_thumbnail(url: String) -> impl Future<Output = Result<Vec<u8>, String>> {
    let (tx, rx) = oneshot::channel();
    thread::spawn(move || {
        let _ = tx.send(metadata::fetch_thumbnail(&url));
    });
    async move {
        rx.await
            .unwrap_or_else(|_| Err("the thumbnail fetch was interrupted".into()))
    }
}

/// Opens `dir` in the system's file manager. `explorer.exe` on Windows is
/// known to return a non-zero exit status even on success, so only the
/// spawn itself is checked, never the exit code.
#[cfg(windows)]
fn open_folder(dir: &Path) -> std::io::Result<()> {
    std::process::Command::new("explorer").arg(dir).spawn()?;
    Ok(())
}

#[cfg(unix)]
fn open_folder(dir: &Path) -> std::io::Result<()> {
    std::process::Command::new("xdg-open").arg(dir).spawn()?;
    Ok(())
}

/// Opens the folder containing `path`, highlighting that file specifically
/// where the platform supports it.
#[cfg(windows)]
fn reveal_file(path: &Path) -> std::io::Result<()> {
    let mut arg = std::ffi::OsString::from("/select,");
    arg.push(path);
    std::process::Command::new("explorer").arg(arg).spawn()?;
    Ok(())
}

#[cfg(unix)]
fn reveal_file(path: &Path) -> std::io::Result<()> {
    open_folder(path.parent().unwrap_or(path))
}

/// A live stream fetches video and audio at the same time, so yt-dlp
/// reports two progress lines. They belong to one download as far as the
/// person watching is concerned, so they are added up.
fn combined(streams: &BTreeMap<u32, Progress>) -> Progress {
    let speed: f64 = streams.values().filter_map(|p| p.speed).sum();
    Progress {
        stream: 0,
        downloaded: streams.values().map(|p| p.downloaded).sum(),
        // Unknown as soon as one part doesn't know its size, which is the
        // usual case for a live stream.
        total: streams.values().map(|p| p.total).sum::<Option<u64>>(),
        speed: (speed > 0.0).then_some(speed),
        // 0 means "no idea" here, not "about to finish".
        eta: streams
            .values()
            .filter_map(|p| p.eta)
            .filter(|e| *e > 0)
            .max(),
        done: streams.values().all(|p| p.done),
    }
}

fn progress_view(progress: Progress) -> Element<'static, Message> {
    let mb = |bytes: u64| bytes as f64 / 1_000_000.0;
    let (amount, fraction) = match progress.total {
        Some(total) if total > 0 => {
            let fraction = (progress.downloaded as f64 / total as f64).min(1.0);
            (
                format!(
                    "{:.1} / {:.1} MB  ({:.0}%)",
                    mb(progress.downloaded),
                    mb(total),
                    fraction * 100.0
                ),
                fraction as f32,
            )
        }
        _ => (format!("{:.1} MB", mb(progress.downloaded)), 0.0),
    };
    let mut details = amount;
    if let Some(speed) = progress.speed {
        details.push_str(&format!("  at {:.1} MB/s", mb(speed as u64)));
    }
    if let Some(eta) = progress.eta.filter(|_| !progress.done) {
        details.push_str(&format!("  {} left", duration(eta)));
    }
    column![
        row![
            text(if progress.done {
                "Processing"
            } else {
                "Downloading"
            })
            .size(13),
            space::horizontal(),
            text(details).size(13).style(|t: &Theme| text::Style {
                color: Some(style::muted(t))
            }),
        ]
        .align_y(Alignment::Center),
        progress_bar(0.0..=1.0, fraction)
            .girth(8)
            .style(style::progress),
    ]
    .spacing(8)
    .into()
}

fn duration(seconds: u64) -> String {
    match seconds {
        s if s < 60 => format!("{s}s"),
        s if s < 3600 => format!("{}m {:02}s", s / 60, s % 60),
        s => format!("{}h {:02}m", s / 3600, (s % 3600) / 60),
    }
}

/// Debug builds only: `YTP_DEV_DOWNLOAD=<link>` starts with that link
/// already typed in, so a real download can be checked without clicking.
/// Release builds ignore it.
/// A comma-separated list queues more than one link, to check that the
/// queue moves on to the next item by itself once one finishes.
pub(super) fn dev_urls() -> Vec<String> {
    if !cfg!(debug_assertions) {
        return Vec::new();
    }
    let Ok(value) = std::env::var("YTP_DEV_DOWNLOAD") else {
        return Vec::new();
    };
    value
        .split(',')
        .map(str::trim)
        .filter(|url| valid_url(url))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tempdir;

    #[test]
    fn parallel_downloads_are_shown_as_one() {
        let streams = BTreeMap::from([
            (
                1,
                Progress {
                    stream: 1,
                    downloaded: 20_000,
                    total: Some(50_000),
                    speed: Some(1_000.0),
                    eta: Some(30),
                    done: false,
                },
            ),
            (
                2,
                Progress {
                    stream: 2,
                    downloaded: 5_000,
                    // A live stream doesn't know its size.
                    total: None,
                    speed: Some(500.0),
                    eta: Some(0),
                    done: false,
                },
            ),
        ]);
        let all = combined(&streams);
        assert_eq!(all.downloaded, 25_000);
        assert_eq!(all.total, None, "one unknown size makes the whole unknown");
        assert_eq!(all.speed, Some(1_500.0));
        assert_eq!(all.eta, Some(30), "a zero eta means unknown, not now");
        assert!(!all.done);
    }

    #[test]
    fn clearing_the_history_asks_first_and_says_how_much_went() {
        let dir = tempdir();
        let paths = Paths::new(dir.clone());
        let settings = Settings {
            archive: true,
            ..Settings::default()
        };
        let history = settings.archive_path(&paths);
        std::fs::write(&history, "youtube a\nyoutube b\n").unwrap();
        let mut screen = Download::new(paths, settings);

        let _ = screen.update(Message::Archive(true));
        assert_eq!(screen.history, Some(2), "the count comes from the file");
        let _ = screen.update(Message::ClearHistory);
        assert!(history.exists(), "nothing goes until it is confirmed");
        let _ = screen.update(Message::CancelClearHistory);
        assert!(history.exists());
        let _ = screen.update(Message::ClearHistory);
        let _ = screen.update(Message::ConfirmClearHistory);
        assert!(!history.exists());
        assert!(
            screen.log.last().unwrap().1.contains("(2 entries)"),
            "{:?}",
            screen.log
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_history_entries_can_be_shown_one_by_one_and_hidden_again() {
        let dir = tempdir();
        let paths = Paths::new(dir.clone());
        let settings = Settings {
            archive: true,
            ..Settings::default()
        };
        let history = settings.archive_path(&paths);
        std::fs::write(&history, "youtube a\nyoutube b\n").unwrap();
        let mut screen = Download::new(paths, settings);
        let _ = screen.update(Message::Archive(true));

        assert_eq!(screen.history_list, None, "closed until asked to open");
        let _ = screen.update(Message::ToggleHistoryList);
        assert_eq!(
            screen.history_list,
            Some(vec!["youtube a".to_string(), "youtube b".to_string()])
        );
        let _ = screen.update(Message::ToggleHistoryList);
        assert_eq!(screen.history_list, None, "toggles back off");

        // Clearing the history closes an open list rather than leaving it
        // pointing at a file that no longer exists.
        let _ = screen.update(Message::ToggleHistoryList);
        let _ = screen.update(Message::ClearHistory);
        let _ = screen.update(Message::ConfirmClearHistory);
        assert_eq!(screen.history_list, None);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_line_yt_dlp_keeps_repeating_is_collapsed() {
        let dir = tempdir();
        let mut screen = Download::new(Paths::new(dir.clone()), Settings::default());
        for _ in 0..3 {
            screen.note((Level::Info, "[youtube] Sleeping 1.5 seconds ...".into()));
        }
        screen.note((Level::Info, "[download] Destination: /tmp/v.mkv".into()));
        assert_eq!(screen.log.len(), 2);
        assert_eq!(screen.log[0].1, "[youtube] Sleeping 1.5 seconds ...  (x3)");
        assert_eq!(screen.log[1].1, "[download] Destination: /tmp/v.mkv");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_activity_log_keeps_only_the_most_recent_lines() {
        let dir = tempdir();
        let mut screen = Download::new(Paths::new(dir.clone()), Settings::default());
        for n in 0..MAX_LOG + 50 {
            screen.note((Level::Info, format!("line {n}")));
        }
        assert_eq!(screen.log.len(), MAX_LOG);
        // The newest line survives, the oldest ones are gone.
        assert_eq!(
            screen.log.last().unwrap().1,
            format!("line {}", MAX_LOG + 49)
        );
        assert_eq!(screen.log.first().unwrap().1, "line 50");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn adding_a_url_queues_it_and_starts_a_metadata_lookup() {
        let dir = tempdir();
        let mut screen = Download::new(Paths::new(dir.clone()), Settings::default());
        let _ = screen.update(Message::UrlInput("https://example.com/video".into()));
        let _ = screen.update(Message::AddUrl);
        assert_eq!(screen.items.len(), 1);
        assert_eq!(screen.items[0].url, "https://example.com/video");
        assert_eq!(screen.items[0].status, ItemStatus::Queued);
        assert_eq!(screen.items[0].meta, MetaState::Fetching);
        assert_eq!(screen.url_input, "", "the input clears once queued");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn an_invalid_url_is_rejected_without_touching_the_queue() {
        let dir = tempdir();
        let mut screen = Download::new(Paths::new(dir.clone()), Settings::default());
        let _ = screen.update(Message::UrlInput("not a link".into()));
        let _ = screen.update(Message::AddUrl);
        assert!(screen.items.is_empty());
        assert!(screen.problem.is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_duplicate_url_is_not_queued_twice() {
        let dir = tempdir();
        let mut screen = Download::new(Paths::new(dir.clone()), Settings::default());
        let _ = screen.update(Message::UrlInput("https://example.com/video".into()));
        let _ = screen.update(Message::AddUrl);
        let _ = screen.update(Message::UrlInput("https://example.com/video".into()));
        let _ = screen.update(Message::AddUrl);
        assert_eq!(screen.items.len(), 1);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn metadata_and_a_thumbnail_fill_the_card_in_once_they_resolve() {
        let dir = tempdir();
        let mut screen = Download::new(Paths::new(dir.clone()), Settings::default());
        let _ = screen.update(Message::UrlInput("https://example.com/video".into()));
        let _ = screen.update(Message::AddUrl);
        let id = screen.items[0].id;

        let meta = Metadata {
            title: "A video".into(),
            thumbnail_url: Some("https://example.com/thumb.jpg".into()),
            duration: Some(90.0),
            approx_size: Some(5_000_000),
        };
        let _ = screen.update(Message::MetadataFetched(id, Ok(meta.clone())));
        assert_eq!(screen.items[0].meta, MetaState::Ready(meta));

        let _ = screen.update(Message::ThumbnailFetched(id, Ok(vec![1, 2, 3])));
        assert!(screen.items[0].thumbnail.is_some());
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_failed_lookup_leaves_the_item_queued_with_a_plainer_card() {
        let dir = tempdir();
        let mut screen = Download::new(Paths::new(dir.clone()), Settings::default());
        let _ = screen.update(Message::UrlInput("https://example.com/video".into()));
        let _ = screen.update(Message::AddUrl);
        let id = screen.items[0].id;

        let _ = screen.update(Message::MetadataFetched(id, Err("network error".into())));
        assert_eq!(
            screen.items[0].meta,
            MetaState::Failed("network error".into())
        );
        assert_eq!(
            screen.items[0].status,
            ItemStatus::Queued,
            "still queueable"
        );
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn removing_an_item_drops_it_from_the_queue() {
        let dir = tempdir();
        let mut screen = Download::new(Paths::new(dir.clone()), Settings::default());
        let _ = screen.update(Message::UrlInput("https://example.com/a".into()));
        let _ = screen.update(Message::AddUrl);
        let _ = screen.update(Message::UrlInput("https://example.com/b".into()));
        let _ = screen.update(Message::AddUrl);
        let first_id = screen.items[0].id;

        let _ = screen.update(Message::RemoveItem(first_id));
        assert_eq!(screen.items.len(), 1);
        assert_eq!(screen.items[0].url, "https://example.com/b");
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn per_item_kind_and_quality_can_be_changed_independently() {
        let dir = tempdir();
        let mut screen = Download::new(Paths::new(dir.clone()), Settings::default());
        let _ = screen.update(Message::UrlInput("https://example.com/a".into()));
        let _ = screen.update(Message::AddUrl);
        let _ = screen.update(Message::UrlInput("https://example.com/b".into()));
        let _ = screen.update(Message::AddUrl);
        let a = screen.items[0].id;

        let _ = screen.update(Message::ItemKind(a, Kind::AudioOnly));
        let _ = screen.update(Message::ItemQuality(a, Quality::P720));
        assert_eq!(screen.items[0].kind, Kind::AudioOnly);
        assert_eq!(screen.items[0].quality, Quality::P720);
        // The other item is untouched.
        assert_eq!(screen.items[1].kind, Kind::VideoAudio);
        std::fs::remove_dir_all(dir).unwrap();
    }
}
