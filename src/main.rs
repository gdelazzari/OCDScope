use std::{
    collections::HashMap,
    path::{Path, PathBuf},
};

use eframe::egui;

mod buffer;
mod gdbremote;
mod openocd;
mod parsablefloat;
mod sampler;

use buffer::SampleBuffer;
use parsablefloat::ParsableFloat;
use sampler::{FakeSampler, MemSampler, RTTSampler, Sampler};

#[derive(Debug, PartialEq, Eq)]
enum SamplingMethod {
    MemorySamping,
    RTT,
    Simulated,
}

struct SignalConfig {
    id: u32,
    name: String,
    enabled: bool,
    scale: ParsableFloat,
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
    plot_auto_follow_time: ParsableFloat,

    buffer_auto_truncate: bool,
    buffer_auto_truncate_at: ParsableFloat,

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
            plot_auto_follow_time: 1.0.into(),
            buffer_auto_truncate: true,
            buffer_auto_truncate_at: 10.0.into(),
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
            rtt_polling_interval_string: "100".into(),
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

        if let Some(sampler) = &self.current_sampler {
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
                        if !self.show_error_dialog {
                            self.error_title = "Sampler error".into();
                            self.error_message = message;
                            self.show_error_dialog = true;
                        } else {
                            log::warn!("Sampler error detected, but error dialog already open");
                        }
                    }
                }
            }
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
                    buffer.truncate(self.buffer_auto_truncate_at.value());
                }
            }

            // TODO: might use `request_repaint_after` to reduce CPU usage
            ctx.request_repaint();
        }
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

                    ui.label(format!("Size: {}", human_readable_size(used)));
                    ui.label(format!("Capacity: {}", human_readable_size(capacity)));
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
                                    export_csv(&filename, &self.signals, &self.samples).unwrap();
                                    // TODO: display success or error message to user
                                }
                                Some(ext) if ext == "npy" => {
                                    log::info!("exporting Numpy file to {:?}", filename);
                                    export_npy(&filename, &self.signals, &self.samples).unwrap();
                                    // TODO: display success or error message to user
                                }
                                Some(_) => {
                                    // TODO: display error to user
                                }
                                None => {} // operation was cancelled
                            }
                        }
                    }
                });

                ui.separator();

                ui.label(egui::RichText::new("Controls").strong());
                reset_plot |= ui.button("Reset plot").clicked();

                ui.checkbox(&mut self.plot_auto_follow, "Plot auto follow");
                ui.horizontal(|ui| {
                    ui.set_enabled(self.plot_auto_follow);

                    ui.label("Show last ");

                    let plot_auto_follow_edit_color = if self.plot_auto_follow_time.is_parsed_ok() {
                        egui::Color32::GREEN
                    } else {
                        egui::Color32::LIGHT_RED
                    };
                    if egui::TextEdit::singleline(self.plot_auto_follow_time.editable_string())
                        .desired_width(50.0)
                        .text_color(plot_auto_follow_edit_color)
                        .show(ui)
                        .response
                        .lost_focus()
                    {
                        self.plot_auto_follow_time.update();
                    }

                    ui.label("s");
                });

                ui.checkbox(&mut self.buffer_auto_truncate, "Buffer auto truncate");
                ui.horizontal(|ui| {
                    ui.set_enabled(self.buffer_auto_truncate);

                    ui.label("Keep last ");

                    let buffer_auto_truncate_edit_color =
                        if self.buffer_auto_truncate_at.is_parsed_ok() {
                            egui::Color32::GREEN
                        } else {
                            egui::Color32::LIGHT_RED
                        };
                    if egui::TextEdit::singleline(self.buffer_auto_truncate_at.editable_string())
                        .desired_width(50.0)
                        .text_color(buffer_auto_truncate_edit_color)
                        .show(ui)
                        .response
                        .lost_focus()
                    {
                        self.buffer_auto_truncate_at.update();
                    }

                    ui.label("s");
                });

                ui.separator();

                ui.label(egui::RichText::new("Signals").strong());

                let mut some_enable_changed = false;
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for signal in self.signals.iter_mut() {
                            ui.horizontal(|item| {
                                // TODO: color scale TextEdit in red if the value is invalid?
                                some_enable_changed |=
                                    item.checkbox(&mut signal.enabled, "").changed();
                                let edit_color = if signal.scale.is_parsed_ok() {
                                    egui::Color32::GREEN
                                } else {
                                    egui::Color32::LIGHT_RED
                                };
                                if egui::TextEdit::singleline(signal.scale.editable_string())
                                    .id(egui::Id::new(format!("signal-scale-{}", signal.id)))
                                    .desired_width(50.0)
                                    .text_color(edit_color)
                                    .show(item)
                                    .response
                                    .changed()
                                {
                                    signal.scale.update();
                                }
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
                use egui::plot::{AxisBools, Legend, Line, Plot, PlotBounds};

                // TODO: a vector with linear search might be more efficient, investigate
                let signal_scales = self
                    .signals
                    .iter()
                    .map(|signal| (signal.name.clone(), signal.scale.value()))
                    .collect::<HashMap<_, _>>();

                // TODO: handle vertical scale for each signal
                let mut plot = Plot::new("main")
                    .legend(Legend::default())
                    .allow_zoom(AxisBools { x: true, y: false })
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
                                        signal.scale.value(),
                                    ),
                                    // buffer.plot_points_generator(x_min - margin / 2.0, x_max + margin / 2.0, 1000),
                                )
                                .name(signal.name.clone());

                                plot_ui.line(line);
                            }
                        }
                    }

                    if plot_ui.plot_clicked() || plot_ui.plot_secondary_clicked() {
                        self.plot_auto_follow = false;
                    }

                    if self.plot_auto_follow {
                        let x_max = self.max_time as f64 * 1e-6;
                        let x_min = x_max - self.plot_auto_follow_time.value();
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
                            ui.label("OpenOCD Telnet endpoint: ");
                            ui.text_edit_singleline(&mut self.telnet_address);
                        });
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
                            let sample_rate = self.sample_rate_string.parse::<f64>();
                            let rtt_polling_interval =
                                self.rtt_polling_interval_string.parse::<u32>();

                            // FIXME/TODO: gracefully handle parsing error on sampling rate
                            let sampler: Box<dyn Sampler> = match self.sampling_method {
                                SamplingMethod::Simulated => Box::new(FakeSampler::start(
                                    sample_rate.expect("Failed to parse sample rate"),
                                )),
                                SamplingMethod::MemorySamping => Box::new(MemSampler::start(
                                    &self.gdb_address,
                                    sample_rate.expect("Failed to parse sample rate"),
                                    self.elf_filename.clone(),
                                )),
                                SamplingMethod::RTT => Box::new(
                                    RTTSampler::start(
                                        &self.telnet_address,
                                        rtt_polling_interval
                                            .expect("Failed to parse polling interval"),
                                    )
                                    .unwrap(),
                                ),
                            };

                            self.reset_buffer();

                            // we expect that there is currently no sampler active, we assert
                            // this in debug mode and try to fix the incident in release mode
                            debug_assert!(self.current_sampler.is_none());
                            if let Some(previous_sampler) = self.current_sampler.take() {
                                previous_sampler.stop();
                                // TODO: report and log this event as a warning
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

                            /*
                            // TEMP: used for debug of export with simulated data

                            for signal in self.signals.iter_mut() {
                                signal.enabled = true;
                            }
                            sampler.set_active_signals(
                                &self
                                    .signals
                                    .iter()
                                    .map(|signal| signal.id)
                                    .collect::<Vec<_>>(),
                            );
                            */

                            self.current_sampler = Some(sampler);

                            self.show_connect_dialog = false;
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

    let options = eframe::NativeOptions {
        initial_window_size: Some(egui::vec2(800.0, 600.0)),
        ..Default::default()
    };

    eframe::run_native(
        "OCDScope",
        options,
        Box::new(|_| {
            let app = OCDScope::new();
            Box::new(app)
        }),
    )
    .expect("eframe::run_native error");
}

