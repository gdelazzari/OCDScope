use std::{
    net::{SocketAddr, ToSocketAddrs},
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::gdbremote::{self, GDBRemote};
use crate::sampler::Sampler;

const SAMPLE_BUFFER_SIZE: usize = 1024;

enum ThreadCommand {
    Stop,
}

pub struct MemSampler {
    join_handle: thread::JoinHandle<()>,
    command_tx: mpsc::Sender<ThreadCommand>,
    sampled_rx: mpsc::Receiver<(f64, f64)>,
}

impl MemSampler {
    pub fn start<A: ToSocketAddrs>(address: A, memory_address: u32, rate: f64) -> MemSampler {
        let (sampled_tx, sampled_rx) = mpsc::sync_channel(SAMPLE_BUFFER_SIZE);
        let (command_tx, command_rx) = mpsc::channel();

        let address = address.to_socket_addrs().unwrap().next().unwrap();

        let join_handle = thread::spawn(move || {
            sampler_thread(address, memory_address, rate, sampled_tx, command_rx)
        });

        let sampler = MemSampler {
            join_handle,
            command_tx,
            sampled_rx,
        };

        sampler
    }
}

impl Sampler for MemSampler {
    fn sampled_channel(&self) -> &mpsc::Receiver<(f64, f64)> {
        &self.sampled_rx
    }

    fn stop(self: Box<Self>) {
        self.command_tx.send(ThreadCommand::Stop).unwrap();
        self.join_handle.join().unwrap();
    }
}

fn sampler_thread(
    address: SocketAddr,
    memory_address: u32,
    rate: f64,
    sampled_tx: mpsc::SyncSender<(f64, f64)>,
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

    let period = Duration::from_secs_f64(1.0 / rate);

    let mut last_sampled_at = Instant::now();
    loop {
        // 1. process commands, if any
        match command_rx.try_recv() {
            Ok(ThreadCommand::Stop) => {
                break;
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

        // 3. sample
        gdb.send_packet(&format!("m {:08x},4", memory_address));
        let response = gdb.read_response(); // TODO: timeout

        match &response {
            gdbremote::Response::Packet(hex_string) => {
                let integer = u32::from_str_radix(
                    &String::from_utf8(hex_string.to_vec())
                        .expect("Failed to parse response payload as string"),
                    16,
                )
                .expect("Failed to parse response as hex value");

                let timestamp = 0.0;
                let float = f32::from_le_bytes(integer.to_be_bytes()) * 40.0;

                sampled_tx
                    .send((timestamp, float as f64))
                    .expect("Failed to send sampled value");
            }
            _ => panic!("Unexpected response to read request"),
        }

        if DEBUG_PRINT {
            println!("{:?} : {:?}", response, response.to_string());
        }
    }
}
