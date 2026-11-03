//! Helper programs (curl, ditto, codesign) run in their own process group, so cancelling an
//! update or quitting Newjen stops them and everything they started.

use std::io::{self, Read};
use std::os::unix::process::CommandExt;
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::Duration;

const POLL: Duration = Duration::from_millis(50);

static GROUPS: Mutex<Vec<i32>> = Mutex::new(Vec::new());

fn groups() -> std::sync::MutexGuard<'static, Vec<i32>> {
    GROUPS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// Kills every helper program that is still running. Call it when Newjen quits: the helpers
/// would otherwise outlive it.
pub fn kill_helpers() {
    for pgid in groups().iter() {
        kill_group(*pgid);
    }
}

fn kill_group(pgid: i32) {
    // SAFETY: only signals a process group this module started and has not yet reaped.
    unsafe { libc::killpg(pgid, libc::SIGKILL) };
}

/// A child that leads its own process group. Dropping it kills the group and reaps the child.
pub(super) struct GroupChild {
    child: Child,
    pgid: i32,
    reaped: bool,
}

impl GroupChild {
    pub fn spawn(command: &mut Command) -> io::Result<GroupChild> {
        let child = command.process_group(0).spawn()?;
        let pgid = child.id() as i32;
        groups().push(pgid);
        Ok(GroupChild {
            child,
            pgid,
            reaped: false,
        })
    }

    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub fn try_wait(&mut self) -> io::Result<Option<ExitStatus>> {
        let status = self.child.try_wait()?;
        if status.is_some() {
            self.forget();
        }
        Ok(status)
    }

    /// Kills the whole group and reaps the child.
    pub fn stop(&mut self) {
        if !self.reaped {
            kill_group(self.pgid);
            let _ = self.child.wait();
            self.forget();
        }
    }

    fn forget(&mut self) {
        self.reaped = true;
        groups().retain(|pgid| *pgid != self.pgid);
    }
}

impl Drop for GroupChild {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Runs `command` to the end and collects its output, unless `cancel` is set first, which
/// kills it and returns [`io::ErrorKind::Interrupted`].
pub(super) fn run(command: &mut Command, cancel: &AtomicBool) -> io::Result<Output> {
    let mut child = GroupChild::spawn(
        command
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped()),
    )?;
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(
        child
            .child_mut()
            .stdout
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let stderr = drain(
        child
            .child_mut()
            .stderr
            .take()
            .map(|pipe| Box::new(pipe) as Box<dyn Read + Send>),
    );
    let status = loop {
        if cancel.load(Ordering::Relaxed) {
            child.stop();
            let _ = (stdout.join(), stderr.join());
            return Err(io::ErrorKind::Interrupted.into());
        }
        if let Some(status) = child.try_wait()? {
            break status;
        }
        thread::sleep(POLL);
    };
    Ok(Output {
        status,
        stdout: stdout.join().unwrap_or_default(),
        stderr: stderr.join().unwrap_or_default(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn output_is_collected() {
        let output = run(
            Command::new("/bin/sh").args(["-c", "echo out; echo err >&2; exit 3"]),
            &AtomicBool::new(false),
        )
        .unwrap();
        assert_eq!(output.status.code(), Some(3));
        assert_eq!(output.stdout, b"out\n");
        assert_eq!(output.stderr, b"err\n");
    }

    #[test]
    fn cancelling_kills_the_whole_group() {
        let dir = tempfile::tempdir().unwrap();
        let marker = dir.path().join("grandchild");
        let cancel = AtomicBool::new(false);
        let script = format!("sleep 30 & echo $! > '{}'; wait", marker.display());
        let cancelled = thread::scope(|scope| {
            let runner =
                scope.spawn(|| run(Command::new("/bin/sh").args(["-c", &script]), &cancel));
            while !marker.exists() || std::fs::read_to_string(&marker).unwrap().trim().is_empty() {
                thread::sleep(POLL);
            }
            cancel.store(true, Ordering::SeqCst);
            runner.join().unwrap()
        });
        assert_eq!(cancelled.unwrap_err().kind(), io::ErrorKind::Interrupted);
        let grandchild: i32 = std::fs::read_to_string(&marker)
            .unwrap()
            .trim()
            .parse()
            .unwrap();
        thread::sleep(Duration::from_millis(200));
        // SAFETY: signal 0 only checks that the process exists.
        assert!(
            unsafe { libc::kill(grandchild, 0) } != 0,
            "the group was left running"
        );
    }
}
