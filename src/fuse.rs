// SPDX-License-Identifier: LGPL-2.1-or-later OR GPL-2.0-or-later OR MPL-2.0
// SPDX-FileCopyrightText: 2026 Gabriel Marcano <gabemarcano@yahoo.com>

use fuser::FileAttr;
use fuser::FileType;
use fuser::Filesystem;
use fuser::ReplyAttr;
use fuser::ReplyData;
use fuser::ReplyDirectory;
use fuser::ReplyEntry;
use fuser::Request;
use gcn_disk::Disc;
use gcn_disk::Entry;
use gcn_disk::Fst;
use std::cmp;
use std::ffi::OsStr;
use std::io::Read;
use std::io::Seek;
use std::io::SeekFrom;
use std::time::Duration;
use std::time::SystemTime;

#[derive(Copy, Clone, Debug)]
struct Inode(u64);

#[derive(Copy, Clone, Debug)]
struct Index(u32);

impl From<u32> for Index {
    fn from(val: u32) -> Self {
        Self(val)
    }
}

impl From<Index> for u32 {
    fn from(val: Index) -> Self {
        val.0
    }
}

impl From<u64> for Inode {
    fn from(val: u64) -> Self {
        Self(val)
    }
}

impl From<Inode> for u64 {
    fn from(val: Inode) -> Self {
        val.0
    }
}

impl From<Inode> for Index {
    fn from(val: Inode) -> Self {
        // FST can only have u32 worth of inodes/entries, so this cast is guaranteed to work
        #[allow(clippy::cast_possible_truncation)]
        ((val.0 - 1) as u32).into()
    }
}

impl From<Index> for Inode {
    fn from(val: Index) -> Self {
        // FST can only have u32 worth of inodes/entries, so this cast is guaranteed to work
        #[allow(clippy::cast_possible_truncation)]
        (u64::from(val.0) + 1).into()
    }
}

pub struct GcnFuse<T: Read + Seek> {
    io: T,
    disc: Disc,
}

impl<T: Read + Seek> GcnFuse<T> {
    pub const fn new(io: T, disc: Disc) -> Self {
        Self { io, disc }
    }
}

fn get_attr(fs: &Fst, index: Index) -> FileAttr {
    let entry = &fs.entries[usize::try_from(u32::from(index)).unwrap()];
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
        uid: 0,
        gid: 0,
        rdev: 0,
        blksize: 512,
        flags: 0,
    };
    match entry {
        Entry::File(file) => {
            attr.ino = Inode::from(index).into();
            attr.size = file.size.into();
            attr.blocks = ((file.size / 512) + 1).into();
        }
        Entry::Directory(directory) => {
            attr.nlink = 2;
            attr.ino = Inode::from(index).into();
            attr.kind = FileType::Directory;
            let index_start = usize::try_from(directory.index + 1).unwrap();
            let index_end = usize::try_from(directory.end_index).unwrap();
            #[allow(clippy::cast_possible_truncation)]
            let subdirs = fs.entries[index_start..index_end]
                .iter()
                .filter(|&e| matches!(e, Entry::Directory(_)))
                .count() as u32; // This cast is fine, index_start..index_end is u32
            attr.nlink += subdirs;
            attr.perm = 0o555;
        }
    }
    attr
}

#[must_use]
fn get_entry(fs: &Fst, inode: Inode) -> &Entry {
    &fs.entries[usize::try_from(u32::from(Index::from(inode))).unwrap()]
}

