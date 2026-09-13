//! The Settings page: everything that used to sit on the Download screen's
//! big form, now acting as **defaults** applied to a queue item when it is
//! added (kind, quality and everything else here), rather than per-item
//! state. `config::Settings`, `Settings::to_options` and `Settings::problems`
//! are untouched by this split; only which page renders which field moved.

use iced::widget::{
    Column, button, column, container, pick_list, radio, row, scrollable, text, text_input, toggler,
};
use iced::{Alignment, Element, Font, Length, Theme};

use super::download::Message;
use super::{card, choice, style};
use crate::config::{
    Audio, Cats, Container, Convert, Kind, PlaylistChoice, Quality, Settings, Sponsor, Subs,
    Template, Thumb, VideoFormat,
};
use crate::provision::Paths;

/// yt-dlp's `--audio-quality` scale.
const AUDIO_QUALITIES: &[u8] = &[0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10];

/// Context `history_row` needs that isn't part of `Settings` itself: the
/// live state of the Download screen's history file.
pub struct History<'a> {
    /// How many videos the history holds, when it has been looked at.
    pub count: Option<usize>,
    /// The history's own entries, shown one by one while this is `Some`.
    pub entries: Option<&'a [String]>,
    pub confirming_clear: bool,
}

pub fn view<'a>(
    settings: &'a Settings,
    paths: &'a Paths,
    history: History<'a>,
) -> Element<'a, Message> {
    column![
        text("Settings").size(26),
        text("These apply as defaults whenever a new link is queued.").style(|t: &Theme| {
            text::Style {
                color: Some(style::muted(t)),
            }
        }),
        card("What to download", format_section(settings)),
        card("Extras", extras_section(settings)),
        card(
            "Automation and naming",
            automation_section(settings, history)
        ),
        card("Advanced", advanced_section(settings)),
        card("Where to save", destination_section(settings, paths)),
    ]
    .spacing(20)
    .into()
}

fn format_section(settings: &Settings) -> Element<'_, Message> {
    let mut body = column![
        radio(
            "Video and audio",
            Kind::VideoAudio,
            Some(settings.kind),
            Message::Kind
        ),
        radio(
            "Audio only",
            Kind::AudioOnly,
            Some(settings.kind),
            Message::Kind
        ),
    ]
    .spacing(12);

    match settings.kind {
        Kind::VideoAudio => {
            body = body.push(choice(
                "Format",
                pick_list(
                    VideoFormat::ALL,
                    Some(settings.video_format),
                    Message::VideoFormat,
                )
                .into(),
            ));
            if settings.video_format == VideoFormat::Custom {
                body = body.push(choice(
                    "Format string",
                    text_input("bestvideo+bestaudio", &settings.custom_format)
                        .on_input(Message::CustomFormat)
                        .padding(8)
                        .width(300)
                        .into(),
                ));
            }
            body = body
                .push(choice(
                    "Quality",
                    pick_list(Quality::ALL, Some(settings.quality), Message::Quality).into(),
                ))
                .push(choice(
                    "Container",
                    pick_list(Container::ALL, Some(settings.container), Message::Container).into(),
                ))
                .push(choice(
                    "Convert the container",
                    pick_list(Convert::ALL, Some(settings.convert), Message::Convert).into(),
                ));
            if settings.convert != Convert::Keep {
                body = body.push(choice(
                    "Convert to",
                    text_input("mp4", &settings.convert_target)
                        .on_input(Message::ConvertTarget)
                        .padding(8)
                        .width(160)
                        .into(),
                ));
            }
        }
        Kind::AudioOnly => {
            body = body.push(
                toggler(settings.audio_convert)
                    .label("Convert it to another format")
                    .on_toggle(Message::AudioConvert),
            );
            if settings.audio_convert {
                body = body.push(choice(
                    "Audio format",
                    pick_list(Audio::ALL, Some(settings.audio), Message::Audio).into(),
                ));
                if !settings.audio.is_lossless() {
                    body = body.push(choice(
                        "Audio quality (0 best, 10 smallest)",
                        pick_list(
                            AUDIO_QUALITIES,
                            Some(settings.audio_quality),
                            Message::AudioQuality,
                        )
                        .into(),
                    ));
                }
            } else {
                body = body.push(
                    text("The audio is kept exactly as the site serves it.")
                        .size(13)
                        .style(|t: &Theme| text::Style {
                            color: Some(style::muted(t)),
                        }),
                );
            }
        }
    }
    body.into()
}

