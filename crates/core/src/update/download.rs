//! HTTP through the system `curl`, so Newjen needs no HTTP library. curl follows GitHub's
//! redirect to the release asset host, enforces timeouts and reads `file://` URLs for local
//! end-to-end tests.

use std::fs;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

use super::UpdateError;
use super::process::{self, GroupChild};

const CURL: &str = "/usr/bin/curl";
const POLL: Duration = Duration::from_millis(100);

/// How a download ended, when it did not fail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Fetched {
    Done,
    Cancelled,
}

#[derive(Debug, Clone)]
pub(super) struct Curl {
    /// Refuse every URL and redirect that is not https. Off only when the feed is overridden.
    pub https_only: bool,
}

impl Curl {
    fn command(&self) -> Command {
        let mut command = Command::new(CURL);
        command.args([
            "--fail",
            "--silent",
            "--show-error",
            "--location",
            "--connect-timeout",
            "15",
            "--speed-limit",
            "1024",
            "--speed-time",
            "60",
            "--user-agent",
        ]);
        command.arg(format!(
            "Newjen/{}",
            super::RELEASE_VERSION.unwrap_or("dev")
        ));
        if self.https_only {
            command.args(["--proto", "=https", "--proto-redir", "=https"]);
        }
        command.stdin(Stdio::null());
        command
    }

    /// A small text file, such as the feed or a `.sig`.
    pub fn text(&self, url: &str) -> Result<String, UpdateError> {
        self.fetch_text(url)?.ok_or_else(|| {
            UpdateError::Other("The download failed: the file isn't there (404).".into())
        })
    }

    /// Like [`Curl::text`], but `Ok(None)` when the server says the file does not exist.
    pub fn fetch_text(&self, url: &str) -> Result<Option<String>, UpdateError> {
        let output = process::run(
            self.command().args(["--max-time", "30", "--", url]),
            &AtomicBool::new(false),
        )
        .map_err(spawn_error)?;
        if output.status.success() {
            return Ok(Some(String::from_utf8_lossy(&output.stdout).into_owned()));
        }
        let stderr = String::from_utf8_lossy(&output.stderr);
        let missing = match output.status.code() {
            Some(22) => stderr.contains("error: 404"),
            Some(37) => true,
            _ => false,
        };
        if missing {
            return Ok(None);
        }
        Err(curl_error(output.status.code(), &stderr))
    }

    /// Downloads `url` to `dest`, calling `progress(received, total)` while it runs. A
    /// cancelled or failed download leaves no file behind.
    pub fn file(
        &self,
        url: &str,
        dest: &Path,
        progress: &mut dyn FnMut(u64, Option<u64>),
        cancel: &AtomicBool,
    ) -> Result<Fetched, UpdateError> {
        let headers = headers_path(dest);
        let _ = fs::remove_file(dest);
        let mut child = GroupChild::spawn(
            self.command()
                .arg("--dump-header")
                .arg(&headers)
                .arg("--output")
                .arg(dest)
                .args(["--", url])
                .stderr(Stdio::piped()),
        )
        .map_err(spawn_error)?;
        let local_total = local_size(url);
        let mut last = None;
        let status = loop {
            if cancel.load(Ordering::Relaxed) {
                child.stop();
                discard(dest, &headers);
                return Ok(Fetched::Cancelled);
            }
            match child.try_wait() {
                Ok(Some(status)) => break status,
                Ok(None) => {}
                Err(error) => {
                    child.stop();
                    discard(dest, &headers);
                    return Err(UpdateError::Other(format!("Lost track of curl: {error}")));
                }
            }
            let now = (
                file_len(dest),
                local_total.or_else(|| header_total(&headers)),
            );
            if last != Some(now) {
                progress(now.0, now.1);
                last = Some(now);
            }
            thread::sleep(POLL);
        };
        let mut stderr = String::new();
        if let Some(mut pipe) = child.child_mut().stderr.take() {
            let _ = pipe.read_to_string(&mut stderr);
        }
        let total = local_total.or_else(|| header_total(&headers));
        let received = file_len(dest);
        let _ = fs::remove_file(&headers);
        if !status.success() {
            let _ = fs::remove_file(dest);
            return Err(curl_error(status.code(), &stderr));
        }
        progress(received, total.or(Some(received)));
        Ok(Fetched::Done)
    }
}

