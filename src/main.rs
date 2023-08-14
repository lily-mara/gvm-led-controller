use std::time::Duration;

use btleplug::{
    api::{Central, Manager as _, Peripheral as _, ScanFilter, Service, WriteType},
    platform::{Adapter, Manager, Peripheral},
};
use egui::{Slider, Ui};
use eyre::{bail, Result};
use futures::stream::StreamExt;
use tokio::time;
use tracing::{debug, info};
use tracing_subscriber::EnvFilter;

use crate::protocol::{HsiCommand, Packable};

mod protocol;

const SERVICE_UUID: uuid::Uuid = uuid::Uuid::from_bytes([
    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d, 0x19, 0x10,
]);

const CMD_INIT_SESSION: &[u8] = &[
    0x4c, 0x54, 0x09, 0x00, 0x00, 0x53, 0x00, 0x00, 0x01, 0x00, 0x94, 0x74,
];

fn main() -> std::result::Result<(), Box<dyn std::error::Error>> {
    tracing_subscriber::fmt()
        .with_env_filter(EnvFilter::from_default_env())
        .init();

    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "GVM Orchestrator",
        native_options,
        Box::new(|cc| Box::new(MyEguiApp::new(cc))),
    )?;

    Ok(())
}

#[derive(Default)]
struct MyEguiApp {
    lights: Vec<ConnectedLight>,
}

impl MyEguiApp {
    fn new(_cc: &eframe::CreationContext<'_>) -> Self {
        Self {
            lights: vec![ConnectedLight::new("LED 1"), ConnectedLight::new("LED 2")],
        }
    }
}

impl eframe::App for MyEguiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical(|ui| {
                for light in &mut self.lights {
                    draw_light_group(ui, light);
                }
            });
        });
    }
}

fn draw_light_group(ui: &mut Ui, light: &mut ConnectedLight) {
    ui.group(|ui| {
        ui.horizontal(|ui| {
            if light.renaming {
                ui.text_edit_singleline(&mut light.name);
                if ui.small_button("Ok").clicked() {
                    light.renaming = false;
                }
            } else {
                ui.toggle_value(&mut light.enabled, &light.name);
                if ui.small_button("Rename").clicked() {
                    light.renaming = true;
                }
            }
        });

        if light.enabled {
            draw_light_settings(ui, light);
        }
    });
}

fn draw_light_settings(ui: &mut Ui, light: &mut ConnectedLight) {
    ui.vertical(|ui| {
        ui.horizontal(|ui| {
            ui.radio_value(&mut light.mode, LightMode::Cct, "CCT");
            ui.radio_value(&mut light.mode, LightMode::Hsi, "HSI");
        });

        ui.group(|ui| match &mut light.mode {
            LightMode::Cct => {
                let mut temperature_f32 = (light.temperature as f32 * 133.333 + 3200.0).floor();
                ui.add(
                    egui::Slider::new(&mut temperature_f32, 3200.0..=5600.0)
                        .text("Color Temperature"),
                );
                light.temperature = ((temperature_f32 - 3200.0) / 133.33).round() as u8;

                slider_u8(ui, &mut light.intensity, |val| {
                    Slider::new(val, 0.0..=100.0).text("Intensity")
                });
            }
            LightMode::Hsi => {
                slider_u8(ui, &mut light.hue, |val| {
                    Slider::new(val, 0.0..=52.0).text("Hue")
                });
                slider_u8(ui, &mut light.intensity, |val| {
                    Slider::new(val, 0.0..=100.0).text("Intensity")
                });
            }
        });
    });
}

fn slider_u8(ui: &mut Ui, value: &mut u8, slider: fn(&mut f32) -> Slider) {
    let mut value_f32 = *value as f32;
    ui.add(slider(&mut value_f32));
    *value = value_f32 as u8;
}

struct ConnectedLight {
    renaming: bool,
    name: String,
    hue: u8,
    intensity: u8,
    temperature: u8,
    mode: LightMode,
    enabled: bool,
}

#[derive(PartialEq)]
enum LightMode {
    Hsi,
    Cct,
}

impl ConnectedLight {
    fn new(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            renaming: false,
            enabled: true,
            hue: 0,
            intensity: 50,
            temperature: 0,
            mode: LightMode::Cct,
        }
    }
}

async fn run_old() -> Result<()> {
    let manager = Manager::new().await?;

    let adapters = manager.adapters().await?;
    let central = adapters.into_iter().nth(0).unwrap();

    central.start_scan(ScanFilter::default()).await?;

    let led = loop {
        if let Ok(led) = find_led(&central).await {
            break led;
        }
        time::sleep(Duration::from_millis(100)).await;
    };

    led.connect().await?;

    info!("connected");

    let service = find_service(&led).await?;

    let characteristic = service.characteristics.iter().next().unwrap();

    led.subscribe(characteristic).await?;

    let mut notifications = led.notifications().await.unwrap();

    tokio::spawn(async move {
        while let Some(notification) = notifications.next().await {
            debug!(
                uuid=?notification.uuid,
                value=%format!("{:02x?}", notification.value),
                "notification"
            );
        }
    });

    // led.write(characteristic, CMD_INIT_SESSION, WriteType::WithoutResponse)
    //     .await?;

    // trace!(
    //     service=?service.uuid,
    //     characteristic=?characteristic.uuid,
    //     cmd = %format!("{cmd:02x?}"),
    //     "write"
    // );

    let setup = [HsiCommand::Intensity(10)];

    let pink = [HsiCommand::Hue(68), HsiCommand::Saturation(100)];
    let white = [HsiCommand::Hue(0), HsiCommand::Saturation(0)];
    let blue = [HsiCommand::Hue(48), HsiCommand::Saturation(100)];

    for cmd in setup {
        led.write(characteristic, &cmd.to_wire(), WriteType::WithoutResponse)
            .await?;
    }

    loop {
        for color in [&blue, &pink, &white] {
            for cmd in color {
                led.write(characteristic, &cmd.to_wire(), WriteType::WithoutResponse)
                    .await?;
            }

            time::sleep(Duration::from_millis(800)).await;
        }
    }
}

async fn find_service(led: &Peripheral) -> Result<Service> {
    led.discover_services().await?;

    for service in led.services() {
        if service.uuid == SERVICE_UUID {
            return Ok(service);
        }
    }

    bail!("didn't find service");
}

async fn find_led(central: &Adapter) -> Result<Peripheral> {
    for p in central.peripherals().await.unwrap() {
        let local_name = p.properties().await.unwrap().unwrap().local_name;

        if let Some(name) = local_name {
            if name == "BT_LED" {
                return Ok(p);
            }
        }
    }

    bail!("didn't find a thing")
}
