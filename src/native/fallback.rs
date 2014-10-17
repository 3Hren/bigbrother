use std::collections::{HashSet};
use std::io::{Timer};
use std::time::Duration;

use super::{Event, Unknown};

pub struct Backend {
    ctx: Sender<()>,
}

impl Backend {
    pub fn new(period: Duration) -> (Backend, Receiver<Event>) {
        let (tx, rx) = sync_channel(0);
        let (ctx, crx) = channel();

        spawn(proc(){
            debug!("Starting fallback timer backend ...");

            let mut timer = Timer::new().unwrap();
            let trx = timer.periodic(period);

            loop {
                select! {
                    () = trx.recv() => { tx.send(Unknown) },
                    () = crx.recv() => { break }
                }
            }

            debug!("Fallback timer backend has been gracefully shutted down ");
        });

        (Backend { ctx: ctx }, rx)
    }

    pub fn register(&mut self, _paths: HashSet<Path>) {}
}

impl Drop for Backend {
    fn drop(&mut self) {
        self.ctx.send(());
    }
}
