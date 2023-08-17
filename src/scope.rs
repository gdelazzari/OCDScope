use std::sync::{Arc, Mutex};

use libui::controls::{Area, AreaDrawParams, AreaHandler};
use libui::draw::{Brush, FillMode, Path, SolidBrush, StrokeParams};

pub enum Mode {
    Scroll { rate: f64 },
}

pub struct State {
    mode: Mode,
    values: Vec<f64>,
    zoom: Option<(f64, f64, f64, f64)>,
    y_scale: f64,
    y_offset: f64,
}

impl State {
    pub fn new() -> State {
        State {
            mode: Mode::Scroll { rate: 1.0 },
            values: vec![0.0; 1024],
            zoom: None,
            y_scale: 0.0,
            y_offset: 0.0,
        }
    }

    pub fn add_values(&mut self, to_add: &[f64]) {
        let target_len = self.values.len();

        if to_add.len() >= target_len {
            self.values = to_add[to_add.len() - target_len..].to_vec();
            debug_assert_eq!(self.values.len(), target_len);
        } else {
            let keep_len = target_len - to_add.len();

            self.values = self.values[target_len - keep_len..].to_vec();
            debug_assert_eq!(self.values.len(), keep_len);

            self.values.extend_from_slice(to_add);
            debug_assert_eq!(self.values.len(), target_len);
        }
    }
}

struct Renderer {
    state: Arc<Mutex<State>>,
}

impl AreaHandler for Renderer {
    fn draw(&mut self, area: &Area, draw_params: &AreaDrawParams) {
        let ctx = &draw_params.context;

        let background = Brush::Solid(SolidBrush {
            r: 0.1,
            g: 0.1,
            b: 0.1,
            a: 1.,
        });

        let signal = Brush::Solid(SolidBrush {
            r: 1.0,
            g: 1.0,
            b: 1.0,
            a: 1.0,
        });

        let state = self.state.lock().unwrap();

        let path = Path::new(ctx, FillMode::Winding);
        path.add_rectangle(ctx, 0., 0., draw_params.area_width, draw_params.area_height);
        path.end(ctx);
        draw_params.context.fill(&path, &background);

        let path = Path::new(ctx, FillMode::Winding);
        path.new_figure(ctx, 0.0, draw_params.area_height / 2.0);
        for (i, &value) in state.values.iter().enumerate() {
            let x = i as f64 / state.values.len() as f64 * draw_params.area_width;
            let y = draw_params.area_height / 2.0 + value;
            path.line_to(ctx, x, y);
        }
        path.end(ctx);
        draw_params.context.stroke(
            &path,
            &signal,
            &StrokeParams {
                cap: 0,
                join: 0,
                thickness: 1.0,
                miter_limit: 1.0,
                dashes: vec![],
                dash_phase: 0.0,
            },
        );

        area.queue_redraw_all();
    }
}

pub fn new() -> (Arc<Mutex<State>>, Area) {
    let state = Arc::new(Mutex::new(State::new()));

    let area = Area::new(Box::new(Renderer {
        state: state.clone(),
    }));

    (state, area)
}
