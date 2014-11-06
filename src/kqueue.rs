#![allow(non_camel_case_types, non_upper_case_globals)] // C types

use std::collections::{HashSet, HashMap};
use std::io::{FileStat, FileType, TypeFile, TypeDirectory};
use std::io::fs;
use std::io::fs::PathExtensions;
use std::os;
use std::ptr;

use sync::{Mutex, Arc};
use sync::comm::{Empty, Disconnected};

use libc::{c_void, c_int, uintptr_t, intptr_t, timespec, time_t, c_long};

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

// Temporary wrapper for FileType, which is non-clonable for some reasons I don't understand.
struct ClonableFileType(FileType);

impl Clone for ClonableFileType {
    fn clone(&self) -> ClonableFileType {
        use std::io::{TypeFile, TypeDirectory, TypeNamedPipe, TypeBlockSpecial, TypeSymlink, TypeUnknown};

        match *self {
            ClonableFileType(TypeFile)         => ClonableFileType(TypeFile),
            ClonableFileType(TypeDirectory)    => ClonableFileType(TypeDirectory),
            ClonableFileType(TypeNamedPipe)    => ClonableFileType(TypeNamedPipe),
            ClonableFileType(TypeBlockSpecial) => ClonableFileType(TypeBlockSpecial),
            ClonableFileType(TypeSymlink)      => ClonableFileType(TypeSymlink),
            ClonableFileType(TypeUnknown)      => ClonableFileType(TypeUnknown),
        }
    }
}

#[deriving(Clone)]
struct WatchedFileStat {
    path: Path,
    kind: ClonableFileType,
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
            kind: ClonableFileType(stat.kind),
            modified: stat.modified,
            inode: stat.unstable.inode,
        }
    }
}

type FileStatMap = HashMap<u64, WatchedFileStat>;

pub struct Watcher {
    pub rx: Receiver<Event>,
    txc: Sender<Control>,
    exit: Arc<Mutex<()>>,
}

impl Watcher {
    pub fn new() -> Watcher {
        let queue = match KQueue::new() {
            Ok(queue) => queue,
            Err(err)  => panic!(err)
        };

        let (tx, rx) = channel();
        let (txc, rxc) = channel();
        let exit = Arc::new(Mutex::new(()));

        let watcher = Watcher {
            rx: rx,
            txc: txc,
            exit: exit.clone(),
        };

        spawn(proc() Watcher::run(queue, tx, rxc, exit));

        watcher
    }

    pub fn watch(&mut self, path: Path) {
        debug!("Trying to add '{}' to the watcher ...", path.display());
        // TODO: Return EBADF (PathNotExists) if path not exists.
        self.txc.send(Add(path));
    }

    fn run(mut queue: KQueue, tx: Sender<Event>, rxc: Receiver<Control>, exit: Arc<Mutex<()>>) {
        debug!("Starting watcher thread ...");

        let mut d = Internal::new(tx);

        let input = [];
        let mut output: [kevent, ..8] = [kevent::invalid(), ..8];
        let timeout = Timespec::new(0, 100_000_000i32);

        loop {
            debug!("Performing watcher loop iteration ...");

            match rxc.try_recv() {
                Ok(value) => {
                    match value {
                        Add(path) => {
                            debug!("Trying to register '{}' with kqueue...", path.display());
                            let input = d.add(path);
                            let mut output: [kevent, ..0] = [];
                            let n = queue.process(input.as_slice(), &mut output, &None);
                            // TODO: Analyze result.
                            debug!("Adding {} descriptors: {}", input.len(), n);
                        }
                        Exit => { break }
                    }
                }
                Err(Empty) => {}
                Err(Disconnected) => { break }
            }

            let n = queue.process(&input, &mut output, &Some(timeout));

            if n == -1 {
                warn!("Failed to process kernel events: [{}] {}", os::errno(), os::error_string(os::errno() as uint));
                break;
            }

            debug!("Processing {} event{} ...", n, if n > 1 {"s"} else {""});
            for ev in output[..n as uint].iter() {
                d.process(ev);
            }
        }

        debug!("Watcher thread has been stopped");
        exit.lock().cond.signal();
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        debug!("Dropping the watcher");

        self.txc.send(Exit);
        self.exit.lock().cond.wait();
    }
}

struct Internal {
    tx: Sender<Event>,

    fds: HashMap<i32, FileHandler>, // RAII wrapper for fd, which closes it when out of scope.
    paths: HashMap<i32, WatchedFileStat>, // fd -> (inode, FileType, Path)
    inodes: HashSet<u64>,
    stats: HashMap<u64, HashMap<u64, Path>>, // inode -> inode -> Path
}

