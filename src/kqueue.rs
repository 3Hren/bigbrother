#![allow(non_camel_case_types, non_uppercase_statics)] // C types

use std::collections::HashMap;
use std::io::FileStat;
use std::io::fs;
use std::io::fs::PathExtensions;
use std::ptr;

use sync::comm::{Empty, Disconnected};

use libc::{c_void, c_int, uintptr_t, intptr_t};

use time::Timespec;

pub enum Event {
    Create(Path),
    Remove(Path),
    Modify(Path),
    Rename(Path, Path),
}

#[deriving(Show)]
pub enum KQueueError {
    UnableToCreateKQueue,
}

enum Control {
    Add(Path),
    Exit,
}

#[deriving(Clone)]
struct WatchedFileStat {
    path: Path,
//    size: u64,
//    perm: FilePermission,
//    created: u64,
    modified: u64,
//    accessed: u64,
//    device: u64,
    inode: u64,
//    rdev: u64,
//    nlink: u64,
//    uid: u64,
//    gid: u64,
//    blksize: u64,
//    blocks: u64,
//    flags: u64,
//    gen: u64,
}

impl WatchedFileStat {
    fn new(path: Path, stat: &FileStat) -> WatchedFileStat {
        WatchedFileStat {
            path: path,
            modified: stat.modified,
            inode: stat.unstable.inode,
        }
    }
}

type FileStatMap = HashMap<u64, WatchedFileStat>;

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

        // TODO: Path exists?
        //  + > Send
        //  - > Return EBADF (PathNotExists).
        self.txc.send(Add(path));
    }

    fn run(mut queue: KQueue, tx: Sender<Event>, rxc: Receiver<Control>) {
        debug!("Starting watcher thread ...");

        let period: f32 = 0.1e9;
        let timeout = Timespec::new(0, period as i32);

        loop {
            debug!("Performing next watcher loop iteration ...");

            let mut fds = HashMap::new();
            let mut paths = HashMap::new();
            let mut stats: FileStatMap = HashMap::new();
            match rxc.try_recv() {
                Ok(value) => {
                    match value {
                        Add(path) => {
                            debug!("Trying to add '{}' ...", path.display());
                            // TODO: Path:
                            //   - file -> add it
                            //   - dir -> add all paths
                            //   - symlink -> follow.
                            let handler = FileHandler::new(&path).unwrap(); // TODO: Unsafe.
                            let fd = handler.fd;

                            if path.is_file() {
                                fds.insert(fd, handler);
                                paths.insert(fd, (path, true));
                                let input = [
                                    kevent::new(fd as u64, EVFILT_VNODE, EV_ADD, NOTE_DELETE | NOTE_WRITE | NOTE_RENAME)
                                ];

                                let mut output: [kevent, ..0] = [];
                                let n = queue.process(&input, &mut output, &None);
                                debug!("Adding {} descriptors: {}", input.len(), n);
                            } else if path.is_dir() {
                                fds.insert(fd, handler);
                                paths.insert(fd, (path.clone(), false));
                                let mut input = Vec::new();
                                input.push(
                                    kevent::new(fd as u64, EVFILT_VNODE, EV_ADD, NOTE_DELETE | NOTE_WRITE | NOTE_RENAME)
                                );

                                for p in fs::walk_dir(&path).unwrap() {
                                    debug!(" - adding sub '{}' ...", p.display());

                                    let stat = p.stat().unwrap();
                                    stats.insert(stat.unstable.inode, WatchedFileStat::new(p.clone(), &stat));

                                    let handler = FileHandler::new(&p).unwrap(); // TODO: Unsafe.
                                    let fd = handler.fd;
                                    let isfile = p.is_file();

                                    fds.insert(fd, handler);
                                    paths.insert(fd, (p.clone(), isfile));
                                    input.push(
                                        kevent::new(fd as u64, EVFILT_VNODE, EV_ADD, NOTE_DELETE | NOTE_WRITE | NOTE_RENAME)
                                    );
                                }

                                let mut output: [kevent, ..0] = [];
                                let n = queue.process(input.as_slice(), &mut output, &None);
                                debug!("Adding {} descriptors: {}", input.len(), n);
                            }
                        }
                        Exit => { break }
                    }
                }
                Err(Empty) => {}
                Err(Disconnected) => { break }
            }

            let input = [];
            let mut output: [kevent, ..1] = [kevent::invalid()];

            let n = queue.process(&input, &mut output, &Some(timeout)) as uint;
            debug!("Processing {} events ...", n);
            for ev in output[..n].iter() {
                debug!(" - Event(fd={}, filter={}, flags={}, fflags={}, data={}, udata={})", ev.ident, ev.filter, ev.flags, ev.fflags, ev.data, ev.udata);
                let fd = ev.ident as i32;
                let (path, isfile) = match paths.find(&fd) {
                    Some(v) => v.clone(),
                    None => {
                        warn!("Received unregistered event on {} fd - ignoring", ev.ident);
                        continue;
                    }
                };

                if !ev.filter.intersects(EVFILT_VNODE) {
                    warn!("Received non vnode event - ignoring");
                    continue;
                }

                if isfile {
                    match ev.fflags {
                        x if x.intersects(NOTE_WRITE) => {
                            tx.send(Modify(path));
                        }
                        x if x.intersects(NOTE_DELETE) => {
                            fds.remove(&fd);
                            paths.remove(&fd);
                            tx.send(Remove(path));
                        }
                        x if x.intersects(NOTE_RENAME) => {
                            let new = getpath(fd);
                            // TODO: Update new info.
                            // TODO: What if new path is not watched?
                            tx.send(Rename(path, new));
                        }
                        // TODO: ModifyAttr
                        x => {
                            debug!("Received unintresting {} event - ignoring", x);
                        }
                    }
                } else {
                    match ev.fflags {
                        x if x.intersects(NOTE_WRITE) => {
                            debug!("Scanning {}", path.display());
                            let mut currstats = HashMap::new();

                            for path in fs::walk_dir(&path).unwrap().filter(|path| path.is_file()) {
                                let stat = path.stat().unwrap();
                                debug!("Found {}, {}", path.display(), stat.modified);
                                currstats.insert(stat.unstable.inode, WatchedFileStat::new(path, &stat));
                            }

                            Watcher::created(&stats, &currstats, &tx);
                            Watcher::modified(&stats, &currstats, &tx);
                            // TODO: Save new stats.
                        }
                        x => {
                            debug!("Received unintresting {} event - ignoring", x);
                        }
                    }
                    // TODO: Write - new file has been created (or any action with files within directory?)
                    //   Scan for new files
                    //   Allocate events list
                    //   An fd contains in `watched` map?
                    //     + > nop
                    //     - > add to `watched` & add to events list
                    //       tx.send Create
                    //   Process events list.

                    // TODO: Remove
                    //   Maybe do nothing with paths, because all files should be evented?
                    // TODO: Rename
                    // TODO: ModifyAttr
                }
            }
        }

        debug!("Watcher thread has been stopped");
    }

