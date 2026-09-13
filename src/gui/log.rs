//! The activity panel: one tagged line per step.

use iced::widget::{Column, container, row, scrollable, space, text};
use iced::{Element, Font, Length, Theme};

use super::style;
use crate::log::Level;

/// A scrolling, monospaced list of tagged lines, anchored to the bottom so
/// the newest line stays visible.
pub fn panel<'a, Message: 'a>(
    entries: &'a [(Level, String)],
    empty: &'a str,
    height: impl Into<Length>,
) -> Element<'a, Message> {
    let height = height.into();
    let body: Element<'a, Message> = if entries.is_empty() {
        text(empty)
            .style(|t: &Theme| text::Style {
                color: Some(style::muted(t)),
            })
            .into()
    } else {
        Column::with_children(entries.iter().map(|(level, line)| entry(*level, line)))
            .spacing(4)
            .into()
    };
    container(
        scrollable(container(body).padding([14, 16]).width(Length::Fill))
            .anchor_bottom()
            .height(height),
    )
    // Keeps the scrollbar inside the rounded corners.
    .padding([6, 4])
    .height(height)
    .width(Length::Fill)
    .style(style::card)
    .into()
}

fn entry<'a, Message: 'a>(level: Level, line: &'a str) -> Element<'a, Message> {
    if line.is_empty() {
        return space::vertical().height(6).into();
    }
    row![
        text(level.tag())
            .font(Font::MONOSPACE)
            .size(13)
            .style(move |t: &Theme| text::Style {
                color: Some(style::level(t, level))
            }),
        text(line).font(Font::MONOSPACE).size(13),
    ]
    .spacing(8)
    .into()
}
