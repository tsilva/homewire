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
    // One application write keeps a deadline from splitting the length prefix
    // and payload into separate TLS writes (and avoids Nagle-delayed records).
    let mut message = Vec::with_capacity(body.len() + 11);
    message.resize(3, 0);
    bytes(number, body, &mut message);
    let length = message.len() - 3;
    if length > limit {
        return Err(error("Oversized Android command"));
    }
    let mut prefix = Vec::with_capacity(3);
    put(length as u64, &mut prefix);
    if prefix.len() > 3 {
        return Err(error("Oversized Android command"));
    }
    let start = 3 - prefix.len();
    message[start..3].copy_from_slice(&prefix);
    stream.write_all(&message[start..])?;
    stream.flush()?;
    Ok(())
}
/// One bounded in-flight frame; socket deadlines must not discard partial reads.
#[derive(Default)]
pub(crate) struct Framed {
    prefix: [u8; 3],
    prefix_len: usize,
    frame: Vec<u8>,
    received: usize,
}
impl Framed {
    pub(crate) fn read(&mut self, stream: &mut impl Read) -> Result<Vec<u8>> {
        while self.frame.is_empty() {
            if self.prefix_len == self.prefix.len() {
                return Err(error("Invalid or oversized Android frame"));
            }
            let i = self.prefix_len;
            stream.read_exact(&mut self.prefix[i..i + 1])?;
            self.prefix_len += 1;
            if self.prefix[i] & 128 == 0 {
                let length = varint(&mut &self.prefix[..self.prefix_len])?;
                if length == 0 || length > MAX as u64 {
                    return Err(error("Invalid or oversized Android frame"));
                }
                self.frame.resize(length as usize, 0);
            }
        }
        while self.received < self.frame.len() {
            match stream.read(&mut self.frame[self.received..]) {
                Ok(0) => return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into()),
                Ok(n) => self.received += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
        let bytes = std::mem::take(&mut self.frame);
        self.prefix_len = 0;
        self.received = 0;
        Message::new(&bytes)?;
        Ok(bytes)
    }
}
#[cfg(test)]
pub(crate) fn read(stream: &mut impl Read) -> Result<Vec<u8>> {
    Framed::default().read(stream)
}
#[cfg(test)]
mod tests {
    use super::*;
    struct InterruptedRead<'a> {
        bytes: &'a [u8],
        at: usize,
        interrupt: usize,
        interrupted: bool,
    }
    impl Read for InterruptedRead<'_> {
        fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
            if !self.interrupted && self.at == self.interrupt {
                self.interrupted = true;
                return Err(std::io::ErrorKind::TimedOut.into());
            }
            let end = if self.interrupted {
                self.bytes.len()
            } else {
                self.interrupt
            };
            let n = out.len().min(end - self.at);
            out[..n].copy_from_slice(&self.bytes[self.at..self.at + n]);
            self.at += n;
            Ok(n)
        }
    }
    #[test]
    fn fragmented_frame_resumes_after_deadline_without_losing_prefix_or_body() -> Result<()> {
        let mut body = Vec::new();
        bytes(100, &[1; 128], &mut body);
        let mut wire = Vec::new();
        write(&mut wire, 8, &body)?;
        let first_length = wire.len();
        write(&mut wire, 8, &body)?;
        for interrupt in [1, 3, first_length - 1] {
            let mut input = InterruptedRead {
                bytes: &wire,
                at: 0,
                interrupt,
                interrupted: false,
            };
            let mut reader = Framed::default();
            assert!(reader.read(&mut input).is_err());
            let frame = reader.read(&mut input)?;
            assert_eq!(
                Message::new(&frame)?.nested(8)?.expect("ping").bytes(100)?,
                Some(&[1; 128][..])
            );
            assert_eq!(reader.read(&mut input)?, frame);
        }
        Ok(())
    }
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