//    fn add(mut queue: KQueue, paths: &mut HashMap<i32, FileHandler>, path: Path, tx: Sender<Event>) {
//    }

    fn created(prev: &FileStatMap, curr: &FileStatMap, tx: &Sender<Event>) {
        for (inode, stat) in curr.iter() {
            if !prev.contains_key(inode) {
                tx.send(Create(stat.path.clone()));
            }
        }
    }

//    fn removed(prev: &FileStatMap, curr: &FileStatMap, tx: &Sender<Event>) {
//        for (inode, stat) in prev.iter() {
//            if !curr.contains_key(inode) {
//                tx.send(Remove(stat.path.clone()));
//            }
//        }
//    }

    fn modified(prev: &FileStatMap, curr: &FileStatMap, tx: &Sender<Event>) {
        for (inode, stat) in curr.iter() {
            if let Some(prevstat) = prev.find(inode) {
                if prevstat.path != stat.path {
                    tx.send(Rename(prevstat.path.clone(), stat.path.clone()));
                }

                if prevstat.modified != stat.modified {
                    tx.send(Modify(stat.path.clone()));
                }
            }
        }
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        debug!("Dropping the watcher");

        self.txc.send(Exit);
    }
}

fn getpath(fd: i32) -> Path {
    use std::c_str::CString;
    let path = [0i8, ..1024];
    unsafe {
        let res = fcntl(fd, 50, path.as_ptr()); // TODO: Magic.
        assert_eq!(0, res);
    }
    let s = unsafe {
        CString::new(path.as_ptr(), false)
    };
    Path::new(s)
}

