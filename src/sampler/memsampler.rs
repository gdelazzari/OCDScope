use std::{
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use anyhow::Context;

use crate::gdbremote::{self, GDBRemote};
use crate::sampler::{Notification, Sample, Sampler, Status};

const SAMPLE_BUFFER_SIZE: usize = 1024;

// TODO:
// - maximize probe clock

#[derive(Debug)]
enum ThreadCommand {
    SetActiveAddresses(Vec<u32>),
    Pause,
    Resume,
    Stop,
}

pub struct MemSampler {
    join_handle: thread::JoinHandle<()>,
    command_tx: mpsc::Sender<ThreadCommand>,
    sampled_rx: mpsc::Receiver<Sample>,
    notifications_rx: mpsc::Receiver<Notification>,
    available_elf_symbols: Vec<(u32, String)>,
}

impl MemSampler {
    pub fn start<A: ToSocketAddrs>(
        address: A,
        rate: f64,
        maybe_elf_filename: Option<PathBuf>,
    ) -> anyhow::Result<MemSampler> {
        let (sampled_tx, sampled_rx) = mpsc::sync_channel(SAMPLE_BUFFER_SIZE);
        let (command_tx, command_rx) = mpsc::channel();
        let (notifications_tx, notifications_rx) = mpsc::channel();

        let address = address
            .to_socket_addrs()?
            .next()
            .context("no addresses provided")?;

        let join_handle = thread::spawn(move || {
            let result = sampler_thread(
                address,
                rate,
                sampled_tx,
                command_rx,
                notifications_tx.clone(),
            );

            if let Err(err) = result {
                log::error!("sampler thread returned with error: {:?}", err);
                log::debug!("sending error notification and switch to terminated state");

                // ignore the send errors instead of unwrapping, at this point if even sending to
                // `notifications_tx` fails, the situation is sort of unrecoverable
                if let Err(e) = notifications_tx.send(Notification::Error(format!("{:?}", err))) {
                    log::error!("error notification send failed: {:?}", e);
                }
                if let Err(e) = notifications_tx.send(Notification::NewStatus(Status::Terminated)) {
                    log::error!("new status notification send failed: {:?}", e);
                }
            }
        });

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
                log::error!("failed to parse ELF symbols");
            }
        }

        let sampler = MemSampler {
            join_handle,
            command_tx,
            sampled_rx,
            notifications_rx,
            available_elf_symbols,
        };

        Ok(sampler)
    }
}

impl Sampler for MemSampler {
    fn available_signals(&self) -> Vec<(u32, String)> {
        self.available_elf_symbols.clone()
    }

    fn set_active_signals(&self, ids: &[u32]) {
        if let Err(err) = self
            .command_tx
            .send(ThreadCommand::SetActiveAddresses(ids.to_vec()))
        {
            log::error!("failed to send SetActiveAddresses command: {:?}", err);
        }
    }

    fn sampled_channel(&self) -> &mpsc::Receiver<Sample> {
        &self.sampled_rx
    }

    fn notification_channel(&self) -> &mpsc::Receiver<Notification> {
        &self.notifications_rx
    }

    
    fn pause(&self) {
        if let Err(err) = self.command_tx.send(ThreadCommand::Pause) {
            log::error!("failed to send pause command: {:?}", err);
        }
    }

    fn resume(&self) {
        if let Err(err) = self.command_tx.send(ThreadCommand::Resume) {
            log::error!("failed to send resume command: {:?}", err);
        }
    }

    fn stop(self: Box<Self>) {
        if let Err(err) = self.command_tx.send(ThreadCommand::Stop) {
            log::debug!("asked to stop sampler but thread seems to already be dead (command send failed: {:?})", err);

            debug_assert!(self.join_handle.is_finished());
        }

        // TODO: if there are implementation errors in the sampler thread, and the Stop command is not processed,
        // this can block indefinitely
        if let Err(err) = self.join_handle.join() {
            log::warn!("failed to join sampler thread: {:?}", err);
        }
    }
}

