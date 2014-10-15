#![feature(phase)]
#![feature(if_let)]

extern crate libc;
extern crate sync;

#[phase(plugin, link)] extern crate log;

pub use self::watcher::Watcher;

pub mod watcher;
