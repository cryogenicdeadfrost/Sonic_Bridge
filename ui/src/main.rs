use eframe::egui;
use std::{
    collections::VecDeque,
    io::{BufRead, BufReader, Read},
    path::PathBuf,
    process::{Child, Command, Stdio},
    sync::mpsc::{self, Receiver, Sender},
    thread,
    time::{Duration, Instant},
};

const MAX_LOG_LINES: usize = 500;

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
            Self::Client => "Windows client",
            Self::Receiver => "Pi receiver",
            Self::Relay => "Pi relay",
        }
    }
}

#[derive(Clone, Copy, PartialEq, Eq, Default)]
enum Tab {
    #[default]
    Overview,
    Advanced,
    Activity,
}

struct SonicBridgeApp {
    target: String,
    listen: String,
    forward_to: String,
    allowed_peers: String,
    allow_insecure_listen: bool,
    bitrate: u32,
    jitter_ms: u32,
    ttl: u8,
    relay_only: bool,
    key_hex: String,
    role: Role,
    tab: Tab,
    process: Option<Child>,
    started_at: Option<Instant>,
    status: String,
    logs: VecDeque<String>,
    log_tx: Sender<String>,
    log_rx: Receiver<String>,
}

impl SonicBridgeApp {
    fn new() -> Self {
        let (log_tx, log_rx) = mpsc::channel();
        let mut app = Self {
            target: "127.0.0.1:8765".into(),
            listen: "127.0.0.1:8765".into(),
            forward_to: String::new(),
            allowed_peers: String::new(),
            allow_insecure_listen: false,
            bitrate: 96_000,
            jitter_ms: 60,
            ttl: 8,
            relay_only: false,
            key_hex: String::new(),
            role: Role::Client,
            tab: Tab::Overview,
            process: None,
            started_at: None,
            status: "Ready".into(),
            logs: VecDeque::new(),
            log_tx,
            log_rx,
        };
        app.log("SonicBridge is ready. Pick a node role and start the stream.");
        app
    }

    fn executable(&self) -> PathBuf {
        let name = match self.role {
            Role::Client => "sonic-bridge-client",
            Role::Receiver | Role::Relay => "sonic-bridge-server",
        };
        let name = if cfg!(windows) {
            format!("{name}.exe")
        } else {
            name.to_owned()
        };

        if let Ok(dir) = std::env::var("SONIC_BRIDGE_BIN_DIR") {
            return PathBuf::from(dir).join(name);
        }

        if let Ok(current) = std::env::current_exe() {
            if let Some(dir) = current.parent() {
                let beside_ui = dir.join(&name);
                if beside_ui.is_file() {
                    return beside_ui;
                }
            }
        }

        PathBuf::from(name)
    }

    fn log(&mut self, message: impl Into<String>) {
        let line = message.into();
        self.logs.push_back(line.clone());
        while self.logs.len() > MAX_LOG_LINES {
            self.logs.pop_front();
        }
    }

    fn start(&mut self) {
        if self.process.is_some() {
            return;
        }

        if self.key_hex.trim().len() != 0
            && sonic_bridge_shared::parse_key_hex(self.key_hex.trim()).is_err()
        {
            self.status = "The encryption key must be exactly 64 hexadecimal characters.".into();
            self.log(self.status.clone());
            return;
        }

        let executable = self.executable();
        let mut command = Command::new(&executable);

        match self.role {
            Role::Client => {
                command
                    .arg("--target")
                    .arg(self.target.trim())
                    .arg("--bitrate")
                    .arg(self.bitrate.to_string())
                    .arg("--ttl")
                    .arg(self.ttl.to_string());
            }
            Role::Receiver | Role::Relay => {
                command
                    .arg("--listen")
                    .arg(self.listen.trim())
                    .arg("--jitter-ms")
                    .arg(self.jitter_ms.to_string());

                if !self.forward_to.trim().is_empty() {
                    command.arg("--forward-to").arg(self.forward_to.trim());
                }

                if self.relay_only {
                    command.arg("--relay-only");
                }
                if !self.allowed_peers.trim().is_empty() {
                    command.arg("--allowed-peer").arg(self.allowed_peers.trim());
                }
                if self.allow_insecure_listen {
                    command.arg("--allow-insecure-listen");
                }
            }
        }

        // Do not accidentally inherit a key from the shell when the field is empty.
        command.env_remove("SONIC_BRIDGE_KEY");
        if !self.key_hex.trim().is_empty() {
            command.env("SONIC_BRIDGE_KEY", self.key_hex.trim());
        }

        match command
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
        {
            Ok(mut child) => {
                if let Some(stdout) = child.stdout.take() {
                    pipe_logs(stdout, self.log_tx.clone(), "out");
                }
                if let Some(stderr) = child.stderr.take() {
                    pipe_logs(stderr, self.log_tx.clone(), "err");
                }

                self.process = Some(child);
                self.started_at = Some(Instant::now());
                self.status = format!("Running {}", self.role.label());
                self.log(format!(
                    "Started {} with {}",
                    self.role.label(),
                    executable.display()
                ));
            }
            Err(error) => {
                self.status = format!("Could not start {}: {error}", executable.display());
                self.log(self.status.clone());
            }
        }
    }

