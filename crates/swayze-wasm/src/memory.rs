use crate::{InstantiationError, Trap};
use std::{
    cell::{Cell, OnceCell},
    collections::TryReserveError,
    fmt,
};

pub(super) const MAX_MEMORY_PAGES: u32 = 1;
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

pub(super) struct LinearMemory {
    // Empty until a single, fully initialized page is installed. Existing
    // pages are never reallocated, and no borrowed view escapes this type.
    page: OnceCell<Vec<Cell<u8>>>,
    maximum_pages: u32,
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
        let maximum_pages =
            limits.map_or(0, |limits| limits.maximum_pages.unwrap_or(MAX_MEMORY_PAGES));
        if limits.is_none_or(|limits| limits.minimum_pages == 0) {
            // Preflight only permits empty segments at offset zero in this case.
            return Ok(Self {
                page: OnceCell::new(),
                maximum_pages,
            });
        }
        let mut bytes =
            Self::allocate_page().map_err(|_| InstantiationError::MemoryAllocationFailed)?;
        // The preflight guarantees these conversions and ranges fit. Copy in
        // source order so later segments overwrite any earlier overlapping bytes.
        for segment in segments {
            let start = segment.offset as usize;
            let destination = &mut bytes[start..start + segment.bytes.len()];
            for (byte, &value) in destination.iter_mut().zip(&segment.bytes) {
                *byte.get_mut() = value;
            }
        }
        Ok(Self {
            page: OnceCell::from(bytes),
            maximum_pages,
        })
    }

    fn allocate_page() -> Result<Vec<Cell<u8>>, TryReserveError> {
        let mut bytes = Vec::new();
        bytes.try_reserve_exact(MEMORY_PAGE_BYTES)?;
        // Capacity has already been reserved; zeroing does not allocate.
        bytes.resize_with(MEMORY_PAGE_BYTES, || Cell::new(0));
        Ok(bytes)
    }

    pub(super) fn size_in_pages(&self) -> i32 {
        i32::from(self.page.get().is_some())
    }

    pub(super) fn grow(&self, delta: u32) -> i32 {
        self.grow_with(delta, Self::allocate_page)
    }

    fn grow_with(
        &self,
        delta: u32,
        allocate_page: impl FnOnce() -> Result<Vec<Cell<u8>>, TryReserveError>,
    ) -> i32 {
        let previous = self.size_in_pages();
        if delta == 0 {
            return previous;
        }
        // Under the one-page cap the only possible growth is from zero to one.
        if previous != 0 || delta != 1 || delta > self.maximum_pages {
            return -1;
        }
        match allocate_page() {
            Ok(page) => self.page.set(page).map_or(-1, |()| previous),
            Err(_) => -1,
        }
    }

    fn bytes(&self) -> &[Cell<u8>] {
        self.page.get().map_or(&[], Vec::as_slice)
    }

    fn access(&self, base: i32, offset: u32, width: AccessWidth) -> Result<&[Cell<u8>], Trap> {
        // Each input has at most 32 bits. Both the unsigned effective address
        // and its exclusive end fit in u64 without wrapping. Only narrow after
        // checking the entire range against the actual allocation length.
        let start = u64::from(base as u32) + u64::from(offset);
        let end = start + width as u64;
        let bytes = self.bytes();
        if end > bytes.len() as u64 {
            return Err(Trap::MemoryOutOfBounds);
        }
        Ok(&bytes[start as usize..end as usize])
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
            .field("byte_len", &self.bytes().len())
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
        let no_reserve = || panic!("this delta must never request a reserve");
        let blocked = make_memory(0, Some(0));
        assert!(blocked.page.get().is_none());
        assert_eq!(blocked.grow_with(1, no_reserve), -1);

        let memory = make_memory(0, None);
        assert!(memory.page.get().is_none());
        assert_eq!(memory.grow_with(0, no_reserve), 0);
        for delta in [2, u32::MAX] {
            assert_eq!(memory.grow_with(delta, no_reserve), -1);
        }

        // A capacity overflow supplies a TryReserveError deterministically,
        // without needing an unsafe global allocator or inducing real OOM.
        let error = Vec::<u8>::new().try_reserve_exact(usize::MAX).unwrap_err();
        assert_eq!(memory.grow_with(1, || Err(error)), -1);
        assert!(memory.page.get().is_none());
        assert_eq!(
            memory.load_i32(0, 0, LoadKind::Full),
            Err(Trap::MemoryOutOfBounds)
        );

        assert_eq!(memory.grow(1), 0);
        let bytes = memory.bytes();
        assert_eq!(bytes.len(), MEMORY_PAGE_BYTES);
        assert!(bytes.iter().all(|byte| byte.get() == 0));
        for memory in [memory, make_memory(1, None)] {
            let base = memory.bytes().as_ptr();
            assert_eq!(memory.grow_with(0, no_reserve), 1);
            for delta in [1, 2, u32::MAX] {
                assert_eq!(memory.grow_with(delta, no_reserve), -1);
            }
            assert_eq!(memory.size_in_pages(), 1);
            assert_eq!(memory.bytes().as_ptr(), base);
        }
    }
}
