// SPDX-License-Identifier: LGPL-2.1-or-later OR GPL-2.0-or-later OR MPL-2.0
// SPDX-FileCopyrightText: 2026 Gabriel Marcano <gabemarcano@yahoo.com>

use clap::Parser;
use fuser::FileAttr;
use fuser::FileType;
use fuser::Filesystem;
use fuser::MountOption;
use fuser::ReplyAttr;
use fuser::ReplyData;
use fuser::ReplyDirectory;
use fuser::ReplyEntry;
use fuser::ReplyOpen;
use fuser::Request;
use gcn_disk::Disc;
use gcn_disk::Entry;
use gcn_disk::Fst;
use rvz::Rvz;
use std::ffi::OsStr;
use std::fs::File;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::path::PathBuf;
use std::time::Duration;
use std::time::SystemTime;

use gcn_disk;
use libc;
use std::cmp;
use std::io;

#[derive(Parser)]
struct Args {
    path: PathBuf,
    mount: PathBuf,
}

struct GcnFuse<T: Read + Seek> {
    io: T,
    disc: Disc,
}

impl<T: Read + Seek> GcnFuse<T> {
    fn new(io: T, disc: Disc) -> Self {
        GcnFuse { io, disc }
    }
}

fn get_attr(fs: &Fst, index: u32) -> FileAttr {
    let entry = &fs.entries[index as usize];
    let mut attr = FileAttr {
        ino: 0,
        size: 0,
        blocks: 0,
        atime: SystemTime::UNIX_EPOCH,
        mtime: SystemTime::UNIX_EPOCH,
        ctime: SystemTime::UNIX_EPOCH,
        crtime: SystemTime::UNIX_EPOCH,
        kind: FileType::RegularFile,
        perm: 0o444,
        nlink: 1,
        uid: 1,
        gid: 1,
        rdev: 0,
        blksize: 512,
        flags: 0,
    };
    match entry {
        Entry::File(file) => {
            attr.ino = (index + 1).into();
            attr.size = file.size.into();
            attr.blocks = ((file.size / 512) + 1).into();
        }
        Entry::Directory(directory) => {
            attr.nlink = 2;
            attr.ino = (index + 1).into();
            attr.kind = FileType::Directory;
            let subdirs = fs.entries[(directory.index + 1) as usize..directory.end_index as usize]
                .iter()
                .filter(|&e| matches!(e, Entry::Directory(_)))
                .count();
            attr.nlink += subdirs as u32;
            attr.perm = 0o555;
        }
    }
    attr
}

fn get_entry(fs: &Fst, inode: u64) -> &Entry {
    &fs.entries[(inode - 1) as usize]
}

impl<T: Read + Seek> Filesystem for GcnFuse<T> {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        println!("lookup {parent} {}", name.to_str().unwrap());
        let parent_entry = get_entry(&self.disc.filesystem, parent);
        let parent = if let Entry::Directory(parent) = parent_entry {
            parent
        } else {
            reply.error(libc::EIO);
            return;
        };

        let mut index = parent.index + 1;
        while index < parent.end_index {
            let current_inode = index + 1;
            let entry_name = self.disc.filesystem.get_filename(&mut self.io, index);
            if entry_name.is_err() {
                let error = entry_name.unwrap_err();
                match error {
                    gcn_disk::Error::Io(err) => {
                        if let Some(err) = err.raw_os_error() {
                            reply.error(err);
                        }
                    }
                    _ => reply.error(libc::EIO),
                }
                return;
            }
            let entry_name = entry_name.unwrap();
            if entry_name.as_str() == name {
                let attr = get_attr(&self.disc.filesystem, index);
                reply.entry(&Duration::from_secs(1), &attr, 0);
                return;
            }

            let current_entry = get_entry(&self.disc.filesystem, current_inode.into());
            match current_entry {
                Entry::File(_) => {
                    index += 1;
                }
                Entry::Directory(directory) => {
                    index = directory.end_index;
                }
            }
        }
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        let attr = get_attr(&self.disc.filesystem, (ino - 1) as u32);
        reply.attr(&Duration::from_secs(1), &attr);
    }

    fn readdir(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        mut reply: ReplyDirectory,
    ) {
        println!("readdir {ino} {offset}");
        let entry = get_entry(&self.disc.filesystem, ino);
        let entry = match entry {
            Entry::File(_) => {
                reply.error(libc::ENOTDIR);
                return;
            }
            Entry::Directory(dir) => dir,
        };

        let mut entries = vec![
            (ino, FileType::Directory, ".".to_string()),
            (ino, FileType::Directory, "..".to_string()),
        ];

        let mut index = ino as u32;
        while index < entry.end_index {
            let sub_entry = &self.disc.filesystem.entries[index as usize];
            let inode = index + 1;
            let type_ = match sub_entry {
                Entry::File(_) => FileType::RegularFile,
                Entry::Directory(_) => FileType::Directory,
            };
            let name = self.disc.filesystem.get_filename(&mut self.io, index);
            if name.is_err() {
                let error = name.unwrap_err();
                match error {
                    gcn_disk::Error::Io(err) => {
                        if let Some(err) = err.raw_os_error() {
                            reply.error(err);
                        }
                    }
                    _ => reply.error(libc::EIO),
                }
                return;
            }
            entries.push((inode.into(), type_, name.unwrap()));

            let current_entry = get_entry(&self.disc.filesystem, inode.into());
            match current_entry {
                Entry::File(_) => {
                    index += 1;
                }
                Entry::Directory(directory) => {
                    index = directory.end_index.into();
                }
            }
        }

        for (i, entry) in entries.into_iter().enumerate().skip(offset as usize) {
            if reply.add(entry.0, (i + 1) as i64, entry.1, entry.2) {
                break;
            }
        }
        reply.ok();
    }

    fn read(
        &mut self,
        _req: &Request,
        ino: u64,
        _fh: u64,
        offset: i64,
        _size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        println!("read {ino} {offset}");
        let entry = get_entry(&self.disc.filesystem, ino);
        let entry = match entry {
            Entry::File(file) => file,
            Entry::Directory(_) => {
                reply.error(libc::ENOTDIR);
                return;
            }
        };
        let offset = entry.offset;
        let read_size = cmp::min(_size, entry.size);
        let mut buffer = vec![0; read_size as usize];
        self.io.seek(SeekFrom::Start(offset.into())).unwrap();
        self.io.read_exact(&mut buffer).unwrap();
        reply.data(&buffer);
    }
}

fn main() {
    let args = Args::parse();
    let file = File::open(args.path).expect("error opening file");
    let mut file = Rvz::new(file).expect("error opening RVZ");
    let disc = Disc::new(&mut file).unwrap();
    let gcn_fuse = GcnFuse::new(file, disc);

    println!("Hello, world!");
    fuser::mount2(gcn_fuse, args.mount, &[]).unwrap();
}