    fn stop(&mut self) {
        if let Some(mut child) = self.process.take() {
            match child.kill() {
                Ok(()) => self.log("Stopping the audio worker…"),
                Err(error) => self.log(format!("Could not stop the audio worker cleanly: {error}")),
            }
            if let Err(error) = child.wait() {
                self.log(format!("Could not reap the audio worker: {error}"));
            }
        }
        self.started_at = None;
        self.status = "Stopped".into();
        self.log("Worker stopped.");
    }

    fn refresh_process(&mut self) {
        let exit = match self.process.as_mut() {
            Some(child) => match child.try_wait() {
                Ok(Some(status)) => Some(Ok(status)),
                Ok(None) => None,
                Err(error) => Some(Err(error)),
            },
            None => None,
        };

        match exit {
            Some(Ok(status)) => {
                self.process = None;
                self.started_at = None;
                self.status = format!("Worker exited: {status}");
                self.log(self.status.clone());
            }
            Some(Err(error)) => {
                self.status = format!("Could not inspect worker: {error}");
                self.log(self.status.clone());
            }
            None => {}
        }

        while let Ok(line) = self.log_rx.try_recv() {
            self.logs.push_back(line);
            while self.logs.len() > MAX_LOG_LINES {
                self.logs.pop_front();
            }
        }
    }

