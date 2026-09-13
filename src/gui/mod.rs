//! The window: a top bar and one screen at a time.

mod download;
mod log;
mod settings;
mod setup;
mod style;

use iced::theme::Mode as ThemeMode;
use iced::widget::{button, column, container, row, scrollable, space, text};
use iced::{Alignment, Element, Length, Subscription, Task, Theme, window};

use crate::config::{Settings, ThemePref};
use crate::provision::Paths;
use download::Download;
use setup::Setup;

pub fn run() -> iced::Result {
    iced::application(App::boot, App::update, App::view)
        .title("MimirDLP")
        .theme(App::theme)
        .subscription(App::subscription)
        .window_size((1000.0, 720.0))
        .run()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Page {
    Download,
    Setup,
    Settings,
}

#[derive(Debug, Clone)]
enum Message {
    Navigate(Page),
    Setup(setup::Message),
    Download(download::Message),
    /// The OS's current light/dark mode, at startup and whenever it changes.
    SystemTheme(ThemeMode),
    ThemePref(ThemePref),
    /// Debug builds only, see [`dev_snapshot`].
    SnapshotDue,
    SnapshotFrame,
    Snapshot(window::Screenshot),
}

enum App {
    /// The folder next to the executable couldn't be determined.
    Broken(String),
    Running {
        page: Page,
        setup: Box<Setup>,
        download: Box<Download>,
        /// Unknown until the startup query resolves; treated as dark, this
        /// app's original and only appearance before this existed.
        system_theme: ThemeMode,
        /// Debug builds only: the state is frozen until the capture is taken.
        snapshot_due: bool,
    },
}

impl App {
    fn boot() -> (Self, Task<Message>) {
        let paths = match Paths::for_current_exe() {
            Ok(paths) => paths,
            Err(e) => {
                let reason = format!("Could not find the folder this app is in: {e}");
                return (App::Broken(reason), Task::none());
            }
        };
        let settings = Settings::load(&paths);
        let (setup, task) = Setup::new(paths.clone());
        let download = Download::new(paths, settings);
        let mut tasks = vec![
            task.map(Message::Setup),
            dev_snapshot(),
            iced::system::theme().map(Message::SystemTheme),
        ];
        // Links handed over for a dev run open on the Download screen,
        // queue every one of them (in order), and start right away.
        let dev_urls = download::dev_urls();
        let page = if !dev_urls.is_empty() {
            for url in dev_urls {
                tasks.push(Task::done(Message::Download(download::Message::UrlInput(
                    url,
                ))));
                tasks.push(Task::done(Message::Download(download::Message::AddUrl)));
            }
            tasks.push(Task::done(Message::Download(download::Message::Start)));
            tasks.push(dev_stop());
            Page::Download
        } else {
            Page::Setup
        };
        let page = dev_page().unwrap_or(page);
        let app = App::Running {
            page,
            setup: Box::new(setup),
            download: Box::new(download),
            system_theme: ThemeMode::None,
            snapshot_due: false,
        };
        (app, Task::batch(tasks))
    }

    fn update(&mut self, message: Message) -> Task<Message> {
        match (self, message) {
            (_, Message::Snapshot(shot)) => save_snapshot(shot),
            (App::Running { snapshot_due, .. }, Message::SnapshotDue) => {
                *snapshot_due = true;
                Task::none()
            }
            (App::Running { .. }, Message::SnapshotFrame) => window::latest().then(|id| match id {
                Some(id) => window::screenshot(id).map(Message::Snapshot),
                None => Task::none(),
            }),
            (
                App::Running {
                    snapshot_due: true, ..
                },
                _,
            ) => Task::none(),
            (App::Running { page, setup, .. }, Message::Navigate(target)) => {
                if matches!(target, Page::Setup | Page::Settings) || setup.ready() {
                    *page = target;
                }
                Task::none()
            }
            (App::Running { setup, .. }, Message::Setup(message)) => {
                setup.update(message).map(Message::Setup)
            }
            (App::Running { download, .. }, Message::Download(message)) => {
                download.update(message).map(Message::Download)
            }
            (App::Running { system_theme, .. }, Message::SystemTheme(mode)) => {
                *system_theme = mode;
                Task::none()
            }
            (App::Running { download, .. }, Message::ThemePref(pref)) => {
                download.set_theme(pref);
                Task::none()
            }
            (App::Broken(_), _) => Task::none(),
        }
    }

    fn view(&self) -> Element<'_, Message> {
        let (page, setup, download) = match self {
            App::Broken(reason) => {
                return container(text(reason).style(text::danger))
                    .padding(24)
                    .into();
            }
            App::Running {
                page,
                setup,
                download,
                ..
            } => (*page, setup, download),
        };

        let nav = |label: &'static str, target: Page, enabled: bool| {
            button(text(label))
                .padding([8, 16])
                .style(style::nav(page == target))
                .on_press_maybe(enabled.then_some(Message::Navigate(target)))
        };
        let theme_choice = |label: &'static str, target: ThemePref| {
            button(text(label).size(13))
                .padding([6, 10])
                .style(style::nav(download.theme() == target))
                .on_press(Message::ThemePref(target))
        };
        let top_bar = row![
            text("MimirDLP").size(16),
            nav("Download", Page::Download, setup.ready() && !setup.busy()),
            nav("Setup", Page::Setup, !download.running()),
            nav("Settings", Page::Settings, true),
            space::horizontal(),
            theme_choice("System", ThemePref::System),
            theme_choice("Light", ThemePref::Light),
            theme_choice("Dark", ThemePref::Dark),
        ]
        .spacing(10)
        .padding([12, 20])
        .align_y(Alignment::Center);

        let content = match page {
            Page::Setup => setup.view().map(Message::Setup),
            Page::Download => download.view().map(Message::Download),
            Page::Settings => {
                let body = settings::view(
                    download.settings(),
                    download.paths(),
                    download.history_view(),
                );
                container(
                    scrollable(
                        container(body.map(Message::Download))
                            .padding(24)
                            .max_width(760),
                    )
                    .height(Length::Fill),
                )
                .width(Length::Fill)
                .center_x(Length::Fill)
                .into()
            }
        };

        column![container(top_bar).style(style::card), content,]
            .height(Length::Fill)
            .into()
    }

    fn theme(&self) -> Theme {
        match self {
            App::Running {
                download,
                system_theme,
                ..
            } => style::theme(download.theme(), *system_theme),
            App::Broken(_) => style::theme(ThemePref::System, ThemeMode::None),
        }
    }

    fn subscription(&self) -> Subscription<Message> {
        match self {
            App::Running { snapshot_due, .. } => {
                let theme = iced::system::theme_changes().map(Message::SystemTheme);
                if *snapshot_due {
                    Subscription::batch([theme, window::frames().map(|_| Message::SnapshotFrame)])
                } else {
                    theme
                }
            }
            App::Broken(_) => Subscription::none(),
        }
    }
}

