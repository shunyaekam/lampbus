/*
 *
 *    Copyright (c) 2026 Project CHIP Authors
 *
 *    Licensed under the Apache License, Version 2.0 (the "License");
 *    you may not use this file except in compliance with the License.
 *    You may obtain a copy of the License at
 *
 *        http://www.apache.org/licenses/LICENSE-2.0
 *
 *    Unless required by applicable law or agreed to in writing, software
 *    distributed under the License is distributed on an "AS IS" BASIS,
 *    WITHOUT WARRANTIES OR CONDITIONS OF ANY KIND, either express or implied.
 *    See the License for the specific language governing permissions and
 *    limitations under the License.
 */

//! `lampbus-matter`: a Matter Extended Color Light whose "hardware" is a running `lampbus serve`.
//!
//! Derived from the `color_light` example of rs-matter (Apache-2.0, (c) Project CHIP Authors). On/Off, LevelControl
//! and ColorControl commands are forwarded to lampbus through its control language, so Apple Home, Google Home, Alexa
//! and Home Assistant can drive the lamp.
#![allow(clippy::uninlined_format_args)]
#![recursion_limit = "512"] // rs-matter's nested handler types are deep once a hook is async

use core::pin::pin;

use std::net::UdpSocket;

use embassy_futures::select::select3;

use async_signal::{Signal, Signals};

use log::info;

use futures_lite::StreamExt;

use rand::Rng;

use rs_matter::crypto::{default_crypto, Crypto};
use rs_matter::dm::clusters::app::color_control::{
    self, ColorCapabilitiesBitmap, ColorControlHooks, RgbGamma, SetDeviceColor,
};
use rs_matter::dm::clusters::app::level_control::{self, LevelControlHooks};
use rs_matter::dm::clusters::app::on_off::{self, OnOffHooks};
use rs_matter::dm::clusters::basic_info::BasicInfoConfig;
use rs_matter::dm::clusters::decl::color_control as color_control_cluster;
use rs_matter::dm::clusters::decl::level_control::{
    AttributeId, CommandId, OptionsBitmap, FULL_CLUSTER as LEVEL_CONTROL_FULL_CLUSTER,
};
use rs_matter::dm::clusters::decl::on_off as on_off_cluster;
use rs_matter::dm::clusters::decl::scenes_management::FULL_CLUSTER as SCENES_FULL_CLUSTER;
use rs_matter::dm::clusters::desc::{self, ClusterHandler as _};
use rs_matter::dm::clusters::groups::{self, ClusterHandler as _};
use rs_matter::dm::clusters::identify::{self, IdentifyHandler};
use rs_matter::dm::clusters::scenes::{SceneClusters, ScenesHandler, ScenesState};
use rs_matter::dm::devices::test::{DAC_PRIVKEY, TEST_DEV_ATT, TEST_DEV_COMM, TEST_DEV_DET};
use rs_matter::dm::devices::DEV_TYPE_EXTENDED_COLOR_LIGHT;
use rs_matter::dm::endpoints;
use rs_matter::dm::networks::eth::EthNetwork;
use rs_matter::dm::networks::SysNetifs;
use rs_matter::dm::{Async, Cluster, DataModel, Dataver, Endpoint, Node};
use rs_matter::error::Error;
use rs_matter::im::{EthInteractionModelState, InteractionModel};
use rs_matter::pairing::qr::QrTextType;
use rs_matter::pairing::DiscoveryCapabilities;
use rs_matter::respond::DefaultResponder;
use rs_matter::sc::pase::MAX_COMM_WINDOW_TIMEOUT_SECS;
use rs_matter::transport::exchange::MatterBuffers;
use rs_matter::transport::MATTER_SOCKET_BIND_ADDR;
use rs_matter::utils::select::Coalesce;
use rs_matter::{clusters, devices, root_endpoint, with, Matter, MATTER_PORT};

mod forward;
mod mdns;

/// How many scenes the scene table can hold, across all fabrics.
const SCENES_CAPACITY: usize = 16;

