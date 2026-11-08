//! BLE worker: finds the pad, keeps the connection, polls status, executes UI
//! commands, and feeds the daily statistics. UI-framework agnostic.

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{anyhow, Context, Result};
use btleplug::api::{Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use serde::Serialize;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::time::{interval, MissedTickBehavior};

use crate::protocol::{self, Belt, Command, Message, Mode, Status};
use crate::stats::{self, DaySummary, SessionTracker, Store};

/// Delay before a speed change is sent, so several clicks become one command (one beep).
const SPEED_DEBOUNCE: Duration = Duration::from_millis(400);
/// The pad auto-stops after ~35 s without detected steps; intervene before that.
const KEEPALIVE_AFTER: Duration = Duration::from_secs(20);
const BLIP: u8 = 5; // +0.5 km/h is the smallest bump that registers as steps
const BLIP_DURATION: Duration = Duration::from_millis(1500);
const PHANTOM_TAIL: Duration = Duration::from_secs(3);
const LINK_TIMEOUT: Duration = Duration::from_secs(6);
const START_AFTER_MODE: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, serde::Deserialize)]
#[serde(tag = "type", rename_all = "camelCase")]
pub enum UiCmd {
    /// Change target speed by `tenths` (may be negative).
    Adjust { tenths: i16 },
    StartStop,
    SetMode { mode: Mode },
    KeepRunning { enabled: bool },
}

#[derive(Debug, Clone, Serialize, PartialEq)]
#[serde(rename_all = "camelCase")]
pub struct Snapshot {
    pub connection: &'static str, // "searching" | "connecting" | "connected" | "no-bluetooth"
    pub device: Option<String>,
    pub belt: Option<Belt>,
    pub mode: Option<Mode>,
    pub speed: f32,
    pub target: Option<f32>,
    pub max_speed: f32,
    pub session_time_s: u32,
    pub session_distance_m: u32,
    pub session_steps: u32,
    pub today_steps: u64,
    pub today_distance_m: u64,
    pub today_active_s: u64,
    pub week: Vec<DaySummaryLite>,
    pub keep_running: bool,
}

#[derive(Debug, Clone, Serialize, PartialEq)]
pub struct DaySummaryLite {
    pub date: String,
    pub steps: u64,
}

impl From<&DaySummary> for DaySummaryLite {
    fn from(d: &DaySummary) -> Self {
        DaySummaryLite { date: d.date.clone(), steps: d.steps }
    }
}

pub type Listener = Arc<dyn Fn(&Snapshot) + Send + Sync>;

pub struct Worker {
    cmds: UnboundedReceiver<UiCmd>,
    shared: Arc<Mutex<Snapshot>>,
    listener: Listener,
    store: Store,
    tracker: SessionTracker,
    keep_running: bool,
}

impl Snapshot {
    pub fn initial(store: &Store) -> Snapshot {
        let today = stats::today();
        let d = store.day(today);
        Snapshot {
            connection: "searching",
            device: None,
            belt: None,
            mode: None,
            speed: 0.0,
            target: None,
            max_speed: protocol::DEFAULT_MAX_SPEED as f32 / 10.0,
            session_time_s: 0,
            session_distance_m: 0,
            session_steps: 0,
            today_steps: d.steps,
            today_distance_m: d.distance_m,
            today_active_s: d.active_s,
            week: store.recent(today, 7).iter().map(Into::into).collect(),
            keep_running: false,
        }
    }
}

/// Per-connection state.
struct Link {
    peripheral: Peripheral,
    write: Characteristic,
    status: Option<Status>,
    last_rx: Instant,
    max_speed: u8,
    start_speed: u8,
    /// Speed we last commanded and when.
    sent_target: Option<(u8, Instant)>,
    /// Speed the user wants; sent after the debounce deadline once the belt runs.
    pending: Option<(u8, Instant)>,
    last_step_change: Instant,
    blip_until: Option<Instant>,
    phantom_until: Option<Instant>,
    /// Deferred start after switching out of standby.
    start_after: Option<Instant>,
}