/// A titled card, the building block of every screen.
fn card<'a, Message: 'a>(title: &'a str, body: Element<'a, Message>) -> Element<'a, Message> {
    column![
        text(title).size(15),
        container(body)
            .padding(16)
            .width(Length::Fill)
            .style(style::card)
    ]
    .spacing(8)
    .into()
}

/// A labelled control, with the control pushed to the right. Shared by the
/// Settings page's sections.
fn choice<'a, Message: 'a>(label: &'a str, control: Element<'a, Message>) -> Element<'a, Message> {
    row![text(label).width(Length::Fill), control]
        .align_y(Alignment::Center)
        .into()
}

/// Debug builds only: `YTP_DEV_PAGE=setup|download|settings` opens directly
/// on that page, so a screen's layout can be checked without installing
/// anything or navigating there by hand. Takes priority over `YTP_DEV_DOWNLOAD`'s
/// own page choice when both are set.
fn dev_page() -> Option<Page> {
    if !cfg!(debug_assertions) {
        return None;
    }
    match std::env::var("YTP_DEV_PAGE").ok()?.as_str() {
        "setup" => Some(Page::Setup),
        "download" => Some(Page::Download),
        "settings" => Some(Page::Settings),
        _ => None,
    }
}

/// Debug builds only: `YTP_DEV_SNAPSHOT=out.rgba` captures the window after
/// `YTP_DEV_SNAPSHOT_AFTER` milliseconds (default 1500), writes the raw
/// RGBA pixels plus `out.rgba.size`, and quits. Used to check the UI.
///
/// A screenshot re-renders the last drawn frame, whose text belongs to the
/// widgets of that frame. Capturing right after an update, before the next
/// redraw, therefore loses text. So the state is frozen first, and the
/// capture waits for the next drawn frame.
fn dev_snapshot() -> Task<Message> {
    if !cfg!(debug_assertions) || std::env::var_os("YTP_DEV_SNAPSHOT").is_none() {
        return Task::none();
    }
    let after = std::env::var("YTP_DEV_SNAPSHOT_AFTER")
        .ok()
        .and_then(|ms| ms.parse().ok())
        .unwrap_or(1500);
    Task::perform(wait_on_a_thread(after), |_| Message::SnapshotDue)
}

/// Debug builds only: `YTP_DEV_STOP_AFTER=<ms>` presses Stop on its own,
/// to check that cancelling a real download works.
fn dev_stop() -> Task<Message> {
    let after = match std::env::var("YTP_DEV_STOP_AFTER")
        .ok()
        .map(|ms| ms.parse())
    {
        Some(Ok(ms)) if cfg!(debug_assertions) => ms,
        _ => return Task::none(),
    };
    Task::perform(wait_on_a_thread(after), |_| {
        Message::Download(download::Message::Stop)
    })
}

/// Waits without blocking the executor: a `thread::sleep` inside a future
/// stalls every task batched with it, the worker threads included.
fn wait_on_a_thread(millis: u64) -> impl std::future::Future<Output = ()> {
    let (done, wait) = iced::futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(millis));
        let _ = done.send(());
    });
    async move {
        let _ = wait.await;
    }
}

fn save_snapshot(shot: window::Screenshot) -> Task<Message> {
    if let Some(out) = std::env::var_os("YTP_DEV_SNAPSHOT") {
        let size = format!("{}x{}", shot.size.width, shot.size.height);
        let mut size_file = out.clone();
        size_file.push(".size");
        let _ = std::fs::write(&out, &shot.rgba);
        let _ = std::fs::write(size_file, size);
    }
    iced::exit()
}
