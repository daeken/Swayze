use crate::{InstantiationError, Trap};
use std::fmt;

pub(super) const MAX_TABLE_ENTRIES: u32 = 16;

pub(super) struct ElementSegment {
    // The unsigned bit pattern of the signed i32.const immediate.
    pub(super) offset: u32,
    pub(super) function_indices: Vec<usize>,
}

impl fmt::Debug for ElementSegment {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ElementSegment")
            .field("offset", &self.offset)
            .field("index_count", &self.function_indices.len())
            .finish_non_exhaustive()
    }
}

pub(super) struct FunctionTable {
    entries: [Option<usize>; MAX_TABLE_ENTRIES as usize],
    len: usize,
}

impl FunctionTable {
    pub(super) fn preflight(
        len: usize,
        segments: &[ElementSegment],
    ) -> Result<(), InstantiationError> {
        for (segment_index, segment) in segments.iter().enumerate() {
            // Widen before adding so even u32::MAX + 1 is out of bounds.
            // For an empty segment, its start itself must be at or below len.
            let end = u64::from(segment.offset) + segment.function_indices.len() as u64;
            if end > len as u64 {
                return Err(InstantiationError::ElementSegmentOutOfBounds { segment_index });
            }
        }
        Ok(())
    }

    pub(super) fn new_preflighted(len: usize, segments: &[ElementSegment]) -> Self {
        let mut table = Self {
            entries: [None; MAX_TABLE_ENTRIES as usize],
            len,
        };
        let entries = &mut table.entries[..len];
        for segment in segments {
            // Preflight guarantees this segment fits in the logical table.
            let start = segment.offset as usize;
            let destination = &mut entries[start..start + segment.function_indices.len()];
            for (entry, &index) in destination.iter_mut().zip(&segment.function_indices) {
                *entry = Some(index);
            }
        }
        table
    }

    pub(super) fn size(&self) -> u32 {
        self.len as u32
    }

    pub(super) fn resolve(&self, selector: u32) -> Result<usize, Trap> {
        if selector >= self.len as u32 {
            return Err(Trap::TableOutOfBounds);
        }
        self.entries[selector as usize].ok_or(Trap::UninitializedElement)
    }
}

impl fmt::Debug for FunctionTable {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("FunctionTable")
            .field("len", &self.len)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests;
