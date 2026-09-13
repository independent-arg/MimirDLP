//! MimirDLP: a portable graphical front end for yt-dlp.

pub mod config;
pub mod engine;
pub mod gui;
pub mod log;
pub mod metadata;
pub mod provision;
pub mod runner;

#[cfg(test)]
pub(crate) mod testing;
