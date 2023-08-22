use std::sync::mpsc;

// TODOs:
// - error handling
// - some samplers might not provide available signals,
//   but allow to ask for arbitrary ones (memory addresses)
// - we might allow for samplers to provide samples only for a subset
//   of the active signals

pub trait Sampler {
    fn available_signals(&self) -> Vec<(u32, String)>;
    fn set_active_signals(&self, ids: &[u32]);

    // TODO: (easy optimization) do not send dynamically allocated Vec<_>
    fn sampled_channel(&self) -> &mpsc::Receiver<(u64, Vec<f64>)>;

    fn stop(self: Box<Self>);
}
