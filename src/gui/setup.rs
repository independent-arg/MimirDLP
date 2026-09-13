//! The setup screen: what is installed, and the actions available on it
//! (install missing, check for updates, update outdated, reinstall all).
//! Every step is written to the activity log with the tags [`crate::log`]
//! defines, and the file being downloaded gets a progress bar.

use std::thread;

use iced::futures::channel::mpsc;
use iced::widget::{Column, button, column, container, progress_bar, row, space, text};
use iced::{Alignment, Element, Font, Length, Task, Theme};

use super::{card, log as log_panel, style};
use crate::provision::{self, Component, Event, Level, Paths, Status, UpdateCheck};

/// What the worker thread sends back while it runs.
#[derive(Debug, Clone)]
pub enum Report {
    Provision(Event),
    Status(Component, Status),
    Update(Component, UpdateCheck),
    Done,
}

#[derive(Debug, Clone)]
pub enum Message {
    Report(Report),
    InstallMissing,
    CheckUpdates,
    UpdateOutdated,
    AskReinstall,
    ConfirmReinstall,
    CancelReinstall,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Job {
    Inspecting,
    Installing(Kind),
    CheckingUpdates,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Kind {
    Missing,
    Outdated,
    All,
}

#[derive(Default)]
struct Row {
    /// None until the first inspection finishes.
    status: Option<Status>,
    /// None until checked ("unchecked" in the label shown for it).
    update: Option<UpdateCheck>,
}

struct Transfer {
    file: String,
    received: u64,
    total: Option<u64>,
}

pub struct Setup {
    paths: Paths,
    rows: [Row; 3],
    job: Option<Job>,
    confirm_reinstall: bool,
    log: Vec<(Level, String)>,
    transfer: Option<Transfer>,
    /// Debug builds only: an action to run once the first inspection ends.
    pending: Option<Message>,
}

impl Setup {
    pub fn new(paths: Paths) -> (Self, Task<Message>) {
        let setup = Setup {
            paths: paths.clone(),
            rows: Default::default(),
            job: Some(Job::Inspecting),
            confirm_reinstall: false,
            log: Vec::new(),
            transfer: None,
            pending: dev_action(),
        };
        let task = work(move |send| {
            provision::clean_leftovers(&paths);
            for component in Component::ALL {
                send(Report::Status(
                    component,
                    provision::inspect(component, &paths),
                ));
            }
        });
        (setup, task)
    }

    /// Everything is installed and runs, so downloading can work.
    pub fn ready(&self) -> bool {
        self.rows
            .iter()
            .all(|row| matches!(row.status, Some(Status::Installed { .. })))
    }

    pub fn busy(&self) -> bool {
        self.job.is_some()
    }

    pub fn update(&mut self, message: Message) -> Task<Message> {
        if self.busy() && !matches!(message, Message::Report(_)) {
            return Task::none();
        }
        match message {
            Message::Report(report) => return self.report(report),
            Message::InstallMissing => {
                let targets =
                    self.select(|row| matches!(row.status, Some(Status::Missing | Status::Broken)));
                return self.install(targets, Kind::Missing);
            }
            Message::CheckUpdates => {
                let targets = self.select(|row| {
                    matches!(row.status, Some(Status::Installed { .. }))
                        && matches!(row.update, None | Some(UpdateCheck::Failed(_)))
                });
                self.start(Job::CheckingUpdates);
                let paths = self.paths.clone();
                return work(move |send| {
                    let log =
                        |level, text: String| send(Report::Provision(Event::Log(level, text)));
                    log(
                        Level::Info,
                        "Checking for updates... (requires internet)".into(),
                    );
                    for component in targets {
                        let result = provision::check_update(component, &paths);
                        if let UpdateCheck::Failed(reason) = &result {
                            log(
                                Level::Warn,
                                format!(
                                    "Could not check {} for updates: {reason}",
                                    component.label()
                                ),
                            );
                        }
                        send(Report::Update(component, result));
                    }
                });
            }
            Message::UpdateOutdated => {
                let targets = self.select(|row| {
                    matches!(row.status, Some(Status::Installed { .. }))
                        && row.update == Some(UpdateCheck::Outdated)
                });
                return self.install(targets, Kind::Outdated);
            }
            Message::AskReinstall => self.confirm_reinstall = true,
            Message::CancelReinstall => self.confirm_reinstall = false,
            Message::ConfirmReinstall => {
                self.confirm_reinstall = false;
                return self.install(Component::ALL.to_vec(), Kind::All);
            }
        }
        Task::none()
    }

    fn report(&mut self, report: Report) -> Task<Message> {
        match report {
            Report::Provision(Event::Log(level, text)) => self.log.push((level, text)),
            Report::Provision(Event::Transfer {
                file,
                received,
                total,
            }) => {
                self.transfer = Some(Transfer {
                    file,
                    received,
                    total,
                })
            }
            Report::Provision(Event::TransferFinished) => self.transfer = None,
            Report::Status(component, status) => {
                let row = &mut self.rows[index(component)];
                row.status = Some(status);
                row.update = None;
            }
            Report::Update(component, check) => self.rows[index(component)].update = Some(check),
            Report::Done => {
                let first_inspection = self.job == Some(Job::Inspecting);
                self.job = None;
                self.transfer = None;
                if first_inspection && let Some(message) = self.pending.take() {
                    return Task::done(message);
                }
            }
        }
        Task::none()
    }

    fn start(&mut self, job: Job) {
        if !self.log.is_empty() {
            // A blank line between runs keeps them visually separate.
            self.log.push((Level::Info, String::new()));
        }
        self.job = Some(job);
    }

    fn select(&self, wanted: impl Fn(&Row) -> bool) -> Vec<Component> {
        Component::ALL
            .into_iter()
            .filter(|component| wanted(&self.rows[index(*component)]))
            .collect()
    }

    fn install(&mut self, targets: Vec<Component>, kind: Kind) -> Task<Message> {
        if targets.is_empty() {
            return Task::none();
        }
        self.start(Job::Installing(kind));
        let paths = self.paths.clone();
        work(move |send| {
            let mut failed = false;
            for component in targets {
                let result =
                    provision::install(component, &paths, &mut |e| send(Report::Provision(e)));
                send(Report::Status(
                    component,
                    provision::inspect(component, &paths),
                ));
                match result {
                    Ok(()) => send(Report::Update(component, UpdateCheck::Current)),
                    Err(_) => failed = true,
                }
            }
            let log = |level, text: String| send(Report::Provision(Event::Log(level, text)));
            if failed {
                log(Level::Warn, "Some components are missing or broken".into());
                return;
            }
            log(
                Level::Success,
                match kind {
                    Kind::Missing => "Installation completed",
                    Kind::Outdated => "Update completed",
                    Kind::All => "Full reinstallation completed",
                }
                .into(),
            );
            if kind != Kind::Outdated {
                log(
                    Level::Info,
                    format!("Binaries located in: {}", paths.bin_dir.display()),
                );
            }
        })
    }

    pub fn view(&self) -> Element<'_, Message> {
        let header = column![
            text("Setup").size(26),
            text(
                "yt-dlp, FFmpeg and Deno are kept in the bin folder next to this app. \
                 Nothing is installed on your system."
            )
            .style(|t: &Theme| text::Style {
                color: Some(style::muted(t))
            }),
        ]
        .spacing(4);

        let components = Column::with_children(
            Component::ALL
                .into_iter()
                .map(|component| component_row(component, &self.rows[index(component)])),
        )
        .spacing(12);

        let mut page = column![
            header,
            card("Components", components.into()),
            self.actions()
        ]
        .spacing(20);
        if let Some(transfer) = &self.transfer {
            page = page.push(transfer_view(transfer));
        }
        page = page.push(
            column![
                text("Activity").size(15),
                log_panel::panel(
                    &self.log,
                    "Nothing yet. Each download and check will be listed here.",
                    Length::Fill,
                ),
            ]
            .spacing(8)
            .height(Length::Fill),
        );

        container(page.padding(24).max_width(760))
            .width(Length::Fill)
            .center_x(Length::Fill)
            .into()
    }

    /// The contextual actions available, given what is installed right now.
    fn actions(&self) -> Element<'_, Message> {
        if self.confirm_reinstall {
            return row![
                text("This will re-download and reinstall ALL components.")
                    .style(text::warning)
                    .width(Length::Fill),
                button(text("Cancel"))
                    .padding([8, 20])
                    .style(style::pill_secondary)
                    .on_press(Message::CancelReinstall),
                button(text("Reinstall"))
                    .padding([8, 20])
                    .style(style::pill_danger)
                    .on_press(Message::ConfirmReinstall),
            ]
            .spacing(10)
            .align_y(Alignment::Center)
            .into();
        }

        let statuses: Vec<&Row> = self.rows.iter().collect();
        let any = |f: &dyn Fn(&Row) -> bool| statuses.iter().any(|row| f(row));
        let installed = |row: &Row| matches!(row.status, Some(Status::Installed { .. }));
        let has_missing = any(&|row| matches!(row.status, Some(Status::Missing | Status::Broken)));
        let has_unchecked =
            any(&|row| installed(row) && matches!(row.update, None | Some(UpdateCheck::Failed(_))));
        let has_updates = any(&|row| installed(row) && row.update == Some(UpdateCheck::Outdated));
        let has_installed = any(&|row| row.status.as_ref().is_some_and(|s| *s != Status::Missing));

        let idle = !self.busy();
        let action = |label: &'static str, message: Message, primary: bool| {
            button(text(label))
                .padding([8, 20])
                .style(if primary {
                    style::pill
                } else {
                    style::pill_secondary
                })
                .on_press_maybe(idle.then_some(message))
        };

