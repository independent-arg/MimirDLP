//! Helpers shared by the unit tests of several modules.

use std::fs;
use std::path::PathBuf;

/// A fresh directory under the system temp folder. The caller removes it.
/// Unique per process and per call, so tests can run in parallel.
pub(crate) fn tempdir() -> PathBuf {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ytp-test-{}-{}",
        std::process::id(),
        N.fetch_add(1, Ordering::SeqCst)
    ));
    fs::create_dir_all(&dir).unwrap();
    dir
}

/// Serialises the tests that write a script and then run it.
///
/// Linux refuses to exec a file that any process still holds open for
/// writing (`ETXTBSY`), and a sibling test forking at the wrong moment
/// inherits that handle. It showed up once as "yt-dlp could not be
/// started" in a test that passes on its own, so the tests that do this
/// take turns. Windows has no such restriction and has no caller for this,
/// which would otherwise make it dead code under `-D warnings`.
#[cfg(unix)]
pub(crate) fn exec_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}
