//! Bounded borrowed protobuf fields for the small Android remote v2 subset.
//! No generated descriptors, full schema runtime, or retained message tree.
use crate::{Result, error};
use std::io::{Read, Write};
pub(crate) const MAX: usize = 8192;
fn varint(bytes: &mut &[u8]) -> Result<u64> {
    let mut value = 0;
    for shift in (0..70).step_by(7) {
        let (&byte, rest) = bytes
            .split_first()
            .ok_or_else(|| error("Truncated protobuf varint"))?;
        *bytes = rest;
        if shift == 63 && byte > 1 {
            return Err(error("Overflowing protobuf varint"));
        }
        value |= ((byte & 127) as u64) << shift;
        if byte & 128 == 0 {
            return Ok(value);
        }
    }
    Err(error("Invalid protobuf varint"))
}
fn put(value: u64, out: &mut Vec<u8>) {
    let mut value = value;
    while value >= 128 {
        out.push((value as u8) | 128);
        value >>= 7;
    }
    out.push(value as u8);
}
#[derive(Clone, Copy)]
enum Field<'a> {
    Int(u64),
    Bytes(&'a [u8]),
    Other,
}
fn take<'a>(bytes: &mut &'a [u8], len: usize) -> Result<&'a [u8]> {
    if len > bytes.len() {
        return Err(error("Truncated protobuf field"));
    }
    let (value, rest) = bytes.split_at(len);
    *bytes = rest;
    Ok(value)
}
fn next<'a>(bytes: &mut &'a [u8]) -> Result<(u32, Field<'a>)> {
    let tag = varint(bytes)?;
    let number = tag >> 3;
    if number == 0 || number > 0x1fff_ffff {
        return Err(error("Invalid protobuf field number"));
    }
    let field = match tag & 7 {
        0 => Field::Int(varint(bytes)?),
        1 => {
            take(bytes, 8)?;
            Field::Other
        }
        2 => {
            let len = varint(bytes)?;
            if len > MAX as u64 {
                return Err(error("Oversized protobuf field"));
            }
            Field::Bytes(take(bytes, len as usize)?)
        }
        5 => {
            take(bytes, 4)?;
            Field::Other
        }
        _ => return Err(error("Unsupported protobuf wire type")),
    };
    Ok((number as u32, field))
}
#[derive(Clone, Copy)]
pub(crate) struct Message<'a>(&'a [u8]);
impl<'a> Message<'a> {
    pub(crate) fn new(bytes: &'a [u8]) -> Result<Self> {
        if bytes.len() > MAX {
            return Err(error("Oversized protobuf message"));
        }
        let mut rest = bytes;
        let mut count = 0;
        while !rest.is_empty() {
            next(&mut rest)?;
            count += 1;
            if count > 128 {
                return Err(error("Too many protobuf fields"));
            }
        }
        Ok(Self(bytes))
    }
    fn field(self, number: u32) -> Result<Option<Field<'a>>> {
        let mut bytes = self.0;
        let mut found = None;
        while !bytes.is_empty() {
            let (n, f) = next(&mut bytes)?;
            if n == number {
                if found.is_some() {
                    return Err(error("Duplicate Android remote field"));
                }
                found = Some(f);
            }
        }
        Ok(found)
    }
    pub(crate) fn int(self, number: u32) -> Result<Option<u64>> {
        match self.field(number)? {
            Some(Field::Int(n)) => Ok(Some(n)),
            None => Ok(None),
            _ => Err(error("Invalid Android integer field")),
        }
    }
    pub(crate) fn bytes(self, number: u32) -> Result<Option<&'a [u8]>> {
        match self.field(number)? {
            Some(Field::Bytes(b)) => Ok(Some(b)),
            None => Ok(None),
            _ => Err(error("Invalid Android message field")),
        }
    }
    pub(crate) fn nested(self, number: u32) -> Result<Option<Self>> {
        self.bytes(number)?.map(Self::new).transpose()
    }
    pub(crate) fn text(self, number: u32) -> Result<Option<&'a str>> {
        self.bytes(number)?
            .map(|b| std::str::from_utf8(b).map_err(|_| error("Invalid Android text field")))
            .transpose()
    }
}
pub(crate) fn integer(number: u32, value: u64, out: &mut Vec<u8>) {
    put((number as u64) << 3, out);
    put(value, out)
}
pub(crate) fn bytes(number: u32, value: &[u8], out: &mut Vec<u8>) {
    put(((number as u64) << 3) | 2, out);
    put(value.len() as u64, out);
    out.extend_from_slice(value)
}
pub(crate) fn write(stream: &mut impl Write, number: u32, body: &[u8]) -> Result<()> {
    write_limit(stream, number, body, MAX)
}
/// Voice has an 8 KiB minimum sample chunk plus bounded protobuf overhead.
/// All normal commands and incoming messages retain the smaller MAX bound.
pub(crate) fn write_limit(
    stream: &mut impl Write,
    number: u32,
    body: &[u8],
    limit: usize,
) -> Result<()> {
    let mut message = Vec::with_capacity(body.len() + 8);
    bytes(number, body, &mut message);
    if message.len() > limit {
        return Err(error("Oversized Android command"));
    }
    let mut prefix = Vec::with_capacity(3);
    put(message.len() as u64, &mut prefix);
    stream.write_all(&prefix)?;
    stream.write_all(&message)?;
    stream.flush()?;
    Ok(())
}
pub(crate) fn read(stream: &mut impl Read) -> Result<Vec<u8>> {
    let mut prefix = [0; 3];
    let mut len = None;
    for i in 0..3 {
        stream.read_exact(&mut prefix[i..i + 1])?;
        if prefix[i] & 128 == 0 {
            len = Some(varint(&mut &prefix[..=i])?);
            break;
        }
    }
    let len = len
        .filter(|n| *n > 0 && *n <= MAX as u64)
        .ok_or_else(|| error("Invalid or oversized Android frame"))?;
    let mut bytes = vec![0; len as usize];
    stream.read_exact(&mut bytes)?;
    Message::new(&bytes)?;
    Ok(bytes)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_bad_frames_and_handles_unknown_fields() -> Result<()> {
        for b in [&[0x80, 0x80, 0x80][..], &[0x81, 0x40][..], &[0][..]] {
            assert!(read(&mut &b[..]).is_err());
        }
        for b in [
            &[0][..],
            &[0x0a, 0x08, 0][..],
            &[
                0x08, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0xff, 0x02,
            ][..],
        ] {
            assert!(Message::new(b).is_err());
        }
        let mut b = Vec::new();
        integer(100, 1, &mut b);
        bytes(40, &[8, 1], &mut b);
        let m = Message::new(&b)?;
        assert_eq!(m.nested(40)?.expect("start").int(1)?, Some(1));
        Ok(())
    }
}
