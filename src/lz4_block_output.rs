use crate::common::{Checksum, ErrorInternal, Result};
use crate::compression::{Compression, Context};
use crate::lz4_block_header::{CompressionLevel, CompressionMethod, Lz4BlockHeader};

use std::cmp::min;
use std::io::Write;
use std::result::Result as StdResult;

/// Wrapper around a [`Write`] object to compress data.
///
/// The data written to [`Lz4BlockOutput`] is be compressed and then written to the wrapped [`Write`].
///
/// # Example
///
/// ```rust
/// use lz4jb::Lz4BlockOutput;
/// use std::io::Write;
///
/// fn main() -> std::io::Result<()> {
///     let mut output = Vec::new(); // Vec<u8> implements the Write trait
///     Lz4BlockOutput::new(&mut output).write_all("...".as_bytes())?;
///     println!("{:?}", output);
///     Ok(())
/// }
/// ```
pub type Lz4BlockOutput<R> = Lz4BlockOutputBase<R, Context>;

impl<W: Write> Lz4BlockOutput<W> {
    /// Create a new [`Lz4BlockOutput`] with the default parameters.
    ///
    /// See [`Self::with_context()`]
    #[inline]
    pub fn new(w: W) -> Self {
        Self::with_context(w, Context::default(), Self::default_block_size()).unwrap()
    }
}

/// Wrapper around a [`Write`] object to compress data.
///
/// Use this struct only if you want to provide your own Compression implementation. Otherwise use the alias [`Lz4BlockOutput`].
#[derive(Debug)]
pub struct Lz4BlockOutputBase<W: Write + Sized, C: Compression> {
    writer: W,
    compression: C,
    compression_level: CompressionLevel,
    write_ptr: usize,
    decompressed_buf: Vec<u8>,
    compressed_buf: Vec<u8>,
    checksum: Checksum,
}

impl<W: Write, C: Compression> Lz4BlockOutputBase<W, C> {
    /// Get the default block size: 65536B.
    #[inline]
    pub fn default_block_size() -> usize {
        1 << 16
    }

    /// Create a new [`Lz4BlockOutputBase`] with the default checksum implementation which is compatible with the Java's default implementation, including the missing 4 bits bug.
    ///
    /// See [`Self::with_checksum()`]
    #[inline]
    pub fn with_context(w: W, c: C, block_size: usize) -> std::io::Result<Self> {
        Self::with_checksum(w, c, block_size, Lz4BlockHeader::default_checksum)
    }

    /// Create a new [`Lz4BlockOutputBase`].
    ///
    /// The `block_size` must be between `64` and `33554432` bytes.
    /// The checksum must return a [`u32`].
    ///
    /// # Errors
    ///
    /// It will return an error if the `block_size` is out of range
    pub fn with_checksum(
        w: W,
        c: C,
        block_size: usize,
        checksum: fn(&[u8]) -> u32,
    ) -> std::io::Result<Self> {
        let compression_level = CompressionLevel::from_block_size(block_size)?;
        let compressed_buf_len = c
            .get_maximum_compressed_buffer_len(compression_level.get_max_decompressed_buffer_len());
        Ok(Self {
            writer: w,
            compression: c,
            compression_level,
            write_ptr: 0,
            compressed_buf: vec![0u8; compressed_buf_len],
            decompressed_buf: vec![0u8; block_size],
            checksum: Checksum::new(checksum),
        })
    }

    fn copy_to_buf(&mut self, buf: &[u8]) -> StdResult<usize, ErrorInternal> {
        let buf_into = &mut self.decompressed_buf[self.write_ptr..];
        if buf.len() > buf_into.len() {
            return ErrorInternal::new_error(
                "Attempt to write a bigger buffer than the available one",
            );
        }

        buf_into[..buf.len()].copy_from_slice(buf);
        self.write_ptr += buf.len();

        Ok(buf.len())
    }

    fn remaining_buf_len(&self) -> StdResult<usize, ErrorInternal> {
        if self.write_ptr <= self.decompressed_buf.len() {
            Ok(self.decompressed_buf.len() - self.write_ptr)
        } else {
            ErrorInternal::new_error("Could not determine the buffer size")
        }
    }

