//! Desktop shell: runs a [`lampbus_core::Core`] on tokio and carries out its actions with `btleplug`.
//!
//! The shell is deliberately thin. It owns no policy: it turns [`Action`]s into Bluetooth calls, turns Bluetooth
//! events into [`LinkEvent`]s, wakes the core when it asked to be woken, and publishes a [`Snapshot`] whenever
//! something changes.

pub mod inspect;

use btleplug::api::{
    Central, CentralEvent, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType,
};
use btleplug::platform::{Adapter, Manager, Peripheral};
use futures::StreamExt;
use lampbus_core::{Action, Command, Core, Error, LinkEvent, Ms, Snapshot, Target};
use lampbus_defs::WriteMode;
use std::collections::HashMap;
use std::time::Duration;
use tokio::sync::{mpsc, oneshot, watch};
use tokio::time::{sleep_until, timeout, Instant};
use uuid::Uuid;

enum Msg {
    Command {
        target: Target,
        cmd: Command,
        reply: oneshot::Sender<Result<usize, Error>>,
    },
    Stream {
        target: Target,
        rgb: [f64; 3],
        brightness: f64,
    },
    Discover,
}

/// A cheap, cloneable handle to a running shell.
#[derive(Clone)]
pub struct Handle {
    tx: mpsc::Sender<Msg>,
    snap: watch::Receiver<Snapshot>,
}

impl Handle {
    /// Apply a command and report how many lamps it applied to.
    ///
    /// # Errors
    /// The core's [`Error`], or [`Error::Bad`] if the shell has stopped.
    pub async fn command(&self, target: Target, cmd: Command) -> Result<usize, Error> {
        let (reply, rx) = oneshot::channel();
        self.tx
            .send(Msg::Command { target, cmd, reply })
            .await
            .map_err(|_| Error::Bad("shell stopped".into()))?;
        rx.await.map_err(|_| Error::Bad("shell stopped".into()))?
    }

    /// Push one frame of a live source. Fire-and-forget: dropped if the shell is busy.
    pub fn stream(&self, target: Target, rgb: [f64; 3], brightness: f64) {
        let _ = self.tx.try_send(Msg::Stream {
            target,
            rgb,
            brightness,
        });
    }

    /// Scan for lamps.
    pub async fn discover(&self) {
        let _ = self.tx.send(Msg::Discover).await;
    }

    /// The latest snapshot.
    pub fn snapshot(&self) -> Snapshot {
        self.snap.borrow().clone()
    }

    /// A receiver that wakes whenever the snapshot changes.
    pub fn subscribe(&self) -> watch::Receiver<Snapshot> {
        self.snap.clone()
    }

    /// Wait until the named lamps (all lamps if `aliases` is empty) have received everything: link ready and
    /// nothing pending. Returns the final snapshot, or the problem if it takes longer than `limit`.
    ///
    /// # Errors
    /// A message naming the lamp and its last error on timeout.
    pub async fn settled(&self, aliases: &[String], limit: Duration) -> Result<Snapshot, String> {
        let mut rx = self.snap.clone();
        let deadline = Instant::now() + limit;
        loop {
            let s = rx.borrow().clone();
            let relevant: Vec<_> = s
                .lamps
                .iter()
                .filter(|l| {
                    aliases.is_empty() || aliases.iter().any(|a| a.eq_ignore_ascii_case(&l.alias))
                })
                .collect();
            // a playing animation or stream always has a frame in flight: it is delivered when the link is up
            let delivered = |l: &&lampbus_core::LampState| {
                l.connected
                    && !l.fading
                    && (l.pending == 0 || matches!(l.source, "animation" | "stream"))
            };
            if !relevant.is_empty() && relevant.iter().all(delivered) {
                return Ok(s);
            }
            if timeout(
                deadline.saturating_duration_since(Instant::now()),
                rx.changed(),
            )
            .await
            .is_err()
            {
                let why = relevant
                    .iter()
                    .map(|l| {
                        format!(
                            "{} is {} ({})",
                            l.alias,
                            l.link,
                            l.last_error.as_deref().unwrap_or("no error reported")
                        )
                    })
                    .collect::<Vec<_>>()
                    .join("; ");
                return Err(if why.is_empty() {
                    "no matching lamp".into()
                } else {
                    why
                });
            }
        }
    }
}

