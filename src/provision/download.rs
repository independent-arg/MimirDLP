//! HTTP for provisioning: small checksum files and large downloads.
//!
//! Large downloads run on a worker thread watched by the caller: if no data
//! arrives for [`STALL`], that attempt is abandoned and retried. There is
//! deliberately no limit on the total duration: a flat time budget (as in
//! `curl --max-time 300`) would make the 151 MB FFmpeg archive impossible to
//! install on any connection slower than about 0.5 MB/s. ureq only offers
//! total time budgets, not an idle timeout, hence the watchdog.

use std::fs::{self, File};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, mpsc};
use std::thread;
use std::time::{Duration, Instant};

use sha2::{Digest, Sha256};

use super::{Emit, Error, Event, Level, hex};

const ATTEMPTS: u32 = 3;
const RETRY_PAUSE: Duration = Duration::from_secs(2);
/// A download that receives nothing for this long is abandoned.
const STALL: Duration = Duration::from_secs(60);
/// Far above any real asset; stops a server that never ends the body.
const MAX_SIZE: u64 = 1 << 30;
/// Progress is reported at most this often.
const PROGRESS_EVERY: Duration = Duration::from_millis(100);

/// `pub(crate)`: also used by `metadata` to fetch a video's thumbnail.
pub(crate) fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_connect(Some(Duration::from_secs(10)))
        .timeout_recv_response(Some(Duration::from_secs(30)))
        .user_agent(concat!(
            env!("CARGO_PKG_NAME"),
            "/",
            env!("CARGO_PKG_VERSION")
        ))
        .build()
        .into()
}

fn net_err(e: impl std::fmt::Display) -> Error {
    Error::Download(e.to_string())
}

fn file_name(url: &str) -> &str {
    url.rsplit('/').next().unwrap_or(url)
}

/// One attempt, no logging. Used by update checks, which just report
/// "check failed" if the network is down.
pub fn fetch_text(url: &str) -> Result<String, Error> {
    agent()
        .get(url)
        .call()
        .map_err(net_err)?
        .into_body()
        .read_to_string()
        .map_err(net_err)
}

/// A small file, logged and retried.
pub fn fetch_text_logged(url: &str, emit: Emit<'_>) -> Result<String, Error> {
    let name = file_name(url);
    emit(Event::Log(Level::Download, name.to_string()));
    retry(name, ATTEMPTS, RETRY_PAUSE, emit, |_| fetch_text(url))
}

/// Downloads `url` to `dest`, returning its SHA-256. Logged, retried, and
/// watched for stalls.
pub fn download_to_file(
    url: &str,
    asset: &str,
    dest: &Path,
    emit: Emit<'_>,
) -> Result<String, Error> {
    download_with(url, asset, dest, emit, STALL, ATTEMPTS, RETRY_PAUSE)
}

fn download_with(
    url: &str,
    asset: &str,
    dest: &Path,
    emit: Emit<'_>,
    stall: Duration,
    attempts: u32,
    pause: Duration,
) -> Result<String, Error> {
    emit(Event::Log(Level::Download, asset.to_string()));
    retry(asset, attempts, pause, emit, |emit| {
        let result = attempt(url, asset, dest, stall, emit);
        emit(Event::TransferFinished);
        result
    })
}

fn retry<T>(
    name: &str,
    attempts: u32,
    pause: Duration,
    emit: Emit<'_>,
    mut op: impl FnMut(Emit<'_>) -> Result<T, Error>,
) -> Result<T, Error> {
    for n in 1..=attempts {
        match op(&mut *emit) {
            Ok(v) => return Ok(v),
            Err(e) if n < attempts => {
                emit(Event::Log(
                    Level::Warn,
                    format!("Download failed ({e}), retrying... (attempt {n}/{attempts})"),
                ));
                thread::sleep(pause);
            }
            Err(e) => {
                emit(Event::Log(
                    Level::Error,
                    format!("Failed to download {name} after {attempts} attempts ({e})"),
                ));
                emit(Event::Log(
                    Level::Error,
                    "Please check your internet connection and try again.".into(),
                ));
                return Err(e);
            }
        }
    }
    unreachable!("attempts is always at least 1")
}

enum Msg {
    Progress(u64, Option<u64>),
    Done(Result<String, Error>),
}

fn attempt(
    url: &str,
    asset: &str,
    dest: &Path,
    stall: Duration,
    emit: Emit<'_>,
) -> Result<String, Error> {
    // Each attempt writes its own file: an abandoned worker may still be
    // blocked on a read and must not collide with the retry.
    let part = unique_part(dest);
    let abandoned = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::channel();

    {
        let (url, part, abandoned) = (url.to_string(), part.clone(), abandoned.clone());
        thread::spawn(move || {
            let result = fetch_to(&url, &part, &abandoned, &tx);
            if result.is_err() || abandoned.load(Ordering::SeqCst) {
                let _ = fs::remove_file(&part);
            }
            let _ = tx.send(Msg::Done(result));
        });
    }

    let mut last_emit: Option<Instant> = None;
    let mut last = (0, None);
    loop {
        match rx.recv_timeout(stall) {
            Ok(Msg::Progress(received, total)) => {
                last = (received, total);
                if last_emit.is_none_or(|t| t.elapsed() >= PROGRESS_EVERY) {
                    emit(Event::Transfer {
                        file: asset.to_string(),
                        received,
                        total,
                    });
                    last_emit = Some(Instant::now());
                }
            }
            Ok(Msg::Done(Ok(hash))) => {
                emit(Event::Transfer {
                    file: asset.to_string(),
                    received: last.0,
                    total: last.1,
                });
                fs::rename(&part, dest)?;
                return Ok(hash);
            }
            Ok(Msg::Done(Err(e))) => return Err(e),
            Err(mpsc::RecvTimeoutError::Timeout) => {
                abandoned.store(true, Ordering::SeqCst);
                return Err(Error::Download(format!(
                    "no data received for {} seconds",
                    stall.as_secs()
                )));
            }
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                return Err(Error::Download("the download stopped unexpectedly".into()));
            }
        }
    }
}