impl Link {
    async fn send(&mut self, cmd: Command) -> Result<()> {
        let bytes = protocol::encode(cmd);
        log::debug!("-> {cmd:?} {bytes:02x?}");
        self.peripheral.write(&self.write, &bytes, WriteType::WithoutResponse).await?;
        Ok(())
    }

    fn running(&self) -> bool {
        matches!(self.status.map(|s| s.belt), Some(Belt::Running))
    }

    fn target(&self) -> Option<u8> {
        self.pending.map(|p| p.0).or(self.sent_target.map(|t| t.0))
    }
}

impl Worker {
    pub fn new(cmds: UnboundedReceiver<UiCmd>, shared: Arc<Mutex<Snapshot>>, listener: Listener, store: Store) -> Self {
        Worker { cmds, shared, listener, store, tracker: SessionTracker::default(), keep_running: false }
    }

    fn publish(&mut self, link: Option<&Link>, connection: &'static str, device: Option<String>) {
        let today = stats::today();
        let d = self.store.day(today);
        let s = link.and_then(|l| l.status);
        let snap = Snapshot {
            connection,
            device,
            belt: s.map(|s| s.belt),
            mode: s.and_then(|s| s.mode),
            speed: s.map(|s| s.speed as f32 / 10.0).unwrap_or(0.0),
            target: link.and_then(|l| l.target()).map(|t| t as f32 / 10.0),
            max_speed: link.map(|l| l.max_speed).unwrap_or(protocol::DEFAULT_MAX_SPEED) as f32 / 10.0,
            session_time_s: s.map(|s| s.time_s).unwrap_or(0),
            session_distance_m: s.map(|s| s.dist_10m * 10).unwrap_or(0),
            session_steps: s.map(|s| s.steps).unwrap_or(0),
            today_steps: d.steps,
            today_distance_m: d.distance_m,
            today_active_s: d.active_s,
            week: self.store.recent(today, 7).iter().map(Into::into).collect(),
            keep_running: self.keep_running,
        };
        let changed = {
            let mut g = self.shared.lock().unwrap();
            let changed = *g != snap;
            *g = snap.clone();
            changed
        };
        if changed {
            (self.listener)(&snap);
        }
    }

    pub async fn run(mut self) {
        let adapter = loop {
            match first_adapter().await {
                Ok(a) => break a,
                Err(e) => {
                    log::warn!("no bluetooth adapter: {e:#}");
                    self.publish(None, "no-bluetooth", None);
                    self.idle(Duration::from_secs(3)).await;
                }
            }
        };
        loop {
            self.publish(None, "searching", None);
            match self.find(&adapter).await {
                Ok(p) => {
                    let name = p.properties().await.ok().flatten().and_then(|p| p.local_name);
                    self.publish(None, "connecting", name.clone());
                    if let Err(e) = self.session(p, name).await {
                        log::warn!("session ended: {e:#}");
                    }
                    self.tracker.reset_connection();
                    self.store.save_if_dirty();
                }
                Err(e) => log::debug!("scan: {e:#}"),
            }
            self.idle(Duration::from_secs(2)).await;
        }
    }

    /// Wait while draining UI commands (only the keep-running toggle matters offline).
    async fn idle(&mut self, d: Duration) {
        let deadline = tokio::time::Instant::now() + d;
        loop {
            tokio::select! {
                _ = tokio::time::sleep_until(deadline) => return,
                Some(c) = self.cmds.recv() => {
                    if let UiCmd::KeepRunning { enabled } = c { self.keep_running = enabled; }
                }
            }
        }
    }

    async fn find(&mut self, adapter: &Adapter) -> Result<Peripheral> {
        adapter.start_scan(ScanFilter { services: vec![protocol::SERVICE] }).await?;
        let deadline = Instant::now() + Duration::from_secs(10);
        let found = loop {
            for p in adapter.peripherals().await? {
                if let Ok(Some(props)) = p.properties().await {
                    let named = props.local_name.as_deref().is_some_and(|n| n.to_lowercase().contains("walkingpad"));
                    if named || props.services.contains(&protocol::SERVICE) {
                        log::info!("found pad {:?}", p.id());
                        let _ = adapter.stop_scan().await;
                        return Ok(p);
                    }
                }
            }
            if Instant::now() > deadline {
                break None;
            }
            self.idle(Duration::from_millis(500)).await;
        };
        let _ = adapter.stop_scan().await;
        found.ok_or_else(|| anyhow!("no WalkingPad found"))
    }

