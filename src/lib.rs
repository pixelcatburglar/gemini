pub mod chardata;
pub mod db;
pub mod destiny;
pub mod game;
pub mod macho;
pub mod market;
mod objects;
pub mod pickle;
pub mod reference;
mod rpc;
pub mod state;
pub mod value;

use std::io::{self, Read, Write};

pub const MAX_FRAME: usize = 16 * 1024 * 1024;

/// Read one frame, tolerating fragmented TCP reads. EOF is an error, including
/// EOF between the length and payload; callers close that connection.
pub fn read_frame(reader: &mut impl Read, limit: usize) -> io::Result<Vec<u8>> {
    let mut header = [0; 4];
    reader.read_exact(&mut header)?;
    let length = u32::from_le_bytes(header) as usize;
    if length > limit {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds limit",
        ));
    }
    let mut payload = vec![0; length];
    reader.read_exact(&mut payload)?;
    Ok(payload)
}

pub fn write_frame(writer: &mut impl Write, payload: &[u8], limit: usize) -> io::Result<()> {
    if payload.len() > limit || payload.len() > u32::MAX as usize {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "frame exceeds limit",
        ));
    }
    writer.write_all(&(payload.len() as u32).to_le_bytes())?;
    writer.write_all(payload)?;
    writer.flush()
}
