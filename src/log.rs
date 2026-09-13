//! Log tags shared by everything that reports progress to the user
//! (provisioning, downloads, the GUI panels).

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    Info,
    Install,
    Download,
    Verify,
    Ok,
    Warn,
    Error,
    Success,
}

impl Level {
    pub fn tag(self) -> &'static str {
        match self {
            Level::Info => "[INFO]",
            Level::Install => "[INSTALL]",
            Level::Download => "[DOWNLOAD]",
            Level::Verify => "[VERIFY]",
            Level::Ok => "[OK]",
            Level::Warn => "[WARN]",
            Level::Error => "[ERROR]",
            Level::Success => "[SUCCESS]",
        }
    }
}