const HELP: &str = "lampbus-matter: expose lampbus lamps to Matter (Apple Home, Google Home, Alexa, Home Assistant)

USAGE  lampbus-matter [--lampbus host:port] [--device ALIAS] [--store DIR] [--name NAME]

  --lampbus host:port   the running `lampbus serve` (default 127.0.0.1:8787)
  --device ALIAS        control only this lamp (default: all lamps)
  --store DIR           where pairings are kept (default ~/.config/lampbus/matter)
  --name NAME           name shown when pairing (default Lamp); an existing pairing keeps its name,
                        rename it in the Home app

The API token is read from LAMPBUS_TOKEN or ~/.config/lampbus/token.
Pair with the setup code printed at start: this is a test-vendor device, so Apple Home will warn that it is uncertified.";

fn home() -> std::path::PathBuf {
    std::env::var_os("HOME").map_or_else(|| std::path::PathBuf::from("."), std::path::PathBuf::from)
}

fn main() -> Result<(), Error> {
    let (mut addr, mut device, mut store_dir) = (
        "127.0.0.1:8787".to_string(),
        None,
        home().join(".config/lampbus/matter"),
    );
    let mut name = "Lamp".to_string();
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match (a.as_str(), args.next()) {
            ("--lampbus", Some(v)) => addr = v,
            ("--device", Some(v)) => device = Some(v),
            ("--store", Some(v)) => store_dir = std::path::PathBuf::from(v),
            ("--name", Some(v)) => name = v.chars().take(32).collect(),
            _ => {
                println!("{HELP}");
                std::process::exit(if a == "-h" || a == "--help" { 0 } else { 2 });
            }
        }
    }
    let token = std::env::var("LAMPBUS_TOKEN")
        .ok()
        .or_else(|| {
            std::fs::read_to_string(home().join(".config/lampbus/token"))
                .ok()
                .map(|t| t.trim().to_string())
        })
        .unwrap_or_default();
    forward::start(addr.clone(), token.clone(), device.clone());
    let (lampbus_addr, forward_token, forward_device) = (addr, token, device);
    let _ = std::fs::create_dir_all(&store_dir);

    env_logger::init_from_env(
        env_logger::Env::default().filter_or(env_logger::DEFAULT_FILTER_ENV, "info"),
    );

    // the test identity, with our own names so the accessory is not called "MyTest"
    let name: &'static str = Box::leak(name.into_boxed_str());
    let details: &'static BasicInfoConfig<'static> = Box::leak(Box::new(BasicInfoConfig {
        vendor_name: "lampbus",
        product_name: "lampbus lamp",
        device_name: name,
        ..TEST_DEV_DET
    }));
    let matter = Matter::new(details, TEST_DEV_COMM, &TEST_DEV_ATT, MATTER_PORT);

    // Persistence
    let store = rs_matter::persist::DirKvBlobStore::new(store_dir);

    // Create the transport buffers
    let buffers: MatterBuffers = MatterBuffers::new();

    // Create the data model state (subscriptions, events, network store).
    let state: EthInteractionModelState = EthInteractionModelState::new(EthNetwork::new_default());

    // Bind the KV access object (the KV scratch buffer lives in `Matter`).
    let kv = matter.kv(store);

    // Re-hydrate the `Matter` instance (fabrics, basic info, RTC).
    matter.startup(&kv)?;

    // Create the crypto instance
    let crypto = default_crypto(rand::rng(), DAC_PRIVKEY);

    let mut rand = crypto.rand()?;

    // The scene table, shared by the Scenes Management cluster and by the
    // three scene-aware handlers below, which flip `SceneValid` through it
    // whenever a command changes a captured attribute. It persists itself
    // under `rs_matter::persist::SCENES_KEY`.
    let scenes_state = ScenesState::<SCENES_CAPACITY>::new();

    // OnOff cluster setup
    let on_off_handler = on_off::OnOffHandler::new(
        Dataver::new_rand(&mut rand),
        1,
        rs_matter::persist::VENDOR_KEYS_START + 0x10,
        OnOffDeviceLogic::new(),
    )
    .with_scene_invalidator(&scenes_state);

    // LevelControl cluster setup
    let level_control_handler = level_control::LevelControlHandler::new(
        Dataver::new_rand(&mut rand),
        1,
        rs_matter::persist::VENDOR_KEYS_START + 0x11,
        LevelControlDeviceLogic::new(),
    )
    .with_scene_invalidator(&scenes_state);

    // ColorControl cluster setup
    let color_control_handler = color_control::ColorControlHandler::new(
        Dataver::new_rand(&mut rand),
        1,
        rs_matter::persist::VENDOR_KEYS_START + 0x12,
        ColorControlDeviceLogic::new(),
    )
    .with_scene_invalidator(&scenes_state);

    // Cluster wiring, validation and initialisation.
    // ColorControl is coupled to the same OnOff handler, so that the
    // `Options.EXECUTE_IF_OFF` gating of the colour commands works.
    on_off_handler.init(Some(&level_control_handler));
    level_control_handler.init(Some(&on_off_handler));
    color_control_handler.init(Some(&on_off_handler));

    // Scenes Management cluster setup - mandatory for the Extended Color
    // Light device type. The registry lists the handlers whose attributes
    // a scene captures and recalls.
    let scenes_handler = ScenesHandler::new(
        Dataver::new_rand(&mut rand),
        &scenes_state,
        (
            &on_off_handler,
            (&level_control_handler, (&color_control_handler, ())),
        ),
    );

    // Create the Data Model instance
    let im = InteractionModel::new(
        &matter,
        &crypto,
        &buffers,
        data_model(
            rand,
            &on_off_handler,
            &level_control_handler,
            &color_control_handler,
            scenes_handler,
        ),
        &kv,
        &state,
    );

    // Bring the Data Model to its operational state: re-hydrate its persisted
    // state and deliver the `Startup` lifecycle op to all cluster handlers.
    futures_lite::future::block_on(im.startup())?;
    // the stack has replayed its stored defaults through the hooks by now; from here on, commands are real
    forward::enable();
    forward::follow(lampbus_addr, forward_token, forward_device);

    // Create a default responder capable of handling up to 3 subscriptions
    // All other subscription requests will be turned down with "resource exhausted"
    let responder = DefaultResponder::new(&im);

    // Run the responder with up to 4 handlers (i.e. 4 exchanges can be handled simultaneously)
    // Clients trying to open more exchanges than the ones currently running will get "I'm busy, please try again later"
    let mut respond = pin!(responder.run::<4, 4>());

    // Run the background job of the data model
    let mut im_job = pin!(im.run());

    let socket = async_io::Async::<UdpSocket>::bind(MATTER_SOCKET_BIND_ADDR)?;

    // Run the Matter and mDNS transports
    let mut mdns = pin!(mdns::run_mdns(&matter, &crypto));
    let mut transport = pin!(matter.run(&crypto, &socket, &socket, &socket));

    // We need to always print the QR text, because the test runner expects it to be printed
    // even if the device is already commissioned
    matter.print_standard_qr_text(DiscoveryCapabilities::IP)?;

    if !matter.has_fabrics() {
        // If the device is not commissioned yet, print the QR code to the console
        // and enable basic commissioning

        matter.print_standard_qr_code(QrTextType::Unicode, DiscoveryCapabilities::IP)?;

        matter.open_basic_comm_window(MAX_COMM_WINDOW_TIMEOUT_SECS, &crypto, &())?;
    }

    // Listen to SIGTERM (or Ctrl-C on Windows, where SIGTERM is not
    // supported by `async-signal`) because at the end of the test we'll
    // receive it.
    #[cfg(not(windows))]
    let mut term_signal = Signals::new([Signal::Term])?;
    #[cfg(windows)]
    let mut term_signal = Signals::new([Signal::Int])?;
    let mut term = pin!(async {
        term_signal.next().await;
        Ok(())
    });

    // Combine all async tasks in a single one
    let all = select3(
        &mut transport,
        &mut mdns,
        select3(&mut respond, &mut im_job, &mut term).coalesce(),
    );

    // Run with a simple `block_on`. Any local executor would do.
    futures_lite::future::block_on(all.coalesce())
}

