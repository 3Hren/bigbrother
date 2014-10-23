#![allow(non_camel_case_types, non_uppercase_statics)] // C types

use std::collections::HashMap;
use std::io::fs::PathExtensions;
use std::ptr;

use sync::comm::{Empty, Disconnected};

use libc::{c_void, c_int, uintptr_t, intptr_t};

use time::Timespec;

pub enum Event {
    Create(Path),
//    Remove(Path),
    Modify(Path),
}

#[deriving(Show)]
pub enum KQueueError {
    UnableToCreateKQueue,
}

enum Control {
    Add(Path),
    Exit,
}

// Можно регистрировать.
pub struct Watcher {
    pub rx: Receiver<Event>,
    txc: Sender<Control>,
}

impl Watcher {
    pub fn new() -> Watcher {
        let queue = match KQueue::new() {
            Ok(queue) => queue,
            Err(err)  => fail!(err)
        };

        let (tx, rx) = channel();
        let (txc, rxc) = channel();

        spawn(proc() Watcher::run(queue, tx, rxc));

        Watcher {
            rx: rx,
            txc: txc,
        }
    }

    pub fn watch(&mut self, path: Path) {
        debug!("Adding {} to the watcher", path.display());

        // Путь существует?
        //  + > Записать в очередь событие.
        //      Разбудить kqueue.
        //  - > Вернуть EBADF (PathNotExists).
        self.txc.send(Add(path));
    }

    fn run(mut queue: KQueue, tx: Sender<Event>, rxc: Receiver<Control>) {
        debug!("Starting watcher thread ...");

        let timeout = Timespec::new(0, 100e6f32 as i32);

        loop {
            debug!("Performing next watcher loop iteration ...");

            let mut paths = HashMap::new();
            match rxc.try_recv() {
                Ok(value) => {
                    match value {
                        Add(path) => {
                            // Path - файл, то тупо добавить его. Если каталог - добавить все файлы в каталоге. Симлинк - следовать.
                            let path = match path.as_str() {
                                Some(v) => v,
                                None    => {
                                    fail!("Failed to convert {} path to string", path.display());
                                }
                            };

                            let handler = FileHandler::new(path).unwrap(); // TODO: Unsafe.
                            let fd = handler.fd;

                            paths.insert(fd, handler);
                            let input = [
                                kevent::new(fd as u64, EVFILT_VNODE, EV_ADD, NOTE_WRITE, 0, ptr::null::<c_void>())
                            ];
                            let mut output: [kevent, ..0] = [];
                            let n = queue.process(&input, &mut output, &None);
                            debug!("add: {} -> {}", fd, n);
                        }
                        Exit => { break }
                    }
                }
                Err(Empty) => {}
                Err(Disconnected) => { break }
            }

            let input = [];
            let mut output: [kevent, ..1] = [kevent::invalid()];

            let n = queue.process(&input, &mut output, &Some(timeout));
            debug!("process: {}", n);
            for i in range(0, n as uint) {
                let ev = output[i];
                debug!(" - p: {} {} {} {}", ev.ident, ev.filter, ev.flags, ev.fflags);
            }
        }

        debug!("Watcher thread has been stopped");
    }

//    fn add(mut queue: KQueue, paths: &mut HashMap<i32, FileHandler>, path: Path, tx: Sender<Event>) {
//    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        debug!("Dropping the watcher");

        self.txc.send(Exit);
    }
}

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
        const NOTE_TRIGGER  = 0x01000000,
    }
}

#[repr(C)]
struct kevent {
    ident: uintptr_t,       // Identifier for this event.
    filter: i16,            // Filter for event.
    flags: u16,             // General flags.
    fflags: u32,            // Filter-specific flags.
    data: intptr_t,         // Filter-specific data.
    udata: *const c_void,   // Opaque user data identifier.
}