fn sampler_thread(
    address: SocketAddr,
    rate: f64,
    sampled_tx: mpsc::SyncSender<Sample>,
    command_rx: mpsc::Receiver<ThreadCommand>,
    notifications_tx: mpsc::Sender<Notification>,
) -> anyhow::Result<()> {
    let mut gdb = GDBRemote::connect(address)?;

    gdb.set_timeout(Duration::from_millis(2000));

    if !gdb.read_response()?.is_ack() {
        anyhow::bail!("expected initial ACK");
    }

    gdb.send_packet("QStartNoAckMode")?;
    if !gdb.read_response()?.is_ack() {
        anyhow::bail!("expected ACK for QStartNoAckMode");
    }
    if !gdb.read_response()?.is_packet_with("OK") {
        anyhow::bail!("expected OK for QStartNoAckMode");
    }

    let period = Duration::from_secs_f64(1.0 / rate);

    let mut status = Status::Initializing;
    let mut last_sampled_at = Instant::now();
    let start = Instant::now();
    let mut active_memory_addresses = Vec::new();

    loop {
        let mut maybe_new_status = None;

        match status {
            Status::Initializing => {
                // make target continue
                gdb.send_packet("c")?;

                maybe_new_status = Some(Status::Sampling);
                last_sampled_at = Instant::now();
            }
            Status::Sampling => {
                // 1. process commands, if any
                match command_rx.try_recv() {
                    Ok(ThreadCommand::Stop) => {
                        maybe_new_status = Some(Status::Terminated);
                    }
                    Ok(ThreadCommand::Pause) => {
                        maybe_new_status = Some(Status::Paused);
                    }
                    Ok(ThreadCommand::SetActiveAddresses(memory_addresses)) => {
                        // TODO: validate before setting, if we can even do that?
                        // TODO: limit the number of addresses that can be sampled?
                        active_memory_addresses = memory_addresses;
                    }
                    Ok(other) => {
                        log::warn!("unexpected command in sampling state: {:?}", other);
                    }
                    Err(mpsc::TryRecvError::Empty) => {}
                    Err(mpsc::TryRecvError::Disconnected) => {
                        anyhow::bail!("thread command channel closed TX end")
                    }
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
                    log::warn!(
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

                let sampled_at = Instant::now();
                let mut samples = Vec::new();

                for &memory_address in &active_memory_addresses {
                    // TODO: support different value sizes?
                    gdb.send_packet(&format!("m {:08x},4", memory_address))?;

                    // TODO: handle timeouts
                    loop {
                        let response = gdb.read_response()?;

                        log::trace!("{:?} : {:?}", response, response.to_string());

                        match response {
                            // OpenOCD sends empty 'O' packets during target execution to keep the
                            // connection alive, we ignore those: everything fine if we get one
                            // https://github.com/openocd-org/openocd/blob/2e60e2eca9d06dcb99a4adb81ebe435a72ab0c7f/src/server/gdb_server.c#L3748
                            gdbremote::Response::Packet(data) if data == b"O" => continue,
                            gdbremote::Response::Packet(data)
                                if parse_hex_value(&data).is_some() =>
                            {
                                let value =
                                    parse_hex_value(&data).context("failed to parse hex data")?;

                                samples.push((
                                    memory_address,
                                    f32::from_le_bytes(value.to_be_bytes()) as f64,
                                ));
                                break;
                            }
                            _ => {
                                #[cfg(debug_assertions)]
                                anyhow::bail!(
                                    "Unexpected/unparsable response to read request: {:?}",
                                    response
                                );
                                #[cfg(not(debug_assertions))]
                                {
                                    // TODO: empty the GDB responses queue, to start fresh, and
                                    // try sampling again at next outer iteration
                                    log::error!(
                                        "unexpected/unparsable response to read request: {:?}",
                                        response
                                    );
                                }
                            }
                        }
                    }
                }

                if samples.len() > 0 {
                    // TODO: conversion from u128 to u64 could fail
                    let timestamp = (sampled_at - start).as_micros() as u64;
                    sampled_tx.send((timestamp, samples))?;
                }
            }
            Status::Paused => match command_rx.recv() {
                // TODO: should we handle the empty 'O' packets sent by OpenOCD also here?

                Ok(ThreadCommand::Stop) => {
                    maybe_new_status = Some(Status::Terminated);
                }
                Ok(ThreadCommand::Resume) => {
                    maybe_new_status = Some(Status::Sampling);
                    last_sampled_at = Instant::now();
                }
                Ok(ThreadCommand::SetActiveAddresses(memory_addresses)) => {
                    // TODO: validate before setting, if we can even do that?
                    // TODO: limit the number of addresses that can be sampled?
                    active_memory_addresses = memory_addresses;
                }
                Ok(other) => {
                    log::warn!("Unexpected command in paused state: {:?}", other);
                }
                Err(err) => {
                    anyhow::bail!("Closed TX end of command channel ({})", err);
                }
            },
            Status::Terminated => {
                // break the main loop, finishing this thread
                break;
            }
        }

        match maybe_new_status {
            Some(new_status) if new_status != status => {
                notifications_tx.send(Notification::NewStatus(new_status))?;
                status = new_status;
            }
            _ => {}
        }
    }

    Ok(())
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

    log::info!("opening ELF file {:?}", path);

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
