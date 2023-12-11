use std::collections::HashMap;
use std::path::PathBuf;

use eframe::egui;

mod buffer;
mod export;
mod gdbremote;
mod openocd;
mod sampler;
mod utils;

use buffer::SampleBuffer;
use sampler::{FakeSampler, MemSampler, RTTSampler, Sampler};

#[derive(Debug, PartialEq, Eq)]
enum SamplingMethod {
    MemorySamping,
    RTT,
    Simulated,
}

pub struct SignalConfig {
    id: u32,
    name: String,
    enabled: bool,
    scale: f64,
}

impl SignalConfig {
    fn new(id: u32, name: String) -> SignalConfig {
        SignalConfig {
            id,
            name,
            enabled: false,
            scale: 1.0.into(),
        }
    }
}

struct OCDScope {
    show_connect_dialog: bool,
    show_add_address_dialog: bool,

    show_error_dialog: bool,
    error_title: String,
    error_message: String,

    plot_auto_follow: bool,
    plot_auto_follow_time: f64,

    buffer_auto_truncate: bool,
    buffer_auto_truncate_at: f64,

    sampling_method: SamplingMethod,
    current_sampler: Option<Box<dyn Sampler>>,
    current_sampler_status: Option<sampler::Status>,
    last_sampler_info: String,
    signals: Vec<SignalConfig>,
    samples: HashMap<u32, SampleBuffer>,
    max_time: u64,

    gdb_address: String,
    elf_filename: Option<PathBuf>,
    telnet_address: String,
    sample_rate_string: String,
    rtt_polling_interval_string: String,
    rtt_relative_time: bool,

    memory_address_to_add_string: String,
}

impl OCDScope {
    pub fn new() -> OCDScope {
        OCDScope {
            show_connect_dialog: true,
            show_add_address_dialog: false,
            show_error_dialog: false,
            error_title: "".into(),
            error_message: "".into(),
            plot_auto_follow: false,
            plot_auto_follow_time: 1.0,
            buffer_auto_truncate: true,
            buffer_auto_truncate_at: 10.0,
            current_sampler: None,
            current_sampler_status: None,
            last_sampler_info: "".into(),
            samples: HashMap::new(),
            max_time: 0,
            sampling_method: SamplingMethod::Simulated,
            gdb_address: "127.0.0.1:3333".into(),
            elf_filename: None,
            telnet_address: "127.0.0.1:4444".into(),
            sample_rate_string: "1000.0".into(),
            rtt_polling_interval_string: "1".into(),
            rtt_relative_time: false,
            signals: Vec::new(),
            memory_address_to_add_string: "BEEF1010".into(),
        }
    }

    fn reset_buffer(&mut self) {
        self.samples.clear();
        self.max_time = 0;
    }

    fn any_dialog_visible(&self) -> bool {
        self.show_add_address_dialog || self.show_connect_dialog || self.show_error_dialog
    }

    fn show_error(&mut self, title: String, message: String) {
        if self.show_error_dialog {
            log::warn!(
                "asked to show error but another one is active, old error will be overwritten"
            );
        }

        self.close_all_dialogs();

        debug_assert!(self.show_error_dialog == false);

        log::error!("displaying error to user: {}: {}", title, message);

        self.error_title = title;
        self.error_message = message;
        self.show_error_dialog = true;
    }

    fn close_all_dialogs(&mut self) {
        self.show_add_address_dialog = false;
        self.show_connect_dialog = false;
        self.show_error_dialog = false;
    }

