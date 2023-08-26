// Notes:
// I think we can explore two ways for achieving good plotting performance:
// - either caching lookup positions and having heuristics for speeding up successive
//   requests to close x values; need to reason about interpolation in this case
// - provide the x range to be plotted and return a copy of the slice of samples;
//   might need to implement undersampling for the case of very large time slices,
//   which might be cacheable
//
// TODOs:

use eframe::egui::plot::{PlotPoint, PlotPoints};

pub struct SampleBuffer {
    samples: Vec<PlotPoint>,
}

impl SampleBuffer {
    pub fn new() -> SampleBuffer {
        SampleBuffer {
            samples: Vec::new(),
        }
    }

    pub fn push(&mut self, t: f64, value: f64) {
        self.samples.push(PlotPoint { x: t, y: value });
    }

    pub fn plot_points(&self) -> PlotPoints {
        PlotPoints::Owned(self.samples.clone())
    }

    pub fn memory_footprint(&self) -> (usize, usize) {
        let sample_size = std::mem::size_of::<PlotPoint>();

        let used = self.samples.len() * sample_size;
        let capacity = self.samples.capacity() * sample_size;

        (used, capacity)
    }
}