bitflags! {
    #[repr(C)]
    #[deriving(Show)]
    flags EventFilter: i16 {
        const EVFILT_VNODE = -4,
        const EVFILT_USER  = -10,
    }
}

bitflags! {
    #[repr(C)]
    #[deriving(Show)]
    flags EventFlags: u16 {
        const EV_ADD    = 0x0001,
        const EV_ENABLE = 0x0004,
        const EV_CLEAR  = 0x0020,
    }
}

bitflags! {
    #[repr(C)]
    #[deriving(Show)]
    flags EventFilterFlags: u32 {
        const NOTE_DELETE   = 0x00000001,
        const NOTE_WRITE    = 0x00000002,
        const NOTE_EXTEND   = 0x00000004,
        const NOTE_ATTRIB   = 0x00000008,
        const NOTE_LINK     = 0x00000010,
        const NOTE_RENAME   = 0x00000020,
        const NOTE_REVOKE   = 0x00000040,
        const NOTE_NONE     = 0x00000080,
        const NOTE_TRIGGER  = 0x01000000,
    }
}

#[repr(C)]
struct kevent {
    ident: uintptr_t,           // Identifier for this event.
    filter: EventFilter,        // Filter for event.
    flags: EventFlags,          // General flags.
    fflags: EventFilterFlags,   // Filter-specific flags.
    data: intptr_t,             // Filter-specific data.
    udata: *const c_void,       // Opaque user data identifier.
}

impl kevent {
    fn new(ident: u64, filter: EventFilter, flags: EventFlags, fflags: EventFilterFlags) -> kevent {
        kevent::raw(ident, filter, flags, fflags, 0, ptr::null::<c_void>())
    }

    fn invalid() -> kevent {
        kevent::new(0, EVFILT_USER, EventFlags::empty(), EventFilterFlags::empty())
    }

    fn raw(ident: u64, filter: EventFilter, flags: EventFlags, fflags: EventFilterFlags, data: intptr_t, udata: *const c_void) -> kevent {
        kevent {
            ident: ident,
            filter: filter,
            flags: flags,
            fflags: fflags,
            data: data,
            udata: udata,
        }
    }
}

// RAII wrapper for read-only file descriptor.
struct FileHandler {
    fd: i32,
}

impl FileHandler {
    fn new(path: &Path) -> Result<FileHandler, i32> {
        let pathstr = match path.as_str() {
            Some(v) => v,
            None => {
                fail!("Failed to convert {} path to string", path.display());
            }
        };

        let fd = unsafe {
            open(pathstr.to_c_str().as_ptr(), 0x0000)
        };

        if fd < 0 {
            return Err(fd)
        }

        let fh = FileHandler {
            fd: fd,
        };

        Ok(fh)
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

        let kq = KQueue {
            fd: fd
        };

        Ok(kq)
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
    fn fcntl(fd: c_int, arg: c_int, path: *const i8) -> c_int;
}

#[cfg(test)]
mod test {

mod kqueue {
    extern crate test;

    use std::io::{File, TempDir};
    use std::io::timer;
    use std::time::Duration;

    use super::super::{
        KQueue, kevent, FileHandler,
        EVFILT_VNODE,
        EV_ADD,
        NOTE_WRITE,
    };

    #[test]
    fn kqueue_create_single_file() {
        use std::os;
        let tmp = TempDir::new("kqueue-create-single").unwrap();
        let path = tmp.path().join("file.log");
        let ntmp = FileHandler::new(tmp.path()).unwrap();

        let mut queue = KQueue::new().unwrap();

        let input = [
            kevent::new(ntmp.fd as u64, EVFILT_VNODE, EV_ADD, NOTE_WRITE)
        ];
        let mut output: [kevent, ..0] = [];

        let n = queue.process(&input, &mut output, &None);
        error!("{} {}", n, os::error_string(os::errno() as uint));

        assert_eq!(0, n);

        timer::sleep(Duration::milliseconds(50));
        File::create(&path).unwrap();

        let input = [];
        let mut output: [kevent, ..1] = [kevent::invalid()];

        let n = queue.process(&input, &mut output, &None);

        assert_eq!(1, n);
        let actual = output[0];
        assert_eq!(ntmp.fd as u64, actual.ident);
        assert_eq!(EVFILT_VNODE, actual.filter);
        assert_eq!(EV_ADD, actual.flags);
        assert_eq!(NOTE_WRITE, actual.fflags);
    }

//    #[test] watch file modified.
//    #[test] watch file attributes modified.
//    #[test] watch file removed.
//    #[test] watch file renamed.
//    #[test] watch file when removed directory.
//    #[test] watch file when renamed directory.

} // mod kqueue

mod watcher {
    extern crate test;