    fn write(&mut self, buf: &[u8]) -> Result<usize> {
        if self.write_ptr == self.decompressed_buf.len() {
            self.flush()?;
        }
        let size_to_copy = min(buf.len(), self.remaining_buf_len()?);
        Ok(self.copy_to_buf(&buf[..size_to_copy])?)
    }

    fn flush(&mut self) -> Result<()> {
        if self.write_ptr > 0 {
            let decompressed_buf = &self.decompressed_buf[..self.write_ptr];
            let compressed_buf = match self
                .compression
                .compress(decompressed_buf, self.compressed_buf.as_mut())
            {
                Ok(s) => &self.compressed_buf[..s],
                Err(err) => return Err(err.into()),
            };
            let (compression_method, buf_to_write) =
                if compressed_buf.len() < decompressed_buf.len() {
                    (CompressionMethod::Lz4, compressed_buf)
                } else {
                    (CompressionMethod::Raw, decompressed_buf)
                };
            Lz4BlockHeader {
                compression_method,
                compression_level: self.compression_level,
                compressed_len: buf_to_write.len() as u32,
                decompressed_len: decompressed_buf.len() as u32,
                checksum: self.checksum.run(decompressed_buf),
            }
            .write(&mut self.writer)?;
            self.writer.write_all(buf_to_write)?;
        }
        self.write_ptr = 0;
        self.writer.flush()?;
        Ok(())
    }
}

impl<W: Write, C: Compression> Write for Lz4BlockOutputBase<W, C> {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        Ok(Self::write(self, buf)?)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(Self::flush(self)?)
    }
}

impl<W: Write, C: Compression> Drop for Lz4BlockOutputBase<W, C> {
    fn drop(&mut self) {
        let _ = self.flush();
    }
}

#[cfg(test)]
mod test_lz4_block_output {
    use super::{CompressionLevel, Context, Lz4BlockOutput};
    use crate::lz4_block_header::data::VALID_DATA;

    use std::io::Write;

    #[test]
    fn valid_default_block_size() {
        let default_block_size = Lz4BlockOutput::<Vec<u8>>::default_block_size();
        assert_eq!(
            CompressionLevel::from_block_size(default_block_size).is_ok(),
            true
        );
    }

    #[test]
    fn write_empty() {
        let mut out = Vec::<u8>::new();
        Lz4BlockOutput::with_context(&mut out, Context::default(), 128).unwrap();
        assert_eq!(out, []);
    }

    #[test]
    fn write_basic() {
        let mut out = Vec::<u8>::new();
        Lz4BlockOutput::with_context(&mut out, Context::default(), 128)
            .unwrap()
            .write_all("...".as_bytes())
            .unwrap();
        assert_eq!(out, VALID_DATA);
    }

    #[test]
    fn write_several_small_blocks() {
        let mut out = Vec::<u8>::new();
        let buf = ['.' as u8; 1024];
        let loops = 1024;
        {
            let mut writer =
                Lz4BlockOutput::with_context(&mut out, Context::default(), buf.len() * loops)
                    .unwrap();
            for _ in 0..loops {
                writer.write_all(&buf).unwrap();
            }
        }
        let needle = &VALID_DATA[..8];
        // count number of blocks
        assert_eq!(
            out.windows(needle.len())
                .filter(|window| *window == needle)
                .count(),
            1
        );
    }

    #[test]
    fn write_several_big_blocks() {
        let mut out = Vec::<u8>::new();
        let buf = ['.' as u8; 128];
        let loops = 1234;
        {
            let mut writer =
                Lz4BlockOutput::with_context(&mut out, Context::default(), buf.len()).unwrap();
            for _ in 0..loops {
                writer.write_all(&buf).unwrap();
            }
        }
        let needle = &VALID_DATA[..8];
        // count number of blocks
        assert_eq!(
            out.windows(needle.len())
                .filter(|window| *window == needle)
                .count(),
            loops
        );
    }

    #[test]
    fn flush_basic() {
        let mut out = Vec::<u8>::new();
        {
            let mut writer =
                Lz4BlockOutput::with_context(&mut out, Context::default(), 128).unwrap();
            writer.write_all("...".as_bytes()).unwrap();
            writer.flush().unwrap();
            writer.write_all("...".as_bytes()).unwrap();
        }
        let mut expected = VALID_DATA.to_vec();
        expected.extend_from_slice(&VALID_DATA[..]);
        assert_eq!(out, expected);
    }
}
