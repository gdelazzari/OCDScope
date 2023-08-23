use std::{
    net::{SocketAddr, ToSocketAddrs},
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::gdbremote::{self, GDBRemote};
use crate::sampler::{Sample, Sampler};

const SAMPLE_BUFFER_SIZE: usize = 1024;

enum ThreadCommand {
    SetActiveAddresses(Vec<u32>),
    Stop,
}

pub struct MemSampler {
    join_handle: thread::JoinHandle<()>,
    command_tx: mpsc::Sender<ThreadCommand>,
    sampled_rx: mpsc::Receiver<Sample>,
}

impl MemSampler {
    pub fn start<A: ToSocketAddrs>(address: A, rate: f64) -> MemSampler {
        let (sampled_tx, sampled_rx) = mpsc::sync_channel(SAMPLE_BUFFER_SIZE);
        let (command_tx, command_rx) = mpsc::channel();

        let address = address.to_socket_addrs().unwrap().next().unwrap();

        let join_handle =
            thread::spawn(move || sampler_thread(address, rate, sampled_tx, command_rx));

        let sampler = MemSampler {
            join_handle,
            command_tx,
            sampled_rx,
        };

        sampler
    }

    fn clear_rx_channel(&self) {
        loop {
            match self.sampled_rx.try_recv() {
                Ok(_) => {}
                Err(mpsc::TryRecvError::Empty) => break,
                Err(mpsc::TryRecvError::Disconnected) => {
                    panic!("RX channel disconnected while clearing")
                }
            }
        }
    }
}

impl Sampler for MemSampler {
    fn available_signals(&self) -> Vec<(u32, String)> {
        // TODO: figure out how to implement this; do we make MemSampler parse the ELF file
        // and return its valid symbols? Or do we just return nothing, maybe unreachable!()
        // here, and let the GUI handle the special case?
        vec![(0x2000001c, "Test signal".into())]
    }

    fn set_active_signals(&self, ids: &[u32]) {
        self.command_tx
            .send(ThreadCommand::SetActiveAddresses(ids.to_vec()))
            .unwrap();

        self.clear_rx_channel();
    }

    fn sampled_channel(&self) -> &mpsc::Receiver<Sample> {
        &self.sampled_rx
    }

    fn stop(self: Box<Self>) {
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
    use std::time::Instant;

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
    'outer: loop {
        // 1. process commands, if any
        match command_rx.try_recv() {
            Ok(ThreadCommand::Stop) => {
                break;
            }
            Ok(ThreadCommand::SetActiveAddresses(memory_addresses)) => {
                // TODO: validate before setting, if we can even do that?
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
            gdb.send_packet(&format!("m {:08x},4", memory_address));

            // TODO: timeout
            let response = gdb.read_response();

            if DEBUG_PRINT {
                println!("{:?} : {:?}", response, response.to_string());
            }

            match &response {
                gdbremote::Response::Packet(hex_string) => {
                    match u32::from_str_radix(
                        &String::from_utf8(hex_string.to_vec())
                            .expect("Failed to parse response payload as string"),
                        16,
                    ) {
                        Ok(integer) => samples.push((
                            memory_address,
                            f32::from_le_bytes(integer.to_be_bytes()) as f64,
                        )),
                        Err(err) => {
                            // TODO/FIXME: something weird is going on here, when we change the
                            //             active signals; investigate this, and change the error
                            //             below so that in debug we fail and in release we log a warning
                            println!(
                                "Failed to parse response {:?} as hex value ({:?})",
                                hex_string, err
                            );
                            continue 'outer;
                        }
                    }
                }
                _ => panic!("Unexpected response to read request"),
            }
        }

        // TODO: conversion from u128 to u64 could fail
        let timestamp = (sampled_at - start).as_micros() as u64;
        sampled_tx
            .send((timestamp, samples))
            .expect("Failed to send sampled value");
    }
}
