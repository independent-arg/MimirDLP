//! Colors and widget styles shared by every screen.

use iced::theme::{Mode, Palette};
use iced::widget::{button, container, progress_bar};
use iced::{Border, Color, Theme, border};

use crate::config::ThemePref;
use crate::provision::Level;

/// The theme to render with, given the user's preference and (for
/// [`ThemePref::System`]) the OS's current light/dark mode. `Mode::None`
/// (unknown, or a platform iced can't ask) falls back to dark, matching
/// this app's original, only appearance.
pub fn theme(pref: ThemePref, system: Mode) -> Theme {
    let dark = match pref {
        ThemePref::Dark => true,
        ThemePref::Light => false,
        ThemePref::System => !matches!(system, Mode::Light),
    };
    if dark { dark_theme() } else { light_theme() }
}

fn dark_theme() -> Theme {
    Theme::custom(
        "Portable Dark",
        Palette {
            background: Color::from_rgb8(0x16, 0x16, 0x16),
            text: Color::from_rgb8(0xe6, 0xe6, 0xe6),
            primary: Color::from_rgb8(0x4c, 0xd1, 0x7a),
            success: Color::from_rgb8(0x8f, 0xe3, 0xb8),
            warning: Color::from_rgb8(0xf5, 0xc2, 0x6b),
            danger: Color::from_rgb8(0xff, 0x9f, 0x9f),
        },
    )
}

fn light_theme() -> Theme {
    Theme::custom(
        "Portable Light",
        Palette {
            background: Color::from_rgb8(0xf6, 0xf6, 0xf6),
            text: Color::from_rgb8(0x1e, 0x1e, 0x1e),
            primary: Color::from_rgb8(0x1e, 0x8f, 0x4f),
            success: Color::from_rgb8(0x1f, 0x8a, 0x55),
            warning: Color::from_rgb8(0xa8, 0x6a, 0x00),
            danger: Color::from_rgb8(0xc9, 0x3b, 0x3b),
        },
    )
}

pub fn card(theme: &Theme) -> container::Style {
    container::Style {
        background: Some(theme.extended_palette().background.weak.color.into()),
        border: border::rounded(14),
        ..container::Style::default()
    }
}

/// The main call to action.
pub fn pill(theme: &Theme, status: button::Status) -> button::Style {
    rounded(button::primary(theme, status))
}

/// Everything else a user can press.
pub fn pill_secondary(theme: &Theme, status: button::Status) -> button::Style {
    rounded(button::secondary(theme, status))
}

pub fn pill_danger(theme: &Theme, status: button::Status) -> button::Style {
    rounded(button::danger(theme, status))
}

pub fn nav(active: bool) -> impl Fn(&Theme, button::Status) -> button::Style {
    move |theme, status| {
        if active {
            pill(theme, status)
        } else {
            rounded(button::text(theme, status))
        }
    }
}

fn rounded(mut style: button::Style) -> button::Style {
    style.border = border::rounded(999);
    style
}

pub fn progress(theme: &Theme) -> progress_bar::Style {
    let palette = theme.extended_palette();
    progress_bar::Style {
        background: palette.background.strong.color.into(),
        bar: palette.primary.base.color.into(),
        border: Border::default().rounded(999),
    }
}

/// Secondary text: hints, sizes, empty states.
pub fn muted(theme: &Theme) -> Color {
    let mut color = theme.palette().text;
    color.a = 0.6;
    color
}

/// The color for each log tag.
pub fn level(theme: &Theme, level: Level) -> Color {
    let palette = theme.palette();
    match level {
        Level::Info => palette.primary,
        Level::Install | Level::Download | Level::Ok | Level::Success => palette.success,
        Level::Verify | Level::Warn => palette.warning,
        Level::Error => palette.danger,
    }
}