fn unique_part(dest: &Path) -> PathBuf {
    use std::sync::atomic::AtomicU64;
    static N: AtomicU64 = AtomicU64::new(0);
    let mut name = dest.as_os_str().to_owned();
    name.push(format!(".{}", N.fetch_add(1, Ordering::SeqCst)));
    PathBuf::from(name)
}

fn fetch_to(
    url: &str,
    part: &Path,
    abandoned: &AtomicBool,
    tx: &mpsc::Sender<Msg>,
) -> Result<String, Error> {
    let response = agent().get(url).call().map_err(net_err)?;
    let total = response
        .headers()
        .get("content-length")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.parse::<u64>().ok());
    if total.is_some_and(|t| t > MAX_SIZE) {
        return Err(Error::Download("the file is unexpectedly large".into()));
    }
    // ureq errors out if the connection closes before Content-Length bytes.
    let mut body = response.into_body().into_reader();
    let mut file = File::create(part)?;
    let mut hasher = Sha256::new();
    let mut buf = vec![0u8; 1 << 16];
    let mut received: u64 = 0;
    let _ = tx.send(Msg::Progress(0, total));
    loop {
        let n = body.read(&mut buf).map_err(net_err)?;
        if abandoned.load(Ordering::SeqCst) {
            return Err(Error::Download("abandoned".into()));
        }
        if n == 0 {
            break;
        }
        received += n as u64;
        if received > MAX_SIZE {
            return Err(Error::Download("the file is unexpectedly large".into()));
        }
        hasher.update(&buf[..n]);
        file.write_all(&buf[..n])?;
        let _ = tx.send(Msg::Progress(received, total));
    }
    file.sync_all()?;
    Ok(hex(&hasher.finalize()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testing::tempdir;
    use std::io::{BufRead, BufReader};
    use std::net::TcpListener;

    /// Serves `respond` to every connection on a local port; returns the URL.
    fn serve(respond: impl Fn(&mut std::net::TcpStream) + Send + 'static) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                // Read the request headers before answering.
                let mut reader = BufReader::new(stream.try_clone().unwrap());
                let mut line = String::new();
                while reader.read_line(&mut line).unwrap_or(0) > 2 {
                    line.clear();
                }
                respond(&mut stream);
            }
        });
        format!("http://{addr}/asset.bin")
    }

    #[test]
    fn a_complete_download_is_hashed_and_moved_into_place() {
        let body = b"hello world".repeat(1000);
        let url = {
            let body = body.clone();
            serve(move |s| {
                write!(
                    s,
                    "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                    body.len()
                )
                .unwrap();
                s.write_all(&body).unwrap();
            })
        };
        let dir = tempdir();
        let dest = dir.join(".download-asset.bin");
        let mut events = Vec::new();
        let hash = download_with(
            &url,
            "asset.bin",
            &dest,
            &mut |e| events.push(e),
            STALL,
            1,
            Duration::ZERO,
        )
        .unwrap();
        let mut h = Sha256::new();
        h.update(&body);
        assert_eq!(hash, hex(&h.finalize()));
        assert_eq!(fs::read(&dest).unwrap(), body);
        assert!(events.contains(&Event::Log(Level::Download, "asset.bin".into())));
        assert!(events.iter().any(
            |e| matches!(e, Event::Transfer { received, total: Some(t), .. } if received == t)
        ));
        let leftovers = fs::read_dir(&dir).unwrap().count();
        assert_eq!(leftovers, 1, "only the finished file is left behind");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_stalled_download_is_abandoned_and_reported() {
        let url = serve(|s| {
            let _ = s.write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\n\r\npartial");
            thread::sleep(Duration::from_secs(30));
        });
        let dir = tempdir();
        let started = Instant::now();
        let mut events = Vec::new();
        let err = download_with(
            &url,
            "asset.bin",
            &dir.join(".download-asset.bin"),
            &mut |e| events.push(e),
            Duration::from_millis(500),
            2,
            Duration::ZERO,
        )
        .unwrap_err();
        assert!(
            started.elapsed() < Duration::from_secs(10),
            "the watchdog must not wait for the server"
        );
        assert!(err.to_string().contains("no data received"), "{err}");
        let warned = events
            .iter()
            .any(|e| matches!(e, Event::Log(Level::Warn, m) if m.contains("attempt 1/2")));
        assert!(warned, "the first stall must be retried: {events:?}");
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn a_truncated_download_is_an_error_not_a_short_file() {
        let url = serve(|s| {
            let _ = s.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 1000\r\nConnection: close\r\n\r\nshort",
            );
        });
        let dir = tempdir();
        let dest = dir.join(".download-asset.bin");
        let result = download_with(
            &url,
            "asset.bin",
            &dest,
            &mut |_| {},
            STALL,
            1,
            Duration::ZERO,
        );
        assert!(result.is_err());
        assert!(!dest.exists());
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn http_errors_fail_the_attempt() {
        let url = serve(|s| {
            let _ = s.write_all(
                b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            );
        });
        let dir = tempdir();
        let result = download_with(
            &url,
            "asset.bin",
            &dir.join("x"),
            &mut |_| {},
            STALL,
            1,
            Duration::ZERO,
        );
        assert!(result.unwrap_err().to_string().contains("404"));
        fs::remove_dir_all(dir).unwrap();
    }
}