fn extras_section(settings: &Settings) -> Element<'_, Message> {
    let mut body = column![
        toggler(settings.thumbnail)
            .label("Embed the thumbnail")
            .on_toggle(Message::Thumbnail),
    ]
    .spacing(12);
    if settings.thumbnail {
        body = body.push(choice(
            "Thumbnail format",
            pick_list(
                Thumb::ALL,
                Some(settings.thumbnail_format),
                Message::ThumbFormat,
            )
            .into(),
        ));
    }
    body = body
        .push(
            toggler(settings.metadata)
                .label("Embed title, artist and date")
                .on_toggle(Message::Metadata),
        )
        .push(
            toggler(settings.chapters)
                .label("Embed chapters")
                .on_toggle(Message::Chapters),
        )
        .push(choice(
            "Subtitles",
            pick_list(Subs::ALL, Some(settings.subs), Message::Subs).into(),
        ));

    if settings.subs != Subs::Off {
        body = body.push(choice(
            "Languages",
            text_input("all", &settings.sub_langs)
                .on_input(Message::SubLangs)
                .padding(8)
                .width(240)
                .into(),
        ));
    }
    body = body.push(choice(
        "SponsorBlock",
        pick_list(Sponsor::ALL, Some(settings.sponsor), Message::Sponsor).into(),
    ));
    if settings.sponsor != Sponsor::Off {
        body = body.push(choice(
            "Segments",
            pick_list(Cats::ALL, Some(settings.cats), Message::Cats).into(),
        ));
        if settings.cats == Cats::Custom {
            body = body.push(choice(
                "Categories",
                text_input("sponsor,intro,outro", &settings.custom_cats)
                    .on_input(Message::CustomCats)
                    .padding(8)
                    .width(240)
                    .into(),
            ));
        }
    }
    if settings.sponsor == Sponsor::Remove {
        body = body.push(
            toggler(settings.keyframes)
                .label("Cut exactly at keyframes (re-encodes, much slower)")
                .on_toggle(Message::Keyframes),
        );
    }
    body.into()
}

fn automation_section<'a>(settings: &'a Settings, history: History<'a>) -> Element<'a, Message> {
    let mut body = column![choice(
        "Playlists",
        pick_list(
            PlaylistChoice::ALL,
            Some(settings.playlist),
            Message::Playlist
        )
        .into(),
    )]
    .spacing(12);

    if settings.playlist == PlaylistChoice::Whole {
        body = body
            .push(choice(
                "Items (empty for all)",
                text_input("1-5,8", &settings.playlist_items)
                    .on_input(Message::PlaylistItems)
                    .padding(8)
                    .width(160)
                    .into(),
            ))
            .push(
                toggler(settings.playlist_reverse)
                    .label("Start from the end of the playlist")
                    .on_toggle(Message::PlaylistReverse),
            )
            .push(
                toggler(settings.organize)
                    .label("One folder per playlist, numbered")
                    .on_toggle(Message::Organize),
            );
    }

    body = body.push(
        toggler(settings.archive)
            .label("Keep a history and skip what was downloaded before")
            .on_toggle(Message::Archive),
    );
    if settings.archive {
        body = body
            .push(choice(
                "History file",
                text_input("download_archive.txt", &settings.archive_file)
                    .on_input(Message::ArchiveFile)
                    .padding(8)
                    .width(240)
                    .into(),
            ))
            .push(
                toggler(settings.archive_stop)
                    .label("Stop as soon as a video is already in the history")
                    .on_toggle(Message::ArchiveStop),
            )
            .push(history_row(history));
    }
    body = body
        .push(choice(
            "Stop after this many files",
            text_input("no limit", &settings.max_downloads)
                .on_input(Message::MaxDownloads)
                .padding(8)
                .width(160)
                .into(),
        ))
        .push(choice(
            "File names",
            pick_list(Template::ALL, Some(settings.template), Message::Template).into(),
        ));
    if settings.template == Template::Custom {
        body = body.push(choice(
            "Template",
            text_input("%(title)s [%(id)s].%(ext)s", &settings.custom_template)
                .on_input(Message::CustomTemplate)
                .padding(8)
                .width(300)
                .into(),
        ));
    }
    body = body.push(
        toggler(settings.restrict_filenames)
            .label("Plain ASCII file names")
            .on_toggle(Message::Restrict),
    );
    body.into()
}

