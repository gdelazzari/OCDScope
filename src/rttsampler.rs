use std::{
    net::{SocketAddr, ToSocketAddrs},
    path::PathBuf,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};

use crate::sampler::{Sample, Sampler};

const SAMPLE_BUFFER_SIZE: usize = 1024;

// TODO:
// - let user specify RTT control block name (?)
// - let user specify RTT channel ID or name, if wanted
// - good heuristics for finding RTT channel automatically, not just with "JScope" string,
//   also to remove "SEGGER branding"
// - maximize probe clock

enum ThreadCommand {
    Stop,
}

pub struct RTTSampler {
    join_handle: thread::JoinHandle<()>,
    command_tx: mpsc::Sender<ThreadCommand>,
    sampled_rx: mpsc::Receiver<Sample>,
    available_signals: Vec<(u32, String)>,
}

impl RTTSampler {
    pub fn start<A: ToSocketAddrs>(
        telnet_address: A,
        polling_interval: u32,
    ) -> RTTSampler {
        let (sampled_tx, sampled_rx) = mpsc::sync_channel(SAMPLE_BUFFER_SIZE);
        let (command_tx, command_rx) = mpsc::channel();

        let telnet_address = telnet_address.to_socket_addrs().unwrap().next().unwrap();

        // TODO: handle and report errors of various kind, during initial connection
        // and handshake

        // TODO: set RTT polling rate

        // TODO: handle failure of RTT TCP channel opening, which can happen for various
        // reasons (like address already in use)

        let join_handle = thread::spawn(move || {
            sampler_thread(telnet_address, polling_interval, sampled_tx, command_rx)
        });

        let available_signals = unimplemented!();

        let sampler = RTTSampler {
            join_handle,
            command_tx,
            sampled_rx,
            available_signals,
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

impl Sampler for RTTSampler {
    fn available_signals(&self) -> Vec<(u32, String)> {
        self.available_signals.clone()
    }

    fn set_active_signals(&self, _ids: &[u32]) {
        // do nothing, since we don't decide what signals we receive
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
    polling_interval: u32,
    sampled_tx: mpsc::SyncSender<Sample>,
    command_rx: mpsc::Receiver<ThreadCommand>,
) {
    // TODO: handle and report errors of various kind, during initial connection
    // and handshake

    let polling_period = Duration::from_millis(polling_interval as u64);

    loop {
        // 1. process commands, if any
        match command_rx.try_recv() {
            Ok(ThreadCommand::Stop) => {
                break;
            }
            Err(mpsc::TryRecvError::Empty) => {}
            Err(mpsc::TryRecvError::Disconnected) => panic!("Thread command channel closed TX end"),
        }

        let timestamp = unimplemented!();
        let samples = unimplemented!();
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