    use std::io::{File, TempDir};
    use std::io::fs;
    use std::io::fs::PathExtensions;
    use std::io::timer;
    use std::time::Duration;

    use super::super::{
        Watcher, Create, Modify, Rename, Remove,
    };

//file exists and watched -> [modify, remove, rename]
//dir exists and watched ->
//    file create, file create + touch other (not duplicate event)
//    file remove
//    file rename to watched, to unwatched, from unwatched
//    dir remove
//    dir rename

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

    #[test]
    fn watch_file_remove_file() {
        let tmp = TempDir::new("watch_file_remove_file").unwrap();
        let path = tmp.path().join("file.log");
        File::create(&path).unwrap();

        let mut watcher = Watcher::new();
        watcher.watch(path.clone());

        timer::sleep(Duration::milliseconds(50));

        fs::unlink(&path).unwrap();

        match watcher.rx.recv() {
            Remove(p) => {
                assert_eq!(b"file.log", p.filename().unwrap())
            }
            _ => { fail!("Expected `Remove` event") }
        }
    }

    #[test]
    fn watch_file_rename_file() {
        let tmp = TempDir::new("watch_file_rename_file").unwrap();
        let path = tmp.path().join("file.log");
        File::create(&path).unwrap();

        let mut watcher = Watcher::new();
        watcher.watch(path.clone());

        timer::sleep(Duration::milliseconds(50));

        fs::rename(&path, &tmp.path().join("file-new.log")).unwrap();

        match watcher.rx.recv() {
            Rename(old, new) => {
                assert_eq!(b"file.log", old.filename().unwrap())
                assert_eq!(b"file-new.log", new.filename().unwrap())
            }
            _ => { fail!("Expected `Rename` event") }
        }
    }

    #[test]
    fn watch_dir_create_single_file() {
        let tmp = TempDir::new("watch_dir_create_single_file").unwrap();
        let path = tmp.path().join("file.log");

        let mut watcher = Watcher::new();
        watcher.watch(tmp.path().clone());

        timer::sleep(Duration::milliseconds(50));

        File::create(&path).unwrap();

        match watcher.rx.recv() {
            Create(p) => {
                assert_eq!(b"file.log", p.filename().unwrap())
            }
            _ => { fail!("Expected `Create` event") }
        }
    }

    #[test]
    fn watch_dir_remove_single_file() {
        let tmp = TempDir::new("watch_dir_remove_single_file").unwrap();
        let path = tmp.path().join("file.log");

        assert!(!path.exists());
        File::create(&path).unwrap();

        timer::sleep(Duration::milliseconds(50));

        let mut watcher = Watcher::new();
        watcher.watch(tmp.path().clone());

        timer::sleep(Duration::milliseconds(50));

        fs::unlink(&path).unwrap();

        match watcher.rx.recv() {
            Remove(p) => {
                assert_eq!(b"file.log", p.filename().unwrap())
            }
            _  => { fail!("Expected `Remove` event") }
        }
    }

    #[test]
    fn watch_dir_rename_single_file() {
        let tmp = TempDir::new("watch_dir_rename_single_file").unwrap();
        let oldpath = tmp.path().join("file-old.log");
        let newpath = tmp.path().join("file-new.log");

        File::create(&oldpath).unwrap();

        timer::sleep(Duration::milliseconds(50));

        let mut watcher = Watcher::new();
        watcher.watch(tmp.path().clone());

        timer::sleep(Duration::milliseconds(50));

        fs::rename(&oldpath, &newpath).unwrap();

        match watcher.rx.recv() {
            Rename(old, new) => {
                assert_eq!(b"file-old.log", old.filename().unwrap());
                assert_eq!(b"file-new.log", new.filename().unwrap());
            }
            _ => { fail!("Expected `Rename` event") }
        }
    }

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
} // mod watcher

} // mod test
