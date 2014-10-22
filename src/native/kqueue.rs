#![allow(non_camel_case_types, non_uppercase_statics)] // C types

use std::collections::{HashSet};
use std::io::{Timer, Open, Read};
use std::time::Duration;
use std::ptr;
use std::io::fs::{File};

use time::Timespec;

use libc::{c_void, c_int, uintptr_t, intptr_t};

#[repr(C)]
enum EventFilter {
    EVFILT_VNODE = -4,
    EVFILT_USER  = -10,
}

bitflags! {
    flags EventFlags: u16 {
        const EV_ADD    = 0x0001,
        const EV_ENABLE = 0x0004,
        const EV_CLEAR  = 0x0020,
    }
}

bitflags! {
    flags EventFilterFlags: u32 {
        const NOTE_DELETE   = 0x00000001,
        const NOTE_WRITE    = 0x00000002,
        const NOTE_EXTEND   = 0x00000004,
        const NOTE_RENAME   = 0x00000020,
    }
}

#[repr(C)]
struct kevent {
    ident: uintptr_t,       /* identifier for this event */
    filter: i16,            /* filter for event */
    flags: u16,             /* general flags */
    fflags: u32,            /* filter-specific flags */
    data: intptr_t,         /* filter-specific data */
    udata: *const c_void,   /* opaque user data identifier */
}

pub enum Event {
    Create(Path),
    Remove(Path),
}

pub struct Watcher {
    pub rx: Receiver<Event>,
}

struct NativePath {
    fd: i32,
}

impl NativePath {
    fn new(path: &str) -> Result<NativePath, i32> {
        let fd = unsafe {
            open(path.to_c_str().as_ptr(), 0x0000)
        };

        if fd < 0 {
            return Err(fd)
        }

        Ok(NativePath { fd: fd })
    }
}

impl Drop for NativePath {
    fn drop(&mut self) {
        unsafe {
            close(self.fd)
        }
    }
}

struct KQueue {
    fd: i32,
}

#[deriving(Show)]
enum KQueueError {
    UnableToCreateKQueue,
}

impl KQueue {
    fn new() -> Result<KQueue, KQueueError> {
        let fd = unsafe { kqueue() };
        if fd < 0 {
            return Err(UnableToCreateKQueue)
        }

        Ok(KQueue { fd: fd })
    }

    fn process(&mut self, input: &[kevent], output: &mut[kevent], timeout: Timespec) -> i32 {
        unsafe {
            kevent(self.fd, input.as_ptr(), input.len() as i32, output.as_ptr(), output.len() as i32, &timeout)
        }
    }
}

impl Drop for KQueue {
    fn drop(&mut self) {
        unsafe {
            close(self.fd);
        }
    }
}

impl kevent {
    fn new(path: &NativePath, filter: EventFilter, flags: EventFlags, fflags: EventFilterFlags, data: intptr_t, udata: *const c_void) -> kevent {
        kevent {
            ident: path.fd as u64,
            filter: filter as i16,
            flags: flags.bits() as u16,
            fflags: fflags.bits() as u32,
            data: data,
            udata: udata,
        }
    }

    fn empty() -> kevent {
        kevent {
            ident: 0,
            filter: 0,
            flags: 0,
            fflags: 0,
            data: 0,
            udata: ptr::null::<c_void>(),
        }
    }
}

impl Watcher {
    pub fn new() -> Watcher {
        let queue = KQueue::new().unwrap();

        let path = "/tmp".to_c_str();
        let fd = unsafe { open(path.as_ptr(), 0x0000) };
        if fd < 0 {
            fail!("unable to open directory");
        }
        debug!("fd: {}", fd);

        let (tx, rx) = channel();

//        spawn(proc(){
//            let ev = kevent {
//                ident: fd as u64,
//                filter: EVFILT_VNODE,
//                flags: EV_ADD | EV_ENABLE | EV_CLEAR,
//                fflags: NOTE_DELETE | NOTE_EXTEND | NOTE_WRITE | NOTE_RENAME,
//                data: 0,
//                udata: ptr::null::<c_void>(),
//            };

//            unsafe {
//                let res = kevent(queue.fd, &ev, 1, ptr::null::<kevent>(), 0, ptr::null::<Timespec>());
//                debug!("RESULT ADD: {}", res);
//            }

//            unsafe {
//                let res = kevent(queue.fd, ptr::null::<kevent>(), 0, &ev, 1, ptr::null::<Timespec>());
//                debug!("RESULT CH: {}", res);
//                if res < 0 {
//                    fail!("unable to poll events");
//                }

//                debug!("filter: {} {} {} {} {}", ev.ident, ev.filter, ev.flags, ev.fflags, ev.data);
//            }

//            unsafe { close(fd); }
//        });

        Watcher {
            rx: rx,
        }
    }

    pub fn watch(&mut self, path: Path) -> Result<(), KQueueError> {
        Ok(())
    }
}

extern {
    fn kqueue() -> c_int;
    fn kevent(kq: c_int, changelist: *const kevent, nchanges: c_int, eventlist: *const kevent, nevents: c_int, timeout: *const Timespec) -> c_int;

    fn open(path: *const i8, flags: c_int) -> c_int;
    fn close(fd: c_int);
}


#[cfg(test)]
mod test {
    extern crate test;

//    use std::str;
//    use std::collections::HashSet;
    use std::io::{File, TempDir};
//    use std::io::fs;
    use std::io::fs::PathExtensions;
    use std::io::timer;
    use std::time::Duration;
    use std::ptr;

    use time::Timespec;

    use libc::{c_void};

    use super::{Watcher, KQueue, NativePath};
    use super::{kevent};
    use super::{EVFILT_VNODE};
    use super::{EV_ADD};
    use super::{NOTE_WRITE, NOTE_RENAME};
    use super::{
        Create,
//        Modify,
//        Rename,
//        Remove,
    };

    #[test]
    fn kqueue_create_single_file() {
        let tmp = TempDir::new("kqueue-create-single").unwrap();
        let path = tmp.path().join("file.log");
        let native = NativePath::new(tmp.path().as_str().unwrap()).unwrap();

        let mut queue = KQueue::new().unwrap();

        let ievents = [
            kevent::new(&native, EVFILT_VNODE, EV_ADD, NOTE_WRITE, 0, ptr::null::<c_void>())
        ];
        let mut oevents: [kevent, ..0] = [];
        let timeout = Timespec::new(0, 0);

        let n = queue.process(&ievents, &mut oevents, timeout);
        assert_eq!(0, n);

//        let events = kq.event(); // recv
        // check
    }
}
