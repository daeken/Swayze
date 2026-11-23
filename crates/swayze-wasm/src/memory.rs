use crate::{InstantiationError, ReadMemoryError, Trap};
use std::{cell::RefCell, collections::TryReserveError, fmt, ops::Range};

pub(super) const MAX_INSTANCE_MEMORY_PAGES: u32 = 2;
pub(super) const MEMORY32_MAX_PAGES: u32 = 65_536;
pub(super) const MEMORY_PAGE_BYTES: usize = 64 * 1024;

#[derive(Clone, Copy, Debug)]
pub(super) struct MemoryLimits {
    pub(super) minimum_pages: u32,
    pub(super) maximum_pages: Option<u32>,
}

#[derive(Clone, Copy, Debug)]
pub(super) enum AccessWidth {
    Byte = 1,
    Halfword = 2,
    Word = 4,
    Doubleword = 8,
}

impl AccessWidth {
    pub(super) const fn alignment_exponent(self) -> u32 {
        (self as u32).trailing_zeros()
    }
}

#[derive(Clone, Copy, Debug)]
pub(super) enum LoadKind {
    ByteSigned,
    ByteUnsigned,
    HalfwordSigned,
    HalfwordUnsigned,
    WordSigned,
    WordUnsigned,
    Doubleword,
}

