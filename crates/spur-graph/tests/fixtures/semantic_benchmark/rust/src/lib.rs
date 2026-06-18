mod support;

use crate::support::helper;

pub trait Labeled {
    fn label(&self) -> &'static str;
}

pub trait Runner: Labeled {
    fn run(&self);
}

pub struct Worker(pub u32);

pub struct Config {
    pub id: u32,
}

impl Worker {
    pub fn new(id: u32) -> Self {
        Worker(id)
    }

    pub fn process(&self) {
        helper();
    }
}

impl Labeled for Worker {
    fn label(&self) -> &'static str {
        "worker"
    }
}

impl Runner for Worker {
    fn run(&self) {
        self.process();
    }
}

pub fn run_direct() {
    helper();
}

pub fn run_method(worker: &Worker) {
    worker.process();
}

pub fn run_scoped() {
    Worker::new(1);
}

pub fn run_macro() {
    json!({
        "call": helper(),
        "field": record.name,
    });
}

pub fn normalize(value: i32) -> i32 {
    value
}

pub fn inline_only(value: i32) -> i32 {
    value + 1
}

pub fn run_hof(items: Vec<i32>) -> Vec<i32> {
    items
        .into_iter()
        .map(normalize)
        .map(|value| inline_only(value))
        .collect()
}

pub fn build_worker() -> Worker {
    Worker(7)
}
