//! Compress and decompress Zstd streams.
//!
//! This module provide a `Read`/`Write` interface to zstd streams of arbitrary length.
//!
//! They are compatible with the `zstd` command-line tool.

use std::io;

mod encoder;
mod decoder;

pub use self::decoder::Decoder;
pub use self::encoder::{AutoFinishEncoder, Encoder};

/// Decompress from the given source as if using a `Decoder`.
///
/// The input data must be in the zstd frame format.
pub fn decode_all<R: io::Read>(source: R) -> io::Result<Vec<u8>> {
    let mut result = Vec::new();
    try!(copy_decode(source, &mut result));
    Ok(result)
}

/// Decompress from the given source as if using a `Decoder`.
///
/// Decompressed data will be appended to `destination`.
pub fn copy_decode<R, W>(source: R, mut destination: W) -> io::Result<()>
    where R: io::Read,
          W: io::Write
{
    let mut decoder = try!(Decoder::new(source));
    try!(io::copy(&mut decoder, &mut destination));
    Ok(())
}

/// Compress all data from the given source as if using an `Encoder`.
///
/// Result will be in the zstd frame format.
pub fn encode_all<R: io::Read>(source: R, level: i32) -> io::Result<Vec<u8>> {
    let mut result = Vec::<u8>::new();
    try!(copy_encode(source, &mut result, level));
    Ok(result)
}

