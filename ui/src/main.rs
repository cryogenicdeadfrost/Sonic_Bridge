use eframe::egui;
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

#[derive(Default)]
struct SonicBridgeApp {
    target: String,
    listen: String,
    forward_to: String,
    bitrate: u32,
    jitter_ms: u32,
    ttl: u8,
    relay_only: bool,
    role: Role,
    process: Option<Child>,
    started_at: Option<Instant>,
    status: String,
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Role {
    #[default]
    Client,
    Receiver,
    Relay,
}

impl Role {
    fn label(self) -> &'static str {
        match self {
            Self::Client => "Windows Client",
            Self::Receiver => "Pi Receiver",
            Self::Relay => "Pi Relay",
        }
    }
}

impl SonicBridgeApp {
    fn new() -> Self {
        Self {
            target: "127.0.0.1:8765".into(),
            listen: "0.0.0.0:8765".into(),
            forward_to: String::new(),
            bitrate: 96_000,
            jitter_ms: 60,
            ttl: 8,
            relay_only: false,
            role: Role::Client,
            ..Default::default()
        }
    }

    fn executable(&self) -> &'static str {
        if cfg!(windows) {
            match self.role {
                Role::Client => "sonic-bridge-client.exe",
                Role::Receiver | Role::Relay => "sonic-bridge-server.exe",
            }
        } else {
            match self.role {
                Role::Client => "sonic-bridge-client",
                Role::Receiver | Role::Relay => "sonic-bridge-server",
            }
        }
    }

    fn start(&mut self) {
        if self.process.is_some() {
            return;
        }

        let mut command = Command::new(self.executable());

        match self.role {
            Role::Client => {
                command.args([
                    "--target",
                    self.target.trim(),
                    "--bitrate",
                    &self.bitrate.to_string(),
                    "--ttl",
                    &self.ttl.to_string(),
                ]);
            }
            Role::Receiver | Role::Relay => {
                command.args([
                    "--listen",
                    self.listen.trim(),
                    "--jitter-ms",
                    &self.jitter_ms.to_string(),
                ]);

                if !self.forward_to.trim().is_empty() {
                    command.args(["--forward-to", self.forward_to.trim()]);
                }

                if self.relay_only {
                    command.arg("--relay-only");
                }
            }
        }

        match command.stdout(Stdio::inherit()).stderr(Stdio::inherit()).spawn() {
            Ok(child) => {
                self.process = Some(child);
                self.started_at = Some(Instant::now());
                self.status = format!("Running {}", self.role.label());
            }
            Err(error) => {
                self.status = format!("Could not start {}: {error}", self.executable());
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.process.take() {
            let _ = child.kill();
            let _ = child.wait();
        }
        self.started_at = None;
        self.status = "Stopped".into();
    }

    fn refresh_process(&mut self) {
        if let Some(child) = self.process.as_mut() {
            match child.try_wait() {
                Ok(Some(status)) => {
                    self.status = format!("Process exited: {status}");
                    self.process = None;
                    self.started_at = None;
                }
                Ok(None) => {}
                Err(error) => {
                    self.status = format!("Process check failed: {error}");
                }
            }
        }
    }
}

impl eframe::App for SonicBridgeApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.refresh_process();
        ctx.request_repaint_after(Duration::from_millis(500));

        egui::TopBottomPanel::top("top").show(ctx, |ui| {
            ui.horizontal(|ui| {
                ui.heading("SonicBridge");
                ui.label("PC → Pi → Bluetooth");
                ui.separator();
                ui.label(if self.process.is_some() { "● RUNNING" } else { "○ STOPPED" });
            });
        });

        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_space(8.0);
            ui.heading("Node control");
            ui.label("The GUI is only the steering wheel. The audio engine remains the small CLI daemon.");

            ui.horizontal(|ui| {
                ui.label("Role");
                egui::ComboBox::from_id_salt("role")
                    .selected_text(self.role.label())
                    .show_ui(ui, |ui| {
                        ui.selectable_value(&mut self.role, Role::Client, "Windows Client");
                        ui.selectable_value(&mut self.role, Role::Receiver, "Pi Receiver");
                        ui.selectable_value(&mut self.role, Role::Relay, "Pi Relay");
                    });
            });

            ui.separator();

            match self.role {
                Role::Client => {
                    ui.horizontal(|ui| {
                        ui.label("Target");
                        ui.text_edit_singleline(&mut self.target);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Opus bitrate");
                        ui.add(egui::Slider::new(&mut self.bitrate, 32_000..=192_000).step_by(8_000));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Packet TTL");
                        ui.add(egui::Slider::new(&mut self.ttl, 1..=32));
                    });
                    ui.small("System audio capture uses the native OS loopback backend.");
                }
                Role::Receiver | Role::Relay => {
                    ui.horizontal(|ui| {
                        ui.label("Listen");
                        ui.text_edit_singleline(&mut self.listen);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Forward to");
                        ui.text_edit_singleline(&mut self.forward_to);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Jitter buffer");
                        ui.add(egui::Slider::new(&mut self.jitter_ms, 20..=300).step_by(20));
                    });
                    ui.checkbox(&mut self.relay_only, "Relay only (do not play locally)");
                    ui.small("A relay can forward audio to another Pi while optionally playing it locally.");
                }
            }

            ui.separator();

            ui.horizontal(|ui| {
                if self.process.is_none() {
                    if ui.button("Start").clicked() {
                        self.start();
                    }
                } else if ui.button("Stop").clicked() {
                    self.stop();
                }

                if ui.button("Reset").clicked() && self.process.is_none() {
                    *self = Self::new();
                }
            });

            if let Some(started) = self.started_at {
                ui.label(format!("Uptime: {}s", started.elapsed().as_secs()));
            }

            if !self.status.is_empty() {
                ui.separator();
                ui.label(&self.status);
            }

            ui.separator();
            ui.collapsing("Bluetooth / Linux notes", |ui| {
                ui.label("Bluetooth pairing and A2DP routing remain owned by BlueZ + PipeWire.");
                ui.label("Use the normal Linux audio sink for the final hop.");
                ui.label("The daemon does not touch raw Bluetooth packets.");
            });

            ui.collapsing("Chain example", |ui| {
                ui.monospace("PC -> Pi #1 -> Pi #2 -> Pi #3 -> earbuds");
                ui.label("Each Pi runs the same server binary. Give each relay a --forward-to address.");
            });
        });
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("SonicBridge")
            .with_inner_size([760.0, 620.0]),
        ..Default::default()
    };

    eframe::run_native(
        "SonicBridge",
        options,
        Box::new(|_cc| Ok(Box::new(SonicBridgeApp::new()))),
    )
}
