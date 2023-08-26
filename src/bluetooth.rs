use std::{
    collections::HashSet,
    fmt::Debug,
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
use eyre::{bail, eyre, Result};
use futures::{pin_mut, stream::StreamExt, Stream};
use tokio::{select, sync::mpsc::channel, time::sleep};
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
/// Slowly yields lights that the GUI sees as connected. Commands
/// sent to the light are logged at INFO level.
pub(crate) async fn scan_and_spawn_demo_mode(lights: Arc<Mutex<Vec<LightGuiState>>>) {
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

        let name = match led.mac {
            MacAddress::Unknown => String::from("New LED"),
            MacAddress::Known(_) => format!("{:?}", led.mac),
        };

        let (tx, rx) = channel(10);
        let gui_state = LightGuiState::new(name, tx);

        {
            lights.lock().unwrap().push(gui_state);
        }

        let rx = debounced::debounced(ReceiverStream::new(rx), Duration::from_millis(100));

        tokio::spawn(led.connection(rx));
    }

    warn!("Scanning stream hung up");
}

/// Combination of the bluetooth peripheral and the characteristic that all
/// commands will be written to
struct Led {
    peripheral: Peripheral,
    characteristic: Characteristic,

    // CoreBluetooth hides the mac address of bluetooth accessories, so the only
    // way to get the mac address is to inspect the device properties. This is
    // done once at connection initialization time and the full mac address is
    // stored here.
    mac: MacAddress,
}

#[derive(PartialEq)]
enum MacAddress {
    Known([u8; 6]),
    Unknown,
}

impl Debug for MacAddress {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Known(data) => {
                for byte in &data[..5] {
                    write!(f, "{byte:02x}:")?;
                }
                write!(f, "{:02x}", data[5])
            }
            Self::Unknown => write!(f, "Unknown"),
        }
    }
}

impl Led {
    /// Write the given command to this light
    async fn cmd(&self, command: impl Packable) -> Result<()> {
        let data = command.to_wire();
        trace!(
            peripheral_id = %self.peripheral.id(),
            peripheral_mac = ?self.mac,
            raw = %format!("{data:02x?}"),
            ?command,
            "write"
        );
        self.peripheral
            .write(&self.characteristic, &data, WriteType::WithoutResponse)
            .await?;

        Ok(())
    }

    async fn discover_mac(&mut self) -> Result<()> {
        let properties = self
            .peripheral
            .properties()
            .await?
            .ok_or_else(|| eyre!("device had no properties"))?;

        for (prefix, suffix) in properties.manufacturer_data {
            if suffix.len() != 4 {
                continue;
            }

            let [prefix_low, prefix_high] = prefix.to_le_bytes();

            let mac = [
                prefix_low,
                prefix_high,
                suffix[0],
                suffix[1],
                suffix[2],
                suffix[3],
            ];

            self.mac = MacAddress::Known(mac);
            break;
        }

        Ok(())
    }

    /// Ensure the connection is healthy and attempt to reconnect if not
    async fn health_check(&self) {
        if let Ok(false) = self.peripheral.is_connected().await {
            warn!(
                peripheral_id = %self.peripheral.id(),
                peripheral_mac = ?self.mac,
                "LED disconnected"
            );

            loop {
                if let Err(e) = self.peripheral.connect().await {
                    warn!(
                        peripheral_id = %self.peripheral.id(),
                        peripheral_mac = ?self.mac,
                        error=?e,
                        "Failed to reconnect"
                    );
                    tokio::time::sleep(Duration::from_secs(5)).await;

                    continue;
                }

                if let Ok(true) = self.peripheral.is_connected().await {
                    info!(
                        peripheral_id = %self.peripheral.id(),
                        peripheral_mac = ?self.mac,
                        "Reconnected"
                    );
                    break;
                }
            }
        }
    }

    /// Listens forever to a stream which yields state changes for a given light and
    /// applies those state changes.
    async fn connection(
        mut self,
        state_stream: impl Stream<Item = LightSettingsState>,
    ) -> Result<()> {
        let mut previous_state = LightSettingsState::default();
        write_state_no_cmp(&self, &previous_state).await?;

        let mut health_interval = tokio::time::interval(Duration::from_secs(1));

        pin_mut!(state_stream);

        self.peripheral.subscribe(&self.characteristic).await?;

        let mut notifications = self.peripheral.notifications().await?;

        loop {
            select! {
                next = state_stream.next() => {
                    let state = match next {
                        None => break,
                        Some(x) => x,
                    };

                    write_state(&self, &state, &previous_state).await?;

                    previous_state = state;
                }
                _ = health_interval.tick() => {
                    if self.mac == MacAddress::Unknown {
                        _ = self.discover_mac().await;
                    }
                    self.health_check().await;
                }
                next = notifications.next() => {
                    if let Some(notif) = next {
                        trace!(
                            peripheral_id = %self.peripheral.id(),
                            peripheral_mac = ?self.mac,
                            ?notif.value,
                            %notif.uuid,
                            "notification"
                        );
                    }
                }
            }
        }

        Ok(())
    }
}

async fn write_state_no_cmp(led: &Led, state: &LightSettingsState) -> Result<()> {
    let cmd = if state.enabled {
        PowerCommand::On
    } else {
        PowerCommand::Off
    };
    led.cmd(cmd).await?;

    match state.mode {
        LightMode::Hsi => {
            led.cmd(HsiCommand::Hue(state.hue)).await?;
            led.cmd(HsiCommand::Saturation(state.saturation)).await?;
            led.cmd(HsiCommand::Intensity(state.intensity)).await?;
            led.cmd(ModeCommand::Hsi).await?;
        }
        LightMode::Cct => {
            led.cmd(ColorTemperatureCommand(state.temperature)).await?;
            led.cmd(HsiCommand::Intensity(state.intensity)).await?;
            led.cmd(ModeCommand::Cct).await?;
        }
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
            let characteristic = service.characteristics.into_iter().nth(0);
            return characteristic.ok_or_else(|| eyre!("service didn't have characteristic"));
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
            .start_scan(ScanFilter::default())
            .await?;

        loop {
            let leds = find_leds(&central).await?;

            for peripheral in leds {
                if !connected.insert(peripheral.id()) {
                    continue;
                }

                peripheral.connect().await?;

                let characteristic = find_characteristic(&peripheral).await?;

                let mut led = Led{ peripheral, characteristic, mac: MacAddress::Unknown };

                _ = led.discover_mac().await;

                info!(
                    peripheral_id = %led.peripheral.id(),
                    peripheral_mac = ?led.mac,
                    "connected"
                );

                yield Ok(led)
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}
