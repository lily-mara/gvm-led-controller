use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use crate::{
    gui::{LightGuiState, LightMode, LightSettingsState},
    protocol::{ColorTemperatureCommand, ModeCommand, PowerCommand},
};
use async_stream::stream;
use btleplug::{
    api::{Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType},
    platform::{Adapter, Manager, Peripheral},
};
use eyre::{bail, Result};
use futures::{pin_mut, stream::StreamExt, Stream};
use tokio::{sync::mpsc::channel, time::sleep};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{error, info, trace, warn};

use crate::protocol::{HsiCommand, Packable};

const SERVICE_UUID: uuid::Uuid = uuid::Uuid::from_bytes([
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x19, 0x10,
]);

// const CMD_INIT_SESSION: &[u8] = &[
//     0x4c, 0x54, 0x09, 0x00, 0x00, 0x53, 0x00, 0x00, 0x01, 0x00, 0x94, 0x74,
// ];

/// Useful for debugging GUI when there are no lights available to connect to -
/// sits around doing nothing for a few seconds then slowly yields lights.
/// Prints out commands sent to the light.
pub(crate) async fn fake_scan_and_spawn(lights: Arc<Mutex<Vec<LightGuiState>>>) {
    sleep(Duration::from_secs(1)).await;
    lights.lock().unwrap().push(fake_device(1));

    sleep(Duration::from_secs(5)).await;
    lights.lock().unwrap().push(fake_device(2));

    sleep(Duration::from_secs(10)).await;
    lights.lock().unwrap().push(fake_device(3));
}

fn fake_device(id: u32) -> LightGuiState {
    let (tx, rx) = channel(10);
    let gui_state = LightGuiState::new(format!("LED {id}"), tx);

    let mut rx = debounced::debounced(ReceiverStream::new(rx), Duration::from_millis(100));
    tokio::spawn(async move {
        while let Some(state) = rx.next().await {
            info!(id, ?state, "recieved state");
        }
    });

    gui_state
}

/// Run a loop that continuously scans for new compatible LEDs, spawns
/// connection managers for those lights, and adds them to the GUI.
pub(crate) async fn scan_and_spawn(lights: Arc<Mutex<Vec<LightGuiState>>>) {
    let device_stream = scan_forever();
    pin_mut!(device_stream);

    while let Some(led) = device_stream.next().await {
        let led = match led {
            Ok(x) => x,
            Err(e) => {
                error!(error = ?e,"error scanning for devic");
                continue;
            }
        };

        let (tx, rx) = channel(10);
        let gui_state = LightGuiState::new("New LED", tx);

        {
            lights.lock().unwrap().push(gui_state);
        }

        let rx = debounced::debounced(ReceiverStream::new(rx), Duration::from_millis(100));

        tokio::spawn(connection(led, rx));
    }

    warn!("Scanning stream hung up");
}

/// Combination of the bluetooth peripheral and the characteristic that all
/// commands will be written to
struct Led {
    peripheral: Peripheral,
    characteristic: Characteristic,
}

impl Led {
    /// Write the given command to this light
    async fn cmd(&self, command: impl Packable) -> Result<()> {
        let data = command.to_wire();
        trace!(
            peripheral_id = ?self.peripheral.id(),
            peripheral_mac = %self.peripheral.address(),
            message = %format!("{data:02x?}"),
            ?command,
            "write"
        );
        self.peripheral
            .write(&self.characteristic, &data, WriteType::WithoutResponse)
            .await?;

        Ok(())
    }
}

/// Listens forever to a stream which yields state changes for a given light and
/// applies those state changes.
async fn connection(led: Led, state_stream: impl Stream<Item = LightSettingsState>) -> Result<()> {
    pin_mut!(state_stream);

    let mut previous_state = LightSettingsState::default();
    while let Some(state) = state_stream.next().await {
        write_state(&led, &state, &previous_state).await?;

        previous_state = state;
    }

    Ok(())
}

/// Given the current and previous state of an LED, write the commands required
/// to update the LED's state to the new state.
async fn write_state(
    led: &Led,
    state: &LightSettingsState,
    previous_state: &LightSettingsState,
) -> Result<()> {
    if state.enabled != previous_state.enabled {
        let cmd = if state.enabled {
            PowerCommand::On
        } else {
            PowerCommand::Off
        };

        led.cmd(cmd).await?;
    }

    match state.mode {
        LightMode::Hsi => {
            if state.hue != previous_state.hue {
                led.cmd(HsiCommand::Hue(state.hue)).await?;
            }
            if state.saturation != previous_state.saturation {
                led.cmd(HsiCommand::Saturation(state.saturation)).await?;
            }
            if state.intensity != previous_state.intensity {
                led.cmd(HsiCommand::Intensity(state.intensity)).await?;
            }
            if state.mode != previous_state.mode {
                led.cmd(ModeCommand::Hsi).await?;
            }
        }
        LightMode::Cct => {
            if state.temperature != previous_state.temperature {
                led.cmd(ColorTemperatureCommand(state.temperature)).await?;
            }
            if state.intensity != previous_state.intensity {
                led.cmd(HsiCommand::Intensity(state.intensity)).await?;
            }
            if state.mode != previous_state.mode {
                led.cmd(ModeCommand::Cct).await?;
            }
        }
    }

    Ok(())
}

/// Find the BTLE characteristic for controlling the GVM LED
async fn find_characteristic(led: &Peripheral) -> Result<Characteristic> {
    led.discover_services().await?;

    for service in led.services() {
        if service.uuid == SERVICE_UUID {
            // TODO - use UUID here
            return Ok(service.characteristics.into_iter().nth(1).unwrap());
        }
    }

    bail!("didn't find service");
}

/// Find all bluetooth peripherals with the name matching what we expect a GVM
/// light to have.
async fn find_leds(central: &Adapter) -> Result<Vec<Peripheral>> {
    let mut peripherals = Vec::new();

    for p in central.peripherals().await? {
        let local_name = match p.properties().await? {
            Some(x) => x.local_name,
            None => continue,
        };

        if let Some(name) = local_name {
            if name == "BT_LED" {
                peripherals.push(p)
            }
        }
    }

    Ok(peripherals)
}

/// Infinite loop scanning for compatible LEDs
fn scan_forever() -> impl Stream<Item = Result<Led>> {
    stream! {
        let manager = Manager::new().await?;

        let adapters = manager.adapters().await?;
        let central = adapters.into_iter().nth(0).unwrap();

        let mut connected = HashSet::new();

        central
            .start_scan(ScanFilter {
                services: vec![SERVICE_UUID],
            })
            .await?;

        loop {
            let leds = find_leds(&central).await?;

            for peripheral in leds {
                if !connected.insert(peripheral.id()) {
                    continue;
                }

                peripheral.connect().await?;
                info!(
                    peripheral_id = ?peripheral.id(),
                    peripheral_mac = %peripheral.address(),
                    "connected"
                );

                let characteristic = find_characteristic(&peripheral).await?;

                yield Ok(Led{ peripheral, characteristic })
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}
