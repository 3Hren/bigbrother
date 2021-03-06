use std::collections::{HashSet, HashMap};
use std::comm::Select;
use std::io::fs;
use std::io::{FileStat};
use std::time::Duration;
use std::io::fs::PathExtensions;

use super::native::Unknown;
use super::native::backend::Backend;

pub enum Event {
    Create(Path),
    Remove(Path),
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

type FileStatMap = HashMap<u64, WatchedFileStat>;

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
        let mut backend = Backend::new(period);

        let mut paths = HashSet::new();
        let mut prev: FileStatMap = HashMap::new();
        loop {
            debug!("Event loop tick ...");

            let mut reg = false;
            {
                let sel = Select::new();
                let mut chrx = sel.handle(&crx);
                let mut ehrx = sel.handle(&backend.watcher.rx);
                unsafe {
                    chrx.add();
                    ehrx.add();
                };
                let ret = sel.wait();

                if ret == chrx.id() {
                    match crx.recv() {
                        Update(newpaths) => {
                            debug!("Received watcher update event");
                            paths = newpaths;
                            prev = Watcher::rescan(&paths);
                            reg = true;
                        }
                        Exit => {
                            debug!("Received watcher exit event - performing graceful shutdown");
                            break;
                        }
                    }
                } else if ret == ehrx.id() {
                    match backend.transform(backend.watcher.rx.recv()) {
//                        Changed(path, flags) => {
//                            Если path - файл
//                              Если created
//                                -> Create, если файл существует.
//                                Добавить в curr.
//                              Если removed
//                                -> Remove, если файл не существует.
//                                Удалить из curr.
//                              Если renamed & !exists, то это событие *Rename(old).
//                                Получить inode по имени из prev.
//                                Найти в curr, сделав полный рескан.
//                                  -> Rename(old, new) если удалось, или
//                                  -> Remove(old) если не нашлось
//                              Если renamed &  exists, то это событие *Rename(new).
//                                Получить inode из stat.
//                                Найти в prev.
//                                  -> Ignore, если нашлось (мы уже испустили событие)
//                                  -> Create(new), если не нашлось
//                            Если path - директория, то пересканировать только эту директорию.
//                        }
                        Unknown => {
                            let curr = Watcher::rescan(&paths);
                            Watcher::created(&prev, &curr, &tx);
                            Watcher::removed(&prev, &curr, &tx);
                            Watcher::modified(&prev, &curr, &tx);
                            prev = curr;
                        }
                    }
                }
            }

            if reg {
                backend.register(paths.clone());
            }
        }
    }

    //TODO: What to do if something has failed?
    fn rescan(paths: &HashSet<Path>) -> FileStatMap {
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

    fn created(prev: &FileStatMap, curr: &FileStatMap, tx: &Sender<Event>) {
        for (inode, stat) in curr.iter() {
            if !prev.contains_key(inode) {
                tx.send(Create(stat.path.clone()));
            }
        }
    }

    fn removed(prev: &FileStatMap, curr: &FileStatMap, tx: &Sender<Event>) {
        for (inode, stat) in prev.iter() {
            if !curr.contains_key(inode) {
                tx.send(Remove(stat.path.clone()));
            }
        }
    }

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
        self.ctx.send(Exit);
    }
}

#[cfg(test)]
mod test {
    extern crate test;

    use std::str;
    use std::collections::HashSet;
    use std::io::{File, TempDir};
    use std::io::fs;
    use std::io::fs::PathExtensions;
    use std::io::timer;
    use std::time::Duration;

    use super::Watcher;
    use super::{Create, Modify, Rename, Remove};

    #[test]
    fn create_single_file() {
        let tmp = TempDir::new("create-single").unwrap();
        let path = tmp.path().join("file.log");

        let mut watcher = Watcher::new();
        watcher.watch(tmp.path().clone());

        timer::sleep(Duration::milliseconds(50));

        File::create(&path).unwrap();

        match watcher.rx.recv() {
            Create(p) => {
                assert_eq!(b"file.log", p.filename().unwrap())
            }
            _ => { panic!("Expected `Create` event") }
        }
    }

    #[test]
    fn remove_single_file() {
        let tmp = TempDir::new("remove-single").unwrap();
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
            _  => { panic!("Expected `Remove` event") }
        }
    }

    #[test]
    fn rename_single_file() {
        let tmp = TempDir::new("rename-single").unwrap();
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
            _ => { panic!("Expected `Rename` event") }
        }
    }

    #[test]
    fn modify_single_file() {
        let tmp = TempDir::new("modify-single").unwrap();
        let path = tmp.path().join("file.log");

        let mut file = File::create(&path).unwrap();

        timer::sleep(Duration::milliseconds(50));

        let mut watcher = Watcher::new();
        watcher.watch(tmp.path().clone());

        // Timeout is need to be at least one second, because at least ext3 filesystem has seconds resolution.
        timer::sleep(Duration::milliseconds(1000));
        debug!("Modifying file ...");

        file.write(b"some bytes!\n").unwrap();
        file.flush().unwrap();
        file.fsync().unwrap();

        match watcher.rx.recv() {
            Modify(p) => {
                assert_eq!(b"file.log", p.filename().unwrap());
            }
            _ => { debug!("Expected `Modify` event") }
        }
    }

    #[test]
    fn create_two_files_in_different_directories() {
        let tmp1 = TempDir::new("create-1").unwrap();
        let tmp2 = TempDir::new("create-2").unwrap();
        let path1 = tmp1.path().join("file1.log");
        let path2 = tmp2.path().join("file2.log");

        let mut watcher = Watcher::new();
        watcher.watch(tmp1.path().clone());
        watcher.watch(tmp2.path().clone());

        timer::sleep(Duration::milliseconds(50));

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
        let tmp1 = TempDir::new("rename-iswatched").unwrap();
        let tmp2 = TempDir::new("rename-nowatched").unwrap();
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
        // Event should be considered as file renaming.
        let tmp1 = TempDir::new("rename-watched1").unwrap();
        let tmp2 = TempDir::new("rename-watched2").unwrap();
        let oldpath = tmp1.path().join("file-old.log");
        let newpath = tmp2.path().join("file-new.log");

        File::create(&oldpath).unwrap();

        timer::sleep(Duration::milliseconds(50));

        let mut watcher = Watcher::new();
        watcher.watch(tmp1.path().clone());
        watcher.watch(tmp2.path().clone());

        timer::sleep(Duration::milliseconds(50));

        fs::rename(&oldpath, &newpath).unwrap();

        match watcher.rx.recv() {
            Rename(old, new) => {
                assert_eq!(b"file-old.log", old.filename().unwrap());
                assert_eq!(b"file-new.log", new.filename().unwrap());
            }
            _ => { panic!("Expected `Rename` event") }
        }
    }
}
