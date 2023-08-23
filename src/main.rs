use std::collections::HashMap;
use std::path::PathBuf;

use elf::endian::LittleEndian;

use eframe::egui;

mod fakesampler;
mod gdbremote;
mod memsampler;
mod sampler;
mod signal;

use fakesampler::FakeSampler;
use memsampler::MemSampler;
use sampler::Sampler;

fn open_elf(path: PathBuf) {
    println!("Opening ELF file {:?}", path);

    let file = std::fs::File::open(path).unwrap();
    let mut elf = elf::ElfStream::<LittleEndian, _>::open_stream(file).unwrap();

    let (symbols, strings) = elf.symbol_table().unwrap().unwrap();

    for symbol in symbols {
        if symbol.st_size == 0 {
            continue;
        }
        if symbol.st_value < 0x20000000 {
            continue;
        }

        let name = strings.get(symbol.st_name as usize).unwrap();
        let type_ = symbol.st_symtype();
        let value = symbol.st_value;
        let size = symbol.st_size;

        println!("{} ({}) = {:08x} (size = {})", name, type_, value, size);
    }
}

#[derive(Debug, PartialEq, Eq)]
enum SamplingMethod {
    MemorySamping,
    RTT,
    Simulated,
}

enum PlotCommand {
    Reset,
    SetAutoFollow(bool),
}

struct OCDScope {
    show_connect_dialog: bool,
    show_add_address_dialog: bool,

    plot_auto_follow: bool,
    buffer_auto_truncate: bool,

    sampling_method: SamplingMethod,
    current_sampler: Option<Box<dyn Sampler>>,
    signals: Vec<(u32, String, bool)>,
    active_signal_ids: Vec<u32>,
    samples: HashMap<u32, Vec<[f64; 2]>>,
    max_time: u64,

    gdb_address: String,
    telnet_address: String,
    sample_rate_string: String,
    rtt_pooling_rate_string: String,
    rtt_relative_time: bool,

    memory_address_to_add_string: String,
}

impl OCDScope {
    pub fn new() -> OCDScope {
        OCDScope {
            show_connect_dialog: true,
            show_add_address_dialog: false,
            plot_auto_follow: false,
            buffer_auto_truncate: true,
            current_sampler: None,
            samples: HashMap::new(),
            active_signal_ids: Vec::new(),
            max_time: 0,
            sampling_method: SamplingMethod::Simulated,
            gdb_address: "127.0.0.1:3333".into(),
            telnet_address: "127.0.0.1:4444".into(),
            sample_rate_string: "1000.0".into(),
            rtt_pooling_rate_string: "1000.0".into(),
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
        self.show_add_address_dialog || self.show_connect_dialog
    }

    fn close_all_dialogs(&mut self) {
        self.show_add_address_dialog = false;
        self.show_connect_dialog = false;
    }
}

impl eframe::App for OCDScope {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(sampler) = &self.current_sampler {
            while let Ok((t, samples)) = sampler.sampled_channel().try_recv() {
                // TODO/FIXME: something weird happens here, and after changing the active
                //             signals we receive a sample with the old number of signals;
                //             investigate and fix this, then re-enable the assert below
                //debug_assert_eq!(self.active_signal_ids.len(), ys.len());

                if self.active_signal_ids.len() != samples.len() {
                    // TODO: show/log a warning
                }

                for (id, y) in samples.into_iter() {
                    self.samples
                        .entry(id)
                        .or_default()
                        .push([t as f64 * 1e-6, y]);
                }

                if t > self.max_time {
                    self.max_time = t;
                }
            }

            // TODO: might use `request_repaint_after` to reduce CPU usage
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            if self.any_dialog_visible() {
                ui.set_enabled(false);
            }

