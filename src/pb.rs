use crate::{DecodingError, NeedMoreBytes};

pub fn read_varint32(data: &[u8]) -> Result<Result<(usize, u32), NeedMoreBytes>, DecodingError> {
    match read_varint(data, 5)? {
        Ok((bytes, val)) => Ok(Ok((bytes, val as u32))),
        Err(e) => Ok(Err(e)),
    }
}

pub fn read_varint64(data: &[u8]) -> Result<Result<(usize, u64), NeedMoreBytes>, DecodingError> {
    read_varint(data, 10)
}

pub fn read_fixed32(data: &[u8]) -> Result<(usize, u32), NeedMoreBytes> {
    if data.len() < 4 {
        Err(NeedMoreBytes)
    } else {
        let mut bytes = [0u8; 4];
        bytes.copy_from_slice(&data[..4]);
        Ok((4, u32::from_le_bytes(bytes)))
    }
}

pub fn read_fixed64(data: &[u8]) -> Result<(usize, u64), NeedMoreBytes> {
    if data.len() < 8 {
        Err(NeedMoreBytes)
    } else {
        let mut bytes = [0u8; 8];
        bytes.copy_from_slice(&data[..8]);
        Ok((8, u64::from_le_bytes(bytes)))
    }
}

pub fn read_varint(
    data: &[u8],
    max_bytes: usize,
) -> Result<Result<(usize, u64), NeedMoreBytes>, DecodingError> {
    let mask = 0x7f;

    let mut val = 0u64;

    let mut count = 0;

    for b in data.iter().take(max_bytes) {
        val |= ((b & mask) as u64) << (count * 7);
        count += 1;

        if b & 0x80 == 0 {
            return Ok(Ok((count, val)));
        }
    }

    if count < max_bytes {
        Ok(Err(NeedMoreBytes))
    } else if max_bytes == 4 {
        Err(DecodingError::TooManyVarint32Bytes)
    } else {
        Err(DecodingError::TooManyVarint64Bytes)
    }
}