impl LoadKind {
    pub(super) const fn width(self) -> AccessWidth {
        match self {
            Self::ByteSigned | Self::ByteUnsigned => AccessWidth::Byte,
            Self::HalfwordSigned | Self::HalfwordUnsigned => AccessWidth::Halfword,
            Self::WordSigned | Self::WordUnsigned => AccessWidth::Word,
            Self::Doubleword => AccessWidth::Doubleword,
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

pub(super) struct LinearMemory {
    // A whole number of pages, never larger than `cap_pages`. No view escapes,
    // and no allocation, deallocation, or guest dispatch holds a borrow.
    bytes: RefCell<Vec<u8>>,
    cap_pages: u32,
}

impl LinearMemory {
    pub(super) fn preflight(
        limits: Option<MemoryLimits>,
        segments: &[DataSegment],
    ) -> Result<(), InstantiationError> {
        let byte_len = limits.map_or(0, |limits| {
            limits.minimum_pages as usize * MEMORY_PAGE_BYTES
        });
        // Payload lengths are bounded to one page; widening the u32 offset
        // avoids wrapping.
        // For an empty segment, its offset itself must be at or below byte_len.
        for (segment_index, segment) in segments.iter().enumerate() {
            let end = u64::from(segment.offset) + segment.bytes.len() as u64;
            if end > byte_len as u64 {
                return Err(InstantiationError::DataSegmentOutOfBounds { segment_index });
            }
        }
        Ok(())
    }

    pub(super) fn new_preflighted(
        limits: Option<MemoryLimits>,
        segments: &[DataSegment],
    ) -> Result<Self, InstantiationError> {
        let (minimum_pages, cap_pages) = limits.map_or((0, 0), |limits| {
            (
                limits.minimum_pages,
                limits
                    .maximum_pages
                    .unwrap_or(MAX_INSTANCE_MEMORY_PAGES)
                    .min(MAX_INSTANCE_MEMORY_PAGES),
            )
        });
        let byte_len = minimum_pages as usize * MEMORY_PAGE_BYTES;
        let mut bytes = Vec::new();
        if byte_len != 0 {
            bytes
                .try_reserve_exact(byte_len)
                .map_err(|_| InstantiationError::MemoryAllocationFailed)?;
            Self::zero_tail(&mut bytes, byte_len);
        }
        // The preflight guarantees these conversions and ranges fit. Copy in
        // source order so later segments overwrite any earlier overlapping bytes.
        for segment in segments {
            let start = segment.offset as usize;
            bytes[start..start + segment.bytes.len()].copy_from_slice(&segment.bytes);
        }
        Ok(Self {
            bytes: RefCell::new(bytes),
            cap_pages,
        })
    }

    fn zero_tail(bytes: &mut Vec<u8>, byte_len: usize) {
        // Vec::push is guaranteed not to allocate while capacity suffices.
        assert!(bytes.capacity() >= byte_len);
        while bytes.len() < byte_len {
            bytes.push(0);
        }
    }

    pub(super) fn size_in_pages(&self) -> i32 {
        (self.bytes.borrow().len() / MEMORY_PAGE_BYTES) as i32
    }

    pub(super) fn grow(&self, delta: u32) -> i32 {
        self.grow_with(delta, Vec::try_reserve_exact)
    }

    fn grow_with(
        &self,
        delta: u32,
        reserve: impl FnOnce(&mut Vec<u8>, usize) -> Result<(), TryReserveError>,
    ) -> i32 {
        let previous = self.size_in_pages();
        if delta == 0 {
            return previous;
        }
        // The current size never exceeds the cap. Check the delta against
        // headroom before adding it, so even an unsigned -1 cannot wrap.
        let previous_pages = previous as u32;
        if delta > self.cap_pages - previous_pages {
            return -1;
        }
        let byte_len = (previous_pages + delta) as usize * MEMORY_PAGE_BYTES;
        let mut replacement = Vec::new();
        if reserve(&mut replacement, byte_len).is_err() {
            return -1;
        }
        // We have reserved the complete new length, from an empty Vec. With
        // no reentry the old length still matches the preflight and is
        // strictly smaller. Use Vec::push's no-allocation guarantee rather
        // than relying on the current allocation behavior of a bulk method.
        assert!(replacement.is_empty() && replacement.capacity() >= byte_len);
        {
            let old = self.bytes.borrow();
            assert_eq!(old.len(), previous_pages as usize * MEMORY_PAGE_BYTES);
            for &byte in old.iter() {
                replacement.push(byte);
            }
        }
        Self::zero_tail(&mut replacement, byte_len);
        let old = self.bytes.replace(replacement);
        drop(old); // RefCell::replace has ended its mutable borrow before deallocation.
        previous
    }

    pub(super) fn copy_out(&self, offset: u32, dest: &mut [u8]) -> Result<(), ReadMemoryError> {
        let bytes = self.bytes.borrow();
        let start = u64::from(offset);
        let end = start
            .checked_add(dest.len() as u64)
            .filter(|&end| end <= bytes.len() as u64)
            .ok_or(ReadMemoryError::OutOfBounds)?;
        dest.copy_from_slice(&bytes[start as usize..end as usize]);
        Ok(())
    }

    fn access_range(
        byte_len: usize,
        base: i32,
        offset: u32,
        width: AccessWidth,
    ) -> Result<Range<usize>, Trap> {
        // Each input has at most 32 bits. Both the unsigned effective address
        // and its exclusive end fit in u64 without wrapping. Only narrow after
        // checking the entire range against the actual allocation length.
        let start = u64::from(base as u32) + u64::from(offset);
        let end = start + width as u64;
        if end > byte_len as u64 {
            return Err(Trap::MemoryOutOfBounds);
        }
        Ok(start as usize..end as usize)
    }

    pub(super) fn load(&self, base: i32, offset: u32, kind: LoadKind) -> Result<i64, Trap> {
        let mut le = [0; 8];
        {
            let bytes = self.bytes.borrow();
            let range = Self::access_range(bytes.len(), base, offset, kind.width())?;
            le[..range.len()].copy_from_slice(&bytes[range]);
        }
        let value = i64::from_le_bytes(le);
        // The zero padding already handles unsigned extension.
        Ok(match kind {
            LoadKind::ByteSigned => i64::from(value as i8),
            LoadKind::HalfwordSigned => i64::from(value as i16),
            LoadKind::WordSigned => i64::from(value as i32),
            LoadKind::ByteUnsigned
            | LoadKind::HalfwordUnsigned
            | LoadKind::WordUnsigned
            | LoadKind::Doubleword => value,
        })
    }

    pub(super) fn store(
        &self,
        base: i32,
        offset: u32,
        width: AccessWidth,
        value: i64,
    ) -> Result<(), Trap> {
        let le = value.to_le_bytes();
        let mut bytes = self.bytes.borrow_mut();
        let range = Self::access_range(bytes.len(), base, offset, width)?;
        bytes[range].copy_from_slice(&le[..width as usize]);
        Ok(())
    }
}

impl fmt::Debug for LinearMemory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let byte_len = self.bytes.borrow().len();
        f.debug_struct("LinearMemory")
            .field("byte_len", &byte_len)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reserves_only_for_eligible_growth_and_can_retry_after_reserve_failure() {
        let make_memory = |minimum_pages, maximum_pages| {
            LinearMemory::new_preflighted(
                Some(MemoryLimits {
                    minimum_pages,
                    maximum_pages,
                }),
                &[],
            )
            .unwrap()
        };
        let no_reserve = |_: &mut Vec<u8>, _| panic!("this delta must never request a reserve");
        assert_eq!(make_memory(0, Some(0)).grow_with(1, no_reserve), -1);
        assert_eq!(make_memory(1, Some(1)).grow_with(1, no_reserve), -1);

        let memory = make_memory(0, Some(MEMORY32_MAX_PAGES));
        assert_eq!(memory.bytes.borrow().capacity(), 0);
        let tail = MEMORY_PAGE_BYTES as i32 - 4;
        for previous in 0..=1 {
            let base = memory.bytes.borrow().as_ptr();
            assert_eq!(memory.grow_with(0, no_reserve), previous);
            for delta in [MAX_INSTANCE_MEMORY_PAGES - previous as u32 + 1, u32::MAX] {
                assert_eq!(memory.grow_with(delta, no_reserve), -1);
            }
            let expected_len = (previous + 1) as usize * MEMORY_PAGE_BYTES;
            for _ in 0..2 {
                let fail = |fresh: &mut Vec<u8>, requested| {
                    assert_eq!((fresh.len(), fresh.capacity()), (0, 0));
                    assert_eq!(requested, expected_len);
                    assert!(memory.bytes.try_borrow_mut().is_ok()); // No live borrow at reserve.
                    // Capacity overflow supplies a TryReserveError without real OOM.
                    fresh.try_reserve_exact(usize::MAX)
                };
                assert_eq!(memory.grow_with(1, fail), -1);
                assert_eq!(memory.size_in_pages(), previous);
                assert_eq!(memory.bytes.borrow().as_ptr(), base);
                assert_eq!(
                    memory.load(tail, 0, LoadKind::WordUnsigned),
                    if previous == 0 {
                        Err(Trap::MemoryOutOfBounds)
                    } else {
                        Ok(0x1234_5678)
                    }
                );
            }
            assert_eq!(memory.grow(1), previous);
            {
                let bytes = memory.bytes.borrow();
                assert_eq!(bytes.len(), expected_len);
                // Scan the entire new page, including after a failed attempt.
                assert!(
                    bytes[previous as usize * MEMORY_PAGE_BYTES..]
                        .iter()
                        .all(|&b| b == 0)
                );
            }
            if previous == 0 {
                memory
                    .store(tail, 0, AccessWidth::Word, 0x1234_5678)
                    .unwrap();
            }
        }
        assert_eq!(memory.grow_with(0, no_reserve), 2);
        assert_eq!(memory.grow_with(1, no_reserve), -1);
        assert_eq!(memory.size_in_pages(), 2);
        assert_eq!(
            memory.load(tail, 0, LoadKind::WordUnsigned),
            Ok(0x1234_5678)
        );
    }
}