    fn show_overview(&mut self, ui: &mut egui::Ui) {
        ui.heading("Stream setup");
        ui.label("The common controls stay here. The knobs you rarely touch live under Advanced.");

        ui.add_space(10.0);
        ui.group(|ui| {
            ui.label("NODE ROLE");
            egui::ComboBox::from_id_salt("node-role")
                .selected_text(self.role.label())
                .show_ui(ui, |ui| {
                    ui.selectable_value(&mut self.role, Role::Client, "Windows client");
                    ui.selectable_value(&mut self.role, Role::Receiver, "Pi receiver");
                    if ui
                        .selectable_value(&mut self.role, Role::Relay, "Pi relay")
                        .clicked()
                    {
                        self.relay_only = true;
                    }
                });
            ui.add_space(8.0);

            match self.role {
                Role::Client => {
                    ui.label("Send your system audio to the next SonicBridge node.");
                    ui.horizontal(|ui| {
                        ui.label("Receiver address");
                        ui.add_sized([260.0, 28.0], egui::TextEdit::singleline(&mut self.target));
                    });
                }
                Role::Receiver | Role::Relay => {
                    ui.label("Listen for incoming audio. A relay can also pass packets onward.");
                    ui.horizontal(|ui| {
                        ui.label("Listen address");
                        ui.add_sized([260.0, 28.0], egui::TextEdit::singleline(&mut self.listen));
                    });
                    if self.role == Role::Relay {
                        ui.horizontal(|ui| {
                            ui.label("Next node");
                            ui.add_sized(
                                [260.0, 28.0],
                                egui::TextEdit::singleline(&mut self.forward_to),
                            );
                        });
                    }
                }
            }
        });

        ui.add_space(10.0);
        ui.group(|ui| {
            ui.horizontal(|ui| {
                ui.vertical(|ui| {
                    ui.label("WORKER");
                    ui.heading(if self.process.is_some() {
                        "Running"
                    } else {
                        "Stopped"
                    });
                    ui.label(&self.status);
                    if let Some(started) = self.started_at {
                        ui.small(format!("Uptime · {}", format_uptime(started.elapsed())));
                    }
                });
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if self.process.is_some() {
                        if ui.button("Stop stream").clicked() {
                            self.stop();
                        }
                    } else if ui.button("Start stream").clicked() {
                        self.start();
                    }
                });
            });
        });

        ui.add_space(8.0);
        ui.horizontal_wrapped(|ui| {
            ui.small("Audio: Opus");
            ui.separator();
            ui.small("Transport: UDP");
            ui.separator();
            ui.small(if self.key_hex.trim().is_empty() {
                "Encryption: off"
            } else {
                "Encryption: on"
            });
        });
    }

    fn show_advanced(&mut self, ui: &mut egui::Ui) {
        ui.heading("Advanced settings");
        ui.label("Fine-tune the stream. Changes apply the next time the worker starts.");
        ui.add_space(10.0);

        egui::Grid::new("advanced-settings")
            .num_columns(2)
            .spacing([24.0, 12.0])
            .show(ui, |ui| {
                match self.role {
                    Role::Client => {
                        ui.label("Opus bitrate");
                        ui.vertical(|ui| {
                            ui.add(egui::Slider::new(&mut self.bitrate, 32_000..=192_000).step_by(8_000.0).suffix(" bps"));
                            ui.small("Higher values use more bandwidth.");
                        });
                        ui.end_row();

                        ui.label("Relay TTL");
                        ui.add(egui::Slider::new(&mut self.ttl, 1..=32));
                        ui.end_row();
                    }
                    Role::Receiver | Role::Relay => {
                        ui.label("Jitter buffer");
                        ui.vertical(|ui| {
                            ui.add(egui::Slider::new(&mut self.jitter_ms, 20..=300).step_by(20.0).suffix(" ms"));
                            ui.small("Raise this on unstable Wi-Fi. Lower it on a clean LAN.");
                        });
                        ui.end_row();

                        ui.label("Forward to");
                        ui.text_edit_singleline(&mut self.forward_to);
                        ui.end_row();

                        ui.label("Allowed sender IPs");
                        ui.vertical(|ui| {
                            ui.text_edit_singleline(&mut self.allowed_peers);
                            ui.small("Comma-separated source IPs. Useful for keyless relays on a private network.");
                        });
                        ui.end_row();

                        ui.label("Network exposure");
                        ui.checkbox(&mut self.allow_insecure_listen, "Allow unencrypted traffic on non-loopback interfaces");
                        ui.end_row();

                        ui.label("Playback");
                        ui.checkbox(&mut self.relay_only, "Relay only (do not play locally)");
                        ui.end_row();
                    }
                }

                ui.label("Transport key");
                ui.vertical(|ui| {
                    ui.add(
                        egui::TextEdit::singleline(&mut self.key_hex)
                            .password(true)
                            .hint_text("64 hexadecimal characters"),
                    );
                    ui.small("Use the same 256-bit key on the sender and final receiver. Relays can forward ciphertext without it.");
                });
                ui.end_row();
            });

        ui.add_space(10.0);
        ui.group(|ui| {
            ui.label("A note on network exposure");
            ui.small("The receiver starts on loopback. Use a private LAN or Tailscale address only when remote nodes need to reach it.");

            ui.small("The receiver and relay must run the same protocol version as the client.");
        });
    }

    fn show_activity(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading("Activity");
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.button("Clear").clicked() {
                    self.logs.clear();
                }
            });
        });
        ui.label("Worker output and lifecycle events. The latest 500 lines are kept in memory.");
        ui.add_space(8.0);

        egui::ScrollArea::vertical()
            .stick_to_bottom(true)
            .auto_shrink([false, false])
            .show(ui, |ui| {
                if self.logs.is_empty() {
                    ui.weak("Nothing to report yet.");
                } else {
                    for line in &self.logs {
                        ui.monospace(line);
                    }
                }
            });
    }
}

