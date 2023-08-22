use std::path::PathBuf;
use std::sync::{mpsc, Arc, Mutex};
use std::thread;

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

    plot_auto_follow: bool,

    current_sampler: Option<Box<dyn Sampler>>,
    samples: Vec<[f64; 2]>,
    max_time: u64,

    signals: Vec<(String, bool)>,

    sampling_method: SamplingMethod,
    gdb_address: String,
    sample_rate_string: String,
}

impl OCDScope {
    pub fn new() -> OCDScope {
        OCDScope {
            show_connect_dialog: true,
            plot_auto_follow: false,
            current_sampler: None,
            samples: Vec::new(),
            max_time: 0,
            sampling_method: SamplingMethod::Simulated,
            gdb_address: "127.0.0.1:3333".into(),
            sample_rate_string: "1000.0".into(),
            signals: (0..20)
                .map(|i| (format!("Signal {}", i), i < 8))
                .collect::<Vec<_>>(),
        }
    }

    fn reset_buffer(&mut self) {
        self.samples.clear();
        self.max_time = 0;
    }
}

impl eframe::App for OCDScope {
    fn update(&mut self, ctx: &egui::Context, frame: &mut eframe::Frame) {
        if let Some(sampler) = &self.current_sampler {
            while let Ok((t, y)) = sampler.sampled_channel().try_recv() {
                self.samples.push([t as f64 * 1e-6, y]);
                if t > self.max_time {
                    self.max_time = t;
                }
            }
            // TODO: might use `request_repaint_after` to reduce CPU usage
            ctx.request_repaint();
        }

        egui::TopBottomPanel::top("toolbar").show(ctx, |ui| {
            if self.show_connect_dialog {
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
            .show(ctx, |ui| {
                ui.label("Controls");
                let maybe_plot_command = ui
                    .group(|ui| {
                        let reset_plot = ui.button("Reset plot").clicked();
                        ui.checkbox(&mut self.plot_auto_follow, "Auto follow");

                        if reset_plot {
                            Some(PlotCommand::Reset)
                        } else {
                            None
                        }
                    })
                    .inner;

                ui.spacing();

                ui.label("Signals");

                egui::ScrollArea::vertical()
                    .max_height(240.0)
                    .show(ui, |ui| {
                        for (name, enable) in self.signals.iter_mut() {
                            ui.horizontal(|item| {
                                item.checkbox(enable, "");
                                let id = name.clone();
                                egui::TextEdit::singleline(name)
                                    .id(egui::Id::new(id))
                                    .desired_width(100.0)
                                    .show(item);
                            });
                        }
                    });

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
                    plot_ui.line(
                        egui::plot::Line::new(egui::plot::PlotPoints::from(self.samples.clone()))
                            .name("curve"),
                    );

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

                    ui.horizontal(|ui| {
                        ui.label("OpenOCD GDB server address: ");
                        ui.text_edit_singleline(&mut self.gdb_address);
                    });
                    ui.horizontal(|ui| {
                        ui.label("Sampling rate [Hz]: ");
                        ui.text_edit_singleline(&mut self.sample_rate_string);
                    });

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
                                        let memory_address: u32 = 0x2000001c;
                                        Box::new(MemSampler::start(
                                            &self.gdb_address,
                                            memory_address,
                                            rate,
                                        ))
                                    }
                                    _ => unimplemented!(),
                                };

                                self.reset_buffer();
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
