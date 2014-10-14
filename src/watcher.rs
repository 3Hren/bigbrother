#![allow(non_camel_case_types, non_uppercase_statics)] // C types

use std::collections::{HashSet};
use std::c_str::CString;
use std::io::{IoError, IoResult};
use std::io::fs::PathExtensions;
use std::mem;
use std::ptr;
use std::raw::Slice;
use std::os;

use libc::{c_void, c_char, c_int, ENOENT};

use sync::{Arc, Mutex};

#[repr(C)]
enum CFStringBuiltInEncodings {
    kCFStringEncodingUnicode = 0x01000000,
    kCFStringEncodingUTF8    = 0x08000100,
}

static kFSEventStreamCreateFlagNoDefer: u32    = 0x00000002;
static kFSEventStreamCreateFlagFileEvents: u32 = 0x00000010;

#[deriving(Show)]
pub enum Event {
    Create(String),
    Remove(String),
    //ModifyMeta,
    Modify(String),
    Rename(String, String),
}

enum Control {
    Update(HashSet<String>),
    Exit,
}

#[repr(C)]
struct FSEventStreamContext {
    version: c_int,
    info: *mut c_void,
    retain: *const c_void,
    release: *const c_void,
    desc: *const c_void,
}

type callback_t = extern "C" fn(
    stream: *const c_void,
    info: *const c_void,
    size: c_int,
    paths: *const *const i8,
    events: *const u32,
    ids: *const u64
);

#[repr(C)]
enum FSEventStreamEventFlags {
    kFSEventStreamEventFlagItemCreated  = 0x00000100,
    kFSEventStreamEventFlagItemRemoved  = 0x00000200,
    kFSEventStreamEventFlagItemRenamed  = 0x00000800,
    kFSEventStreamEventFlagItemModified = 0x00001000,
    kFSEventStreamEventFlagItemIsFile   = 0x00010000,
}

static kFSEventStreamEventIdSinceNow: u64 = 0xFFFFFFFFFFFFFFFF;

fn has_flag(event: u32, expected: FSEventStreamEventFlags) -> bool {
    event & expected as u32 == expected as u32
}

extern "C"
fn callback(_stream: *const c_void,
            info: *const c_void,
            size: c_int,
            paths: *const *const i8,
            events: *const u32,
            ids: *const u64)
{
    let tx: &mut Sender<Event> = unsafe {
        &mut *(info as *mut Sender<Event>)
    };

    let events: &[u32] = unsafe {
        mem::transmute(Slice {
            data: events,
            len: size as uint,
        })
    };

    let ids: &[u64] = unsafe {
        mem::transmute(Slice {
            data: ids,
            len: size as uint,
        })
    };

    let paths: &[*const i8] = unsafe {
        mem::transmute(Slice {
            data: paths,
            len: size as uint,
        })
    };

    let paths = Vec::from_fn(size as uint, |id| {
        unsafe { CString::new(paths[id], false) }
    });

    let mut renamed = false;
    let mut oldname = String::new();
    for id in range(0, size as uint) {
        debug!("Received filesystem event: [id: {}, ev: {}] from '{}'", ids[id], events[id], paths[id]);
        let event = events[id];
        let path = String::from_str(paths[id].as_str().unwrap());

        if event & kFSEventStreamEventFlagItemIsFile as u32 == 0 {
            continue;
        }

        let path_ = Path::new(path.as_slice());
        if has_flag(event, kFSEventStreamEventFlagItemCreated) && path_.exists() {
            tx.send(Create(path.clone()));
        }

        if has_flag(event, kFSEventStreamEventFlagItemRemoved) && !path_.exists() {
            tx.send(Remove(path.clone()));
        }

        if has_flag(event, kFSEventStreamEventFlagItemRenamed) {
            if renamed {
                tx.send(Rename(oldname.clone(), path));
                oldname.clear();
            } else {
                oldname = path;
            }
            renamed = !renamed;
        }
    }
}

struct CoreFoundationString {
    d: *const c_void,
}

impl CoreFoundationString {
    fn new(string: &str) -> CoreFoundationString {
        CoreFoundationString {
            d: unsafe {
                CFStringCreateWithCString(
                    kCFAllocatorDefault,
                    string.to_c_str().as_ptr(),
                    kCFStringEncodingUTF8
                )
            }
        }
    }
}

