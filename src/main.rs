// SPDX-License-Identifier: LGPL-2.1-or-later OR GPL-2.0-or-later OR MPL-2.0
// SPDX-FileCopyrightText: 2026 Gabriel Marcano <gabemarcano@yahoo.com>

use clap::Parser;
use fuser::MountOption;
use gcn_disk::Disc;
use gcnfuse::GcnFuse;
use rvz::Rvz;
use std::fs::File;
use std::path::PathBuf;

#[derive(Parser)]
struct Args {
    path: PathBuf,
    mount: PathBuf,
}

fn main() {
    let args = Args::parse();
    let file = File::open(args.path).expect("error opening file");
    let mut file = Rvz::new(file).expect("error opening RVZ");
    let disc = Disc::new(&mut file).unwrap();
    let gcn_fuse = GcnFuse::new(file, disc);
    let options = vec![MountOption::RO];
    fuser::mount2(gcn_fuse, args.mount, &options).unwrap();
}
