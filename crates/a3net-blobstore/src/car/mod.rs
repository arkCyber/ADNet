//! CAR (Content Addressable aRchives) file format support.

pub mod dag;
pub mod streaming;

pub use dag::{DagBlock, DagCarWriter, DagWalker, export_dag, DagBlockStoreExt, MissingRootError};
pub use streaming::{CarReader, CarWriter, BatchedCarWriter, WriteCarExt};

use a3net_types::ContentHash;
use std::io;

#[derive(Debug, thiserror::Error)]
pub enum CarError {
    #[error("I/O error: {0}")]
    Io(#[from] io::Error),
    #[error("invalid CAR format")]
    InvalidFormat,
    #[error("invalid CID hash: {0}")]
    InvalidHash(String),
    #[error("missing root block: {0}")]
    MissingRoot(#[from] ContentHash),
}

#[derive(Debug, Clone)]
pub struct CarBlock {
    pub cid: ContentHash,
    pub data: Vec<u8>,
}

impl CarBlock {
    pub fn new(cid: ContentHash, data: Vec<u8>) -> Self {
        Self { cid, data }
    }
}

#[derive(Debug, Clone)]
pub struct CarHeader {
    pub roots: Vec<ContentHash>,
    pub version: u64,
}

impl CarHeader {
    pub fn new(roots: Vec<ContentHash>) -> Self {
        Self { roots, version: 1 }
    }
}

pub fn read_car<R: io::Read>(reader: R) -> Result<(CarHeader, Vec<CarBlock>), CarError> {
    streaming::read_car(reader)
}

pub fn write_car<W: io::Write>(writer: W, header: &CarHeader, blocks: &[CarBlock]) -> Result<(), CarError> {
    streaming::write_car(writer, header, blocks)
}
