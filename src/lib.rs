#![feature(phase)]
#![feature(if_let)]
#![feature(slicing_syntax)]

extern crate libc;
extern crate sync;
extern crate time;

#[phase(plugin, link)] extern crate log;

//pub use self::watcher::Watcher;

//pub mod watcher;

pub use self::kqueue::Watcher;
pub mod kqueue; // TODO: temporary pub to prevent unused warnings.
