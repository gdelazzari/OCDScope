pub struct ParsableFloat {
    value: f64,
    string: String,
    last_parse_ok: bool,
}

impl ParsableFloat {
    pub fn new(value: f64) -> ParsableFloat {
        ParsableFloat {
            value,
            string: value.to_string(),
            last_parse_ok: true,
        }
    }

    pub fn editable_string(&mut self) -> &mut String {
        &mut self.string
    }

    pub fn update(&mut self) {
        if let Ok(value) = self.string.parse::<f64>() {
            self.value = value;
            self.last_parse_ok = true;
        } else {
            self.last_parse_ok = false;
        }
    }

    pub fn value(&self) -> f64 {
        self.value
    }

    pub fn is_parsed_ok(&self) -> bool {
        self.last_parse_ok
    }
}

impl From<f64> for ParsableFloat {
    fn from(value: f64) -> Self {
        ParsableFloat::new(value)
    }
}

impl From<ParsableFloat> for f64 {
    fn from(pf: ParsableFloat) -> Self {
        pf.value()
    }
}
