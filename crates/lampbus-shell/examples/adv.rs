//! Print everything a named device advertises, so a lamp definition author can see whether the advertisement
//! carries state. Read-only.
#![allow(clippy::unwrap_used, clippy::expect_used)]
use btleplug::api::{Central, Manager as _, Peripheral as _, ScanFilter};
use btleplug::platform::Manager;
use std::time::Duration;

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let name = std::env::args().nth(1).expect("device name");
    let secs: u64 = std::env::args()
        .nth(2)
        .and_then(|s| s.parse().ok())
        .unwrap_or(6);
    let central = Manager::new()
        .await
        .unwrap()
        .adapters()
        .await
        .unwrap()
        .remove(0);
    central.start_scan(ScanFilter::default()).await.unwrap();
    let mut seen = std::collections::BTreeSet::new();
    for _ in 0..secs * 4 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        for p in central.peripherals().await.unwrap() {
            let Some(pr) = p.properties().await.unwrap() else {
                continue;
            };
            if pr.local_name.as_deref() != Some(name.as_str()) {
                continue;
            }
            let line = format!(
                "mfr={:02x?} svc_data={:02x?} services={:?} tx={:?}",
                pr.manufacturer_data, pr.service_data, pr.services, pr.tx_power_level
            );
            if seen.insert(line.clone()) {
                println!("{line}  rssi={:?}", pr.rssi);
            }
        }
    }
    if seen.is_empty() {
        println!("(not advertising)");
    }
}