            ui.heading("OCDScope");
            ui.group(|toolbar_group| {
                toolbar_group.horizontal(|toolbar| {
                    if toolbar.button("Open ELF").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            println!("Picked file {}", path.display());
                        }
                    }

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
                    }
                });
            });
        });

        let maybe_plot_command = egui::panel::SidePanel::left("sidebar")
            .resizable(true)
            .default_width(300.0)
            .show(ctx, |ui| {
                ui.label(egui::RichText::new("Status").strong());

                ui.group(|ui| {
                    let sample_size = std::mem::size_of::<[f64; 2]>();
                    ui.label(format!(
                        "Buffer size: {}",
                        human_readable_size(
                            self.samples.values().map(|ys| ys.len() * sample_size).sum()
                        )
                    ));
                    ui.label(format!(
                        "Buffer capacity: {}",
                        human_readable_size(
                            self.samples
                                .values()
                                .map(|ys| ys.capacity() * sample_size)
                                .sum()
                        )
                    ));
                });

                ui.spacing();

                ui.label(egui::RichText::new("Controls").strong());

                let maybe_plot_command = ui
                    .group(|ui| {
                        let reset_plot = ui.button("Reset plot").clicked();
                        ui.checkbox(&mut self.plot_auto_follow, "Plot auto follow");
                        ui.checkbox(&mut self.buffer_auto_truncate, "Buffer auto truncate");

                        if reset_plot {
                            Some(PlotCommand::Reset)
                        } else {
                            None
                        }
                    })
                    .inner;

                ui.spacing();

                ui.label(egui::RichText::new("Signals").strong());

                let mut some_enable_changed = false;
                egui::ScrollArea::vertical()
                    .max_height(120.0)
                    .show(ui, |ui| {
                        for (_, name, enable) in self.signals.iter_mut() {
                            ui.horizontal(|item| {
                                some_enable_changed |= item.checkbox(enable, "").changed();
                                let id = name.clone();
                                egui::TextEdit::singleline(name)
                                    .id(egui::Id::new(id))
                                    .desired_width(100.0)
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
                            .filter_map(|&(id, _, enabled)| if enabled { Some(id) } else { None })
                            .collect::<Vec<_>>();

                        sampler.set_active_signals(&active_ids);
                        self.active_signal_ids = active_ids;
                    }
                }

                maybe_plot_command
            })
            .inner;

        egui::CentralPanel::default()
            .show(ctx, |ui| {
                let mut plot = egui::plot::Plot::new("main")
                    .legend(egui::plot::Legend::default())
                    .allow_zoom(egui::plot::AxisBools { x: true, y: false })
                    .label_formatter(|name, value| {
                        if !name.is_empty() {
                            format!("{}\nx: {}\ny: {}", name, value.x, value.y)
                        } else {
                            "".to_owned()
                        }
                    });

                if let Some(PlotCommand::Reset) = maybe_plot_command {
                    plot = plot.reset();
                    self.plot_auto_follow = false;
                }

                plot.show(ui, |plot_ui| {
                    for (id, name) in self.signals.iter().filter_map(|(id, name, enabled)| {
                        if *enabled {
                            Some((*id, name.clone()))
                        } else {
                            None
                        }
                    }) {
                        if let Some(points) = self.samples.get(&id) {
                            plot_ui.line(
                                egui::plot::Line::new(egui::plot::PlotPoints::from(points.clone()))
                                    .name(name),
                            );
                        }
                    }

                    if plot_ui.plot_clicked() || plot_ui.plot_secondary_clicked() {
                        self.plot_auto_follow = false;
                    }

                    if self.plot_auto_follow {
                        let x_max = self.max_time as f64 * 1e-6;
                        let x_min = x_max - 1.0;
                        plot_ui.set_plot_bounds(egui::plot::PlotBounds::from_min_max(
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
                                    .push((address, format!("0x{:08x}", address), false));
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
                            ui.label("Pooling rate [Hz]: ");
                            ui.text_edit_singleline(&mut self.rtt_pooling_rate_string);
                        });
                        ui.checkbox(&mut self.rtt_relative_time, "Relative timestamp");
                    }

                    ui.separator();

                    ui.horizontal(|ui| {
                        if ui.button("Cancel").clicked() {
                            self.show_connect_dialog = false;
                        }
                        if ui.button("Connect").clicked() {
                            if let Ok(rate) = self.sample_rate_string.parse::<f64>() {
                                let sampler: Box<dyn Sampler> = match self.sampling_method {
                                    SamplingMethod::Simulated => Box::new(FakeSampler::start(rate)),
                                    SamplingMethod::MemorySamping => {
                                        Box::new(MemSampler::start(&self.gdb_address, rate))
                                    }
                                    _ => unimplemented!(),
                                };

                                self.reset_buffer();

                                // we expect that there is currently no sampler active, we assert
                                // this in debug mode and try to fix the incident in release mode
                                debug_assert!(self.current_sampler.is_none());
                                if let Some(sampler) = self.current_sampler.take() {
                                    sampler.stop();
                                    // TODO: report and log this event as a warning
                                }

                                self.signals = sampler
                                    .available_signals()
                                    .into_iter()
                                    .map(|(id, name)| (id, name, false))
                                    .collect();

                                self.current_sampler = Some(sampler);

                                self.show_connect_dialog = false;
                            }
                        }
                    });
                });
        }
    }
}

fn main() {
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

/*
fn main_custom_telnet() {
    const TELNET_ESCAPE: u8 = 0xFF;
    const TELNET_DONT: u8 = 254;
    const TELNET_ECHO: u8 = 1;

    let mut stream = TcpStream::connect("127.0.0.1:4444").unwrap();
    stream.set_read_timeout(None).unwrap();

    /* Telnet: DON'T ECHO */
    // stream.write_all(&[TELNET_ESCAPE, TELNET_DONT, TELNET_ECHO]).unwrap();

    let command: &[u8] = b"mdw 0x20000000\n";

    let mut buffer = [0 as u8; 128];
    let mut read_buffer = Vec::new();

    let len = stream.read(&mut buffer).unwrap();
    let read = &buffer[0..len];

    println!("Opening packet: {:?}", read);

    if &read[len - 2..] != b"> " {
        panic!("Unexpected hello message");
    }

    println!("Got hello");

    // stream.write_all(command).unwrap();
    stream
        .write_all(&[TELNET_ESCAPE, TELNET_DONT, TELNET_ECHO])
        .unwrap();

    loop {
        let len = stream.read(&mut buffer).unwrap();

        println!("Read: {:?}", &buffer[0..len]);

        read_buffer.extend_from_slice(&buffer[0..len]);

        if let Some(i) = read_buffer.iter().position(|&x| x == b'\n') {
            let line = read_buffer[..i].to_vec();
            read_buffer = read_buffer[i + 1..].to_vec();

            println!("Got line: {}", String::from_utf8(line.to_vec()).unwrap());
        }
    }
}
*/
