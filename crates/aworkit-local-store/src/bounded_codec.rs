//! A tiny bounded byte codec for optional local evidence.
//!
//! Keeping the codec in-tree avoids adding a native or transitive compression
//! dependency to the trusted storage boundary. The format is a PackBits-style
//! sequence of literal and repeated-byte blocks with an explicit magic header.

use thiserror::Error;

const MAGIC: &[u8; 8] = b"AWKRLE1\0";
const MAX_BLOCK: usize = 128;

pub(crate) fn compress(input: &[u8]) -> Vec<u8> {
    let mut encoded = Vec::with_capacity(input.len().saturating_add(MAGIC.len()));
    encoded.extend_from_slice(MAGIC);
    let mut cursor = 0;
    while cursor < input.len() {
        let run = repeated(input, cursor);
        if run >= 3 {
            let length = run.min(MAX_BLOCK);
            encoded.push(0x80 | u8::try_from(length - 1).expect("bounded run"));
            encoded.push(input[cursor]);
            cursor += length;
            continue;
        }

        let literal_start = cursor;
        cursor += run.max(1);
        while cursor < input.len()
            && cursor - literal_start < MAX_BLOCK
            && repeated(input, cursor) < 3
        {
            cursor += repeated(input, cursor)
                .max(1)
                .min(MAX_BLOCK - (cursor - literal_start));
        }
        let length = cursor - literal_start;
        encoded.push(u8::try_from(length - 1).expect("bounded literal"));
        encoded.extend_from_slice(&input[literal_start..cursor]);
    }
    encoded
}

pub(crate) fn decompress(encoded: &[u8], max_output: usize) -> Result<Vec<u8>, CodecError> {
    let Some(mut remaining) = encoded.strip_prefix(MAGIC) else {
        return Err(CodecError::InvalidHeader);
    };
    let mut decoded = Vec::new();
    while let Some((&header, tail)) = remaining.split_first() {
        remaining = tail;
        let length = usize::from(header & 0x7f) + 1;
        if decoded.len().saturating_add(length) > max_output {
            return Err(CodecError::OutputLimitExceeded);
        }
        if header & 0x80 == 0 {
            if remaining.len() < length {
                return Err(CodecError::TruncatedBlock);
            }
            decoded.extend_from_slice(&remaining[..length]);
            remaining = &remaining[length..];
        } else {
            let Some((&byte, tail)) = remaining.split_first() else {
                return Err(CodecError::TruncatedBlock);
            };
            decoded.resize(decoded.len() + length, byte);
            remaining = tail;
        }
    }
    Ok(decoded)
}

fn repeated(input: &[u8], cursor: usize) -> usize {
    let byte = input[cursor];
    input[cursor..]
        .iter()
        .take(MAX_BLOCK)
        .take_while(|candidate| **candidate == byte)
        .count()
}

#[derive(Debug, Error, Clone, Copy, Eq, PartialEq)]
pub(crate) enum CodecError {
    #[error("compressed evidence has an invalid header")]
    InvalidHeader,
    #[error("compressed evidence contains a truncated block")]
    TruncatedBlock,
    #[error("compressed evidence exceeds its declared output bound")]
    OutputLimitExceeded,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trips_literals_runs_and_empty_input() {
        for bytes in [
            Vec::new(),
            b"ordinary literal data".to_vec(),
            vec![b'x'; 400],
            b"aaabcccccdefgggggggg".to_vec(),
        ] {
            assert_eq!(
                decompress(&compress(&bytes), bytes.len()).expect("decode"),
                bytes
            );
        }
    }

    #[test]
    fn refuses_expansion_past_the_caller_bound() {
        let encoded = compress(&[0; 129]);
        assert_eq!(
            decompress(&encoded, 128),
            Err(CodecError::OutputLimitExceeded)
        );
    }
}