fn human_readable_size(size: usize) -> String {
    const T: usize = 2048;

    if size < T {
        format!("{} B", size)
    } else if (size / 1024) < T {
        format!("{} KiB", size / 1024)
    } else if (size / 1024 / 1024) < T {
        format!("{} MiB", size / 1024 / 1024)
    } else {
        format!("{} GiB", size / 1024 / 1024 / 1024)
    }
}

fn export_csv(
    filename: &Path,
    signals: &[SignalConfig],
    samples: &HashMap<u32, SampleBuffer>,
) -> std::io::Result<()> {
    use std::fmt::Display;
    use std::io::Write;

    if signals.len() == 0 {
        // nothing to do
        return Ok(());
    }

    let mut file = std::fs::File::create(filename)?;

    fn write_csv_row<I, T>(writer: &mut impl Write, items: I) -> std::io::Result<()>
    where
        I: Iterator<Item = T>,
        T: Display,
    {
        // NOTE: this can be heavily optimized for performance, if needed;
        // currently a lot of allocations are happening

        let row = items
            .map(|item| format!("{}", item))
            .reduce(|row, item_string| row + "," + &item_string)
            .unwrap_or_default();

        writer.write_all(row.as_bytes())?;
        writer.write_all(b"\n")?;

        Ok(())
    }

    write_csv_row(&mut file, signals.iter().map(|signal| &signal.name))?;

    // FIXME: currently we only support exporting a set of signals which share the same
    //        time vector, but we may want to handle the case of different sampling times
    //        (that arises, for instance, when sampling memory, or when the acquisition of
    //        some signals is paused)

    let signal_buffers: Vec<&SampleBuffer> = signals
        .iter()
        .map(|signal| samples.get(&signal.id).unwrap())
        .collect();

    let n_samples = signal_buffers
        .iter()
        .map(|buffer| buffer.samples().len())
        .min()
        .unwrap();

    for i in 0..n_samples {
        let t = signal_buffers[0].samples()[i].x;

        // we assert here, in debug mode, that all the time values are equal
        for buffer in &signal_buffers {
            debug_assert_eq!(t, buffer.samples()[i].x);
        }

        write_csv_row(
            &mut file,
            signal_buffers.iter().map(|buffer| buffer.samples()[i].y),
        )?;
    }

    file.sync_all()?;

    Ok(())
}