/// Start the shell on the current tokio runtime.
pub fn start(core: Core) -> Handle {
    let (tx, rx) = mpsc::channel(256);
    let epoch = Instant::now();
    let (snap_tx, snap) = watch::channel(core.snapshot(0));
    tokio::spawn(run(core, rx, snap_tx, epoch));
    Handle { tx, snap }
}

struct Radio {
    adapter: Adapter,
    known: HashMap<String, Peripheral>,
    chars: HashMap<(String, Uuid), Characteristic>,
    last_adv: HashMap<String, (Option<String>, usize)>,
    scanning: bool,
}

async fn find_adapter() -> Result<Adapter, String> {
    let manager = Manager::new()
        .await
        .map_err(|e| format!("bluetooth unavailable: {e}"))?;
    manager
        .adapters()
        .await
        .map_err(|e| e.to_string())?
        .into_iter()
        .next()
        .ok_or_else(|| "no bluetooth adapter".to_string())
}

fn ms(epoch: Instant) -> Ms {
    Instant::now().duration_since(epoch).as_millis() as Ms
}

async fn run(
    mut core: Core,
    mut rx: mpsc::Receiver<Msg>,
    snap_tx: watch::Sender<Snapshot>,
    epoch: Instant,
) {
    let (itx, mut irx) = mpsc::unbounded_channel::<LinkEvent>();
    let adapter = find_adapter().await;
    let mut radio = adapter.as_ref().ok().map(|a| Radio {
        adapter: a.clone(),
        known: HashMap::new(),
        chars: HashMap::new(),
        last_adv: HashMap::new(),
        scanning: false,
    });
    let mut events = match &adapter {
        Ok(a) => a.events().await.ok(),
        Err(_) => None,
    };
    let radio_error = adapter.err();

    loop {
        while let Some(action) = core.poll_action() {
            exec(
                action,
                radio.as_mut(),
                radio_error.as_deref(),
                &itx,
                &mut core,
                epoch,
            )
            .await;
        }
        if core.take_changed() {
            snap_tx.send_replace(core.snapshot(ms(epoch)));
        }
        let wake = core
            .poll_timeout(ms(epoch))
            .map(|t| epoch + Duration::from_millis(t));
        tokio::select! {
            msg = rx.recv() => match msg {
                None => break,
                Some(Msg::Command { target, cmd, reply }) => {
                    let result = core.handle_command(ms(epoch), &target, cmd);
                    if core.take_changed() {
                        snap_tx.send_replace(core.snapshot(ms(epoch)));
                    }
                    let _ = reply.send(result);
                }
                Some(Msg::Stream { target, rgb, brightness }) => {
                    let _ = core.handle_command(ms(epoch), &target, Command::Stream { rgb, brightness });
                }
                Some(Msg::Discover) => core.discover(ms(epoch)),
            },
            ev = async { match events.as_mut() { Some(s) => s.next().await, None => std::future::pending().await } } => {
                match ev {
                    Some(e) => on_central_event(e, radio.as_mut(), &mut core, epoch).await,
                    None => events = None,
                }
            }
            Some(ev) = irx.recv() => {
                if let (LinkEvent::Disconnected { id } | LinkEvent::WriteFailed { id, .. }, Some(r)) = (&ev, radio.as_mut()) {
                    r.chars.retain(|(d, _), _| d != id);
                }
                core.handle_link(ms(epoch), ev);
            }
            _ = async { match wake { Some(t) => sleep_until(t).await, None => std::future::pending().await } } => {
                core.handle_timeout(ms(epoch));
            }
        }
    }
}

async fn on_central_event(
    ev: CentralEvent,
    radio: Option<&mut Radio>,
    core: &mut Core,
    epoch: Instant,
) {
    let Some(radio) = radio else { return };
    match ev {
        CentralEvent::DeviceDisconnected(id) => {
            let id = id.to_string();
            radio.chars.retain(|(d, _), _| *d != id);
            core.handle_link(ms(epoch), LinkEvent::Disconnected { id });
        }
        CentralEvent::DeviceDiscovered(id)
        | CentralEvent::DeviceUpdated(id)
        | CentralEvent::ServicesAdvertisement { id, .. } => {
            let seen = if radio.scanning {
                radio.adapter.peripheral(&id).await.ok()
            } else {
                None
            };
            if let Some(p) = seen {
                advertise(radio, &p, core, epoch).await;
            }
        }
        _ => {}
    }
}

