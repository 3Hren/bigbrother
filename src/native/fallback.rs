use std::collections::{HashSet};
use std::io::{Timer};
use std::time::Duration;

use super::{Event, Unknown};

pub struct Backend {
    _timer: Timer,
}

impl Backend {
    pub fn new(period: Duration) -> (Backend, Receiver<()>) {
        let mut timer = Timer::new().unwrap();
        let rx = timer.periodic(period);

        (Backend { _timer: timer }, rx)
    }

    pub fn register(&mut self, _paths: HashSet<Path>) {}

    pub fn transform(&self, _ev: ()) -> Event {
        Unknown
    }
}