    fn handle_messages(&mut self, ctx: &egui::Context) {
        let mut sampler_terminated = false;

        if let Some(sampler) = self.current_sampler.take() {
            while let Ok(notification) = sampler.notification_channel().try_recv() {
                match notification {
                    sampler::Notification::NewStatus(status) => {
                        self.current_sampler_status = Some(status);

                        if status == sampler::Status::Terminated {
                            sampler_terminated = true;
                        }
                    }
                    sampler::Notification::Info(message) => {
                        self.last_sampler_info = message;
                    }
                    sampler::Notification::Error(message) => {
                        self.show_error("Sampler error".into(), message);
                    }
                }
            }

            // we temporarely took ownership of the sampler by moving it out of `self`,
            // now we put it back
            self.current_sampler = Some(sampler);
        }

        if sampler_terminated {
            debug_assert!(self.current_sampler.is_some());

            let sampler = self.current_sampler.take().unwrap();
            sampler.stop();

            debug_assert!(self.current_sampler.is_none());
        }

        if let Some(sampler) = &self.current_sampler {
            debug_assert!(!sampler_terminated);

            while let Ok((t, samples)) = sampler.sampled_channel().try_recv() {
                for (id, y) in samples.into_iter() {
                    self.samples
                        .entry(id)
                        .or_insert_with(|| SampleBuffer::new())
                        .push(t as f64 * 1e-6, y);
                }

                if t > self.max_time {
                    self.max_time = t;
                }
            }

            if self.buffer_auto_truncate {
                for (_, buffer) in self.samples.iter_mut() {
                    buffer.truncate(self.buffer_auto_truncate_at);
                }
            }

            // TODO: might use `request_repaint_after` to reduce CPU usage
            ctx.request_repaint();
        }
    }

    fn try_connect_sampler(&mut self) -> anyhow::Result<Box<dyn Sampler>> {
        let sample_rate = self.sample_rate_string.parse::<f64>();
        let rtt_polling_interval = self.rtt_polling_interval_string.parse::<u32>();

        let sampler: Box<dyn Sampler> = match self.sampling_method {
            SamplingMethod::Simulated => Box::new(FakeSampler::start(sample_rate?)),
            SamplingMethod::MemorySamping => Box::new(MemSampler::start(
                &self.gdb_address,
                &self.telnet_address,
                sample_rate?,
                self.elf_filename.clone(),
            )?),
            SamplingMethod::RTT => Box::new(RTTSampler::start(
                &self.telnet_address,
                rtt_polling_interval?,
            )?),
        };

        self.reset_buffer();

        // we expect that there is currently no sampler active, we assert
        // this in debug mode and try to fix the incident in release mode
        debug_assert!(self.current_sampler.is_none());
        if let Some(previous_sampler) = self.current_sampler.take() {
            log::warn!("found an active sampler before connecting a new one, trying to stop it");
            previous_sampler.stop();
        }

        self.signals = sampler
            .available_signals()
            .into_iter()
            .map(|(id, name)| SignalConfig::new(id, name))
            .collect();

        if let Some(first) = self.signals.iter_mut().next() {
            first.enabled = true;
            sampler.set_active_signals(&[first.id]);
        }

        Ok(sampler)
    }
}

impl eframe::App for OCDScope {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_messages(ctx);

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            if self.any_dialog_visible() {
                ui.set_enabled(false);
            }

