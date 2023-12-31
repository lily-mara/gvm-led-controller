use std::sync::{Arc, Mutex};

use eframe::{IconData, NativeOptions};
use egui::{Button, Color32, Direction, Response, Slider, Ui};
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
    /// Range: [0, 0x53)
    pub hue: u8,

    /// Range: [0, 100]
    pub intensity: u8,

    /// Range: [0, 100]
    pub saturation: u8,

    /// 100s of Kelvin - Range: [32, 56]
    pub temperature: u8,

    pub mode: LightMode,
    pub enabled: bool,
}

impl Default for LightSettingsState {
    fn default() -> Self {
        Self {
            enabled: true,
            hue: 0,
            intensity: 10,
            saturation: 100,
            temperature: 32,
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
pub fn run(
    lights: Arc<Mutex<Vec<LightGuiState>>>,
    demo: bool,
) -> Result<(), Box<dyn std::error::Error>> {
    let icon_png_data = include_bytes!(concat!(env!("CARGO_MANIFEST_DIR"), "/data/app-icon.png"));
    let native_options = NativeOptions {
        icon_data: Some(IconData::try_from_png_bytes(icon_png_data)?),
        ..Default::default()
    };

    eframe::run_native(
        "GVM Director",
        native_options,
        Box::new(move |_cc| Box::new(Gui::new(lights, demo))),
    )?;

    Ok(())
}

struct Gui {
    lights: Arc<Mutex<Vec<LightGuiState>>>,
    update_mode: UpdateMode,
    use_global: bool,
    global_state: LightSettingsState,
    demo: bool,
}

#[derive(PartialEq, Clone, Copy)]
enum UpdateMode {
    Immediate,
    Commit,
}

impl Gui {
    fn new(lights: Arc<Mutex<Vec<LightGuiState>>>, demo: bool) -> Self {
        Self {
            lights,
            update_mode: UpdateMode::Immediate,
            use_global: false,
            global_state: Default::default(),
            demo,
        }
    }

    fn draw_global_pane(&mut self, ui: &mut Ui) {
        ui.group(|ui| {
            let previous = self.global_state.clone();
            ui.toggle_value(&mut self.global_state.enabled, "GLOBAL");
            if self.global_state.enabled {
                draw_light_settings(ui, &mut self.global_state);
            }

            if self.global_state != previous {
                for light in self.lights.lock().unwrap().iter_mut() {
                    light.state = self.global_state.clone();
                    if self.update_mode == UpdateMode::Immediate {
                        light.pending_send = true;
                    } else {
                        light.state_needs_update = true;
                    }
                }
            }
        });
    }

    fn draw_settings(&mut self, ui: &mut Ui) {
        if self.demo {
            ui.colored_label(Color32::YELLOW, "DEMO MODE");
        }
        ui.group(|ui| {
            ui.label("Update Mode");
            ui.radio_value(&mut self.update_mode, UpdateMode::Immediate, "Immediate");
            ui.radio_value(&mut self.update_mode, UpdateMode::Commit, "Commit");
        });
        ui.group(|ui| {
            ui.checkbox(&mut self.use_global, "Use Global Setting Pane");
        });
        if self.update_mode == UpdateMode::Commit {
            if ui.small_button("Commit All States").clicked() {
                for light in self.lights.lock().unwrap().iter_mut() {
                    light.pending_send = true;
                }
            }
        }
    }
}

impl eframe::App for Gui {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.vertical(|ui| {
                ui.horizontal(|ui| self.draw_settings(ui));
                ui.horizontal(|ui| {
                    if self.lights.lock().unwrap().is_empty() {
                        ui.with_layout(
                            egui::Layout::centered_and_justified(Direction::TopDown),
                            |ui| ui.label("There are no light devices connected. This application will attempt to connect to any light that it can, with no configuration. Ensure your lights are powered on and in the 'APP' mode. To see a demo of the UI without controlling any real lights, re-launch the application with the `--demo` flag."),
                        );
                        return;
                    }

                    ui.vertical(|ui| {
                        for light in self.lights.lock().unwrap().iter_mut() {
                            draw_light_group(ui, light, self.update_mode);
                        }
                    });
                    ui.vertical(|ui| {
                        if self.use_global {
                            self.draw_global_pane(ui)
                        }
                    });
                });
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
                if ui.text_edit_singleline(&mut light.name).lost_focus() {
                    light.renaming = false;
                };
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
                let mut temperature_f32 = (state.temperature as f32 * 100.0).floor();
                ui.add(
                    egui::Slider::new(&mut temperature_f32, 3200.0..=5600.0)
                        .text("Color Temperature"),
                );
                state.temperature = (temperature_f32 / 100.0).round() as u8;

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
