use super::ModulationSource;
use crate::graph::{ModulationTransformation, ModulationType};
use cubed_simd::Simd;

// ModulationResult is no longer needed as we process inplace or directly to target buffers
// pub struct ModulationResult {
//     pub additive: Vec<f32>,
//     pub multiplicative: Vec<f32>,
// }

pub trait ModulationProcessor {
    // Original process_modulations - can be kept for compatibility or removed if unused
    // Now uses the inplace accumulator internally, but still allocates for the final result.
    // Consider deprecating if performance is critical everywhere.
    fn process_modulations(
        &self,
        buffer_size: usize,
        sources: Option<&[ModulationSource]>,
        initial_value: f32,
        // Temporary buffers needed for the inplace accumulation
        add_buf: &mut Vec<f32>,
        mult_buf: &mut Vec<f32>,
    ) -> Vec<f32> {
        let sources_slice = sources;

        // Ensure temp buffers are large enough
        if add_buf.len() < buffer_size {
            add_buf.resize(buffer_size, 0.0);
        }
        if mult_buf.len() < buffer_size {
            mult_buf.resize(buffer_size, 1.0);
        }

        // Perform accumulation inplace
        Self::accumulate_modulations_inplace(
            buffer_size,
            sources_slice,
            &mut add_buf[..buffer_size],
            &mut mult_buf[..buffer_size],
        );

        // Combine results (still involves an allocation for the return Vec)
        add_buf[..buffer_size]
            .iter()
            .zip(mult_buf[..buffer_size].iter())
            .map(|(&a, &m)| (a + initial_value) * m)
            .collect()
    }

    // process_modulations_ex is removed - superseded by direct inplace accumulation
    // and combination within the node's process method. Nodes needing separate
    // additive/multiplicative results will call accumulate_modulations_inplace directly.

    /// Accumulates modulation sources directly into provided mutable slices.
    /// This avoids heap allocations during processing.
    #[inline(always)]
    fn accumulate_modulations_inplace(
        buffer_size: usize,
        sources: Option<&[ModulationSource]>,
        // Use mutable slices for output
        add_target: &mut [f32],
        mult_target: &mut [f32],
    ) {
        // Ensure slices have the correct size (or are at least large enough)
        assert!(
            add_target.len() >= buffer_size,
            "Additive target buffer too small"
        );
        assert!(
            mult_target.len() >= buffer_size,
            "Multiplicative target buffer too small"
        );

        // Reset the relevant portion of the target buffers for this accumulation pass
        add_target[..buffer_size].fill(0.0f32);
        mult_target[..buffer_size].fill(1.0f32);

        if let Some(sources) = sources {
            for source in sources {
                // Ensure source buffer isn't shorter than buffer_size,
                // process only up to the minimum length.
                let process_len = std::cmp::min(buffer_size, source.buffer.len());
                if process_len == 0 {
                    continue;
                } // Skip empty sources

                let buf = &source.buffer[..process_len];
                let amt = source.amount;
                let transform = source.transformation;

                // Get slices corresponding to the actual length we can process
                let add_slice = &mut add_target[..process_len];
                let mult_slice = &mut mult_target[..process_len];

                match source.mod_type {
                    ModulationType::VCA => {
                        // Apply to the portion we can process
                        Self::apply_mul(buf, mult_slice, amt, transform);
                    }
                    ModulationType::Bipolar => {
                        Self::apply_bipolar(buf, mult_slice, amt, transform);
                    }
                    ModulationType::Additive => {
                        Self::apply_add(buf, add_slice, amt, transform);
                    }
                }

                // If source buffer was shorter than buffer_size, the rest of the
                // target buffers retain their initial 0.0/1.0 values, which is correct.
            }
        }
        // No return value, results are written inplace
    }

    // SIMD transformation function remains unchanged.
    #[inline(always)]
    fn transform_simd<const LANES: usize>(
        x: Simd<f32, LANES>,
        transform: ModulationTransformation,
        one: Simd<f32, LANES>,
    ) -> Simd<f32, LANES>
    where
        [f32; LANES]: Sized,
    {
        match transform {
            ModulationTransformation::None => x,
            ModulationTransformation::Invert => one - x,
            ModulationTransformation::Square => x * x,
            ModulationTransformation::Cube => x * x * x,
        }
    }

