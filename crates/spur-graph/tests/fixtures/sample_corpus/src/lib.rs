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

pub fn run_dyn(runner: &dyn Runner) {
    runner.run();
}

pub fn normalize(value: i32) -> i32 {
    value
}

pub fn apply(items: Option<i32>) -> Option<i32> {
    items.map(normalize)
}

pub fn build_app() -> App {
    App { helper: Helper }
}