            ui.heading("OCDScope");
            ui.group(|toolbar_group| {
                toolbar_group.horizontal(|toolbar| {
                    if self.current_sampler.is_none() {
                        if toolbar.button("Connect...").clicked() {
                            self.show_connect_dialog = true;
                        }
                    } else {
                        if toolbar.button("Disconnect").clicked() {
                            let sampler = self.current_sampler.take().unwrap();
                            sampler.stop();

                            debug_assert!(self.current_sampler.is_none());
                        }

                        // TODO: enable/disable buttons based on current state, or only show one
                        // TODO: fancy icons
                        if toolbar.button("Pause").clicked() {
                            self.current_sampler.as_ref().unwrap().pause();
                        }
                        if toolbar.button("Resume").clicked() {
                            self.current_sampler.as_ref().unwrap().resume();
                        }

                        // TODO: fancy icons
                        if let Some(status) = self.current_sampler_status {
                            toolbar.label(format!("{:?}", status));
                        }

                        toolbar.label(&self.last_sampler_info);
                    }
                });
            });
        });

        let mut reset_plot = false;

        egui::panel::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Buffer").strong());

                ui.group(|ui| {
                    let (used, capacity) = self.samples.values().fold((0, 0), |(u, c), buffer| {
                        let (bu, bc) = buffer.memory_footprint();
                        (u + bu, c + bc)
                    });

                    ui.label(format!("Size: {}", utils::human_readable_size(used)));
                    ui.label(format!(
                        "Capacity: {}",
                        utils::human_readable_size(capacity)
                    ));
                });

                if ui.button("Export data...").clicked() {
                    let maybe_filename = rfd::FileDialog::new()
                        .add_filter("CSV file (*.csv)", &["csv"])
                        .add_filter("Numpy data (*.npy)", &["npy"])
                        .set_file_name("export.csv")
                        .save_file();
                    if let Some(filename) = maybe_filename {
                        match filename
                            .extension()
                            .map(|s| s.to_str().unwrap().to_ascii_lowercase())
                        {
                            Some(ext) if ext == "csv" => {
                                log::info!("exporting CSV file to {:?}", filename);

                                match export::write_csv(&filename, &self.signals, &self.samples) {
                                    Ok(_) => log::info!("export successful"),
                                    Err(err) => {
                                        self.show_error(
                                            "CSV export error".into(),
                                            format!("{:?}", err),
                                        );
                                    }
                                }
                            }
                            Some(ext) if ext == "npy" => {
                                log::info!("exporting NumPy file to {:?}", filename);
                                match export::write_npy(&filename, &self.signals, &self.samples) {
                                    Ok(_) => log::info!("export successful"),
                                    Err(err) => {
                                        self.show_error(
                                            "NumPy export error".into(),
                                            format!("{:?}", err),
                                        );
                                    }
                                }
                            }
                            Some(ext) => {
                                self.show_error(
                                    "Unsupported export format".into(),
                                    format!("Cannot export file with extension {ext:?}"),
                                );
                            }
                            None => {} // operation was cancelled
                        }
                    }
                }

                ui.separator();

                ui.label(egui::RichText::new("Controls").strong());
                reset_plot |= ui.button("Reset plot").clicked();

                ui.checkbox(&mut self.plot_auto_follow, "Plot auto follow");
                ui.horizontal(|ui| {
                    ui.set_enabled(self.plot_auto_follow);

                    ui.label("Show last ");
                    ui.add(
                        egui::DragValue::new(&mut self.plot_auto_follow_time)
                            .clamp_range(1.0..=3600.0)
                            .suffix(" s"),
                    );
                });

                ui.checkbox(&mut self.buffer_auto_truncate, "Buffer auto truncate");
                ui.horizontal(|ui| {
                    ui.set_enabled(self.buffer_auto_truncate);

                    ui.label("Keep last ");
                    ui.add(
                        egui::DragValue::new(&mut self.buffer_auto_truncate_at)
                            .clamp_range(1.0..=3600.0)
                            .suffix(" s"),
                    );
                });

                ui.separator();

                ui.label(egui::RichText::new("Signals").strong());

                let mut some_enable_changed = false;
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for signal in self.signals.iter_mut() {
                            ui.horizontal(|item| {
                                some_enable_changed |=
                                    item.checkbox(&mut signal.enabled, "").changed();

                                item.add(egui::DragValue::new(&mut signal.scale).max_decimals(12).min_decimals(1).speed(0.1));

                                egui::TextEdit::singleline(&mut signal.name)
                                    .id(egui::Id::new(format!("signal-name-{}", signal.id)))
                                    .show(item);
                            });
                        }
                    });

                if matches!(self.sampling_method, SamplingMethod::MemorySamping) {
                    if ui.button("Add memory address").clicked() {
                        self.show_add_address_dialog = true;
                    }
                }

                if some_enable_changed {
                    if let Some(sampler) = &self.current_sampler {
                        let active_ids = self
                            .signals
                            .iter()
                            .filter_map(|signal| {
                                if signal.enabled {
                                    Some(signal.id)
                                } else {
                                    None
                                }
                            })
                            .collect::<Vec<_>>();

                        sampler.set_active_signals(&active_ids);
                    }
                }
            });

        egui::CentralPanel::default()
            .show(ctx, |ui| {
                use egui_plot::{Legend, Line, Plot, PlotBounds};

                // TODO: a vector with linear search might be more efficient, investigate
                let signal_scales = self
                    .signals
                    .iter()
                    .map(|signal| (signal.name.clone(), signal.scale))
                    .collect::<HashMap<_, _>>();

                let mut plot = Plot::new("main")
                    .legend(Legend::default())
                    .allow_zoom(egui::Vec2b::new(true, false))
                    .y_axis_width(2)
                    .auto_bounds_x()
                    .auto_bounds_y()
                    .label_formatter(move |name, value| {
                        if let Some(scale) = signal_scales.get(name) {
                            format!("{}\nx: {}\ny: {}", name, value.x, value.y / scale)
                        } else {
                            "".to_owned()
                        }
                    });

                if reset_plot {
                    plot = plot.reset();
                    self.plot_auto_follow = false;
                }

                plot.show(ui, |plot_ui| {
                    for signal in &self.signals {
                        if signal.enabled {
                            if let Some(buffer) = self.samples.get(&signal.id) {
                                // TODO: constructing PlotPoints from an explicit callback makes the Plot widget
                                // only ask for visible points; figure out how to provide the points in a memory and
                                // computationally efficient way (some clever data structure might be needed), so that
                                // we make use of this fact; moreover:
                                // - try to dynamically adjust the resolution of the plotted signal (`points` parameter)
                                // - figure out if and how we need to resample the signal, especially if the resolution
                                //   changes dynamically
                                // - find a way to handle memory: we might need some `Sync` data structure since the
                                //   callback must be `'static`

                                /*
                                let t0 = points[0][0];
                                let tf = points[points.len() - 1][0];
                                let count = points.len();

                                let points = points.clone();

                                let plot_points = PlotPoints::from_explicit_callback(
                                    move |x| {
                                        // TODO: binary search, or some optimized underlying data structure
                                        let mut res = points[0][1];
                                        for &[t, y] in &points {
                                            res = y;
                                            if t > x {
                                                break;
                                            }
                                        }
                                        res
                                    },
                                    t0..tf,
                                    count,
                                );
                                */

                                // TODO: handle the auto follow mode
                                let bounds = plot_ui.plot_bounds();
                                let (x_min, x_max) = (bounds.min()[0], bounds.max()[0]);
                                let width = x_max - x_min;
                                debug_assert!(width >= 0.0);
                                let margin = if width == 0.0 { 0.1 } else { width };

                                let line = Line::new(
                                    buffer.plot_points(
                                        x_min - margin / 2.0,
                                        x_max + margin / 2.0,
                                        signal.scale,
                                    ),
                                    // buffer.plot_points_generator(x_min - margin / 2.0, x_max + margin / 2.0, 1000),
                                )
                                .name(signal.name.clone());

                                plot_ui.line(line);
                            }
                        }
                    }

                    let response = plot_ui.response();

                    if response.clicked() || response.secondary_clicked() {
                        self.plot_auto_follow = false;
                    }

                    if self.plot_auto_follow {
                        let x_max = self.max_time as f64 * 1e-6;
                        let x_min = x_max - self.plot_auto_follow_time;
                        plot_ui.set_plot_bounds(PlotBounds::from_min_max(
                            [x_min, -10.0],
                            [x_max, 10.0],
                        ))
                    }
                });
            })
            .inner;

        if self.show_add_address_dialog {
            egui::Window::new("Add memory address")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label("Hex address: ");
                        ui.text_edit_singleline(&mut self.memory_address_to_add_string);
                    });

                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_add_address_dialog = false;
                        }
                        if ui.button("Add").clicked() {
                            if let Ok(address) =
                                u32::from_str_radix(&self.memory_address_to_add_string, 16)
                            {
                                self.signals
                                    .push(SignalConfig::new(address, format!("0x{:08x}", address)));

                                self.show_add_address_dialog = false;
                            }
                        }
                    });
                });
        }

        if self.show_connect_dialog {
            egui::Window::new("Connection settings")
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.radio_value(
                        &mut self.sampling_method,
                        SamplingMethod::MemorySamping,
                        "Memory sampling",
                    );
                    ui.radio_value(&mut self.sampling_method, SamplingMethod::RTT, "RTT");
                    ui.radio_value(
                        &mut self.sampling_method,
                        SamplingMethod::Simulated,
                        "Simulated fake data",
                    );

                    ui.separator();

                    if matches!(
                        self.sampling_method,
                        SamplingMethod::MemorySamping | SamplingMethod::RTT
                    ) {
                        ui.horizontal(|ui| {
                            ui.label("OpenOCD Telnet endpoint: ");
                            ui.text_edit_singleline(&mut self.telnet_address);
                        });
                    }
                    if matches!(self.sampling_method, SamplingMethod::MemorySamping) {
                        ui.horizontal(|ui| {
                            ui.label("OpenOCD GDB endpoint: ");
                            ui.text_edit_singleline(&mut self.gdb_address);
                        });
                        ui.horizontal(|ui| {
                            let elf_label_text = match &self.elf_filename {
                                Some(path) => {
                                    path.file_name().unwrap().to_string_lossy().to_owned()
                                }
                                None => "<no ELF file>".into(),
                            };
                            ui.label(elf_label_text);
                            if ui.button("Open..").clicked() {
                                self.elf_filename = rfd::FileDialog::new()
                                    .add_filter("ELF executable (*.elf)", &["elf"])
                                    .pick_file();
                            }
                        });
                    }
                    if matches!(
                        self.sampling_method,
                        SamplingMethod::MemorySamping | SamplingMethod::Simulated
                    ) {
                        ui.horizontal(|ui| {
                            ui.label("Sampling rate [Hz]: ");
                            ui.text_edit_singleline(&mut self.sample_rate_string);
                        });
                    }
                    if matches!(self.sampling_method, SamplingMethod::RTT) {
                        ui.horizontal(|ui| {
                            ui.label("Polling interval [ms]: ");
                            ui.text_edit_singleline(&mut self.rtt_polling_interval_string);
                        });
                        ui.checkbox(&mut self.rtt_relative_time, "Relative timestamp");
                    }

                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_connect_dialog = false;
                        }
                        if ui.button("Connect").clicked() {
                            debug_assert!(self.current_sampler.is_none());

                            self.show_connect_dialog = false;

                            match self.try_connect_sampler() {
                                Ok(sampler) => {
                                    self.current_sampler = Some(sampler);
                                }
                                Err(err) => {
                                    self.show_error(
                                        "Failed to start sampler".to_string(),
                                        format!("{:?}", err),
                                    );
                                }
                            }
                        }
                    });
                });
        }

        if self.show_error_dialog {
            egui::Window::new(&self.error_title)
                .collapsible(false)
                .resizable(false)
                .show(ctx, |ui| {
                    ui.label(&self.error_message);
                    if ui.button("Close").clicked() {
                        self.show_error_dialog = false;
                    }
                });
        }
    }
}

fn main() {
    simple_logger::SimpleLogger::new()
        .with_level(log::LevelFilter::Info)
        .env()
        .init()
        .unwrap();

    let viewport = egui::ViewportBuilder::default()
        .with_inner_size([800.0, 600.0])
        .with_title("OCDScope")
        .with_app_id("ocdscope");

    let options = eframe::NativeOptions {
        viewport,
        default_theme: eframe::Theme::Dark,
        follow_system_theme: false,
        ..Default::default()
    };

    eframe::run_native(
        "ocdscope",
        options,
        Box::new(|_| {
            let app = OCDScope::new();
            Box::new(app)
        }),
    )
    .expect("eframe::run_native error");
}