    // Scalar version of the transformation.
    #[inline(always)]
    fn transform_scalar(x: f32, transform: ModulationTransformation) -> f32 {
        match transform {
            ModulationTransformation::None => x,
            ModulationTransformation::Invert => 1.0 - x,
            ModulationTransformation::Square => x * x,
            ModulationTransformation::Cube => x * x * x,
        }
    }

    // Unsafe SIMD loop - unchanged conceptually, but ensure it uses slices correctly
    /// # Safety
    ///
    /// No caller obligation: every raw access is within
    /// `min(source.len(), target.len())`.
    #[inline(always)]
    unsafe fn simd_process_unchecked<const LANES: usize, F, FS>(
        source: &[f32],     // Source data slice
        target: &mut [f32], // Target data slice (mutable)
        amt: f32,
        transform: ModulationTransformation,
        update: F, // SIMD update function: (target_chunk, weighted_source, one) -> new_target_chunk
        update_scalar: FS, // Scalar update function: (target_val, weighted_source, one) -> new_target_val
    ) where
        F: Fn(Simd<f32, LANES>, Simd<f32, LANES>, Simd<f32, LANES>) -> Simd<f32, LANES>,
        FS: Fn(f32, f32, f32) -> f32,
        [f32; LANES]: Sized,
    {
        let simd_amt = Simd::<f32, LANES>::splat(amt);
        let simd_one = Simd::<f32, LANES>::splat(1.0);

        // Process only the minimum length of source and target
        let len = std::cmp::min(source.len(), target.len());
        let chunks = len / LANES;
        let remainder = len % LANES;

        let src_ptr = source.as_ptr();
        let tgt_ptr = target.as_mut_ptr();

        for i in 0..chunks {
            let offset = i * LANES;
            // Directly load from source pointer
            let src_simd = Simd::<f32, LANES>::from_slice(std::slice::from_raw_parts(
                src_ptr.add(offset),
                LANES,
            ));
            // Apply transform and amount
            let weighted = Self::transform_simd(src_simd, transform, simd_one) * simd_amt;

            // Load current target value
            let tgt_simd = Simd::<f32, LANES>::from_slice(std::slice::from_raw_parts(
                tgt_ptr.add(offset),
                LANES,
            ));

            // Apply the update function
            let result_simd = update(tgt_simd, weighted, simd_one);

            // Write the result back to target pointer
            result_simd.copy_to_slice(std::slice::from_raw_parts_mut(tgt_ptr.add(offset), LANES));
        }

        // Process any remaining elements scalarly.
        if remainder > 0 {
            let start = chunks * LANES; // Correct start index for remainder
            for i in 0..remainder {
                let current_offset = start + i;
                let s = *src_ptr.add(current_offset);
                let weighted = Self::transform_scalar(s, transform) * amt;
                let old = *tgt_ptr.add(current_offset);
                *tgt_ptr.add(current_offset) = update_scalar(old, weighted, 1.0);
            }
        }
    }

    // Safe wrapper that calls the unsafe version.
    #[inline(always)]
    fn simd_process<const LANES: usize, F, FS>(
        source: &[f32],
        target: &mut [f32],
        amt: f32,
        transform: ModulationTransformation,
        update: F,
        update_scalar: FS,
    ) where
        F: Fn(Simd<f32, LANES>, Simd<f32, LANES>, Simd<f32, LANES>) -> Simd<f32, LANES>,
        FS: Fn(f32, f32, f32) -> f32,
        [f32; LANES]: Sized,
    {
        // Basic bounds check (optional, as simd_process_unchecked does min length)
        // assert!(target.len() >= source.len());
        unsafe {
            Self::simd_process_unchecked::<LANES, F, FS>(
                source,
                target,
                amt,
                transform,
                update,
                update_scalar,
            )
        }
    }

    // Optimized modulation helper functions - use the safe simd_process wrapper.
    #[inline(always)]
    fn apply_mul(
        source: &[f32],
        target: &mut [f32],
        amt: f32,
        transform: ModulationTransformation,
    ) {
        Self::simd_process::<4, _, _>(
            // Assuming LANES = 4
            source,
            target,
            amt,
            transform,
            |t, w, _one| t * w, // SIMD: target * weighted_source
            |t, w, _one| t * w, // Scalar: target * weighted_source
        );
    }

