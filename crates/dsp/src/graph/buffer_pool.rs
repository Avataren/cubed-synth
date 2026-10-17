use std::collections::HashSet;

pub struct AudioBufferPool {
    pub buffers: Vec<Vec<f32>>,
    available: Vec<usize>,
    in_use: HashSet<usize>,
}

impl AudioBufferPool {
    pub fn new(buffer_size: usize, initial_capacity: usize) -> Self {
        let mut buffers = Vec::with_capacity(initial_capacity);
        let mut available = Vec::with_capacity(initial_capacity);
        let in_use = HashSet::with_capacity(initial_capacity); // Initialize empty HashSet

        // Initialize all buffers as available
        for i in 0..initial_capacity {
            buffers.push(vec![0.0; buffer_size]);
            available.push(i);
        }

        Self {
            buffers,
            available,
            in_use, // Initially empty since no buffers are in use
        }
    }

    pub fn acquire(&mut self, buffer_size: usize) -> usize {
        let index = if let Some(index) = self.available.pop() {
            index
        } else {
            let index = self.buffers.len();
            self.buffers.push(vec![0.0; buffer_size]);
            index
        };
        self.in_use.insert(index);
        index
    }

    pub fn get_multiple_buffers<'a>(
        &'a self,
        indices: &[usize],
    ) -> Result<Vec<(usize, &'a [f32])>, String> {
        let mut result = Vec::with_capacity(indices.len());
        let mut requested_indices = HashSet::new(); // To check for duplicates if necessary, though not strictly needed for immutable borrows

        for &idx in indices {
            if idx >= self.buffers.len() {
                return Err(format!(
                    "Immutable buffer index {} out of bounds (total: {})",
                    idx,
                    self.buffers.len()
                ));
            }
            if !requested_indices.insert(idx) {
                // Allow duplicate requests for immutable slices if needed,
                // or return error: return Err(format!("Duplicate index requested: {}", idx));
            }
            // Safety: Bounds check done above. Multiple immutable borrows are safe.
            result.push((idx, self.buffers[idx].as_slice()));
        }
        Ok(result)
    }

    pub fn get_multiple_buffers_mut<'a>(
        &'a mut self,
        indices: &[usize],
    ) -> Result<Vec<(usize, &'a mut [f32])>, String> {
        // Return Result for consistency
        let mut result = Vec::new();
        let mut requested_indices = HashSet::new(); // Check for duplicates is crucial here

        for &idx in indices {
            if idx >= self.buffers.len() {
                return Err(format!(
                    "Mutable buffer index {} out of bounds (total: {})",
                    idx,
                    self.buffers.len()
                ));
            }
            if !requested_indices.insert(idx) {
                // Cannot provide multiple mutable references to the same buffer
                return Err(format!("Duplicate mutable index requested: {}", idx));
            }
        }

        // All indices unique and bounds checked. Now get references using unsafe.
        for &idx in indices {
            // Safety: We checked for duplicates and bounds above.
            unsafe {
                let buffer = &mut *self.buffers.as_mut_ptr().add(idx);
                result.push((idx, buffer.as_mut_slice()));
            }
        }
        Ok(result)
    }

    pub fn release(&mut self, index: usize) {
        if self.in_use.remove(&index) {
            self.available.push(index);
        }
    }

    pub fn fill(&mut self, index: usize, value: f32) {
        self.buffers[index].fill(value);
    }

    pub fn copy_in(&mut self, index: usize, data: &[f32]) {
        let buffer = &mut self.buffers[index];
        buffer[..data.len()].copy_from_slice(data);
    }

    pub fn copy_out(&self, index: usize) -> &[f32] {
        &self.buffers[index]
    }

    pub fn clear(&mut self, index: usize) {
        self.buffers[index].fill(0.0);
    }

    pub fn release_all(&mut self) {
        // Clear the in-use set and mark all buffers as available
        self.in_use.clear();
        self.available.clear();
        for i in 0..self.buffers.len() {
            self.available.push(i);
        }
    }

    /// Get multiple buffers with mixed mutability in a single call.
    /// Returns (immutable_refs, mutable_refs).
    /// SAFETY: Caller must ensure that immutable_indices and mutable_indices are disjoint.
    pub fn get_mixed_buffers<'a>(
        &'a mut self,
        immutable_indices: &[usize],
        mutable_indices: &[usize],
    ) -> Result<(Vec<(usize, &'a [f32])>, Vec<(usize, &'a mut [f32])>), String> {
        // Validate bounds and check for duplicates
        let mut all_indices = HashSet::new();

        for &idx in immutable_indices {
            if idx >= self.buffers.len() {
                return Err(format!(
                    "Immutable buffer index {} out of bounds (total: {})",
                    idx,
                    self.buffers.len()
                ));
            }
            if !all_indices.insert(idx) {
                return Err(format!(
                    "Duplicate index in immutable/mutable sets: {}",
                    idx
                ));
            }
        }

        for &idx in mutable_indices {
            if idx >= self.buffers.len() {
                return Err(format!(
                    "Mutable buffer index {} out of bounds (total: {})",
                    idx,
                    self.buffers.len()
                ));
            }
            if !all_indices.insert(idx) {
                return Err(format!(
                    "Duplicate index in immutable/mutable sets: {}",
                    idx
                ));
            }
        }

        let mut immutable_refs = Vec::new();
        let mut mutable_refs = Vec::new();

        // Safety: We've verified that all indices are unique and in bounds.
        // We can safely create multiple references (immutable and mutable to different buffers).
        unsafe {
            for &idx in immutable_indices {
                let buffer = &*self.buffers.as_ptr().add(idx);
                immutable_refs.push((idx, buffer.as_slice()));
            }

            for &idx in mutable_indices {
                let buffer = &mut *self.buffers.as_mut_ptr().add(idx);
                mutable_refs.push((idx, buffer.as_mut_slice()));
            }
        }

        Ok((immutable_refs, mutable_refs))
    }
}
