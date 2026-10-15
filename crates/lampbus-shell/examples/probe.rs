//! Lamp probe: connect, list every GATT characteristic, subscribe to notifications on the control service, send
//! plaintext 16-byte frames (hex, AES-encrypted here) and print every decrypted reply.
//!
//!   cargo run -p lampbus-shell --example probe -- SSL-E089ED 54520057010100000000000000000000 ...
//!
//! Stop `lampbus serve` first: the lamp accepts one connection. Never writes to the OTA service (0000ae00).
#![allow(clippy::unwrap_used, clippy::expect_used, clippy::indexing_slicing)]

use aes::cipher::{Array, BlockCipherDecrypt, BlockCipherEncrypt, KeyInit};
use btleplug::api::{Central, CharPropFlags, Manager as _, Peripheral as _, ScanFilter, WriteType};
use btleplug::platform::Manager;
use futures::StreamExt;
use std::time::Duration;

const KEY: [u8; 16] = [
    0x34, 0x52, 0x2a, 0x5b, 0x7a, 0x6e, 0x49, 0x2c, 0x08, 0x09, 0x0a, 0x9d, 0x8d, 0x2a, 0x23, 0xf8,
];

fn hex(b: &[u8]) -> String {
    b.iter()
        .map(|x| format!("{x:02x}"))
        .collect::<Vec<_>>()
        .join(" ")
}

#[tokio::main(flavor = "current_thread")]
async fn main() {
    let mut args = std::env::args().skip(1);
    let name = args.next().expect("lamp name");
    let frames: Vec<[u8; 16]> = args
        .map(|h| {
            let v: Vec<u8> = (0..h.len() / 2)
                .map(|i| u8::from_str_radix(&h[2 * i..2 * i + 2], 16).unwrap())
                .collect();
            let mut f = [0u8; 16];
            f[..v.len()].copy_from_slice(&v);
            f
        })
        .collect();
    let cipher = aes::Aes128::new(&Array::from(KEY));

    let central = Manager::new()
        .await
        .unwrap()
        .adapters()
        .await
        .unwrap()
        .remove(0);
    central.start_scan(ScanFilter::default()).await.unwrap();
    let mut lamp = None;
    for _ in 0..40 {
        tokio::time::sleep(Duration::from_millis(250)).await;
        for p in central.peripherals().await.unwrap() {
            if p.properties()
                .await
                .unwrap()
                .and_then(|x| x.local_name)
                .as_deref()
                == Some(name.as_str())
            {
                lamp = Some(p);
            }
        }
        if lamp.is_some() {
            break;
        }
    }
    let lamp = lamp.expect("lamp not found");
    central.stop_scan().await.unwrap();
    lamp.connect().await.unwrap();
    lamp.discover_services().await.unwrap();
    println!("connected; services and characteristics:");
    for s in lamp.services() {
        println!("  service {}", s.uuid);
        for c in &s.characteristics {
            println!("    {}  {:?}", c.uuid, c.properties);
        }
    }
    // read what can be read (device information), outside the OTA service
    for c in lamp.characteristics() {
        if c.properties.contains(CharPropFlags::READ)
            && !c.service_uuid.to_string().starts_with("0000ae00")
        {
            match lamp.read(&c).await {
                Ok(v) => println!(
                    "read {} = {} {:?}",
                    c.uuid,
                    hex(&v),
                    String::from_utf8_lossy(&v)
                ),
                Err(e) => println!("read {} failed: {e}", c.uuid),
            }
        }
    }
    // subscribe to every notify/indicate characteristic except the OTA service
    for c in lamp.characteristics() {
        let notify = c
            .properties
            .intersects(CharPropFlags::NOTIFY | CharPropFlags::INDICATE);
        if notify && !c.service_uuid.to_string().starts_with("0000ae00") {
            println!("subscribing {}", c.uuid);
            lamp.subscribe(&c).await.unwrap();
        }
    }
    let mut notes = lamp.notifications().await.unwrap();
    let write = lamp
        .characteristics()
        .into_iter()
        .find(|c| c.uuid.to_string().starts_with("0000ac52"))
        .unwrap();
    for f in &frames {
        let mut b = Array::from(*f);
        cipher.encrypt_block(&mut b);
        println!("> {}   (encrypted {})", hex(f), hex(&b));
        lamp.write(&write, &b, WriteType::WithoutResponse)
            .await
            .unwrap();
        let until = tokio::time::Instant::now() + Duration::from_millis(2500);
        while let Ok(Some(n)) = tokio::time::timeout_at(until, notes.next()).await {
            let mut r = [0u8; 16];
            if n.value.len() == 16 {
                r.copy_from_slice(&n.value);
                let mut blk = Array::from(r);
                cipher.decrypt_block(&mut blk);
                println!("< {} raw {}  plain {}", n.uuid, hex(&n.value), hex(&blk));
            } else {
                println!("< {} {} bytes: {}", n.uuid, n.value.len(), hex(&n.value));
            }
        }
    }
    // listen a little longer for unsolicited notifications
    let until = tokio::time::Instant::now() + Duration::from_secs(3);
    while let Ok(Some(n)) = tokio::time::timeout_at(until, notes.next()).await {
        println!("< (unsolicited) {} {}", n.uuid, hex(&n.value));
    }
    lamp.disconnect().await.unwrap();
}