        let mut actions = row![].spacing(10).align_y(Alignment::Center);
        if has_missing {
            actions = actions.push(action(
                "Install missing components",
                Message::InstallMissing,
                true,
            ));
        }
        if has_updates {
            actions = actions.push(action(
                "Update outdated components",
                Message::UpdateOutdated,
                true,
            ));
        }
        if has_unchecked {
            actions = actions.push(action(
                "Check for updates",
                Message::CheckUpdates,
                !has_missing && !has_updates,
            ));
        }
        if has_installed {
            actions = actions.push(action("Reinstall all", Message::AskReinstall, false));
        }
        if let Some(job) = self.job {
            let busy = match job {
                Job::Inspecting => "Looking at what is installed...",
                Job::Installing(_) => "Working...",
                Job::CheckingUpdates => "Checking for updates...",
            };
            actions = actions.push(text(busy).style(|t: &Theme| text::Style {
                color: Some(style::muted(t)),
            }));
        }
        actions.into()
    }
}

fn index(component: Component) -> usize {
    Component::ALL
        .iter()
        .position(|c| *c == component)
        .expect("every component is in ALL")
}

/// Starts `job` on its own thread and turns what it sends into messages.
/// Blocking work (network, hashing, extracting) never runs on the UI.
fn work(job: impl FnOnce(&dyn Fn(Report)) + Send + 'static) -> Task<Message> {
    let (sender, receiver) = mpsc::unbounded();
    thread::spawn(move || {
        let send = |report| {
            let _ = sender.unbounded_send(report);
        };
        job(&send);
        send(Report::Done);
    });
    Task::run(receiver, Message::Report)
}

