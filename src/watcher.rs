use std::collections::{HashSet, HashMap};
use std::collections::hashmap::{Occupied, Vacant};
use std::io::fs;
use std::io::{FileStat, Timer};
use std::time::Duration;
use std::io::fs::PathExtensions;

//#[deriving(Show)]
pub enum Event {
    Create(Path),
    Remove(Path),
    //ModifyMeta,
    Modify(Path),
    Rename(Path, Path),
}

enum Control {
    Update(HashSet<Path>),
    Exit,
}

pub struct Watcher {
    pub rx: Receiver<Event>,
    ctx: Sender<Control>,
    paths: HashSet<Path>,
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

impl Watcher {
    pub fn new() -> Watcher {
        let (tx, rx) = channel();
        let (ctx, crx) = channel();
        let watcher = Watcher {
            rx: rx,
            ctx: ctx,
            paths: HashSet::new(),
        };

        spawn(proc() Watcher::run(tx, crx));
        watcher
    }

    pub fn watch(&mut self, path: Path) {
        debug!("Adding '{}' to the watch", path.display());
        self.paths.insert(path);
        self.ctx.send(Update(self.paths.clone()));
    }

    fn run(tx: Sender<Event>, crx: Receiver<Control>) {
        debug!("Starting watcher thread ...");

        let period = Duration::milliseconds(100);
        let mut timer = Timer::new().unwrap();
        let timeout = timer.periodic(period);

        let mut paths = HashSet::new();
        let mut prev: HashMap<u64, WatchedFileStat> = HashMap::new();
        loop {
            debug!("Event loop tick ...");

            let curr = Watcher::rescan(&paths);

            // Check for created files.
            for (inode, stat) in curr.iter() {
                if !prev.contains_key(inode) {
                    tx.send(Create(stat.path.clone()));
                }
            }

            // Check for removed files.
            for (inode, stat) in prev.iter() {
                if !curr.contains_key(inode) {
                    tx.send(Remove(stat.path.clone()));
                }
            }

            // Check for changed files.
            for (inode, stat) in curr.iter() {
                match prev.entry(*inode) {
                    Vacant(..) => continue,
                    Occupied(entry) => {
                        let prevstat = entry.get();
                        if prevstat.path != stat.path {
                            tx.send(Rename(prevstat.path.clone(), stat.path.clone()));
                        }

                        if prevstat.modified != stat.modified {
                            tx.send(Modify(stat.path.clone()));
                        }
                    }
                };
            }

            prev = curr;
            select! {
                control = crx.recv() => {
                    match control {
                        Update(newpaths) => {
                            debug!("Received watcher update event");
                            paths = newpaths;
                            prev = Watcher::rescan(&paths);
                        }
                        Exit => {
                            debug!("Received watcher exit event - performing graceful shutdown");
                            break;
                        }
                    }
                },
                () = timeout.recv() => {}
            }
        }
    }

    //TODO: What to do if something failed?
    fn rescan(paths: &HashSet<Path>) -> HashMap<u64, WatchedFileStat> {
        let mut stats = HashMap::new();
        for path in paths.iter() {
            debug!("Scanning {}", path.display());
            for path in fs::walk_dir(path).unwrap().filter(|path| path.is_file()) {
                let stat = path.stat().unwrap();
                debug!("Found {}, {}", path.display(), stat.modified);
                stats.insert(stat.unstable.inode, WatchedFileStat::new(path, &stat));
            }
        }
        stats
    }
}

impl Drop for Watcher {
    fn drop(&mut self) {
        self.ctx.send(Exit);
    }
}

#[cfg(test)]
mod test {
    extern crate test;

    use std::io::{File, TempDir};
    use std::io::fs;
    use std::io::fs::PathExtensions;
    use std::io::timer;
    use std::time::Duration;

    use super::Watcher;
    use super::{Create, Modify, Rename, Remove};

    #[test]
    fn create_single_file() {
        let tmp = TempDir::new("").unwrap();
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
    fn remove_single_file() {
        let tmp = TempDir::new("").unwrap();
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
            _  => { fail!("Expected Remove event") }
        }
    }

    #[test]
    fn rename_single_file() {
        let tmp = TempDir::new("").unwrap();
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
            _ => { fail!("Expected Rename event") }
        }
    }

    #[test]
    fn modify_single_file() {
        let tmp = TempDir::new("modify").unwrap();
        let path = tmp.path().join("file.log");

        let mut file = File::create(&path).unwrap();

        timer::sleep(Duration::milliseconds(50));

        let mut watcher = Watcher::new();
        watcher.watch(tmp.path().clone());

        // Timeout is need to be at least one second, because most filesystems have seconds resolution.
        timer::sleep(Duration::milliseconds(1000));

        file.write(b"some bytes!\n").unwrap();
        file.flush().unwrap();
        file.fsync().unwrap();

        match watcher.rx.recv() {
            Modify(p) => {
                assert_eq!(b"file.log", p.filename().unwrap());
            }
            _ => { debug!("Expected Modify event") }
        }
    }
}
