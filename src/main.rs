use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::thread;

use elf::endian::LittleEndian;
use libui::controls::{Area, Button, Entry, HorizontalBox, Label, Spacer, Table, VerticalBox};
use libui::prelude::*;

mod gdbremote;
mod memsampler;
mod scope;
mod signal;

use memsampler::MemSampler;

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

fn main() {
    let current_sampler: Arc<Mutex<Option<(MemSampler, thread::JoinHandle<()>)>>> =
        Arc::new(Mutex::new(None));

    let ui = UI::init().expect("Couldn't initialize UI library");

    let mut win = Window::new(&ui.clone(), "OCDScope", 400, 400, WindowType::NoMenubar);

    // controls
    let (scope_handle, scope_area) = scope::new();
    let mut button_open = Button::new("Open ELF...");
    let label_file = Label::new("<no file>");
    let mut button_connect = Button::new("Connect...");
    let label_connection = Label::new("<not connected>");

    /*
    let (sampler, sampled_rx) = MemSampler::start("127.0.0.1:3333", 0x2000001c, 1000.0);

    thread::spawn(move || loop {
        let value = sampled_rx.recv().expect("Failed to .recv() sample value");
        scope_handle.lock().unwrap().add_values(&[value]);
    });
    */

    /*
    let sine = (0..1000)
        .map(|i| {
            let t = i as f64 / 1000.0 * 30.0;
            t.sin() * 50.0
        })
        .collect::<Vec<_>>();

    scope_handle.lock().unwrap().add_values(&values);
    */

    button_open.on_clicked({
        let win = win.clone();
        let mut label_file = label_file.clone();
        move |_| {
            let maybe_path = win.open_file();
            if let Some(path) = maybe_path {
                label_file.set_text(path.file_name().unwrap().to_str().unwrap());
                open_elf(path);
            }
        }
    });

    button_connect.on_clicked({
        let ui = ui.clone();
        let mut parent_button_connect = button_connect.clone();
        let mut parent_label_connection = label_connection.clone();
        let scope_handle = scope_handle.clone();
        let current_sampler = current_sampler.clone();

        move |_| {
            if current_sampler.lock().unwrap().is_none() {
                let mut dialog =
                    Window::new(&ui, "Connection settings", 360, 240, WindowType::NoMenubar);

                let label_address = Label::new("OpenOCD GDB server address: ");
                let mut entry_address = Entry::new();
                entry_address.set_value("127.0.0.1:3333");
                let label_rate = Label::new("Sampling rate [Hz]: ");
                let mut entry_rate = Entry::new();
                entry_rate.set_value("1000.0");
                let mut button_cancel = Button::new("Cancel");
                let mut button_connect = Button::new("Connect");

                button_cancel.on_clicked({
                    let mut dialog = dialog.clone();
                    move |_| {
                        dialog.hide();
                        // TODO: memory leak here, not destroying dialog
                    }
                });

                button_connect.on_clicked({
                    let mut dialog = dialog.clone();
                    let entry_address = entry_address.clone();
                    let entry_rate = entry_rate.clone();
                    let mut parent_button_connect = parent_button_connect.clone();
                    let mut parent_label_connection = parent_label_connection.clone();
                    let scope_handle = scope_handle.clone();
                    let current_sampler = current_sampler.clone();

                    move |_| {
                        let address = entry_address.value();
                        let rate = entry_rate
                            .value()
                            .parse::<f64>()
                            .expect("Failed to parse rate as f64");

                        let (sampler, sampled_rx) =
                            MemSampler::start(address.clone(), 0x2000001c, rate);

                        let scope_handle = scope_handle.clone();
                        let join_handle = thread::spawn(move || loop {
                            match sampled_rx.recv() {
                                Ok(value) => scope_handle.lock().unwrap().add_values(&[value]),
                                Err(_) => break,
                            }
                        });

                        *current_sampler.lock().unwrap() = Some((sampler, join_handle));
                        parent_button_connect.set_text("Disconnect");
                        parent_label_connection.set_text(&address);

                        dialog.hide();
                        // TODO: memory leak here, not destroying dialog
                    }
                });

                let mut hbox = HorizontalBox::new();
                hbox.append(button_cancel, LayoutStrategy::Compact);
                hbox.append(button_connect, LayoutStrategy::Compact);

                let mut vbox = VerticalBox::new();
                vbox.append(label_address, LayoutStrategy::Compact);
                vbox.append(entry_address, LayoutStrategy::Compact);
                vbox.append(label_rate, LayoutStrategy::Compact);
                vbox.append(entry_rate, LayoutStrategy::Compact);
                vbox.append(Spacer::new(), LayoutStrategy::Stretchy);
                vbox.append(hbox, LayoutStrategy::Compact);

                dialog.set_child(vbox);
                dialog.show();
            } else {
                let (sampler, join_handle) = current_sampler.lock().unwrap().take().unwrap();

                sampler.stop();
                println!("Join result: {:?}", join_handle.join());

                parent_button_connect.set_text("Connect...");
                parent_label_connection.set_text("<not connected>");
            }
        }
    });

    // layout
    let mut vbox = VerticalBox::new();
    let mut hbox = HorizontalBox::new();
    hbox.append(button_open, LayoutStrategy::Compact);
    hbox.append(label_file, LayoutStrategy::Stretchy);
    hbox.append(button_connect, LayoutStrategy::Compact);
    hbox.append(label_connection, LayoutStrategy::Stretchy);
    vbox.append(hbox, LayoutStrategy::Compact);
    vbox.append(scope_area, LayoutStrategy::Stretchy);
    win.set_child(vbox);
    win.show();

    ui.main();
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