impl kevent {
    fn new(ident: u64, filter: EventFilter, flags: EventFlags, fflags: EventFilterFlags, data: intptr_t, udata: *const c_void) -> kevent {
        kevent {
            ident: ident,
            filter: filter as i16,
            flags: flags.bits() as u16,
            fflags: fflags.bits() as u32,
            data: data,
            udata: udata,
        }
    }

    fn invalid() -> kevent {
        kevent::new(0, EVFILT_USER, EventFlags::empty(), EventFilterFlags::empty(), 0, ptr::null::<c_void>())
    }
}

struct FileHandler {
    fd: i32,
}

impl FileHandler {
    fn new(path: &str) -> Result<FileHandler, i32> {
        let fd = unsafe {
            open(path.to_c_str().as_ptr(), 0x0000)
        };

        if fd < 0 {
            return Err(fd)
        }

        Ok(FileHandler { fd: fd })
    }
}

impl Drop for FileHandler {
    fn drop(&mut self) {
        unsafe {
            close(self.fd)
        }
    }
}

struct KQueue {
    fd: i32,
}

impl KQueue {
    fn new() -> Result<KQueue, KQueueError> {
        let fd = unsafe { kqueue() };
        if fd < 0 {
            return Err(UnableToCreateKQueue)
        }

        Ok(KQueue { fd: fd })
    }