impl Drop for CoreFoundationString {
    fn drop(&mut self) {
        unsafe { CFRelease(self.d) }
    }
}

struct CoreFoundationArray {
    d: *const c_void,
    #[allow(dead_code)] items: Vec<CoreFoundationString>, // It's a RAII container.
}

impl CoreFoundationArray {
    fn new(collection: &HashSet<String>) -> CoreFoundationArray {
        let d = unsafe {
            CFArrayCreateMutable(
                kCFAllocatorDefault,
                collection.len() as i32,
                ptr::null::<c_void>()
            )
        };

        let mut items = Vec::new();
        for item in collection.iter() {
            let item = CoreFoundationString::new(item.as_slice());
            unsafe {
                CFArrayAppendValue(d, item.d);
            }
            items.push(item);
        }

        CoreFoundationArray {
            d: d,
            items: items,
        }
    }
}

impl Drop for CoreFoundationArray {
    fn drop(&mut self) {
        unsafe { CFRelease(self.d) }
    }
}

fn recreate_stream(eventloop: *mut c_void, context: *const FSEventStreamContext, paths: HashSet<String>) -> *mut c_void {
    let paths = CoreFoundationArray::new(&paths);

    let latency = 0.05f64;
    let stream = unsafe {
        FSEventStreamCreate(
            kCFAllocatorDefault,
            callback,
            context,
            paths.d,
            kFSEventStreamEventIdSinceNow,
            latency,
            kFSEventStreamCreateFlagNoDefer | kFSEventStreamCreateFlagFileEvents
        )
    };

    unsafe {
        FSEventStreamScheduleWithRunLoop(stream, eventloop, kCFRunLoopDefaultMode);
        FSEventStreamStart(stream);
        stream
    }
}

pub struct Watcher {
    pub rx: Receiver<Event>,
    ctx: SyncSender<Control>,
    paths: HashSet<String>,
    stream: Arc<Mutex<*mut c_void>>,
    eventloop: Arc<Mutex<*mut c_void>>,
}

impl Watcher {
    pub fn new() -> Watcher {
        let (mut tx, rx) = channel::<Event>();
        let (ctx, crx) = sync_channel::<Control>(0);

        let eventloop = Arc::new(Mutex::new(ptr::null_mut::<c_void>()));
        let stream = Arc::new(Mutex::new(ptr::null_mut::<c_void>()));

        let watcher = Watcher {
            rx: rx,
            ctx: ctx,
            paths: HashSet::new(),
            stream: stream.clone(),
            eventloop: eventloop.clone(),
        };

        spawn(proc() {
            debug!("starting watcher thread");
            unsafe {
                *eventloop.lock() = CFRunLoopGetCurrent();

                let tx: *mut c_void = &mut tx as *mut _ as *mut c_void;
                let context = FSEventStreamContext {
                    version: 0,
                    info: tx,
                    retain: ptr::null::<c_void>(),
                    release: ptr::null::<c_void>(),
                    desc: ptr::null::<c_void>(),
                };

                loop {
                    debug!("new watcher loop iteration");
                    match crx.recv() {
                        Update(paths) => {
                            debug!("updating watcher loop with {}", paths);
                            *stream.lock() = recreate_stream(*eventloop.lock(), &context, paths);
                            CFRunLoopRun();
                        }
                        Exit => {
                            debug!("graceful shutdown");
                            break
                        }
                    }
                }
            }
        });

        watcher
    }

    pub fn watch(&mut self, path: &Path) -> IoResult<()> {
        if path.exists() {
            debug!("adding {} to watch", path.display());
            let path = os::make_absolute(path);
            let path = match path.as_str() {
                Some(path) => String::from_str(path),
                None => return Err(IoError::from_errno(ENOENT as uint, false))
            };
            self.paths.insert(path.clone());
            self.update();
            Ok(())
        } else {
            Err(IoError::from_errno(ENOENT as uint, false))
        }
    }

    pub fn unwatch(&mut self, path: &String) -> IoResult<()> {
        self.paths.remove(path);
        self.update();
        Ok(())
    }

    fn update(&self) {
        self.stop_stream();
        self.ctx.send(Update(self.paths.clone()));
    }

