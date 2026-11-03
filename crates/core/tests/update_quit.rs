//! Quitting Newjen stops a download that is still running. This test has its own process,
//! because stopping helpers is global.

use std::fs;
use std::io::Read;
use std::net::TcpListener;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use newjen_core::update::{InstallTarget, UpdateState, Updater, kill_helpers};

#[test]
fn quitting_stops_a_download_that_is_in_flight() {
    let dir = tempfile::tempdir().unwrap();
    let exe = fs::canonicalize(dir.path())
        .unwrap()
        .join("Newjen.app/Contents/MacOS/newjen");
    fs::create_dir_all(exe.parent().unwrap()).unwrap();
    fs::write(&exe, "old").unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let base = format!("http://{}", listener.local_addr().unwrap());
    std::thread::spawn(move || {
        let mut held = Vec::new();
        while let Ok((mut stream, _)) = listener.accept() {
            let _ = stream.read(&mut [0; 1024]);
            held.push(stream);
        }
    });
    let updater = Updater::new(
        &base,
        "0.1.0".parse().unwrap(),
        InstallTarget::from_exe(&exe).unwrap(),
        [1; 32],
    );
    let started = Instant::now();
    let worker =
        std::thread::spawn(move || updater.run_once(None, &mut |_| {}, &AtomicBool::new(false)));
    std::thread::sleep(Duration::from_millis(700));
    assert!(!worker.is_finished(), "the request hangs on purpose");
    kill_helpers();
    let last = worker.join().unwrap();
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    assert!(matches!(last, UpdateState::Failed(_)), "{last:?}");
}
