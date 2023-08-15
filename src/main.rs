use std::{
    collections::HashSet,
    sync::{Arc, Mutex},
    time::Duration,
};

use async_stream::stream;
use btleplug::{
    api::{Central, Characteristic, Manager as _, Peripheral as _, ScanFilter, WriteType},
    platform::{Adapter, Manager, Peripheral},
};
use egui::{Response, Slider, Ui};
use eyre::{bail, Result};
use futures::{pin_mut, stream::StreamExt, Stream};
use protocol::{ColorTemperatureCommand, ModeCommand};
use tokio::sync::mpsc::{channel, Sender};
use tokio_stream::wrappers::ReceiverStream;
use tracing::{info, trace};
use tracing_subscriber::EnvFilter;

use crate::protocol::{HsiCommand, Packable};

mod protocol;

const SERVICE_UUID: uuid::Uuid = uuid::Uuid::from_bytes([
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x19, 0x10,
]);

// const CMD_INIT_SESSION: &[u8] = &[
//     0x4c, 0x54, 0x09, 0x00, 0x00, 0x53, 0x00, 0x00, 0x01, 0x00, 0x94, 0x74,
// ];

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let lights = Arc::new(Mutex::new(Vec::new()));

    let runtime = tokio::runtime::Runtime::new()?;
    runtime.spawn(scan_and_spawn(lights.clone()));

    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "GVM Orchestrator",
        native_options,
        Box::new(|cc| Box::new(MyEguiApp::new(cc, lights))),
    )?;

    Ok(())
}

#[derive(Default)]
struct MyEguiApp {
    lights: Arc<Mutex<Vec<LightGuiState>>>,
}

impl MyEguiApp {
    fn new(_cc: &eframe::CreationContext<'_>, lights: Arc<Mutex<Vec<LightGuiState>>>) -> Self {
        Self { lights }
    }
}

impl eframe::App for MyEguiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical(|ui| {
                for light in self.lights.lock().unwrap().iter_mut() {
                    draw_light_group(ui, light);
                }
            });
        });
    }
}

fn draw_light_group(ui: &mut Ui, light: &mut LightGuiState) {
    let previous = light.state.clone();
    ui.group(|ui| {
        ui.horizontal(|ui| {
            if light.renaming {
                ui.text_edit_singleline(&mut light.name);
                if ui.small_button("Ok").clicked() {
                    light.renaming = false;
                }
            } else {
                ui.toggle_value(&mut light.state.enabled, &light.name);
                if ui.small_button("Rename").clicked() {
                    light.renaming = true;
                }
            }
        });

        if light.state.enabled {
            draw_light_settings(ui, &mut light.state);
        }
    });
    if light.pending_send || light.state != previous {
        light.pending_send = light.tx.try_send(light.state.clone()).is_err();
    }
}

fn draw_light_settings(ui: &mut Ui, state: &mut LightSettingsState) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut state.mode, LightMode::Cct, "CCT");
            ui.radio_value(&mut state.mode, LightMode::Hsi, "HSI");
        });

        ui.group(|ui| match &mut state.mode {
            LightMode::Cct => {
                let mut temperature_f32 = (state.temperature as f32 * 133.333 + 3200.0).floor();
                ui.add(
                    egui::Slider::new(&mut temperature_f32, 3200.0..=5600.0)
                        .text("Color Temperature"),
                );
                state.temperature = ((temperature_f32 - 3200.0) / 133.33).round() as u8;

                slider_u8(ui, &mut state.intensity, |val| {
                    Slider::new(val, 0.0..=100.0).text("Intensity")
                });
            }
            LightMode::Hsi => {
                slider_u8(ui, &mut state.hue, |val| {
                    Slider::new(val, 0.0..=52.0).text("Hue")
                });
                let intensity_change = slider_u8(ui, &mut state.intensity, |val| {
                    Slider::new(val, 0.0..=100.0).text("Intensity")
                });
                if intensity_change.changed() {
                    dbg!(state.intensity);
                }
            }
        });
    });
}

fn slider_u8(ui: &mut Ui, value: &mut u8, slider: fn(&mut f32) -> Slider) -> Response {
    let mut value_f32 = *value as f32;
    let response = ui.add(slider(&mut value_f32));
    *value = value_f32 as u8;

    response
}

struct LightGuiState {
    renaming: bool,
    name: String,
    state: LightSettingsState,
    tx: Sender<LightSettingsState>,
    pending_send: bool,
}

/// The state of the settings that we should write to the light
#[derive(Clone, PartialEq)]
struct LightSettingsState {
    hue: u8,
    intensity: u8,
    saturation: u8,
    temperature: u8,
    mode: LightMode,
    enabled: bool,
}

impl Default for LightSettingsState {
    fn default() -> Self {
        Self {
            enabled: true,
            hue: 0,
            intensity: 50,
            saturation: 100,
            temperature: 0,
            mode: LightMode::Cct,
        }
    }
}

#[derive(PartialEq, Clone)]
enum LightMode {
    Hsi,
    Cct,
}

impl LightGuiState {
    fn new(name: impl Into<String>, tx: Sender<LightSettingsState>) -> Self {
        Self {
            name: name.into(),
            renaming: false,
            state: LightSettingsState::default(),
            tx,
            pending_send: false,
        }
    }
}

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
                    id = ?peripheral.id(),
                    mac = %peripheral.address(),
                    "connected"
                );

                let characteristic = find_characteristic(&peripheral).await?;

                yield Ok(Led{ peripheral, characteristic })
            }

            tokio::time::sleep(std::time::Duration::from_millis(500)).await;
        }
    }
}

async fn scan_and_spawn(lights: Arc<Mutex<Vec<LightGuiState>>>) -> Result<()> {
    let device_stream = scan_forever();
    pin_mut!(device_stream);

    while let Some(led) = device_stream.next().await {
        let led = led?;

        let (tx, rx) = channel(10);
        let gui_state = LightGuiState::new("New LED", tx);

        {
            lights.lock().unwrap().push(gui_state);
        }

        let rx = debounced::debounced(ReceiverStream::new(rx), Duration::from_millis(100));

        tokio::spawn(connection(led, rx));
    }

    Ok(())
}

struct Led {
    peripheral: Peripheral,
    characteristic: Characteristic,
}

impl Led {
    async fn cmd(&self, command: impl Packable) -> Result<()> {
        let data = command.to_wire();
        trace!(
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

async fn connection(led: Led, state_stream: impl Stream<Item = LightSettingsState>) -> Result<()> {
    pin_mut!(state_stream);

    let mut previous_state = LightSettingsState::default();
    while let Some(state) = state_stream.next().await {
        write_state(&led, &state, &previous_state).await?;

        previous_state = state;
    }

    Ok(())
}

async fn write_state(
    led: &Led,
    state: &LightSettingsState,
    previous_state: &LightSettingsState,
) -> Result<()> {
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
