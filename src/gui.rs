use std::sync::{Arc, Mutex};

use egui::{Response, Slider, Ui};
use eyre::Result;
use tokio::sync::mpsc::Sender;

pub struct LightGuiState {
    renaming: bool,
    name: String,
    state: LightSettingsState,
    tx: Sender<LightSettingsState>,
    pending_send: bool,
}

/// The state of the settings that we should write to the light
#[derive(Clone, PartialEq, Debug)]
pub struct LightSettingsState {
    pub hue: u8,
    pub intensity: u8,
    pub saturation: u8,
    pub temperature: u8,
    pub mode: LightMode,
    pub enabled: bool,
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

#[derive(PartialEq, Debug, Clone)]
pub enum LightMode {
    Hsi,
    Cct,
}

impl LightGuiState {
    pub fn new(name: impl Into<String>, tx: Sender<LightSettingsState>) -> Self {
        Self {
            name: name.into(),
            renaming: false,
            state: LightSettingsState::default(),
            tx,
            pending_send: false,
        }
    }
}

/// Start the GUI, blocks the main thread. Accepts a guarded list of lights
/// which should be initially empty but will be filled in with real data by the
/// `bluetooth` module as it scans and finds devices.
pub fn run(lights: Arc<Mutex<Vec<LightGuiState>>>) -> Result<(), Box<dyn std::error::Error>> {
    let native_options = eframe::NativeOptions::default();
    eframe::run_native(
        "GVM Orchestrator",
        native_options,
        Box::new(|_cc| Box::new(Gui::new(lights))),
    )?;

    Ok(())
}

#[derive(Default)]
struct Gui {
    lights: Arc<Mutex<Vec<LightGuiState>>>,
}

impl Gui {
    fn new(lights: Arc<Mutex<Vec<LightGuiState>>>) -> Self {
        Self { lights }
    }
}

impl eframe::App for Gui {
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

/// Single LED accessory. At the end of the render pass tries to determine if
/// the state of the light was changed and sends changes to the bluetooth module
/// if so.
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

/// State for an LED (mode, H/S/I, CCT/I)
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
                slider_u8(ui, &mut state.intensity, |val| {
                    Slider::new(val, 0.0..=100.0).text("Intensity")
                });
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