    #[inline(always)]
    fn apply_bipolar(
        source: &[f32],
        target: &mut [f32],
        amt: f32,
        transform: ModulationTransformation,
    ) {
        Self::simd_process::<4, _, _>(
            source,
            target,
            amt,
            transform,
            |t, w, one| t * (one + w), // SIMD: target * (1.0 + weighted_source)
            |t, w, one| t * (one + w), // Scalar: target * (1.0 + weighted_source)
        );
    }

    #[inline(always)]
    fn apply_add(
        source: &[f32],
        target: &mut [f32],
        amt: f32,
        transform: ModulationTransformation,
    ) {
        Self::simd_process::<4, _, _>(
            source,
            target,
            amt,
            transform,
            |t, w, _one| t + w, // SIMD: target + weighted_source
            |t, w, _one| t + w, // Scalar: target + weighted_source
        );
    }

    /// Helper for combining base value with accumulated modulation results (inplace).
    /// Operates directly on a target buffer.
    #[inline(always)]
    fn combine_modulation_inplace(
        target_buffer: &mut [f32], // Buffer to write final combined values into
        buffer_size: usize,
        base_value: f32,
        add_mod_buffer: &[f32],  // Buffer with additive results
        mult_mod_buffer: &[f32], // Buffer with multiplicative results
    ) {
        // Ensure buffers are adequately sized
        let len = std::cmp::min(
            buffer_size,
            std::cmp::min(
                target_buffer.len(),
                std::cmp::min(add_mod_buffer.len(), mult_mod_buffer.len()),
            ),
        );

        let base_simd = Simd::<f32, 4>::splat(base_value); // Assuming LANES = 4
        let chunks = len / 4;

        for i in 0..chunks {
            let offset = i * 4;
            let add_simd = Simd::<f32, 4>::from_slice(&add_mod_buffer[offset..offset + 4]);
            let mult_simd = Simd::<f32, 4>::from_slice(&mult_mod_buffer[offset..offset + 4]);
            let combined = (base_simd + add_simd) * mult_simd;
            combined.copy_to_slice(&mut target_buffer[offset..offset + 4]);
        }

        // Scalar remainder
        let remainder_start = chunks * 4;
        for i in remainder_start..len {
            target_buffer[i] = (base_value + add_mod_buffer[i]) * mult_mod_buffer[i];
        }

        // If buffer_size was larger than target/mod buffers, this correctly only processes the valid range.
        // If target_buffer is longer than buffer_size, only the first buffer_size elements are touched.
    }

    /// Version of combine_modulation_inplace where the base value is also a slice.
    #[inline(always)]
    fn combine_modulation_inplace_varying_base(
        target_buffer: &mut [f32], // Buffer to write final combined values into
        buffer_size: usize,
        base_value_buffer: &[f32], // Buffer with base values
        add_mod_buffer: &[f32],    // Buffer with additive results
        mult_mod_buffer: &[f32],   // Buffer with multiplicative results
    ) {
        // Ensure buffers are adequately sized
        let len = std::cmp::min(
            buffer_size,
            std::cmp::min(
                target_buffer.len(),
                std::cmp::min(
                    base_value_buffer.len(),
                    std::cmp::min(add_mod_buffer.len(), mult_mod_buffer.len()),
                ),
            ),
        );

        let chunks = len / 4; // Assuming LANES = 4

        for i in 0..chunks {
            let offset = i * 4;
            let base_simd = Simd::<f32, 4>::from_slice(&base_value_buffer[offset..offset + 4]);
            let add_simd = Simd::<f32, 4>::from_slice(&add_mod_buffer[offset..offset + 4]);
            let mult_simd = Simd::<f32, 4>::from_slice(&mult_mod_buffer[offset..offset + 4]);
            let combined = (base_simd + add_simd) * mult_simd;
            combined.copy_to_slice(&mut target_buffer[offset..offset + 4]);
        }

        // Scalar remainder
        let remainder_start = chunks * 4;
        for i in remainder_start..len {
            target_buffer[i] = (base_value_buffer[i] + add_mod_buffer[i]) * mult_mod_buffer[i];
        }
    }
}
