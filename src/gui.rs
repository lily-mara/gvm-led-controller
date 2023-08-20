use std::sync::{Arc, Mutex};

use eframe::{IconData, NativeOptions};
use egui::{Button, Response, Slider, Ui};
use eyre::Result;
use tokio::sync::mpsc::Sender;

pub struct LightGuiState {
    renaming: bool,
    name: String,
    state: LightSettingsState,
    tx: Sender<LightSettingsState>,
    pending_send: bool,
    state_needs_update: bool,
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
            state_needs_update: false,
        }
    }
}

/// Start the GUI, blocks the main thread. Accepts a guarded list of lights
/// which should be initially empty but will be filled in with real data by the
/// `bluetooth` module as it scans and finds devices.
pub fn run(lights: Arc<Mutex<Vec<LightGuiState>>>) -> Result<(), Box<dyn std::error::Error>> {
    let icon_png_data = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/app-icon.png"));
    let native_options = NativeOptions {
        icon_data: Some(IconData::try_from_png_bytes(icon_png_data)?),
        ..Default::default()
    };

    eframe::run_native(
        "GVM Director",
        native_options,
        Box::new(|_cc| Box::new(Gui::new(lights))),
    )?;

    Ok(())
}

struct Gui {
    lights: Arc<Mutex<Vec<LightGuiState>>>,
    update_mode: UpdateMode,
}

#[derive(PartialEq, Clone, Copy)]
enum UpdateMode {
    Immediate,
    Commit,
}

impl Gui {
    fn new(lights: Arc<Mutex<Vec<LightGuiState>>>) -> Self {
        Self {
            lights,
            update_mode: UpdateMode::Immediate,
        }
    }
}

impl eframe::App for Gui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| {
                    ui.group(|ui| {
                        ui.label("Update Mode");
                        ui.radio_value(&mut self.update_mode, UpdateMode::Immediate, "Immediate");
                        ui.radio_value(&mut self.update_mode, UpdateMode::Commit, "Commit");
                    });
                    if self.update_mode == UpdateMode::Commit {
                        if ui.small_button("Commit All States").clicked() {
                            for light in self.lights.lock().unwrap().iter_mut() {
                                light.pending_send = true;
                            }
                        }
                    }
                });
                for light in self.lights.lock().unwrap().iter_mut() {
                    draw_light_group(ui, light, self.update_mode);
                }
            });
        });
    }
}

/// Single LED accessory. At the end of the render pass tries to determine if
/// the state of the light was changed and sends changes to the bluetooth module
/// if so.
fn draw_light_group(ui: &mut Ui, light: &mut LightGuiState, update_mode: UpdateMode) {
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

                if update_mode == UpdateMode::Commit {
                    if ui
                        .add_enabled(light.state_needs_update, Button::new("Commit State"))
                        .clicked()
                    {
                        light.pending_send = true;
                    }
                }
            }
        });

        if light.state.enabled {
            draw_light_settings(ui, &mut light.state);
        }
    });

    if light.state != previous {
        light.state_needs_update = true;
    }

    if light.pending_send || (update_mode == UpdateMode::Immediate && light.state_needs_update) {
        light.state_needs_update = false;
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
                slider_u8(ui, &mut state.saturation, |val| {
                    Slider::new(val, 0.0..=100.0).text("Saturation")
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