    fn process(&mut self, input: &[kevent], output: &mut[kevent], timeout: &Option<Timespec>) -> i32 {
        unsafe {
            match *timeout {
                Some(ref v) => kevent(self.fd, input.as_ptr(), input.len() as i32, output.as_ptr(), output.len() as i32, v),
                None        => kevent(self.fd, input.as_ptr(), input.len() as i32, output.as_ptr(), output.len() as i32, ptr::null::<Timespec>()),
            }
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

extern {
    fn kqueue() -> c_int;
    fn kevent(kq: c_int, changelist: *const kevent, nchanges: c_int, eventlist: *const kevent, nevents: c_int, timeout: *const Timespec) -> c_int;

    fn open(path: *const i8, flags: c_int) -> c_int;
    fn close(fd: c_int);
}


#[cfg(test)]
mod test {
    extern crate test;

    use std::io::{File, TempDir};
    use std::io::timer;
    use std::time::Duration;
    use std::ptr;

    use time::Timespec;

    use libc::{c_void};

    use super::{KQueue, kevent, FileHandler};
    use super::{EVFILT_VNODE};
    use super::{EV_ADD};
    use super::{NOTE_WRITE};
    use super::{Watcher,
//        Create,
        Modify,
//        Rename,
//        Remove,
    };

    #[test]
    fn kqueue_create_single_file() {
        use std::os;
        let tmp = TempDir::new("kqueue-create-single").unwrap();
        let path = tmp.path().join("file.log");
        let ntmp = FileHandler::new(tmp.path().as_str().unwrap()).unwrap();

        let mut queue = KQueue::new().unwrap();

        let ievents = [
            kevent::new(ntmp.fd as u64, EVFILT_VNODE, EV_ADD, NOTE_WRITE, 0, ptr::null::<c_void>())
        ];
        let mut oevents: [kevent, ..0] = [];

        let n = queue.process(&ievents, &mut oevents, &None);
        error!("{} {}", n, os::error_string(os::errno() as uint));

        assert_eq!(0, n);

        timer::sleep(Duration::milliseconds(50));
        File::create(&path).unwrap();

        let ievents = [];
        let mut oevents: [kevent, ..1] = [kevent::invalid()];

        let n = queue.process(&ievents, &mut oevents, &None);

        assert_eq!(1, n);
        let actual = oevents[0];
        assert_eq!(ntmp.fd as u64, actual.ident);
        assert_eq!(EVFILT_VNODE as i16, actual.filter);
        assert_eq!(EV_ADD.bits(), actual.flags);
        assert_eq!(NOTE_WRITE.bits(), actual.fflags);
    }

//    #[test] watch file modified.
//    #[test] watch file attributes modified.
//    #[test] watch file removed.
//    #[test] watch file renamed.
//    #[test] watch file when removed directory.
//    #[test] watch file when renamed directory.

    #[test]
    fn watch_file_modify_file() {
        let tmp = TempDir::new("watch_file_modify_file").unwrap();
        let path = tmp.path().join("file.log");
        let mut file = File::create(&path).unwrap();

        let mut watcher = Watcher::new();
        watcher.watch(path.clone());

        timer::sleep(Duration::milliseconds(50));

        file.write(b"some bytes!\n").unwrap();
        file.flush().unwrap();
        file.fsync().unwrap();

        match watcher.rx.recv() {
            Modify(p) => {
                assert_eq!(b"file.log", p.filename().unwrap())
            }
            _ => { fail!("Expected `Modify` event") }
        }
    }
//    #[test]
//    fn create_single_file() {
//        let tmp = TempDir::new("create-single").unwrap();
//        let path = tmp.path().join("file.log");

//        let mut watcher = Watcher::new();
//        watcher.watch(tmp.path().clone());

//        timer::sleep(Duration::milliseconds(50));

//        File::create(&path).unwrap();

//        match watcher.rx.recv() {
//            Create(p) => {
//                assert_eq!(b"file.log", p.filename().unwrap())
//            }
//            _ => { fail!("Expected `Create` event") }
//        }
//    }

//    #[test]
//    fn remove_single_file() {
//        let tmp = TempDir::new("remove-single").unwrap();
//        let path = tmp.path().join("file.log");

//        assert!(!path.exists());
//        File::create(&path).unwrap();

//        timer::sleep(Duration::milliseconds(50));

//        let mut watcher = Watcher::new();
//        watcher.watch(tmp.path().clone());

//        timer::sleep(Duration::milliseconds(50));

//        fs::unlink(&path).unwrap();

//        match watcher.rx.recv() {
//            Remove(p) => {
//                assert_eq!(b"file.log", p.filename().unwrap())
//            }
//            _  => { fail!("Expected `Remove` event") }
//        }
//    }

//    #[test]
//    fn rename_single_file() {
//        let tmp = TempDir::new("rename-single").unwrap();
//        let oldpath = tmp.path().join("file-old.log");
//        let newpath = tmp.path().join("file-new.log");

//        File::create(&oldpath).unwrap();

//        timer::sleep(Duration::milliseconds(50));

//        let mut watcher = Watcher::new();
//        watcher.watch(tmp.path().clone());

//        timer::sleep(Duration::milliseconds(50));

//        fs::rename(&oldpath, &newpath).unwrap();

//        match watcher.rx.recv() {
//            Rename(old, new) => {
//                assert_eq!(b"file-old.log", old.filename().unwrap());
//                assert_eq!(b"file-new.log", new.filename().unwrap());
//            }
//            _ => { fail!("Expected `Rename` event") }
//        }
//    }

//    #[test]
//    fn modify_single_file() {
//        let tmp = TempDir::new("modify-single").unwrap();
//        let path = tmp.path().join("file.log");

//        let mut file = File::create(&path).unwrap();

//        timer::sleep(Duration::milliseconds(50));

//        let mut watcher = Watcher::new();
//        watcher.watch(tmp.path().clone());

//        // Timeout is need to be at least one second, because at least ext3 filesystem has seconds resolution.
//        timer::sleep(Duration::milliseconds(1000));
//        debug!("Modifying file ...");

//        file.write(b"some bytes!\n").unwrap();
//        file.flush().unwrap();
//        file.fsync().unwrap();

//        match watcher.rx.recv() {
//            Modify(p) => {
//                assert_eq!(b"file.log", p.filename().unwrap());
//            }
//            _ => { debug!("Expected `Modify` event") }
//        }
//    }

//    #[test]
//    fn create_two_files_in_different_directories() {
//        let tmp1 = TempDir::new("create-1").unwrap();
//        let tmp2 = TempDir::new("create-2").unwrap();
//        let path1 = tmp1.path().join("file1.log");
//        let path2 = tmp2.path().join("file2.log");

//        let mut watcher = Watcher::new();
//        watcher.watch(tmp1.path().clone());
//        watcher.watch(tmp2.path().clone());

//        timer::sleep(Duration::milliseconds(50));

//        File::create(&path1).unwrap();
//        File::create(&path2).unwrap();

//        let mut matches = HashSet::new();
//        matches.insert(String::from_str("file1.log"));
//        matches.insert(String::from_str("file2.log"));

//        let mut counter = 2u8;
//        while counter > 0 {
//            match watcher.rx.recv() {
//                Create(p) => {
//                    assert!(matches.remove(&String::from_str(str::from_utf8(p.filename().unwrap()).unwrap())));
//                }
//                _ => { fail!("Expected `Create` event") }
//            }
//            counter -= 1;
//        }
//        assert!(matches.is_empty());
//    }

//    #[test]
//    fn rename_file_from_nonwatched_directory_to_watched() {
//        // Event should be considered as file creation in watched directory.
//        let tmp1 = TempDir::new("rename-nowatched").unwrap();
//        let tmp2 = TempDir::new("rename-towatched").unwrap();
//        let oldpath = tmp1.path().join("file-old.log");
//        let newpath = tmp2.path().join("file-new.log");

//        File::create(&oldpath).unwrap();

//        timer::sleep(Duration::milliseconds(50));

//        let mut watcher = Watcher::new();
//        watcher.watch(tmp2.path().clone());

//        timer::sleep(Duration::milliseconds(50));

//        fs::rename(&oldpath, &newpath).unwrap();

//        match watcher.rx.recv() {
//            Create(p) => {
//                assert_eq!(b"file-new.log", p.filename().unwrap());
//            }
//            _ => { fail!("Expected `Create` event") }
//        }
//    }

//    #[test]
//    fn rename_file_from_watched_directory_to_nonwatched() {
//        // Event should be considered as file removing in watched directory.
//        let tmp1 = TempDir::new("rename-iswatched").unwrap();
//        let tmp2 = TempDir::new("rename-nowatched").unwrap();
//        let oldpath = tmp1.path().join("file-old.log");
//        let newpath = tmp2.path().join("file-new.log");

//        File::create(&oldpath).unwrap();

//        timer::sleep(Duration::milliseconds(50));

//        let mut watcher = Watcher::new();
//        watcher.watch(tmp1.path().clone());

//        timer::sleep(Duration::milliseconds(50));

//        fs::rename(&oldpath, &newpath).unwrap();

//        match watcher.rx.recv() {
//            Remove(p) => {
//                assert_eq!(b"file-old.log", p.filename().unwrap());
//            }
//            _ => { fail!("Expected `Remove` event") }
//        }
//    }

//    #[test]
//    fn rename_file_from_watched_directory_to_watched() {
//        // Event should be considered as file renaming.
//        let tmp1 = TempDir::new("rename-watched1").unwrap();
//        let tmp2 = TempDir::new("rename-watched2").unwrap();
//        let oldpath = tmp1.path().join("file-old.log");
//        let newpath = tmp2.path().join("file-new.log");

//        File::create(&oldpath).unwrap();

//        timer::sleep(Duration::milliseconds(50));

//        let mut watcher = Watcher::new();
//        watcher.watch(tmp1.path().clone());
//        watcher.watch(tmp2.path().clone());

//        timer::sleep(Duration::milliseconds(50));

//        fs::rename(&oldpath, &newpath).unwrap();

//        match watcher.rx.recv() {
//            Rename(old, new) => {
//                assert_eq!(b"file-old.log", old.filename().unwrap());
//                assert_eq!(b"file-new.log", new.filename().unwrap());
//            }
//            _ => { fail!("Expected `Rename` event") }
//        }
//    }
}
