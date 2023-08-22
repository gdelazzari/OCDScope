use std::{sync::mpsc, thread, time::Duration};

use crate::sampler::Sampler;

const SAMPLE_BUFFER_SIZE: usize = 1024;

enum ThreadCommand {
    Stop,
}

pub struct FakeSampler {
    join_handle: thread::JoinHandle<()>,
    command_tx: mpsc::Sender<ThreadCommand>,
    sampled_rx: mpsc::Receiver<(u64, f64)>,
}

impl FakeSampler {
    pub fn start(rate: f64) -> FakeSampler {
        let (sampled_tx, sampled_rx) = mpsc::sync_channel(SAMPLE_BUFFER_SIZE);
        let (command_tx, command_rx) = mpsc::channel();

        let join_handle = thread::spawn(move || sampler_thread(rate, sampled_tx, command_rx));

        let sampler = FakeSampler {
            join_handle,
            command_tx,
            sampled_rx,
        };

        sampler
    }
}

impl Sampler for FakeSampler {
    fn sampled_channel(&self) -> &mpsc::Receiver<(u64, f64)> {
        &self.sampled_rx
    }

    fn stop(self: Box<Self>) {
        self.command_tx.send(ThreadCommand::Stop).unwrap();
        self.join_handle.join().unwrap();
    }
}

fn sampler_thread(
    rate: f64,
    sampled_tx: mpsc::SyncSender<(u64, f64)>,
    command_rx: mpsc::Receiver<ThreadCommand>,
) {
    use std::time::Instant;

    let period = Duration::from_secs_f64(1.0 / rate);
    let mut t = 10.0;
    let omega = 1.0;

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
        t += period.as_secs_f64();

        let y = (t * omega).sin();

        sampled_tx
            .send(((t * 1e6) as u64, y))
            .expect("Failed to send sampled value");
    }
}