fn pipe_logs<R>(reader: R, tx: Sender<String>, stream: &'static str)
where
    R: Read + Send + 'static,
{
    thread::spawn(move || {
        for line in BufReader::new(reader).lines() {
            match line {
                Ok(line) => {
                    if tx.send(format!("[{stream}] {line}")).is_err() {
                        break;
                    }
                }
                Err(error) => {
                    let _ = tx.send(format!("[{stream}] log read failed: {error}"));
                    break;
                }
            }
        }
    });
}

fn format_uptime(elapsed: Duration) -> String {
    let seconds = elapsed.as_secs();
    let hours = seconds / 3_600;
    let minutes = (seconds % 3_600) / 60;
    let seconds = seconds % 60;

    if hours > 0 {
        format!("{hours}h {minutes:02}m {seconds:02}s")
    } else if minutes > 0 {
        format!("{minutes}m {seconds:02}s")
    } else {
        format!("{seconds}s")
    }
}

impl eframe::App for SonicBridgeApp {
    fn ui(&mut self, ui: &mut egui::Ui, _frame: &mut eframe::Frame) {
        self.refresh_process();
        ui.ctx().request_repaint_after(Duration::from_millis(250));

        egui::Panel::top("header").show(ui, |ui| {
            ui.horizontal(|ui| {
                ui.heading("SonicBridge");
                ui.label("0.3 · PC → Pi → Bluetooth");
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let (label, color) = if self.process.is_some() {
                        ("● LIVE", egui::Color32::from_rgb(88, 196, 139))
                    } else {
                        ("○ IDLE", egui::Color32::from_rgb(150, 158, 170))
                    };
                    ui.colored_label(color, label);
                });
            });
        });

        egui::CentralPanel::default().show(ui, |ui| {
            ui.add_space(4.0);
            ui.horizontal(|ui| {
                if ui
                    .selectable_label(self.tab == Tab::Overview, "Overview")
                    .clicked()
                {
                    self.tab = Tab::Overview;
                }
                if ui
                    .selectable_label(self.tab == Tab::Advanced, "Advanced")
                    .clicked()
                {
                    self.tab = Tab::Advanced;
                }
                if ui
                    .selectable_label(self.tab == Tab::Activity, "Activity")
                    .clicked()
                {
                    self.tab = Tab::Activity;
                }
            });
            ui.separator();

            egui::ScrollArea::vertical().show(ui, |ui| match self.tab {
                Tab::Overview => self.show_overview(ui),
                Tab::Advanced => self.show_advanced(ui),
                Tab::Activity => self.show_activity(ui),
            });
        });
    }
}

impl Drop for SonicBridgeApp {
    fn drop(&mut self) {
        if let Some(child) = self.process.as_mut() {
            let _ = child.kill();
            let _ = child.wait();
        }
    }
}

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        renderer: eframe::Renderer::Glow,
        viewport: egui::ViewportBuilder::default()
            .with_title("SonicBridge")
            .with_inner_size([900.0, 680.0])
            .with_min_inner_size([760.0, 560.0]),
        ..Default::default()
    };

    eframe::run_native(
        "SonicBridge",
        options,
        Box::new(|cc| {
            let mut visuals = egui::Visuals::dark();
            visuals.panel_fill = egui::Color32::from_rgb(18, 22, 29);
            visuals.window_fill = egui::Color32::from_rgb(24, 29, 38);
            visuals.selection.bg_fill = egui::Color32::from_rgb(53, 112, 154);
            visuals.widgets.active.bg_fill = egui::Color32::from_rgb(53, 112, 154);
            visuals.widgets.hovered.bg_fill = egui::Color32::from_rgb(39, 49, 63);
            cc.egui_ctx.set_visuals(visuals);
            cc.egui_ctx.style_mut_of(egui::Theme::Dark, |style| {
                style.spacing.item_spacing = egui::vec2(10.0, 9.0);
                style.spacing.button_padding = egui::vec2(12.0, 7.0);
            });
            Ok(Box::new(SonicBridgeApp::new()))
        }),
    )
}