impl<T: Read + Seek> Filesystem for GcnFuse<T> {
    fn lookup(&mut self, _req: &Request, parent: u64, name: &OsStr, reply: ReplyEntry) {
        let parent: Inode = parent.into();
        println!("lookup {} {}", parent.0, name.to_str().unwrap());
        let parent_entry = get_entry(&self.disc.filesystem, parent);
        let Entry::Directory(parent) = parent_entry else {
            println!("Error 1");
            reply.error(libc::EIO);
            return;
        };

        let mut index: Index = (parent.index + 1).into();
        while u32::from(index) < parent.end_index {
            let current_inode: Inode = index.into();
            let entry_name = self
                .disc
                .filesystem
                .get_filename(&mut self.io, index.into());
            if let Err(error) = entry_name {
                println!("Error 2");
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
                println!("found match");
                let attr = get_attr(&self.disc.filesystem, index);
                reply.entry(&Duration::from_secs(1), &attr, 0);
                return;
            }

            let current_entry = get_entry(&self.disc.filesystem, current_inode);
            match current_entry {
                Entry::File(_) => {
                    index = (u32::from(index) + 1).into();
                }
                Entry::Directory(directory) => {
                    index = directory.end_index.into();
                }
            }
        }
        reply.error(libc::ENOENT);
    }

    fn getattr(&mut self, _req: &Request, ino: u64, _fh: Option<u64>, reply: ReplyAttr) {
        let ino: Inode = ino.into();
        let attr = get_attr(&self.disc.filesystem, ino.into());
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
        let ino: Inode = ino.into();
        println!("readdir {} {offset}", ino.0);
        let entry = get_entry(&self.disc.filesystem, ino);
        let entry = match entry {
            Entry::File(_) => {
                reply.error(libc::ENOTDIR);
                return;
            }
            Entry::Directory(dir) => dir,
        };

        let parent_index: Index = entry.parent_index.into();

        let mut entries = vec![
            (ino, FileType::Directory, ".".to_string()),
            (parent_index.into(), FileType::Directory, "..".to_string()),
        ];

        let mut index: Index = (u32::from(Index::from(ino)) + 1).into();
        while u32::from(index) < entry.end_index {
            let sub_entry =
                &self.disc.filesystem.entries[usize::try_from(u32::from(index)).unwrap()];
            let inode: Inode = index.into();
            let type_ = match sub_entry {
                Entry::File(_) => FileType::RegularFile,
                Entry::Directory(_) => FileType::Directory,
            };
            let name = self
                .disc
                .filesystem
                .get_filename(&mut self.io, index.into());
            if let Err(error) = name {
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
            entries.push((inode, type_, name.unwrap()));

            let current_entry = get_entry(&self.disc.filesystem, inode);
            match current_entry {
                Entry::File(_) => {
                    index = (u32::from(index) + 1).into();
                }
                Entry::Directory(directory) => {
                    index = directory.end_index.into();
                }
            }
        }

        let offset = usize::try_from(offset).unwrap();
        for (i, entry) in entries.into_iter().enumerate().skip(offset) {
            // There will always be u32 max entries, so there's no i64 possible wrapping
            #[allow(clippy::cast_possible_wrap)]
            if reply.add(entry.0.into(), (i + 1) as i64, entry.1, entry.2) {
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
        size: u32,
        _flags: i32,
        _lock: Option<u64>,
        reply: ReplyData,
    ) {
        let ino: Inode = ino.into();
        println!("read {} {offset}", ino.0);
        let entry = get_entry(&self.disc.filesystem, ino);
        let entry = match entry {
            Entry::File(file) => file,
            Entry::Directory(_) => {
                reply.error(libc::ENOTDIR);
                return;
            }
        };
        // Negative offsets can't happen here
        // Additionally, in this filesystem files max size is capped at u32
        #[allow(clippy::cast_sign_loss, clippy::cast_possible_truncation)]
        let offset = offset as u32;
        let read_size = cmp::min(size, entry.size - offset);
        let mut buffer = vec![0; read_size as usize];
        println!("read {} 0x{:08X} 0x{offset:08X}", ino.0, entry.offset);
        self.io
            .seek(SeekFrom::Start(
                u64::from(entry.offset).strict_add(offset.into()),
            ))
            .unwrap();
        self.io.read_exact(&mut buffer).unwrap();
        reply.data(&buffer);
    }
}
