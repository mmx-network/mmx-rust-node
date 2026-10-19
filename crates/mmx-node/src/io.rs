//! Grow public receive buffers only after bytes arrive, as in VNX safe_read.
use std::io;

pub(crate) fn safe_read(
    output: &mut Vec<u8>,
    length: usize,
    mut read: impl FnMut(&mut [u8]) -> io::Result<()>,
) -> io::Result<()> {
    let mut chunk = [0; 16 * 1024];
    let mut left = length;
    while left != 0 {
        let count = left.min(chunk.len());
        read(&mut chunk[..count])?;
        output.try_reserve(count).map_err(io::Error::other)?;
        output.extend_from_slice(&chunk[..count]);
        left -= count;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Cursor, Read};

    #[test]
    fn truncated_payloads_do_not_reserve_the_declared_length() {
        for available in [0, 10, 16 * 1024] {
            let mut input = Cursor::new(vec![7; available]);
            let mut output = vec![0; 8];
            let error = safe_read(&mut output, 64 * 1024 * 1024, |part| {
                assert!(part.len() <= 16 * 1024);
                input.read_exact(part)
            })
            .unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
            assert!(output.capacity() <= 32 * 1024);
            assert_eq!(output.len(), 8 + available / (16 * 1024) * (16 * 1024));
        }
    }

    #[test]
    fn complete_multichunk_payload_preserves_the_prefix_and_exact_bytes() {
        let bytes: Vec<_> = (0..(3 * 16 * 1024 + 17)).map(|i| i as u8).collect();
        let mut input = Cursor::new(&bytes);
        let mut output = vec![42; 8];
        safe_read(&mut output, bytes.len(), |part| input.read_exact(part)).unwrap();
        assert_eq!(&output[..8], &[42; 8]);
        assert_eq!(&output[8..], &bytes);
    }
}