impl Internal {
    fn new(tx: Sender<Event>) -> Internal {
        Internal {
            tx: tx,
            fds:   HashMap::new(),
            paths : HashMap::new(),
            inodes: HashSet::new(),
            stats : HashMap::new(),
        }
    }

    fn add(&mut self, path: Path) -> Vec<kevent> {
        // TODO: Follow if path - symlink.
        let handler = FileHandler::new(&path).unwrap(); // TODO: Unsafe.
        let fd = handler.fd;
        let stat = path.stat().unwrap();

        let mut input = Vec::new();

        if path.is_file() {
            self.fds.insert(fd, handler);
            self.paths.insert(fd, WatchedFileStat::new(path.clone(), &stat));
            self.inodes.insert(stat.unstable.inode);

            input.push(
                kevent::new(fd as u64, EVFILT_VNODE, EV_ADD | EV_CLEAR, NOTE_DELETE | NOTE_WRITE | NOTE_RENAME)
            );
        } else if path.is_dir() {
            self.fds.insert(fd, handler);
            self.paths.insert(fd, WatchedFileStat::new(path.clone(), &stat));
            self.inodes.insert(stat.unstable.inode);

            input.push(
                kevent::new(fd as u64, EVFILT_VNODE, EV_ADD | EV_CLEAR, NOTE_DELETE | NOTE_WRITE | NOTE_RENAME)
            );

            let mut curr = HashMap::new();

            for path in fs::walk_dir(&path).unwrap() {
                debug!(" - adding sub '{}' ...", path.display());

                let handler = FileHandler::new(&path).unwrap(); // TODO: Unsafe.
                let fd = handler.fd;
                let stat = path.stat().unwrap();

                self.fds.insert(fd, handler);
                self.paths.insert(fd, WatchedFileStat::new(path.clone(), &stat));
                self.inodes.insert(stat.unstable.inode);

                curr.insert(stat.unstable.inode, path.clone());

                input.push(
                    kevent::new(fd as u64, EVFILT_VNODE, EV_ADD | EV_CLEAR, NOTE_DELETE | NOTE_WRITE | NOTE_RENAME)
                );
            }
            self.stats.insert(stat.unstable.inode, curr);
        }

        input
    }

