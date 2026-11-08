//! Hardware smoke test: drives the real pad through the Worker, like the UI would.
//! cargo run --example drive
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::mpsc::unbounded_channel;
use tokio::time::sleep;
use walkingpad_lib::pad::{Listener, Snapshot, UiCmd, Worker};
use walkingpad_lib::protocol::Belt;
use walkingpad_lib::stats::Store;

async fn wait_for(shared: &Arc<Mutex<Snapshot>>, what: &str, f: impl Fn(&Snapshot) -> bool, secs: u64) {
    for _ in 0..secs * 10 {
        if f(&shared.lock().unwrap()) {
            println!("== reached: {what}");
            return;
        }
        sleep(Duration::from_millis(100)).await;
    }
    panic!("timeout waiting for {what}");
}

#[tokio::main]
async fn main() {
    env_logger::Builder::from_env(env_logger::Env::default().default_filter_or("info,walkingpad_lib=debug")).init();
    let path = std::env::temp_dir().join("walkingpad-drive-stats.json");
    let _ = std::fs::remove_file(&path);
    let store = Store::load(path.clone());
    let shared = Arc::new(Mutex::new(Snapshot::initial(&store)));
    let (tx, rx) = unbounded_channel();
    let t0 = std::time::Instant::now();
    let listener: Listener = Arc::new(move |s: &Snapshot| {
        println!(
            "{:6.1} {} belt={:?} mode={:?} speed={} target={:?} session={}s/{}steps today={}steps/{}m keep={}",
            t0.elapsed().as_secs_f32(), s.connection, s.belt, s.mode, s.speed, s.target,
            s.session_time_s, s.session_steps, s.today_steps, s.today_distance_m, s.keep_running
        )
    });
    tokio::spawn(Worker::new(rx, shared.clone(), listener, store).run());

    wait_for(&shared, "connected", |s| s.connection == "connected" && s.belt.is_some(), 30).await;
    tx.send(UiCmd::StartStop).unwrap();
    wait_for(&shared, "running at start speed", |s| s.belt == Some(Belt::Running) && s.speed >= 2.0, 20).await;
    println!("== two quick +0.5 presses (expect ONE speed command to 3.0)");
    tx.send(UiCmd::Adjust { tenths: 5 }).unwrap();
    sleep(Duration::from_millis(150)).await;
    tx.send(UiCmd::Adjust { tenths: 5 }).unwrap();
    wait_for(&shared, "speed 3.0", |s| (s.speed - 3.0).abs() < 0.05, 15).await;
    println!("== down to 1.0");
    tx.send(UiCmd::Adjust { tenths: -20 }).unwrap();
    wait_for(&shared, "speed 1.0", |s| (s.speed - 1.0).abs() < 0.05, 15).await;
    println!("== keep running on; empty belt for 60 s (expect blips, no auto-stop)");
    tx.send(UiCmd::KeepRunning { enabled: true }).unwrap();
    for _ in 0..60 {
        sleep(Duration::from_secs(1)).await;
        assert_eq!(shared.lock().unwrap().belt, Some(Belt::Running), "belt auto-stopped despite keep-running");
    }
    println!("== stop");
    tx.send(UiCmd::StartStop).unwrap();
    wait_for(&shared, "belt idle", |s| s.belt == Some(Belt::Idle), 20).await;
    sleep(Duration::from_secs(3)).await;
    tx.send(UiCmd::SetMode { mode: walkingpad_lib::protocol::Mode::Standby }).unwrap();
    sleep(Duration::from_secs(12)).await; // let the stats save
    println!("== stats file: {}", std::fs::read_to_string(&path).unwrap_or_else(|e| e.to_string()));
}
