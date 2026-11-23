use crate::{InstantiationError, Trap};
use std::{cell::Cell, fmt};

pub(super) const MEMORY_PAGE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
pub(super) enum AccessWidth {
    Byte = 1,
    Halfword = 2,
    Word = 4,
}

impl AccessWidth {
    pub(super) const fn alignment_exponent(self) -> u32 {
        (self as u32).trailing_zeros()
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum LoadKind {
    Full,
    ByteSigned,
    ByteUnsigned,
    HalfwordSigned,
    HalfwordUnsigned,
}

impl LoadKind {
    pub(super) const fn width(self) -> AccessWidth {
        match self {
            Self::Full => AccessWidth::Word,
            Self::ByteSigned | Self::ByteUnsigned => AccessWidth::Byte,
            Self::HalfwordSigned | Self::HalfwordUnsigned => AccessWidth::Halfword,
        }
    }
}

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

    pub(super) fn size_in_pages(&self) -> i32 {
        (self.0.len() / MEMORY_PAGE_BYTES) as i32
    }

    fn access(&self, base: i32, offset: u32, width: AccessWidth) -> Result<&[Cell<u8>], Trap> {
        // Each input has at most 32 bits. Both the unsigned effective address
        // and its exclusive end fit in u64 without wrapping. Only narrow after
        // checking the entire range against the actual allocation length.
        let start = u64::from(base as u32) + u64::from(offset);
        let end = start + width as u64;
        if end > self.0.len() as u64 {
            return Err(Trap::MemoryOutOfBounds);
        }
        Ok(&self.0[start as usize..end as usize])
    }

    pub(super) fn load_i32(&self, base: i32, offset: u32, kind: LoadKind) -> Result<i32, Trap> {
        let bytes = self.access(base, offset, kind.width())?;
        let mut le = [0; 4];
        for (dst, byte) in le.iter_mut().zip(bytes) {
            *dst = byte.get();
        }
        let value = i32::from_le_bytes(le);
        // The zero padding already handles unsigned extension.
        Ok(match kind {
            LoadKind::ByteSigned => i32::from(value as i8),
            LoadKind::HalfwordSigned => i32::from(value as i16),
            LoadKind::Full | LoadKind::ByteUnsigned | LoadKind::HalfwordUnsigned => value,
        })
    }

    pub(super) fn store_i32(
        &self,
        base: i32,
        offset: u32,
        width: AccessWidth,
        value: i32,
    ) -> Result<(), Trap> {
        // The range is checked before setting any cell. Zipping with the slice
        // writes only the low bytes of the i32.
        let bytes = self.access(base, offset, width)?;
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