    async fn session(&mut self, p: Peripheral, name: Option<String>) -> Result<()> {
        p.connect().await.context("connect")?;
        let result = self.connected(&p, name).await;
        let _ = p.disconnect().await;
        result
    }

    async fn connected(&mut self, p: &Peripheral, name: Option<String>) -> Result<()> {
        p.discover_services().await?;
        let chars = p.characteristics();
        let notify = chars.iter().find(|c| c.uuid == protocol::NOTIFY_CHAR).cloned().context("no FE01")?;
        let write = chars.iter().find(|c| c.uuid == protocol::WRITE_CHAR).cloned().context("no FE02")?;
        p.subscribe(&notify).await?;
        let mut notes = p.notifications().await?;

        let now = Instant::now();
        let mut link = Link {
            peripheral: p.clone(),
            write,
            status: None,
            last_rx: now,
            max_speed: protocol::DEFAULT_MAX_SPEED,
            start_speed: 20,
            sent_target: None,
            pending: None,
            last_step_change: now,
            blip_until: None,
            phantom_until: None,
            start_after: None,
        };
        link.send(Command::AskPrefs).await?;

        let mut poll = interval(Duration::from_secs(1));
        poll.set_missed_tick_behavior(MissedTickBehavior::Delay);
        let mut tick = interval(Duration::from_millis(100));
        tick.set_missed_tick_behavior(MissedTickBehavior::Skip);
        let mut last_save = Instant::now();

        loop {
            tokio::select! {
                _ = poll.tick() => {
                    if link.last_rx.elapsed() > LINK_TIMEOUT {
                        return Err(anyhow!("pad stopped answering"));
                    }
                    link.send(Command::AskStatus).await?;
                }
                _ = tick.tick() => {
                    self.housekeeping(&mut link).await?;
                    if last_save.elapsed() > Duration::from_secs(10) {
                        self.store.save_if_dirty();
                        last_save = Instant::now();
                    }
                }
                n = notes.next() => {
                    let Some(n) = n else { return Err(anyhow!("notification stream closed")) };
                    if n.uuid == protocol::NOTIFY_CHAR {
                        link.last_rx = Instant::now();
                        self.on_message(&mut link, protocol::decode(&n.value)).await?;
                    }
                }
                c = self.cmds.recv() => {
                    let Some(c) = c else { return Ok(()) };
                    self.on_ui(&mut link, c).await?;
                }
            }
            self.publish(Some(&link), "connected", name.clone());
        }
    }

    async fn on_message(&mut self, link: &mut Link, m: Option<Message>) -> Result<()> {
        match m {
            Some(Message::Status(s)) => {
                let prev = link.status.replace(s);
                let now = Instant::now();
                let phantom = link.blip_until.is_some() || link.phantom_until.is_some_and(|t| now < t);
                let (delta, ended) = self.tracker.on_status(&s, phantom);
                self.store.add(stats::today(), delta);
                if ended {
                    link.send(Command::AskLastRecord).await?;
                }
                let was_running = matches!(prev.map(|p| p.belt), Some(Belt::Running));
                if prev.map(|p| p.steps) != Some(s.steps) && !phantom || (!was_running && s.belt == Belt::Running) {
                    link.last_step_change = now;
                }
                if !s.belt.is_moving_or_starting() {
                    // Forget commanded speed once the belt stops; keep a user preset (pending).
                    link.sent_target = None;
                    link.blip_until = None;
                }
            }
            Some(Message::Record(r)) => {
                let d = self.tracker.on_record(&r);
                self.store.add(stats::today(), d);
                self.store.save_if_dirty();
            }
            Some(Message::Prefs(p)) => {
                if p.max_speed >= protocol::MIN_SPEED {
                    link.max_speed = p.max_speed;
                }
                if p.start_speed >= protocol::MIN_SPEED {
                    link.start_speed = p.start_speed;
                }
            }
            Some(Message::Other(raw)) => log::debug!("<- other {raw:02x?}"),
            None => {}
        }
        Ok(())
    }

