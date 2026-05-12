use crate::utils::{helper, Helper};

mod inline {
    pub enum Mode {
        Fast,
    }
}

pub struct App {
    helper: Helper,
}

impl App {
    pub fn run(&self) {
        helper();
    }
}

pub trait Runner {
    fn run(&self);
}

pub fn build_app() -> App {
    App { helper: Helper }
}
