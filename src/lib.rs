#![feature(phase)]
#![feature(if_let)]

extern crate libc;
extern crate sync;
extern crate time;

#[phase(plugin, link)] extern crate log;

//pub use self::watcher::Watcher;

//pub mod watcher;
mod native;
