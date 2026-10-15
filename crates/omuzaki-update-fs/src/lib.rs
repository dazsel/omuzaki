//! The real file and clock seams for the updater (SE-08): [`disk::StdDisk`] and [`clock::SystemWallClock`].
//!
//! `omuzaki-update-api` holds no I/O, so these live here, where both `omuzaki-update` and the launcher can use them.
//! Neither type holds state; build one wherever a `Disk` or a `WallClock` is wanted.
#![deny(missing_docs)]

pub mod clock;
pub mod disk;
