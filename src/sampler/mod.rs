use std::sync::mpsc;

mod fakesampler;
mod rttsampler;
mod memsampler;

pub use fakesampler::FakeSampler;
pub use rttsampler::RTTSampler;
pub use memsampler::MemSampler;

// TODOs:
// - error handling
// - some samplers might not provide available signals,
//   but allow to ask for arbitrary ones (memory addresses)
// - support different value types?

pub type Sample = (u64, Vec<(u32, f64)>);

#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub enum Status {
    Initializing,
    Sampling,
    Paused,
    Terminated,
}

#[derive(Debug, Clone)]
pub enum Notification {
    NewStatus(Status),
    Info(String),
    Error(String),
}

pub trait Sampler {
    fn available_signals(&self) -> Vec<(u32, String)>;
    fn set_active_signals(&self, ids: &[u32]);

    // TODO: (easy optimization) do not send dynamically allocated Vec<_>
    fn sampled_channel(&self) -> &mpsc::Receiver<Sample>;

    fn notification_channel(&self) -> &mpsc::Receiver<Notification>;

    fn pause(&self);
    fn resume(&self);

    fn stop(self: Box<Self>);
}