/// One line of the component list.
fn component_row(component: Component, row: &Row) -> Element<'static, Message> {
    #[derive(Clone, Copy)]
    enum Tone {
        Good,
        Warn,
        Bad,
        Neutral,
    }
    let (tone, detail) = match (&row.status, &row.update) {
        (None, _) => (Tone::Neutral, "Checking...".to_string()),
        (Some(Status::Missing), _) => (Tone::Bad, "Not installed".to_string()),
        (Some(Status::Broken), _) => (Tone::Bad, "Installed but not responding".to_string()),
        (Some(Status::Installed { version }), update) => match update {
            None => (Tone::Good, format!("Version {version} (installed)")),
            Some(UpdateCheck::Current) => (Tone::Good, format!("Version {version} (up to date)")),
            Some(UpdateCheck::Outdated) => {
                (Tone::Warn, format!("Version {version} (update available)"))
            }
            Some(UpdateCheck::Unknown) => (
                Tone::Warn,
                format!(
                    "Version {version} (installed before update tracking, reinstall to enable it)"
                ),
            ),
            Some(UpdateCheck::Failed(_)) => (
                Tone::Warn,
                format!("Version {version} (update check failed)"),
            ),
        },
    };
    let mark = match tone {
        Tone::Good => "✓",
        Tone::Warn => "⚠",
        Tone::Bad => "✗",
        Tone::Neutral => "…",
    };
    let color = move |theme: &Theme| {
        let palette = theme.palette();
        text::Style {
            color: Some(match tone {
                Tone::Good => palette.success,
                Tone::Warn => palette.warning,
                Tone::Bad => palette.danger,
                Tone::Neutral => style::muted(theme),
            }),
        }
    };
    row![
        text(mark).width(22).style(color),
        text(component.label()).width(90),
        text(detail).style(move |t: &Theme| match tone {
            Tone::Good | Tone::Neutral => text::Style {
                color: Some(style::muted(t))
            },
            _ => color(t),
        }),
    ]
    .align_y(Alignment::Center)
    .into()
}

fn transfer_view(transfer: &Transfer) -> Element<'_, Message> {
    let mb = |bytes: u64| bytes as f64 / 1_000_000.0;
    let (amount, fraction) = match transfer.total {
        Some(total) if total > 0 => {
            let fraction = (transfer.received as f64 / total as f64).min(1.0);
            (
                format!(
                    "{:.1} / {:.1} MB  ({:.0}%)",
                    mb(transfer.received),
                    mb(total),
                    fraction * 100.0
                ),
                fraction as f32,
            )
        }
        _ => (format!("{:.1} MB", mb(transfer.received)), 0.0),
    };
    column![
        row![
            text(&transfer.file).font(Font::MONOSPACE).size(13),
            space::horizontal(),
            text(amount).size(13).style(|t: &Theme| text::Style {
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

/// Debug builds only: `YTP_DEV_ACTION=install|check|update|ask-reinstall|reinstall` runs
/// that action on start, so screenshots of a running install can be taken
/// without clicking. Release builds ignore it.
fn dev_action() -> Option<Message> {
    if !cfg!(debug_assertions) {
        return None;
    }
    match std::env::var("YTP_DEV_ACTION").ok()?.as_str() {
        "install" => Some(Message::InstallMissing),
        "check" => Some(Message::CheckUpdates),
        "update" => Some(Message::UpdateOutdated),
        "ask-reinstall" => Some(Message::AskReinstall),
        "reinstall" => Some(Message::ConfirmReinstall),
        _ => None,
    }
}
