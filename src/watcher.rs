use std::collections::{HashSet, HashMap};
use std::io::fs;
use std::io::{FileStat, FilePermission, Timer};
use std::time::Duration;
use std::io::fs::PathExtensions;

//#[deriving(Show)]
pub enum Event {
    Create(Path),
    Remove(Path),
    //ModifyMeta,
    Modify(String),
    Rename(String, String),
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
struct CopyableFileStat {
    path: Path,
    size: u64,
    perm: FilePermission,
    created: u64,
    modified: u64,
    accessed: u64,
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

impl CopyableFileStat {
    fn new(path: Path, stat: &FileStat) -> CopyableFileStat {
        CopyableFileStat {
            path: path,
            size: stat.size,
            perm: stat.perm,
            created: stat.created,
            modified: stat.modified,
            accessed: stat.accessed,
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
        let mut prev: HashMap<u64, CopyableFileStat> = HashMap::new();
        loop {
            debug!("Event loop tick ...");

            let curr = Watcher::rescan(&paths);

            for (inode, stat) in curr.iter() {
                if !prev.contains_key(inode) {
                    tx.send(Create(stat.path.clone()));
                }
            }

            for (inode, stat) in prev.iter() {
                if !curr.contains_key(inode) {
                    tx.send(Remove(stat.path.clone()));
                }
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
    fn rescan(paths: &HashSet<Path>) -> HashMap<u64, CopyableFileStat> {
        let mut stats = HashMap::new();
        for path in paths.iter() {
            debug!("Handling {}", path.display());
            for path in fs::walk_dir(path).unwrap().filter(|path| path.is_file()) {
                debug!("File {}", path.display());
                let stat = path.stat().unwrap();
                stats.insert(stat.unstable.inode, CopyableFileStat::new(path, &stat));
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
    use super::{Create, /*Modify, Rename,*/ Remove};

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

//    #[test]
//    fn rename_file() {
//        let tempdir = TempDir::new("").unwrap();
//        let mut oldfilepath = tempdir.path().clone();
//        oldfilepath.push(Path::new("file-old.log"));

//        File::create(&oldfilepath).unwrap();

//        let mut watcher = Watcher::new();
//        watcher.watch(tempdir.path()).unwrap();

//        let mut newfilepath = tempdir.path().clone();
//        newfilepath.push(Path::new("file-new.log"));

//        fs::rename(&oldfilepath, &newfilepath).unwrap();

//        match watcher.rx.recv() {
//            Rename(old, new) => {
//                assert_eq!(b"file-old.log", Path::new(old.as_slice()).filename().unwrap());
//                assert_eq!(b"file-new.log", Path::new(new.as_slice()).filename().unwrap());
//            }
//            event @ _ => { fail!("Expected Rename event, actual: {}", event) }
//        }
//    }

//    #[test]
//    fn modify_single_file() {
//        let tempdir = TempDir::new("modify").unwrap();
//        let mut filepath = tempdir.path().clone();
//        filepath.push(Path::new("file.log"));

//        let mut file = File::create(&filepath).unwrap();

//        let mut watcher = Watcher::new();
//        watcher.watch(tempdir.path()).unwrap();

//        loop {
//        match watcher.rx.recv() {
//            Modify(path) => {
//                assert_eq!(b"file.log", Path::new(path.as_slice()).filename().unwrap());
//            }
//            event @ _ => { debug!("Expected Modify event, actual: {}", event) }
//        }
//        timer::sleep(Duration::milliseconds(10000));
//        debug!("After sleep");
//        file.write(b"some bytes!\n").unwrap();
//        file.flush().unwrap();
//        file.fsync().unwrap();
//        }
//    }
}