    fn process(&mut self, ev: &kevent) {
        debug!(" - Event(fd={}, filter={}, flags={}, fflags={}, data={})", ev.ident, ev.filter, ev.flags, ev.fflags, ev.data);
        let fd = ev.ident as i32;
        let stat = match self.paths.find_copy(&fd) {
            Some(v) => v,
            None => {
                warn!("Received unregistered event on {} fd - ignoring", ev.ident);
                return;
            }
        };

        if !ev.filter.intersects(EVFILT_VNODE) {
            warn!("Received non vnode event - ignoring");
            return;
        }

        let path = stat.path;
        let inode = stat.inode;

        match stat.kind {
            ClonableFileType(TypeFile) => {
                match ev.fflags {
                    x if x.intersects(NOTE_WRITE) => {
                        debug!(" <- Modify: {}", path.display());
                        // TODO: Seems like I should sync current position with the kevent.
                        self.tx.send(Modify(path));
                    }
                    x if x.intersects(NOTE_DELETE) => {
                        debug!(" <- Remove: {}", path.display());
                        self.fds.remove(&fd);
                        self.tx.send(Remove(path));
                    }
                    x if x.intersects(NOTE_RENAME) => {
                        let newpath = getpath(fd);
                        let dirinode = newpath.dir_path().stat().unwrap().unstable.inode;

                        if !self.inodes.contains(&dirinode) {
                            debug!(" <- Remove: {}", path.display());
                            self.tx.send(Remove(path));
                        } else {
                            debug!(" <- Rename: {} -> {}", path.display(), newpath.display());
                            // TODO: Update new info.
                            self.tx.send(Rename(path, newpath));
                        }
                    }
                    // TODO: ModifyAttr
                    x => {
                        debug!("Received unintresting {} event - ignoring", x);
                    }
                }
            }
            ClonableFileType(TypeDirectory) => {
                match ev.fflags {
                    x if x.intersects(NOTE_WRITE) => {
                        debug!("Directory {} has changed - scanning ...", path.display());
                        let mut curr = HashMap::new();

                        for path in fs::walk_dir(&path).unwrap() {
                            let stat = path.stat().unwrap();
                            curr.insert(stat.unstable.inode, path);
                        }

                        {
                            let prev = match self.stats.find(&inode) {
                                Some(v) => v,
                                None    => {debug!("2"); return; }
                            };

                            for (inode, path) in curr.iter() {
                                if !prev.contains_key(inode) && !self.inodes.contains(inode) {
                                    debug!(" <- Create: {}", path.display());
                                    self.inodes.insert(*inode);
                                    self.tx.send(Create(path.clone()));
                                }
                            }
                        }
                        self.stats.insert(inode, curr);
                        // TODO: Register new paths.
                    }
                    x => {
                        debug!("Received unintresting {} event - ignoring", x);
                    }
                }
            }
            _ => {
                warn!("Received event from non-file descriptor - ignoring");
            }
            // TODO: Write - new file has been created (or any action with files within directory?)
            // TODO: Remove. Maybe do nothing with paths, because all files should be evented?
            // TODO: Rename
            // TODO: ModifyAttr
        }
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
                panic!("Failed to convert {} path to string", path.display());
            }
        };

        let fd = unsafe {
            open(pathstr.to_c_str().as_ptr(), 0x0000) // TODO: Magic
        };

        if fd < 0 {
            return Err(fd) // TODO: Return Enum, not i32.
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
            close(self.fd);
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
        let timeout = match *timeout {
            Some(value) => {
                let timeout = timespec {
                    tv_sec: value.sec as time_t,
                    tv_nsec: value.nsec as c_long,
                };
                &timeout as *const timespec
            }
            None => {
                ptr::null::<timespec>()
            }
        };

        unsafe {
            kevent(self.fd, input.as_ptr(), input.len() as i32, output.as_ptr(), output.len() as i32, timeout)
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
    fn kevent(kq: c_int, changelist: *const kevent, nchanges: c_int, eventlist: *const kevent, nevents: c_int, timeout: *const timespec) -> c_int;

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
        let tmp = TempDir::new("kqueue-create-single").unwrap();
        let path = tmp.path().join("file.log");
        let ntmp = FileHandler::new(tmp.path()).unwrap();

        let mut queue = KQueue::new().unwrap();

        let input = [
            kevent::new(ntmp.fd as u64, EVFILT_VNODE, EV_ADD, NOTE_WRITE)
        ];
        let mut output: [kevent, ..0] = [];

        let n = queue.process(&input, &mut output, &None);
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

    use super::super::{Watcher, Create, Modify, Rename, Remove};

//    #[test] watch dir rename file inplace
//    #[test] watch dir rename file to unwatched
//    #[test] watch dir rename file from unwatched
//    #[test] watch dir create file, touch other (prevent already emitted events)
//    #[test] watch dir create file create file
//    #[test] watch dir rename dir
//    #[test] watch dir remove dir

    /// Test single watched file modifying.
    /*
     * We create temporary directory with single file and register file with the watcher.
     * After writing some bytes we expect Modify event to be received.
     */
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
            Modify(actual) => {
                assert!(path == actual)
            }
            _ => { panic!("Expected `Modify` event") }
        }
    }

    /// Test single watched file removing.
    /*
     * We create temporary directory with single file and register file with the watcher.
     * Then we remove the file and expect Remove event to be received.
     */
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
            Remove(actual) => {
                assert!(path == actual)
            }
            _ => { panic!("Expected `Remove` event") }
        }
    }

    /// Test single watched file renaming.
    /*
     * We create temporary directory with single file and register file with the watcher.
     * After giving it some new name (in the same directory) we expect Remove event to be received,
     * because that file is no longer watched by the watcher. To receive Rename event we must
     * register the directory itself with the watcher.
     */
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
            Remove(old) => {
                assert!(old == path)
            }
            _ => { panic!("Expected `Remove` event") }
        }
    }

    /// TODO: Test single watched file attribute modifying.

    /// Test file creation in single registered directory.
    /*
     * We create empty temporary directory and register it with the watcher.
     * After file creation we expect Create event to be received.
     */
    #[test]
    fn watch_dir_create_single_file() {
        let tmp = TempDir::new("watch_dir_create_single_file").unwrap();
        let path = tmp.path().join("file.log");

        let mut watcher = Watcher::new();
        watcher.watch(tmp.path().clone());

        timer::sleep(Duration::milliseconds(50));

        File::create(&path).unwrap();

        match watcher.rx.recv() {
            Create(actual) => {
                assert!(path == actual)
            }
            _ => { panic!("Expected `Create` event") }
        }
    }

    /// TODO: Test file modifying in single registered directory.

    /// Test file removing in single registered directory.
    /*
     * We create temporary directory with single file and register it with the watcher.
     * After removing the file we expect Remove event to be received.
     */
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
            Remove(actual) => {
                assert!(path == actual)
            }
            _  => { panic!("Expected `Remove` event") }
        }
    }

    #[test]
    fn watch_dir_rename_single_file() {
        use std::os;
        use std::io::fs;

        let tmp = TempDir::new_in(&os::make_absolute(&Path::new(".")), "watch_dir_rename_single_file").unwrap();
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
                assert!(oldpath == os::make_absolute(&old));
                assert!(newpath == os::make_absolute(&new));
                assert_eq!(b"file-old.log", old.filename().unwrap());
                assert_eq!(b"file-new.log", new.filename().unwrap());
            }
            _ => { panic!("Expected `Rename` event") }
        }
    }

    #[test]
    fn create_two_files_in_different_directories() {
        use std::str;
        use std::collections::HashSet;

        let tmp1 = TempDir::new("create-1").unwrap();
        let tmp2 = TempDir::new("create-2").unwrap();
        let path1 = tmp1.path().join("file1.log");
        let path2 = tmp2.path().join("file2.log");

        let mut watcher = Watcher::new();
        watcher.watch(tmp1.path().clone());
        watcher.watch(tmp2.path().clone());

        // Sleep a bit more to match at lease 3-4 internal loop cycles.
        timer::sleep(Duration::milliseconds(400));

        File::create(&path1).unwrap();
        File::create(&path2).unwrap();

        let mut matches = HashSet::new();
        matches.insert(String::from_str("file1.log"));
        matches.insert(String::from_str("file2.log"));

        let mut counter = 2u8;
        while counter > 0 {
            match watcher.rx.recv() {
                Create(p) => {
                    assert!(matches.remove(&String::from_str(str::from_utf8(p.filename().unwrap()).unwrap())));
                }
                _ => { panic!("Expected `Create` event") }
            }

            counter -= 1;
        }
        assert!(matches.is_empty());
    }

    #[test]
    fn rename_file_from_nonwatched_directory_to_watched() {
        // Event should be considered as file creation in watched directory.
        let tmp1 = TempDir::new("rename-nowatched").unwrap();
        let tmp2 = TempDir::new("rename-towatched").unwrap();
        let oldpath = tmp1.path().join("file-old.log");
        let newpath = tmp2.path().join("file-new.log");

        File::create(&oldpath).unwrap();

        timer::sleep(Duration::milliseconds(50));

        let mut watcher = Watcher::new();
        watcher.watch(tmp2.path().clone());

        timer::sleep(Duration::milliseconds(50));

        fs::rename(&oldpath, &newpath).unwrap();

        match watcher.rx.recv() {
            Create(p) => {
                assert_eq!(b"file-new.log", p.filename().unwrap());
            }
            _ => { panic!("Expected `Create` event") }
        }
    }

    #[test]
    fn rename_file_from_watched_directory_to_nonwatched() {
        // Event should be considered as file removing in watched directory.
        let tmp1 = TempDir::new("rename-watched").unwrap();
        let tmp2 = TempDir::new("rename-nonwatched").unwrap();
        let oldpath = tmp1.path().join("file-old.log");
        let newpath = tmp2.path().join("file-new.log");

        File::create(&oldpath).unwrap();

        timer::sleep(Duration::milliseconds(50));

        let mut watcher = Watcher::new();
        watcher.watch(tmp1.path().clone());

        timer::sleep(Duration::milliseconds(50));

        fs::rename(&oldpath, &newpath).unwrap();

        match watcher.rx.recv() {
            Remove(p) => {
                assert_eq!(b"file-old.log", p.filename().unwrap());
            }
            _ => { panic!("Expected `Remove` event") }
        }
    }

    #[test]
    fn rename_file_from_watched_directory_to_watched() {
        use std::os;

        // Event should be considered as file renaming.
        let tmp1 = TempDir::new_in(&os::make_absolute(&Path::new(".")), "rename_watched1").unwrap();
        let tmp2 = TempDir::new_in(&os::make_absolute(&Path::new(".")), "rename_watched2").unwrap();
        let oldpath = tmp1.path().join("file-old.log");
        let newpath = tmp2.path().join("file-new.log");

        File::create(&oldpath).unwrap();

        timer::sleep(Duration::milliseconds(50));

        let mut watcher = Watcher::new();
        watcher.watch(tmp1.path().clone());
        watcher.watch(tmp2.path().clone());

        timer::sleep(Duration::milliseconds(200));

        fs::rename(&oldpath, &newpath).unwrap();

        match watcher.rx.recv() {
            Rename(old, new) => {
                assert!(oldpath == os::make_absolute(&old));
                assert!(newpath == os::make_absolute(&new));
            }
            _ => { panic!("Expected `Rename` event") }
        }
    }
} // mod watcher

} // mod test
