//! Preflight allocations made by file-format's ZIP reader before it consults Read.
//! In particular, its stored `mimetype` entry length is an untrusted u32 allocation size.
//! Unsafe-to-refine ZIPs retain their signature-proven generic container type.

use std::io::{self, Read, Seek, SeekFrom};

const MAX_EOCD_BYTES: u64 = 22 + u16::MAX as u64;
const MAX_MIMETYPE_BYTES: u32 = 256;
const MAX_ENTRIES: u64 = 1024;

pub(super) fn can_refine(reader: &mut (impl Read + Seek)) -> io::Result<bool> {
    let length = reader.seek(SeekFrom::End(0))?;
    if length < 22 {
        return Ok(false);
    }
    let tail_length = usize::try_from(length.min(MAX_EOCD_BYTES)).map_err(io::Error::other)?;
    let tail_start = length - u64::try_from(tail_length).map_err(io::Error::other)?;
    reader.seek(SeekFrom::Start(tail_start))?;
    let mut tail = vec![0; tail_length];
    reader.read_exact(&mut tail)?;
    // Match the library's last-signature search; reject signatures embedded in a comment.
    let Some(index) = tail.windows(4).rposition(|bytes| bytes == b"PK\x05\x06") else {
        return Ok(false);
    };
    let Some(footer) = tail.get(index..) else {
        return Ok(false);
    };
    if footer.len() < 22 || footer.len() != 22 + usize::from(u16::from_le_bytes(field(footer, 20)?))
    {
        return Ok(false);
    }
    let footer_start = tail_start + u64::try_from(index).map_err(io::Error::other)?;
    let (entries, directory) = if footer_start >= 20 {
        reader.seek(SeekFrom::Start(footer_start - 20))?;
        let mut locator = [0; 20];
        reader.read_exact(&mut locator)?;
        if locator.starts_with(b"PK\x06\x07") {
            let offset = u64::from_le_bytes(field(&locator, 8)?);
            if !offset
                .checked_add(56)
                .is_some_and(|end| end <= footer_start - 20)
            {
                return Ok(false);
            }
            reader.seek(SeekFrom::Start(offset))?;
            let mut zip64 = [0; 56];
            reader.read_exact(&mut zip64)?;
            if !zip64.starts_with(b"PK\x06\x06") {
                return Ok(false);
            }
            (
                u64::from_le_bytes(field(&zip64, 32)?),
                u64::from_le_bytes(field(&zip64, 48)?),
            )
        } else {
            classic_directory(footer)?
        }
    } else {
        classic_directory(footer)?
    };
    if directory > footer_start || usize::try_from(entries).is_err() {
        return Ok(false);
    }
    reader.seek(SeekFrom::Start(directory))?;
    for _ in 0..entries.min(MAX_ENTRIES) {
        let position = reader.stream_position()?;
        if !position
            .checked_add(46)
            .is_some_and(|end| end <= footer_start)
        {
            return Ok(false);
        }
        let mut header = [0; 46];
        reader.read_exact(&mut header)?;
        if !header.starts_with(b"PK\x01\x02") {
            return Ok(false);
        }
        let name_length = u64::from(u16::from_le_bytes(field(&header, 28)?));
        let extra_length = u64::from(u16::from_le_bytes(field(&header, 30)?));
        let comment_length = u64::from(u16::from_le_bytes(field(&header, 32)?));
        let Some(next) = position.checked_add(46 + name_length + extra_length + comment_length)
        else {
            return Ok(false);
        };
        if next > footer_start {
            return Ok(false);
        }
        if name_length == 8 {
            let mut name = [0; 8];
            reader.read_exact(&mut name)?;
            let compressed = u32::from_le_bytes(field(&header, 20)?);
            let uncompressed = u32::from_le_bytes(field(&header, 24)?);
            if name == *b"mimetype" && compressed == uncompressed && compressed > MAX_MIMETYPE_BYTES
            {
                return Ok(false);
            }
        }
        reader.seek(SeekFrom::Start(next))?;
    }
    Ok(true)
}

fn classic_directory(footer: &[u8]) -> io::Result<(u64, u64)> {
    Ok((
        u64::from(u16::from_le_bytes(field(footer, 10)?)),
        u64::from(u32::from_le_bytes(field(footer, 16)?)),
    ))
}

fn field<const N: usize>(bytes: &[u8], start: usize) -> io::Result<[u8; N]> {
    start
        .checked_add(N)
        .and_then(|end| bytes.get(start..end))
        .and_then(|value| value.try_into().ok())
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidData,
                "truncated ZIP classification metadata",
            )
        })
}
