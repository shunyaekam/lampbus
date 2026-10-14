//! Owns the one BLE connection to the lamp.
//!
//! Callers submit plaintext frames through a `watch` channel, so the newest pending frame wins
//! (stale ones are dropped, which is what animations and live streams want). Identical consecutive
//! frames are skipped, writes are paced, and the connection is opened on demand and dropped after an
//! idle period so the phone app can still connect when you want it.

use crate::protocol::{self, Frame};
use btleplug::api::{Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::{Adapter, Manager, Peripheral};
use std::time::Duration;
use tokio::sync::watch;
use tokio::time::{Instant, sleep, sleep_until, timeout};
use uuid::Uuid;

#[derive(Clone, Default)]
pub struct Status {
    pub connected: bool,
    pub last_error: Option<String>,
}

#[derive(Clone, Default)]
struct Pending {
    seq: u64,
    frame: Option<Frame>,
    force: bool,
}

/// Cheap handle the engine uses to talk to the device task.
#[derive(Clone)]
pub struct Device {
    frames: watch::Sender<Pending>,
    holds: watch::Sender<usize>,
    pub status: watch::Receiver<Status>,
}

impl Device {
    /// Queue a frame. Newest wins; `force` resends even if identical to the last one.
    pub fn submit(&self, frame: Frame, force: bool) {
        self.frames.send_modify(|p| {
            p.seq += 1;
            p.frame = Some(frame);
            p.force = force;
        });
    }

    /// Keep the connection open while the count is above zero (animations, streams).
    pub fn hold(&self, delta: isize) {
        self.holds.send_modify(|h| *h = h.saturating_add_signed(delta));
    }
}

pub struct Config {
    pub name: String,
    pub idle: Duration,
    pub min_interval: Duration,
}

pub fn spawn(cfg: Config) -> Device {
    let (frames, frames_rx) = watch::channel(Pending::default());
    let (holds, holds_rx) = watch::channel(0usize);
    let (status_tx, status) = watch::channel(Status::default());
    tokio::spawn(Task { cfg, frames_rx, holds_rx, status_tx, adapter: None, link: None, cached: None, last_sent: None, idle_at: None }.run());
    Device { frames, holds, status }
}

struct Link {
    peripheral: Peripheral,
    write: Characteristic,
}

struct Task {
    cfg: Config,
    frames_rx: watch::Receiver<Pending>,
    holds_rx: watch::Receiver<usize>,
    status_tx: watch::Sender<Status>,
    adapter: Option<Adapter>,
    link: Option<Link>,
    cached: Option<Peripheral>, // last known lamp, kept for fast reconnects
    last_sent: Option<Frame>,
    idle_at: Option<Instant>,
}

impl Task {
    async fn run(mut self) {
        loop {
            let idle = async {
                match self.idle_at {
                    Some(t) => sleep_until(t).await,
                    None => std::future::pending().await,
                }
            };
            tokio::select! {
                r = self.frames_rx.changed() => {
                    if r.is_err() { return; }
                    self.send_latest().await;
                }
                r = self.holds_rx.changed() => {
                    if r.is_err() { return; }
                    self.arm_idle();
                }
                _ = idle => {
                    eprintln!("lampd: idle for {:?}, disconnecting", self.cfg.idle);
                    self.disconnect().await;
                }
            }
        }
    }

    fn set_status(&self, connected: bool, err: Option<String>) {
        self.status_tx.send_replace(Status { connected, last_error: err });
    }

    fn arm_idle(&mut self) {
        let holds = *self.holds_rx.borrow();
        self.idle_at = (holds == 0 && !self.cfg.idle.is_zero() && self.link.is_some())
            .then(|| Instant::now() + self.cfg.idle);
    }

    async fn disconnect(&mut self) {
        self.idle_at = None;
        if let Some(link) = self.link.take() {
            let _ = link.peripheral.disconnect().await;
            self.cached = Some(link.peripheral);
        }
        let err = self.status_tx.borrow().last_error.clone(); // drop the read lock before sending
        self.set_status(false, err);
    }

    async fn send_latest(&mut self) {
        let mut backoff = Duration::from_secs(1);
        let mut pending = self.frames_rx.borrow_and_update().clone();
        if pending.force {
            self.last_sent = None;
        }
        loop {
            let Some(frame) = pending.frame else { return };
            if Some(frame) == self.last_sent {
                return;
            }
            match self.write(&frame).await {
                Ok(()) => {
                    self.last_sent = Some(frame);
                    self.set_status(true, None);
                    self.arm_idle();
                    sleep(self.cfg.min_interval).await;
                    return;
                }
                Err(e) => {
                    eprintln!("lampd: write failed: {e} (retry in {backoff:?})");
                    self.disconnect().await;
                    self.set_status(false, Some(e));
                    sleep(backoff).await;
                    backoff = (backoff * 2).min(Duration::from_secs(15));
                    if self.frames_rx.has_changed().unwrap_or(false) {
                        pending = self.frames_rx.borrow_and_update().clone(); // a newer frame supersedes
                    }
                }
            }
        }
    }

    async fn write(&mut self, frame: &Frame) -> Result<(), String> {
        if !self.link_alive().await {
            self.connect().await?;
        }
        let link = self.link.as_ref().ok_or("not connected")?;
        link.peripheral
            .write(&link.write, &protocol::encrypt(frame), WriteType::WithoutResponse)
            .await
            .map_err(|e| e.to_string())
    }

    async fn link_alive(&self) -> bool {
        match &self.link {
            Some(l) => l.peripheral.is_connected().await.unwrap_or(false),
            None => false,
        }
    }

    async fn adapter(&mut self) -> Result<Adapter, String> {
        if self.adapter.is_none() {
            let manager = Manager::new().await.map_err(|e| e.to_string())?;
            let adapter = manager.adapters().await.map_err(|e| e.to_string())?.into_iter().next()
                .ok_or("no Bluetooth adapter")?;
            self.adapter = Some(adapter);
        }
        Ok(self.adapter.clone().unwrap())
    }

    async fn connect(&mut self) -> Result<(), String> {
        // Cached peripheral first (fast reconnect), then a fresh scan.
        if let Some(p) = self.cached.take() {
            if let Ok(link) = self.open(p).await {
                return self.adopt(link);
            }
        }
        let adapter = self.adapter().await?;
        eprintln!("lampd: scanning for {}", self.cfg.name);
        adapter.start_scan(ScanFilter::default()).await.map_err(|e| e.to_string())?;
        let found = timeout(Duration::from_secs(15), async {
            loop {
                for p in adapter.peripherals().await.unwrap_or_default() {
                    let name = p.properties().await.ok().flatten().and_then(|pr| pr.local_name);
                    if name.as_deref() == Some(self.cfg.name.as_str()) {
                        return p;
                    }
                }
                sleep(Duration::from_millis(150)).await;
            }
        })
        .await;
        let _ = adapter.stop_scan().await;
        let peripheral = found.map_err(|_| format!("{} not found (is another app connected to it?)", self.cfg.name))?;
        let link = self.open(peripheral).await?;
        self.adopt(link)
    }

    async fn open(&self, peripheral: Peripheral) -> Result<Link, String> {
        timeout(Duration::from_secs(20), peripheral.connect()).await
            .map_err(|_| "connect timed out".to_string())?.map_err(|e| e.to_string())?;
        peripheral.discover_services().await.map_err(|e| e.to_string())?;
        let want = Uuid::parse_str(protocol::WRITE_CHAR).unwrap();
        let write = peripheral.characteristics().into_iter().find(|c| c.uuid == want)
            .ok_or("lamp write characteristic not found")?;
        Ok(Link { peripheral, write })
    }

    fn adopt(&mut self, link: Link) -> Result<(), String> {
        self.link = Some(link);
        self.last_sent = None; // lamp state is unknown after a reconnect
        eprintln!("lampd: connected to {}", self.cfg.name);
        self.set_status(true, None);
        Ok(())
    }
}