async fn advertise(radio: &mut Radio, p: &Peripheral, core: &mut Core, epoch: Instant) {
    let id = p.id().to_string();
    let props = p.properties().await.ok().flatten();
    let name = props.as_ref().and_then(|x| {
        x.local_name
            .clone()
            .or_else(|| x.advertisement_name.clone())
    });
    let services: Vec<String> = props
        .map(|x| x.services.iter().map(Uuid::to_string).collect())
        .unwrap_or_default();
    radio.known.insert(id.clone(), p.clone());
    let fingerprint = (name.clone(), services.len());
    if radio.last_adv.get(&id) != Some(&fingerprint) {
        radio.last_adv.insert(id.clone(), fingerprint);
        core.handle_link(ms(epoch), LinkEvent::Advertised { id, name, services });
    }
}

async fn exec(
    action: Action,
    radio: Option<&mut Radio>,
    radio_error: Option<&str>,
    itx: &mpsc::UnboundedSender<LinkEvent>,
    core: &mut Core,
    epoch: Instant,
) {
    let Some(radio) = radio else {
        // no Bluetooth at all: fail every connect so the problem shows up on the lamp
        if let Action::Connect { device } = action {
            let _ = itx.send(LinkEvent::ConnectFailed {
                id: device,
                reason: radio_error.unwrap_or("no bluetooth").into(),
            });
        }
        return;
    };
    match action {
        Action::Scan => {
            radio.scanning = true;
            radio.last_adv.clear();
            if radio
                .adapter
                .start_scan(ScanFilter::default())
                .await
                .is_ok()
            {
                for p in radio.adapter.peripherals().await.unwrap_or_default() {
                    advertise(radio, &p, core, epoch).await;
                }
            }
        }
        Action::StopScan => {
            radio.scanning = false;
            let _ = radio.adapter.stop_scan().await;
        }
        Action::Connect { device } => {
            let Some(p) = radio.known.get(&device).cloned() else {
                let _ = itx.send(LinkEvent::ConnectFailed {
                    id: device,
                    reason: "device not seen yet".into(),
                });
                return;
            };
            let itx = itx.clone();
            tokio::spawn(async move {
                let result = async {
                    timeout(Duration::from_secs(20), p.connect())
                        .await
                        .map_err(|_| "connect timed out".to_string())?
                        .map_err(|e| e.to_string())?;
                    p.discover_services().await.map_err(|e| e.to_string())
                }
                .await;
                let _ = itx.send(match result {
                    Ok(()) => LinkEvent::Connected { id: device },
                    Err(reason) => LinkEvent::ConnectFailed { id: device, reason },
                });
            });
        }
        Action::Disconnect { device } => {
            radio.chars.retain(|(d, _), _| *d != device);
            if let Some(p) = radio.known.get(&device).cloned() {
                tokio::spawn(async move {
                    let _ = p.disconnect().await;
                });
            }
        }
        Action::Write {
            device,
            characteristic,
            mode,
            data,
            ..
        } => {
            let fail = |reason: String| {
                let _ = itx.send(LinkEvent::WriteFailed {
                    id: device.clone(),
                    reason,
                });
            };
            let (Some(p), Ok(uuid)) = (
                radio.known.get(&device).cloned(),
                Uuid::parse_str(&characteristic),
            ) else {
                return fail("unknown device or characteristic".into());
            };
            let key = (device.clone(), uuid);
            if !radio.chars.contains_key(&key) {
                match p.characteristics().into_iter().find(|c| c.uuid == uuid) {
                    Some(c) => {
                        radio.chars.insert(key.clone(), c);
                    }
                    None => {
                        return fail(format!(
                            "characteristic {characteristic} not found on the lamp"
                        ))
                    }
                }
            }
            let Some(c) = radio.chars.get(&key) else {
                return;
            };
            let wt = if mode == WriteMode::WithResponse {
                WriteType::WithResponse
            } else {
                WriteType::WithoutResponse
            };
            match timeout(Duration::from_secs(3), p.write(c, &data, wt)).await {
                Ok(Ok(())) => {}
                Ok(Err(e)) => fail(e.to_string()),
                Err(_) => fail("write timed out".into()),
            }
        }
    }
}
