use std::{
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crate::gdbremote::{self, GDBRemote};
use crate::sampler::{Sample, Sampler};

const SAMPLE_BUFFER_SIZE: usize = 1024;

// TODO:
// - maximize probe clock

enum ThreadCommand {
    SetActiveAddresses(Vec<u32>),
    Stop,
}

pub struct MemSampler {
    join_handle: thread::JoinHandle<()>,
    command_tx: mpsc::Sender<ThreadCommand>,
    sampled_rx: mpsc::Receiver<Sample>,
    available_elf_symbols: Vec<(u32, String)>,
}

impl MemSampler {
    pub fn start<A: ToSocketAddrs>(
        address: A,
        rate: f64,
        maybe_elf_filename: Option<PathBuf>,
    ) -> MemSampler {
        let (sampled_tx, sampled_rx) = mpsc::sync_channel(SAMPLE_BUFFER_SIZE);
        let (command_tx, command_rx) = mpsc::channel();

        let address = address.to_socket_addrs().unwrap().next().unwrap();

        let join_handle =
            thread::spawn(move || sampler_thread(address, rate, sampled_tx, command_rx));

        let mut available_elf_symbols = Vec::new();
        if let Some(elf_filename) = maybe_elf_filename {
            if let Some(parsed_symbols) = parse_elf_symbols(elf_filename) {
                available_elf_symbols = parsed_symbols
                    .into_iter()
                    .filter_map(|symbol| {
                        use elf::abi::{STT_COMMON, STT_OBJECT, STT_TLS};

                        if symbol.size != 4 {
                            return None;
                        }
                        if ![STT_COMMON, STT_OBJECT, STT_TLS].contains(&symbol.type_) {
                            return None;
                        }
                        if (symbol.value & !0x00000000FFFFFFFF) != 0 {
                            return None;
                        }

                        let signal_name =
                            format!("{} (0x{:08x})", symbol.name, symbol.value as u32);

                        Some((symbol.value as u32, signal_name))
                    })
                    .collect();
            } else {
                // TODO: better error handling and reporting
                println!("Failed to parse ELF symbols");
            }
        }

        let sampler = MemSampler {
            join_handle,
            command_tx,
            sampled_rx,
            available_elf_symbols,
        };

        sampler
    }
}

impl Sampler for MemSampler {
    fn available_signals(&self) -> Vec<(u32, String)> {
        self.available_elf_symbols.clone()
    }

    fn set_active_signals(&self, ids: &[u32]) {
        self.command_tx
            .send(ThreadCommand::SetActiveAddresses(ids.to_vec()))
            .unwrap();
    }

    fn sampled_channel(&self) -> &mpsc::Receiver<Sample> {
        &self.sampled_rx
    }

    fn stop(self: Box<Self>) {
        // TODO: do not panic here if sending fails because the channel is closed;
        // it __should__ mean that the thread is already terminated. Maybe, check for
        // this through the `join_handle`.
        self.command_tx.send(ThreadCommand::Stop).unwrap();
        self.join_handle.join().unwrap();
    }
}

fn sampler_thread(
    address: SocketAddr,
    rate: f64,
    sampled_tx: mpsc::SyncSender<Sample>,
    command_rx: mpsc::Receiver<ThreadCommand>,
) {
    // TODO: handle and report errors of various kind, during initial connection
    // and handshake

    let mut gdb = GDBRemote::connect(address);

    const DEBUG_PRINT: bool = false;

    if !gdb.read_response().is_ack() {
        panic!("Expected initial ACK");
    }

    gdb.send_packet("QStartNoAckMode");
    if !gdb.read_response().is_ack() {
        panic!("Expected ACK for QStartNoAckMode");
    }
    if !gdb.read_response().is_packet_with("OK") {
        panic!("Expected OK for QStartNoAckMode");
    }

    gdb.send_packet("c");

    let mut active_memory_addresses = Vec::new();

    let period = Duration::from_secs_f64(1.0 / rate);

    let mut last_sampled_at = Instant::now();
    let start = Instant::now();
    loop {
        // 1. process commands, if any
        match command_rx.try_recv() {
            Ok(ThreadCommand::Stop) => {
                break;
            }
            Ok(ThreadCommand::SetActiveAddresses(memory_addresses)) => {
                // TODO: validate before setting, if we can even do that?
                // TODO: limit the number of addresses that can be sampled?
                active_memory_addresses = memory_addresses;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => panic!("Thread command channel closed TX end"),
        }

        // 2. wait for the next sample time
        let elapsed = last_sampled_at.elapsed();
        if elapsed < period {
            thread::sleep(period - elapsed);
        }
        last_sampled_at += period;

        let lag = last_sampled_at.elapsed();
        if lag > period / 2 {
            // TODO: keep track of the frequency of these events, and react appropriately;
            // some considerations/ideas, to be evaluated:
            // - skip the next sample if we lagged more than 50%/75%
            // - if the frequency of this event is over a given threshold, decrease the sampling
            //   rate; this should be somehow communicated through the sampler interface, but the
            //   details of the interface related to the sampling rate are still to be defined
            //   (if any)
            println!(
                "lagging behind by {}us ({}%)",
                lag.as_micros(),
                (lag.as_secs_f64() * rate * 100.0).round() as i32
            );
        }

        // 3. sample
        // TODOs:
        // + find where it's better to acquire the timestamp of the samples:
        //   - before sending request
        //   - between sending request and waiting response
        //   - after receiving response
        //   we might use some reference signal (like a sine wave) we can compute the
        //   distortion of, and choose the option that minimizes such metric;
        // + reason about the best way to sample multiple signals, in particular by considering
        //   that the sampling rate is fixed at the beginning
        //   - can send all requests at once and wait for all responses, should improve timing
        //   - can interleave samples ("chop"?), but then what values do we send for the other
        //     signals we didn't read?
        //   - can "chop" and just return the value we sampled if the interface allows for this
        // + handle the possibility of the target breaking into some exception handler, calling
        //   for the debugger, or other events that might happend after issuing a GDB "continue"
        //   command to the OpenOCD
        if active_memory_addresses.len() == 0 {
            continue;
        }

        let sampled_at = Instant::now();
        let mut samples = Vec::new();

        for &memory_address in &active_memory_addresses {
            // TODO: support different value sizes?
            gdb.send_packet(&format!("m {:08x},4", memory_address));

            // TODO: handle timeouts
            loop {
                let response = gdb.read_response();

                if DEBUG_PRINT {
                    println!("{:?} : {:?}", response, response.to_string());
                }

                match response {
                    // OpenOCD sends empty 'O' packets during target execution to keep the
                    // connection alive, we ignore those: everything fine if we get one
                    // https://github.com/openocd-org/openocd/blob/2e60e2eca9d06dcb99a4adb81ebe435a72ab0c7f/src/server/gdb_server.c#L3748
                    gdbremote::Response::Packet(data) if data == b"O" => continue,
                    gdbremote::Response::Packet(data) if parse_hex_value(&data).is_some() => {
                        let value = parse_hex_value(&data).unwrap();
                        samples.push((
                            memory_address,
                            f32::from_le_bytes(value.to_be_bytes()) as f64,
                        ));
                        break;
                    }
                    _ => {
                        #[cfg(debug_assertions)]
                        panic!(
                            "Unexpected/unparsable response to read request: {:?}",
                            response
                        );
                        #[cfg(not(debug_assertions))]
                        {
                            // TODO: empty the GDB responses queue, to start fresh, and
                            // try sampling again at next outer iteration
                            println!(
                                "Unexpected/unparsable response to read request: {:?}",
                                response
                            );
                        }
                    }
                }
            }
        }

        // TODO: conversion from u128 to u64 could fail
        let timestamp = (sampled_at - start).as_micros() as u64;
        sampled_tx
            .send((timestamp, samples))
            .expect("Failed to send sampled value");
    }
}

fn parse_hex_value(data: &[u8]) -> Option<u32> {
    let data_string = String::from_utf8(data.to_vec()).ok()?;
    let value = u32::from_str_radix(&data_string, 16).ok()?;
    Some(value)
}

#[derive(Debug)]
struct ParsedELFSymbol {
    name: String,
    type_: u8,
    value: u64,
    size: u64,
}

fn parse_elf_symbols(path: PathBuf) -> Option<Vec<ParsedELFSymbol>> {
    // TODO: propagate errors, maybe use `anyhow`

    use elf::endian::LittleEndian;

    println!("Opening ELF file {:?}", path);

    let file = std::fs::File::open(path).unwrap();
    let mut elf = elf::ElfStream::<LittleEndian, _>::open_stream(file).unwrap();

    let (symbols, strings) = elf.symbol_table().unwrap().unwrap();

    Some(
        symbols
            .into_iter()
            .filter_map(|symbol| {
                Some(ParsedELFSymbol {
                    name: strings.get(symbol.st_name as usize).ok()?.to_owned(),
                    type_: symbol.st_symtype(),
                    value: symbol.st_value,
                    size: symbol.st_size,
                })
            })
            .collect::<Vec<_>>(),
    )
}