/// What the history holds, and a two step way to throw it away.
fn history_row(history: History<'_>) -> Element<'_, Message> {
    if history.confirming_clear {
        return row![
            text("This deletes the history file for good.")
                .style(text::warning)
                .width(Length::Fill),
            button(text("Cancel").size(13))
                .padding([4, 14])
                .style(style::pill_secondary)
                .on_press(Message::CancelClearHistory),
            button(text("Clear").size(13))
                .padding([4, 14])
                .style(style::pill_danger)
                .on_press(Message::ConfirmClearHistory),
        ]
        .spacing(10)
        .align_y(Alignment::Center)
        .into();
    }
    let summary = match history.count {
        Some(1) => "1 video in the history".to_string(),
        Some(n) => format!("{n} videos in the history"),
        None => "The history file doesn't exist yet.".to_string(),
    };
    let row = row![
        text(summary)
            .size(13)
            .style(|t: &Theme| text::Style {
                color: Some(style::muted(t))
            })
            .width(Length::Fill),
        button(
            text(if history.entries.is_some() {
                "Hide entries"
            } else {
                "View entries"
            })
            .size(13)
        )
        .padding([4, 14])
        .style(style::pill_secondary)
        .on_press_maybe(
            history
                .count
                .is_some()
                .then_some(Message::ToggleHistoryList)
        ),
        button(text("Clear history").size(13))
            .padding([4, 14])
            .style(style::pill_secondary)
            .on_press_maybe(history.count.is_some().then_some(Message::ClearHistory)),
    ]
    .spacing(10)
    .align_y(Alignment::Center);

    match history.entries {
        None => row.into(),
        Some(lines) => column![row, history_list_view(lines)].spacing(10).into(),
    }
}

/// The history's own entries, one per line as yt-dlp wrote them. Scrollable
/// and height capped, so a long history doesn't push the rest of the card
/// off the screen.
fn history_list_view(lines: &[String]) -> Element<'_, Message> {
    let body: Element<'_, Message> = if lines.is_empty() {
        text("The history is empty.")
            .size(13)
            .style(|t: &Theme| text::Style {
                color: Some(style::muted(t)),
            })
            .into()
    } else {
        Column::with_children(
            lines
                .iter()
                .map(|line| text(line).font(Font::MONOSPACE).size(13).into()),
        )
        .spacing(4)
        .into()
    };
    container(scrollable(container(body).padding(10).width(Length::Fill)).height(140))
        .style(style::card)
        .into()
}

fn advanced_section(settings: &Settings) -> Element<'_, Message> {
    column![
        choice(
            "Speed limit",
            text_input("no limit", &settings.limit_rate)
                .on_input(Message::LimitRate)
                .padding(8)
                .width(160)
                .into(),
        ),
        choice(
            "Parts downloaded at once",
            text_input("5", &settings.fragments)
                .on_input(Message::Fragments)
                .padding(8)
                .width(160)
                .into(),
        ),
        choice(
            "Seconds between requests",
            text_input("1.5", &settings.sleep)
                .on_input(Message::Sleep)
                .padding(8)
                .width(160)
                .into(),
        ),
        toggler(settings.live_from_start)
            .label("Record live streams from the start")
            .on_toggle(Message::LiveFromStart),
        choice(
            "Wait for a scheduled stream (seconds)",
            text_input("60-3600", &settings.wait_for_video)
                .on_input(Message::WaitForVideo)
                .padding(8)
                .width(160)
                .into(),
        ),
        toggler(settings.preserve_mtime)
            .label("Keep the original upload date on the file")
            .on_toggle(Message::Mtime),
        toggler(settings.ignore_errors)
            .label("Keep going when one video of a playlist fails")
            .on_toggle(Message::IgnoreErrors),
        toggler(settings.verbose)
            .label("Verbose log (for troubleshooting)")
            .on_toggle(Message::Verbose),
    ]
    .spacing(12)
    .into()
}

fn destination_section<'a>(settings: &'a Settings, paths: &'a Paths) -> Element<'a, Message> {
    let default = paths.app_dir.join("downloads");
    column![
        text_input(&default.to_string_lossy(), &settings.output_dir)
            .on_input(Message::OutputDir)
            .padding(10),
        text("Leave it empty to use the downloads folder next to this app.")
            .size(13)
            .style(|t: &Theme| text::Style {
                color: Some(style::muted(t))
            }),
    ]
    .spacing(8)
    .into()
}