/// The Node meta-data describing our Matter device.
const NODE: Node<'static> = Node {
    endpoints: &[
        root_endpoint!(eth),
        Endpoint::new(
            1,
            devices!(DEV_TYPE_EXTENDED_COLOR_LIGHT),
            clusters!(
                desc::DescHandler::CLUSTER,
                identify::CLUSTER,
                groups::GroupsHandler::CLUSTER,
                OnOffDeviceLogic::CLUSTER,
                LevelControlDeviceLogic::CLUSTER,
                ColorControlDeviceLogic::CLUSTER,
                SCENES_FULL_CLUSTER,
            ),
        ),
    ],
};

/// The Data Model handler + meta-data for our Matter device.
/// The handler is the root endpoint 0 handler plus the light endpoint handlers.
fn data_model<'a, LH: LevelControlHooks, OH: OnOffHooks, CH: ColorControlHooks, R>(
    mut rand: impl Rng + Copy,
    on_off: &'a on_off::OnOffHandler<'a, OH, LH>,
    level_control: &'a level_control::LevelControlHandler<'a, LH, OH>,
    color_control: &'a color_control::ColorControlHandler<'a, CH, OH, LH>,
    scenes: ScenesHandler<'a, SCENES_CAPACITY, R>,
) -> impl DataModel + 'a
where
    R: SceneClusters + 'a,
{
    (
        NODE,
        endpoints::EthSysHandlerBuilder::new()
            .netif_diag(&SysNetifs)
            .build(rand)
            .chain(
                |e, c| e == 1 && c == desc::DescHandler::CLUSTER.id,
                Async(desc::DescHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e == 1 && c == identify::CLUSTER.id,
                Async(IdentifyHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e == 1 && c == groups::GroupsHandler::CLUSTER.id,
                Async(groups::GroupsHandler::new(Dataver::new_rand(&mut rand)).adapt()),
            )
            .chain(
                |e, c| e == 1 && c == OnOffDeviceLogic::CLUSTER.id,
                Async(on_off::HandlerAdaptor(on_off)),
            )
            .chain(
                |e, c| e == 1 && c == LevelControlDeviceLogic::CLUSTER.id,
                Async(level_control::HandlerAdaptor(level_control)),
            )
            .chain(
                |e, c| e == 1 && c == ColorControlDeviceLogic::CLUSTER.id,
                Async(color_control::HandlerAdaptor(color_control)),
            )
            .chain(|e, c| e == 1 && c == SCENES_FULL_CLUSTER.id, scenes.adapt()),
    )
}

// Implementing the ColorControl business logic
//
// The cluster handler owns and persists every colour attribute, runs the
// transitions and picks the colour space to drive the device in; the device
// logic only has to push the colour onto the hardware.
#[derive(Default)]
pub struct ColorControlDeviceLogic;

impl ColorControlDeviceLogic {
    pub const fn new() -> Self {
        Self
    }
}

impl ColorControlHooks for ColorControlDeviceLogic {
    // An RGB LED has no dedicated white channel, so colour temperature looks
    // like a feature to leave out - but it seems a colour light cannot be
    // modelled without it: `Extended Color Light` makes `COLOR_TEMPERATURE`
    // mandatory, and some ecosystems (Google Home, for one) appear to go by
    // the device type rather than by `FeatureMap`, sending
    // `MoveToColorTemperature` regardless. Enabling it is free anyway: the
    // cluster converts the mireds into a colour `set_device_color` can drive.
    const CLUSTER: Cluster<'static> = color_control_cluster::FULL_CLUSTER
        .with_features(
            color_control_cluster::Feature::HUE_AND_SATURATION.bits()
                | color_control_cluster::Feature::XY.bits()
                | color_control_cluster::Feature::COLOR_TEMPERATURE.bits(),
        )
        .with_attrs(with!(
            required;
            color_control_cluster::AttributeId::CurrentHue
                | color_control_cluster::AttributeId::CurrentSaturation
                | color_control_cluster::AttributeId::CurrentX
                | color_control_cluster::AttributeId::CurrentY
                | color_control_cluster::AttributeId::RemainingTime
                | color_control_cluster::AttributeId::ColorTemperatureMireds
                | color_control_cluster::AttributeId::ColorTempPhysicalMinMireds
                | color_control_cluster::AttributeId::ColorTempPhysicalMaxMireds
                | color_control_cluster::AttributeId::StartUpColorTemperatureMireds
        ))
        .with_cmds(with!(
            color_control_cluster::CommandId::MoveToHue
                | color_control_cluster::CommandId::MoveHue
                | color_control_cluster::CommandId::StepHue
                | color_control_cluster::CommandId::MoveToSaturation
                | color_control_cluster::CommandId::MoveSaturation
                | color_control_cluster::CommandId::StepSaturation
                | color_control_cluster::CommandId::MoveToHueAndSaturation
                | color_control_cluster::CommandId::MoveToColor
                | color_control_cluster::CommandId::MoveColor
                | color_control_cluster::CommandId::StepColor
                | color_control_cluster::CommandId::MoveToColorTemperature
                | color_control_cluster::CommandId::MoveColorTemperature
                | color_control_cluster::CommandId::StepColorTemperature
                | color_control_cluster::CommandId::StopMoveStep
        ));

    // Mirrors the features enabled above.
    const COLOR_CAPABILITIES: ColorCapabilitiesBitmap = ColorCapabilitiesBitmap::from_bits_truncate(
        ColorCapabilitiesBitmap::HUE_SATURATION.bits()
            | ColorCapabilitiesBitmap::XY.bits()
            | ColorCapabilitiesBitmap::COLOR_TEMPERATURE.bits(),
    );

    // The colour-temperature range we accept, i.e. 6535K to 2000K.
    const COLOR_TEMP_PHYSICAL_MIN_MIREDS: u16 = 153;
    const COLOR_TEMP_PHYSICAL_MAX_MIREDS: u16 = 500;

    // Come up in saturated red until the handler has persisted a colour.
    const COLOR: Option<SetDeviceColor> = Some(SetDeviceColor::HueSaturation {
        enhanced_hue: 0,
        saturation: 254,
    });

    fn set_device_color(&self, target: SetDeviceColor) -> Result<(), ()> {
        // This is where business logic is implemented to physically change
        // the colour of the device. The level (brightness) comes separately,
        // from LevelControl. A colour temperature arrives here as its point
        // on the Planckian locus, i.e. the LED approximates white the way
        // RGB-only bulbs do.
        //
        // The two triplets below are the same colour in two encodings, and
        // they are far apart at the dim end - 2000K is linear (255, 66, 2)
        // but sRGB (255, 139, 22) - so never read one as if it were the
        // other. `linear` is proportional to light output and is what drives
        // a PWM duty cycle, i.e. what a real device would use here. `srgb` is
        // display-encoded, and is the one to compare against a colour picker
        // or a screenshot of a controller's UI.
        let linear = target.to_rgb(RgbGamma::Linear);
        let srgb = target.to_rgb(RgbGamma::SRgb);

        info!(
            "ColorControlDeviceLogic: setting color to {:?} (linear RGB {}, {}, {} / sRGB {}, {}, {})",
            target, linear.0, linear.1, linear.2, srgb.0, srgb.1, srgb.2
        );

        // lampbus colours are display-encoded (sRGB) with brightness as a separate dimension
        forward::color([srgb.0, srgb.1, srgb.2]);
        Ok(())
    }
}

// Implementing the LevelControl business logic
//
// The cluster handler owns and persists every attribute, including `CurrentLevel`;
// the device logic only drives the hardware.
pub struct LevelControlDeviceLogic;

impl Default for LevelControlDeviceLogic {
    fn default() -> Self {
        Self::new()
    }
}

impl LevelControlDeviceLogic {
    pub const fn new() -> Self {
        Self
    }
}

impl LevelControlHooks for LevelControlDeviceLogic {
    const MIN_LEVEL: u8 = 1;
    const MAX_LEVEL: u8 = 254;
    const FASTEST_RATE: u8 = 50;
    const ON_LEVEL: Option<u8> = None; // turning on keeps the last brightness (the example forces 42)
    const OPTIONS: OptionsBitmap = OptionsBitmap::EXECUTE_IF_OFF;
    const CURRENT_LEVEL: Option<u8> = Some(1);
    const CLUSTER: Cluster<'static> = LEVEL_CONTROL_FULL_CLUSTER
        .with_features(
            level_control::Feature::LIGHTING.bits() | level_control::Feature::ON_OFF.bits(),
        )
        .with_attrs(with!(
            required;
            AttributeId::CurrentLevel
            | AttributeId::RemainingTime
            | AttributeId::MinLevel
            | AttributeId::MaxLevel
            | AttributeId::OnOffTransitionTime
            | AttributeId::OnLevel
            | AttributeId::OnTransitionTime
            | AttributeId::OffTransitionTime
            | AttributeId::DefaultMoveRate
            | AttributeId::Options
            | AttributeId::StartUpCurrentLevel
        ))
        .with_cmds(with!(
            CommandId::MoveToLevel
                | CommandId::Move
                | CommandId::Step
                | CommandId::Stop
                | CommandId::MoveToLevelWithOnOff
                | CommandId::MoveWithOnOff
                | CommandId::StepWithOnOff
                | CommandId::StopWithOnOff
        ));

    fn set_device_level(&self, level: u8) -> Result<Option<u8>, ()> {
        // This is where business logic is implemented to physically change the level of the device.
        info!("LevelControlDeviceLogic: setting level to {}", level);
        forward::level(level);
        Ok(Some(level))
    }
}

// Implementing the OnOff business logic
//
// The cluster handler owns and persists the `OnOff` attribute; the device logic
// only drives the hardware.
#[derive(Default)]
pub struct OnOffDeviceLogic;

impl OnOffDeviceLogic {
    pub const fn new() -> Self {
        Self
    }
}

impl OnOffHooks for OnOffDeviceLogic {
    const CLUSTER: Cluster<'static> = on_off_cluster::FULL_CLUSTER
        .with_revision(6)
        .with_features(on_off_cluster::Feature::LIGHTING.bits())
        .with_attrs(with!(
            required;
            on_off_cluster::AttributeId::OnOff
            | on_off_cluster::AttributeId::GlobalSceneControl
            | on_off_cluster::AttributeId::OnTime
            | on_off_cluster::AttributeId::OffWaitTime
            | on_off_cluster::AttributeId::StartUpOnOff
        ))
        .with_cmds(with!(
            on_off_cluster::CommandId::Off
                | on_off_cluster::CommandId::On
                | on_off_cluster::CommandId::Toggle
                | on_off_cluster::CommandId::OffWithEffect
                | on_off_cluster::CommandId::OnWithRecallGlobalScene
                | on_off_cluster::CommandId::OnWithTimedOff
        ));

    fn set_on_off(&self, on: bool) {
        // This is where business logic is implemented to physically switch the device.
        info!("OnOff state set to: {}", on);
        forward::power(on);
    }

    async fn handle_off_with_effect(&self, _effect: on_off::EffectVariantEnum) {
        // no effect
    }

    // The lamp can be switched without us (lampbus CLI, schedules, the vendor app's timers, another controller):
    // follow it, without driving it, so Siri and the Home app always act on the real state.
    async fn run<F: Fn(on_off::OutOfBandMessage)>(&self, notify: F) {
        loop {
            let on = forward::lamp_state_change().await;
            info!("lamp is {}", if on { "on" } else { "off" });
            notify(on_off::OutOfBandMessage::Update(on));
        }
    }
}