fn headers_path(dest: &Path) -> PathBuf {
    let mut name = dest.file_name().unwrap_or_default().to_os_string();
    name.push(".headers");
    dest.with_file_name(name)
}

fn discard(dest: &Path, headers: &Path) {
    let _ = fs::remove_file(dest);
    let _ = fs::remove_file(headers);
}

fn file_len(path: &Path) -> u64 {
    fs::metadata(path).map_or(0, |metadata| metadata.len())
}

fn local_size(url: &str) -> Option<u64> {
    let path = url.strip_prefix("file://")?;
    fs::metadata(path).ok().map(|metadata| metadata.len())
}

/// The `Content-Length` of the last successful response in a curl `--dump-header` file. A
/// redirect's own length is skipped, and so is zero.
fn header_total(headers: &Path) -> Option<u64> {
    parse_content_length(&fs::read_to_string(headers).ok()?)
}

fn parse_content_length(headers: &str) -> Option<u64> {
    let mut success = false;
    let mut total = None;
    for line in headers.lines() {
        let line = line.trim();
        if line
            .as_bytes()
            .get(..5)
            .is_some_and(|start| start.eq_ignore_ascii_case(b"HTTP/"))
        {
            success = line
                .split_whitespace()
                .nth(1)
                .is_some_and(|code| code.starts_with('2'));
            total = None;
        } else if success
            && let Some((name, value)) = line.split_once(':')
            && name.trim().eq_ignore_ascii_case("content-length")
        {
            total = value.trim().parse().ok().filter(|length| *length > 0);
        }
    }
    total
}

fn spawn_error(error: std::io::Error) -> UpdateError {
    UpdateError::Other(format!(
        "Newjen couldn't run curl to download the update: {error}"
    ))
}

/// curl's exit codes that mean the network, not the server or the request, was the problem.
fn curl_error(code: Option<i32>, stderr: &str) -> UpdateError {
    match code {
        Some(5 | 6 | 7 | 18 | 28 | 35 | 52 | 55 | 56) => UpdateError::Offline,
        _ => {
            let detail = stderr.trim().trim_start_matches("curl: ").trim();
            UpdateError::Other(if detail.is_empty() {
                "The download failed.".into()
            } else {
                format!("The download failed: {detail}")
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn content_length_comes_from_the_final_successful_response() {
        let headers = "HTTP/2 302\r\nlocation: https://x\r\ncontent-length: 0\r\n\r\n\
                       HTTP/2 200\r\nContent-Type: application/zip\r\nContent-Length: 4096\r\n\r\n";
        assert_eq!(parse_content_length(headers), Some(4096));
    }

    #[test]
    fn headers_with_multibyte_text_do_not_panic() {
        let headers = "HTTP/2 200\r\nx-ünï: 1\r\nÄÄÄ\r\n日本語日本語\r\ncontent-length: 7\r\n\r\n";
        assert_eq!(parse_content_length(headers), Some(7));
        assert_eq!(parse_content_length("日本"), None);
        assert_eq!(parse_content_length("abcdé"), None);
    }

    #[test]
    fn a_redirects_own_length_is_not_the_total() {
        let headers = "HTTP/2 302\r\ncontent-length: 95\r\n\r\n";
        assert_eq!(parse_content_length(headers), None);
        assert_eq!(parse_content_length(""), None);
    }

    #[test]
    fn network_failures_are_offline_and_the_rest_carry_curls_words() {
        assert_eq!(curl_error(Some(6), ""), UpdateError::Offline);
        assert_eq!(curl_error(Some(28), ""), UpdateError::Offline);
        assert_eq!(
            curl_error(Some(22), "curl: (22) The requested URL returned error: 404"),
            UpdateError::Other(
                "The download failed: (22) The requested URL returned error: 404".into()
            )
        );
    }
}