    fn stop_stream(&self) {
        let mut stream = self.stream.lock();
        if !(*stream).is_null() {
            unsafe {
                FSEventStreamStop(*stream);
                FSEventStreamInvalidate(*stream);
                FSEventStreamRelease(*stream);
                CFRunLoopWakeUp(*self.eventloop.lock());
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        debug!("dropping! {:p}", self);
        self.stop_stream();
        self.ctx.send(Exit);
    }
}

#[link(name = "Carbon", kind = "framework")]
#[link(name = "CoreFoundation", kind = "framework")]
extern {
    static kCFAllocatorDefault: *mut c_void;
    static kCFRunLoopDefaultMode: *mut c_void;

    fn CFStringCreateWithCString(allocator: *mut c_void, string: *const c_char, encoding: CFStringBuiltInEncodings) -> *const c_void;

    fn CFArrayCreateMutable(allocator: *mut c_void, size: c_int, callbacks: *const c_void) -> *const c_void;
    fn CFArrayAppendValue(array: *const c_void, value: *const c_void);

    fn FSEventStreamCreate(allocator: *mut c_void, cb: callback_t, context: *const FSEventStreamContext, paths: *const c_void, since: u64, latency: f64, flags: u32) -> *mut c_void;

    fn FSEventStreamScheduleWithRunLoop(stream: *mut c_void, eventloop: *mut c_void, mode: *mut c_void);
    fn FSEventStreamStart(stream: *mut c_void);
    fn FSEventStreamStop(stream: *mut c_void);
    fn FSEventStreamInvalidate(stream: *mut c_void);
    fn FSEventStreamRelease(stream: *mut c_void);

    fn CFRunLoopGetCurrent() -> *mut c_void;
    fn CFRunLoopRun();
    fn CFRunLoopWakeUp(ev: *mut c_void);

    fn CFRelease(p: *const c_void);
}

#[cfg(test)]
mod test {
    extern crate test;

    use std::io::{File, TempDir};
    use std::io::fs;
    use std::io::fs::PathExtensions;

    use super::Watcher;
    use super::{Create, Remove, Rename};

    #[test]
    fn create_file() {
        let tempdir = TempDir::new("").unwrap();
        let mut path = tempdir.path().clone();
        path.push(Path::new("file.log"));

        let mut watcher = Watcher::new();
        watcher.watch(tempdir.path()).unwrap();

        File::create(&path).unwrap();

        match watcher.rx.recv() {
            Create(p) => { assert_eq!(b"file.log", Path::new(p.as_slice()).filename().unwrap()) }
            event @ _ => { fail!("expected Create event, actual: {}", event) }
        }
    }

    #[test]
    fn remove_file() {
        let tempdir = TempDir::new("").unwrap();
        let mut filepath = tempdir.path().clone();

        filepath.push(Path::new("file.log"));

        assert!(!filepath.exists());
        File::create(&filepath).unwrap();

        let mut watcher = Watcher::new();
        watcher.watch(tempdir.path()).unwrap();

        fs::unlink(&filepath).unwrap();

        match watcher.rx.recv() {
            Remove(p) => { assert_eq!(b"file.log", Path::new(p.as_slice()).filename().unwrap()) }
            event @ _  => { fail!("expected Remove event, actual: {}", event) }
        }
    }

    #[test]
    fn rename_file() {
        let tempdir = TempDir::new("").unwrap();
        let mut oldfilepath = tempdir.path().clone();
        oldfilepath.push(Path::new("file-old.log"));

        File::create(&oldfilepath).unwrap();

        let mut watcher = Watcher::new();
        watcher.watch(tempdir.path()).unwrap();

        let mut newfilepath = tempdir.path().clone();
        newfilepath.push(Path::new("file-new.log"));

        fs::rename(&oldfilepath, &newfilepath).unwrap();

        match watcher.rx.recv() {
            Rename(old, new) => {
                assert_eq!(b"file-old.log", Path::new(old.as_slice()).filename().unwrap());
                assert_eq!(b"file-new.log", Path::new(new.as_slice()).filename().unwrap());
            }
            event @ _ => { fail!("expected Rename event, actual: {}", event) }
        }
    }
}
