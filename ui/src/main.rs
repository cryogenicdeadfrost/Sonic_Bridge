use eframe::egui;
use std::path::PathBuf;
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
    key_hex: String,
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
            key_hex: String::new(),
            role: Role::Client,
            ..Default::default()
        }
    }

    fn executable(&self) -> PathBuf {
        let name = if cfg!(windows) {
            match self.role {
                Role::Client => "sonic-bridge-client.exe",
                Role::Receiver | Role::Relay => "sonic-bridge-server.exe",
            }
        } else {
            match self.role {
                Role::Client => "sonic-bridge-client",
                Role::Receiver | Role::Relay => "sonic-bridge-server",
            }
        };

        if let Ok(dir) = std::env::var("SONIC_BRIDGE_BIN_DIR") {
            return PathBuf::from(dir).join(name);
        }

        let exe_dir = std::env::current_exe()
            .ok()
            .and_then(|path| path.parent().map(PathBuf::from));

        if let Some(dir) = exe_dir {
            let beside_ui = dir.join(name);
            if beside_ui.exists() {
                return beside_ui;
            }

            let target_release = dir.join("target").join("release").join(name);
            if target_release.exists() {
                return target_release;
            }

            if let Some(parent) = dir.parent() {
                let target_release = parent.join("target").join("release").join(name);
                if target_release.exists() {
                    return target_release;
                }
            }
        }

        PathBuf::from(name)
    }

    fn start(&mut self) {
        if self.process.is_some() {
            return;
        }

        let executable = self.executable();
        let mut command = Command::new(&executable);

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

                if !self.key_hex.trim().is_empty() {
                    command.args(["--key-hex", self.key_hex.trim()]);
                }
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

                if !self.key_hex.trim().is_empty() {
                    command.args(["--key-hex", self.key_hex.trim()]);
                }
            }
        }

        match command
            .stdout(Stdio::inherit())
            .stderr(Stdio::inherit())
            .spawn()
        {
            Ok(child) => {
                self.process = Some(child);
                self.started_at = Some(Instant::now());
                self.status = format!("Running {}", self.role.label());
            }
            Err(error) => {
                self.status = format!("Could not start {}: {error}", executable.display());
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
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.refresh_process();
        ui.ctx().request_repaint_after(Duration::from_millis(500));

        egui::Panel::top("top").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("SonicBridge v0.3");
                ui.label("PC → Pi → Bluetooth");
                ui.separator();
                ui.label(if self.process.is_some() {
                    "● RUNNING"
                } else {
                    "○ STOPPED"
                });
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(8.0);
            ui.heading("Node control");
            ui.label(
                "The GUI is the steering wheel. The audio engine remains the small CLI daemon.",
            );

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
                        ui.add(
                            egui::Slider::new(&mut self.bitrate, 32_000..=192_000)
                                .step_by(8_000.0),
                        );
                    });
                    ui.horizontal(|ui| {
                        ui.label("Packet TTL");
                        ui.add(egui::Slider::new(&mut self.ttl, 1..=32));
                    });
                    ui.horizontal(|ui| {
                        ui.label("Encryption key");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.key_hex)
                                .password(true)
                                .hint_text("64 hex characters"),
                        );
                    });
                    ui.small(
                        "Optional XChaCha20-Poly1305 key. The same key must be configured on the receiver.",
                    );
                    ui.small(
                        "System audio capture uses native Windows WASAPI loopback through flexaudio.",
                    );
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
                        ui.add(
                            egui::Slider::new(&mut self.jitter_ms, 20..=300)
                                .step_by(20.0),
                        );
                    });
                    ui.checkbox(
                        &mut self.relay_only,
                        "Relay only (do not play locally)",
                    );
                    ui.horizontal(|ui| {
                        ui.label("Encryption key");
                        ui.add(
                            egui::TextEdit::singleline(&mut self.key_hex)
                                .password(true)
                                .hint_text("64 hex characters"),
                        );
                    });
                    ui.small(
                        "Relays forward encrypted packets without needing the encryption key.",
                    );
                    ui.small(
                        "A relay can forward audio to another Pi while optionally playing it locally.",
                    );
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
            ui.collapsing("Bluetooth / Linux", |ui| {
                ui.label("BlueZ owns Bluetooth pairing and A2DP.");
                ui.label("PipeWire owns the audio graph and final Bluetooth sink.");
                ui.label("SonicBridge only moves PCM-compressed audio between nodes.");
            });

            ui.collapsing("Chain example", |ui| {
                ui.monospace("PC -> Pi #1 -> Pi #2 -> Pi #3 -> earbuds");
                ui.label(
                    "Every Pi runs the same server binary. Set --forward-to on each relay.",
                );
                ui.label(
                    "TTL prevents an accidental relay loop from becoming a distributed noise machine.",
                );
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
