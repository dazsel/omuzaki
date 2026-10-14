//! Platforms with no file system ops can open files on (WebAssembly in a browser): every open is `Unsupported`.

use std::fs::File;

use super::{DirEntryInfo, Failure, How};
use crate::file_access::form::Lexical;

/// No folder can ever be opened here, so there is no value of this type.
#[derive(Debug)]
pub(in crate::file_access) enum Dir {}

impl Dir {
    pub(super) fn anchor(_place: &Lexical, _depth: usize) -> Result<Self, Failure> {
        Err(Failure::Unsupported)
    }

    pub(super) fn descend(self, _names: &[String], _create: bool) -> Result<Self, Failure> {
        match self {}
    }

    pub(super) fn file(&self, _name: &str, _how: How) -> Result<File, Failure> {
        match *self {}
    }

    pub(super) fn list(&self) -> Result<Vec<DirEntryInfo>, Failure> {
        match *self {}
    }

    pub(super) fn temp(
        &self,
        _replacing: &str,
        _names: &mut dyn Iterator<Item = Result<String, Failure>>,
    ) -> Result<Temp, Failure> {
        match *self {}
    }
}

/// A replacement's new file; never made here.
#[derive(Debug)]
pub(in crate::file_access) struct Temp {
    pub(super) file: File,
}

pub(super) fn commit(
    dir: &Dir,
    _temp: &mut Temp,
    _name: &str,
    _names: &mut dyn Iterator<Item = Result<String, Failure>>,
) -> Result<(), Failure> {
    match *dir {}
}

pub(super) fn discard(dir: &Dir, _temp: &Temp) {
    match *dir {}
}
