//! Bounded, usage-free subprocess I/O shared by environment and provider probes.
use std::io::{Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;
use std::process::{Child, ChildStdout, Command, ExitStatus, Stdio};
use std::thread;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, bail};

const MAX_OUTPUT: usize = 4 * 1024 * 1024;

pub(crate) struct Probe {
    child: Child,
    stdout: ChildStdout,
    buffered: Vec<u8>,
    deadline: Instant,
}

impl Probe {
    pub(crate) fn spawn(command: &mut Command, deadline: Instant) -> Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .process_group(0)
            .spawn()
            .context("Could not start the CLI")?;
        let stdout = child.stdout.take().context("CLI output pipe is missing")?;
        let probe = Self {
            child,
            stdout,
            buffered: Vec::new(),
            deadline,
        };
        nonblocking(probe.stdout.as_raw_fd())?;
        if let Some(stdin) = &probe.child.stdin {
            nonblocking(stdin.as_raw_fd())?;
        }
        Ok(probe)
    }

    pub(crate) fn send(&mut self, value: &serde_json::Value) -> Result<()> {
        let mut bytes = serde_json::to_vec(value)?;
        bytes.push(b'\n');
        let mut remaining = bytes.as_slice();
        while !remaining.is_empty() {
            self.check_deadline()?;
            match self
                .child
                .stdin
                .as_mut()
                .context("CLI input pipe is missing")?
                .write(remaining)
            {
                Ok(0) => bail!("The CLI closed its input"),
                Ok(n) => remaining = &remaining[n..],
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    thread::sleep(Duration::from_millis(5))
                }
                Err(_) => bail!("Could not send a read-only CLI request"),
            }
        }
        Ok(())
    }

    fn check_deadline(&self) -> Result<()> {
        if Instant::now() >= self.deadline {
            bail!("The CLI did not respond before the detection deadline");
        }
        Ok(())
    }

    fn read_available(&mut self) -> Result<bool> {
        self.check_deadline()?;
        let mut bytes = [0; 8192];
        match self.stdout.read(&mut bytes) {
            Ok(0) => Ok(false),
            Ok(n) => {
                if self.buffered.len() + n > MAX_OUTPUT {
                    bail!("The CLI returned too much output");
                }
                self.buffered.extend_from_slice(&bytes[..n]);
                Ok(true)
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                thread::sleep(Duration::from_millis(5));
                Ok(true)
            }
            Err(_) => bail!("Could not read CLI output"),
        }
    }

    pub(crate) fn line(&mut self) -> Result<Vec<u8>> {
        loop {
            self.check_deadline()?;
            if let Some(end) = self.buffered.iter().position(|byte| *byte == b'\n') {
                return Ok(self.buffered.drain(..=end).collect());
            }
            if !self.read_available()? {
                bail!("The CLI closed its output before answering");
            }
        }
    }

    pub(crate) fn output(mut self) -> Result<(ExitStatus, Vec<u8>)> {
        self.child.stdin.take();
        loop {
            let more = self.read_available()?;
            if !more && let Some(status) = self.child.try_wait()? {
                return Ok((status, std::mem::take(&mut self.buffered)));
            }
            if !more {
                thread::sleep(Duration::from_millis(5));
            }
        }
    }
}

impl Drop for Probe {
    fn drop(&mut self) {
        // SAFETY: the child leads its own group. Kill descendants holding pipes open too.
        unsafe { libc::killpg(self.child.id() as libc::pid_t, libc::SIGKILL) };
        let _ = self.child.wait();
    }
}

pub(crate) fn output(command: &mut Command, deadline: Instant) -> Result<(ExitStatus, Vec<u8>)> {
    Probe::spawn(command, deadline)?.output()
}

fn nonblocking(fd: std::os::fd::RawFd) -> Result<()> {
    // SAFETY: fd is an open pipe owned by the probe; fcntl changes only its I/O flags.
    let flags = unsafe { libc::fcntl(fd, libc::F_GETFL) };
    if flags == -1 || unsafe { libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK) } == -1 {
        bail!("Could not configure bounded CLI I/O");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_peer_that_never_reads_stdin_cannot_block_detection() {
        let start = Instant::now();
        let mut probe = Probe::spawn(
            Command::new("/bin/sh").args(["-c", "sleep 30"]),
            start + Duration::from_millis(70),
        )
        .unwrap();
        let error = probe
            .send(&serde_json::json!({"cursor": "x".repeat(256 * 1024)}))
            .unwrap_err();
        assert!(error.to_string().contains("deadline"));
        drop(probe);
        assert!(start.elapsed() < Duration::from_secs(2));
    }
}