fn export_npy(
    filename: &Path,
    signals: &[SignalConfig],
    samples: &HashMap<u32, SampleBuffer>,
) -> std::io::Result<()> {
    use npyz::WriterBuilder;

    if signals.len() == 0 {
        // nothing to do
        return Ok(());
    }

    let mut file = std::fs::File::create(filename)?;

    // FIXME: currently we only support exporting a set of signals which share the same
    //        time vector, but we may want to handle the case of different sampling times
    //        (that arises, for instance, when sampling memory, or when the acquisition of
    //        some signals is paused)

    let signal_buffers: Vec<&SampleBuffer> = signals
        .iter()
        .map(|signal| samples.get(&signal.id).unwrap())
        .collect();

    let n_samples = signal_buffers
        .iter()
        .map(|buffer| buffer.samples().len())
        .min()
        .unwrap();

    let mut writer = {
        npyz::WriteOptions::new()
            .default_dtype()
            .shape(&[n_samples as u64, signal_buffers.len() as u64])
            .writer(&mut file)
            .begin_nd()?
    };

    for i in 0..n_samples {
        let t = signal_buffers[0].samples()[i].x;

        // we assert here, in debug mode, that all the time values are equal
        for buffer in &signal_buffers {
            debug_assert_eq!(t, buffer.samples()[i].x);
        }

        writer.extend(signal_buffers.iter().map(|buffer| buffer.samples()[i].y))?;
    }

    writer.finish()?;
    file.sync_all()?;

    Ok(())
}
