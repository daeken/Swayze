use crate::{InstantiationError, Trap};
use std::{cell::Cell, fmt};

pub(super) struct DataSegment {
    // The bit pattern decoded from the signed LEB32 constant.
    pub(super) offset: u32,
    pub(super) bytes: Vec<u8>,
}

impl fmt::Debug for DataSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DataSegment")
            .field("offset", &self.offset)
            .field("byte_len", &self.bytes.len())
            .finish_non_exhaustive()
    }
}

pub(super) struct LinearMemory(Vec<Cell<u8>>);

impl LinearMemory {
    pub(super) fn new(
        byte_len: usize,
        segments: &[DataSegment],
    ) -> Result<Self, InstantiationError> {
        // Preflight every segment before allocating or copying. Payload lengths
        // are bounded to one page; widening the u32 offset avoids wrapping.
        // For an empty segment, its offset itself must be at or below byte_len.
        for (segment_index, segment) in segments.iter().enumerate() {
            let end = u64::from(segment.offset) + segment.bytes.len() as u64;
            if end > byte_len as u64 {
                return Err(InstantiationError::DataSegmentOutOfBounds { segment_index });
            }
        }
        let mut bytes = Vec::new();
        bytes
            .try_reserve_exact(byte_len)
            .map_err(|_| InstantiationError::MemoryAllocationFailed)?;
        // Capacity has already been reserved; zeroing does not allocate.
        bytes.resize_with(byte_len, || Cell::new(0));
        // The preflight guarantees these conversions and ranges fit. Copy in
        // source order so later segments overwrite any earlier overlapping bytes.
        for segment in segments {
            let start = segment.offset as usize;
            let destination = &mut bytes[start..start + segment.bytes.len()];
            for (byte, &value) in destination.iter_mut().zip(&segment.bytes) {
                *byte.get_mut() = value;
            }
        }
        Ok(Self(bytes))
    }

    fn word(&self, base: i32, offset: u32) -> Result<&[Cell<u8>], Trap> {
        // Each input has at most 32 bits. Both the unsigned effective address
        // and its exclusive end fit in u64 without wrapping. Only narrow after
        // checking the entire range against the actual allocation length.
        let start = u64::from(base as u32) + u64::from(offset);
        let end = start + 4;
        if end > self.0.len() as u64 {
            return Err(Trap::MemoryOutOfBounds);
        }
        Ok(&self.0[start as usize..end as usize])
    }

    pub(super) fn load_i32(&self, base: i32, offset: u32) -> Result<i32, Trap> {
        let bytes = self.word(base, offset)?;
        Ok(i32::from_le_bytes(std::array::from_fn(|i| bytes[i].get())))
    }

    pub(super) fn store_i32(&self, base: i32, offset: u32, value: i32) -> Result<(), Trap> {
        let bytes = self.word(base, offset)?;
        for (byte, value) in bytes.iter().zip(value.to_le_bytes()) {
            byte.set(value);
        }
        Ok(())
    }
}

impl fmt::Debug for LinearMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LinearMemory")
            .field("byte_len", &self.0.len())
            .finish_non_exhaustive()
    }
}
