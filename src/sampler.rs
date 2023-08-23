use std::sync::mpsc;

// TODOs:
// - error handling
// - some samplers might not provide available signals,
//   but allow to ask for arbitrary ones (memory addresses)
// - support different value types?

pub type Sample = (u64, Vec<(u32, f64)>);

pub trait Sampler {
    fn available_signals(&self) -> Vec<(u32, String)>;
    fn set_active_signals(&self, ids: &[u32]);

    // TODO: (easy optimization) do not send dynamically allocated Vec<_>
    fn sampled_channel(&self) -> &mpsc::Receiver<Sample>;

    fn stop(self: Box<Self>);
}