/// Compress all data from the given source as if using an `Encoder`.
///
/// Compressed data will be appended to `destination`.
pub fn copy_encode<R, W>(mut source: R, destination: W, level: i32)
                         -> io::Result<()>
    where R: io::Read,
          W: io::Write
{
    let mut encoder = try!(Encoder::new(destination, level));
    try!(io::copy(&mut source, &mut encoder));
    try!(encoder.finish());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{Decoder, Encoder};
    use super::{copy_encode, decode_all, encode_all};
    use std::cmp;
    use std::io;

    #[test]
    fn test_end_of_frame() {
        use std::io::{Read, Write};

        let mut enc = Encoder::new(Vec::new(), 1).unwrap();
        enc.write_all(b"foo").unwrap();
        let mut compressed = enc.finish().unwrap();

        // Add footer/whatever to underlying storage.
        compressed.push(0);

        // Drain zstd stream until end-of-frame.
        let mut dec = Decoder::new(&compressed[..]).unwrap().single_frame();
        let mut buf = Vec::new();
        dec.read_to_end(&mut buf).unwrap();
        assert_eq!(&buf, b"foo");
    }

    #[test]
    fn test_concatenated_frames() {

        let mut buffer = Vec::new();
        copy_encode(&b"foo"[..], &mut buffer, 1).unwrap();
        copy_encode(&b"bar"[..], &mut buffer, 2).unwrap();
        copy_encode(&b"baz"[..], &mut buffer, 3).unwrap();

        assert_eq!(&decode_all(&buffer[..]).unwrap(), b"foobarbaz");
    }

    #[test]
    fn test_flush() {
        use std::io::Write;

        let buf = Vec::new();
        let mut z = Encoder::new(buf, 19).unwrap();

        z.write_all(b"hello").unwrap();

        z.flush().unwrap(); // Might corrupt stream
        let buf = z.finish().unwrap();

        let s = decode_all(&buf[..]).unwrap();
        let s = ::std::str::from_utf8(&s).unwrap();
        assert_eq!(s, "hello");
    }

    #[derive(Debug)]
    pub struct WritePartial {
        inner: Vec<u8>,
        accept: Option<usize>,
    }

    impl WritePartial {
        pub fn new() -> Self {
            WritePartial {
                inner: Vec::new(),
                accept: Some(0),
            }
        }

        /// Make the writer only accept a certain number of bytes per write call.
        /// If `bytes` is Some(0), accept an arbitrary number of bytes.
        /// If `bytes` is None, reject with WouldBlock.
        pub fn accept(&mut self, bytes: Option<usize>) {
            self.accept = bytes;
        }

        pub fn into_inner(self) -> Vec<u8> {
            self.inner
        }
    }

    impl io::Write for WritePartial {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            match self.accept {
                None => Err(io::Error::new(io::ErrorKind::WouldBlock, "reject")),
                Some(0) => self.inner.write(buf),
                Some(n) => self.inner.write(&buf[..cmp::min(n, buf.len())]),
            }
        }

        fn flush(&mut self) -> io::Result<()> {
            if self.accept.is_none() {
                return Err(io::Error::new(io::ErrorKind::WouldBlock, "reject"));
            }
            self.inner.flush()
        }
    }

    #[test]
    fn test_try_finish() {
        use std::io::Write;
        let mut z = setup_try_finish();

        z.get_mut().accept(Some(0));

        // flush() should continue to work even though write() doesn't.
        z.flush().unwrap();

        let buf = match z.try_finish() {
            Ok(buf) => buf.into_inner(),
            Err((_z, e)) => panic!("try_finish failed with {:?}", e),
        };

        // Make sure the multiple try_finish calls didn't screw up the internal
        // buffer and continued to produce valid compressed data.
        assert_eq!(&decode_all(&buf[..]).unwrap(), b"hello");
    }

    #[test]
    #[should_panic]
    fn test_write_after_try_finish() {
        use std::io::Write;
        let mut z = setup_try_finish();
        z.write_all(b"hello world").unwrap();
    }

    fn setup_try_finish() -> Encoder<WritePartial> {
        use std::io::Write;

        let buf = WritePartial::new();
        let mut z = Encoder::new(buf, 19).unwrap();

        z.write_all(b"hello").unwrap();

        z.get_mut().accept(None);

        let (z, err) = z.try_finish().unwrap_err();
        assert_eq!(err.kind(), io::ErrorKind::WouldBlock);

        z
    }

    #[test]
    fn test_invalid_frame() {
        use std::io::Read;

        // I really hope this data is invalid...
        let data = &[1u8, 2u8, 3u8, 4u8, 5u8];
        let mut dec = Decoder::new(&data[..]).unwrap();
        assert_eq!(dec.read_to_end(&mut Vec::new()).err().map(|e| e.kind()),
                   Some(io::ErrorKind::Other));
    }

    #[test]
    fn test_incomplete_frame() {
        use std::io::{Read, Write};

        let mut enc = Encoder::new(Vec::new(), 1).unwrap();
        enc.write_all(b"This is a regular string").unwrap();
        let mut compressed = enc.finish().unwrap();

        let half_size = compressed.len() - 2;
        compressed.truncate(half_size);

        let mut dec = Decoder::new(&compressed[..]).unwrap();
        assert_eq!(dec.read_to_end(&mut Vec::new()).err().map(|e| e.kind()),
                   Some(io::ErrorKind::UnexpectedEof));
    }

    #[test]
    fn test_legacy() {
        use std::fs;
        use std::io::Read;

        let mut target = Vec::new();

        // Read the content from that file
        fs::File::open("assets/example.txt")
            .unwrap()
            .read_to_end(&mut target)
            .unwrap();

        for version in &[5, 6, 7, 8] {
            let filename = format!("assets/example.txt.v{}.zst", version);
            let file = fs::File::open(filename).unwrap();
            let mut decoder = Decoder::new(file).unwrap();

            let mut buffer = Vec::new();
            decoder.read_to_end(&mut buffer).unwrap();

            assert!(target == buffer,
                    "Error decompressing legacy version {}",
                    version);
        }
    }

    // Check that compressing+decompressing some data gives back the original
    fn test_full_cycle(input: &[u8], level: i32) {
        ::test_cycle_unwrap(input,
                            |data| encode_all(data, level),
                            |data| decode_all(data));
    }

    #[test]
    fn test_ll_source() {
        // Where could I find some long text?...
        let data = include_bytes!("../../zstd-sys/src/bindings.rs");
        // Test a few compression levels.
        // TODO: check them all?
        for level in 1..5 {
            test_full_cycle(data, level);
        }
    }
}
