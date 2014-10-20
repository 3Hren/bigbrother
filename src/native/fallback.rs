use std::collections::{HashSet};
use std::io::{Timer};
use std::time::Duration;

use super::{Event, Unknown};

pub struct Watcher {
    _timer: Timer,
    pub rx: Receiver<()>,
}

pub struct Backend {
    pub watcher: Watcher,
}

impl Backend {
    pub fn new(period: Duration) -> Backend {
        let mut timer = Timer::new().unwrap();
        let rx = timer.periodic(period);

        Backend {
            watcher: Watcher {
                _timer: timer,
                rx: rx,
            },
        }
    }

    pub fn register(&mut self, _paths: HashSet<Path>) {}

    pub fn transform(&self, _ev: ()) -> Event {
        Unknown
    }
}