    async fn housekeeping(&mut self, link: &mut Link) -> Result<()> {
        let now = Instant::now();
        if let Some(at) = link.start_after {
            let ready = link.status.is_some_and(|s| s.belt == Belt::Idle && s.mode == Some(Mode::Manual));
            if now >= at && ready {
                link.start_after = None;
                link.send(Command::StartStopToggle).await?;
            } else if now >= at + Duration::from_secs(5) {
                log::warn!("pad did not leave standby; start abandoned");
                link.start_after = None;
            }
        }
        // End of a keep-alive blip: return to the target.
        if let Some(until) = link.blip_until {
            if now >= until {
                link.blip_until = None;
                link.phantom_until = Some(now + PHANTOM_TAIL);
                if let Some(t) = link.target() {
                    link.send(Command::SetSpeed(t)).await?;
                    link.sent_target = Some((t, now));
                }
            }
            return Ok(());
        }
        // Debounced speed change.
        if let Some((t, due)) = link.pending {
            if now >= due && link.running() && link.status.is_some_and(|s| s.speed > 0) {
                link.pending = None;
                if link.sent_target.map(|s| s.0) != Some(t) || link.status.is_some_and(|s| s.speed != t) {
                    link.send(Command::SetSpeed(t)).await?;
                }
                link.sent_target = Some((t, now));
            }
        }
        // Keep-alive.
        let manual = link.status.and_then(|s| s.mode) == Some(Mode::Manual);
        if self.keep_running && manual && link.running() && link.pending.is_none() && now.duration_since(link.last_step_change) > KEEPALIVE_AFTER {
            let base = link.target().unwrap_or_else(|| link.status.map(|s| s.speed).unwrap_or(link.start_speed));
            let bump = if base + BLIP <= link.max_speed { base + BLIP } else { base.saturating_sub(BLIP).max(protocol::MIN_SPEED) };
            if bump != base {
                log::info!("keep-alive blip {base} -> {bump}");
                link.sent_target.get_or_insert((base, now));
                link.send(Command::SetSpeed(bump)).await?;
                link.blip_until = Some(now + BLIP_DURATION);
                link.last_step_change = now;
            }
        }
        Ok(())
    }

    async fn on_ui(&mut self, link: &mut Link, c: UiCmd) -> Result<()> {
        let status = link.status;
        match c {
            UiCmd::KeepRunning { enabled } => self.keep_running = enabled,
            UiCmd::Adjust { tenths } => {
                let base = link.target().or_else(|| status.filter(|s| s.belt == Belt::Running).map(|s| s.speed)).unwrap_or(link.start_speed);
                let t = (base as i16 + tenths).clamp(protocol::MIN_SPEED as i16, link.max_speed as i16) as u8;
                link.pending = Some((t, Instant::now() + SPEED_DEBOUNCE));
            }
            UiCmd::SetMode { mode } => {
                if status.and_then(|s| s.mode) != Some(mode) {
                    link.send(Command::SetMode(mode)).await?;
                }
            }
            UiCmd::StartStop => {
                let Some(s) = status else { return Ok(()) };
                if s.belt.is_moving_or_starting() {
                    link.pending = None;
                    link.send(Command::StartStopToggle).await?;
                } else if s.mode == Some(Mode::Standby) {
                    // The pad ignores a start sent too soon after leaving standby.
                    link.send(Command::SetMode(Mode::Manual)).await?;
                    link.start_after = Some(Instant::now() + START_AFTER_MODE);
                } else {
                    link.send(Command::StartStopToggle).await?;
                }
            }
        }
        Ok(())
    }
}

async fn first_adapter() -> Result<Adapter> {
    let manager = Manager::new().await?;
    manager.adapters().await?.into_iter().next().ok_or_else(|| anyhow!("no adapter"))
}
