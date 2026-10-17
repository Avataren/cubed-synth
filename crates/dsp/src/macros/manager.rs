use rustc_hash::FxHashMap;

use super::types::{ModulationMacro, ModulationTarget};
use crate::graph::{AudioBufferPool, ModulationType};
use crate::{NodeId, PortId};
use cubed_simd::f32x4;

#[derive(Debug)]
pub struct MacroManager {
    macros: Vec<ModulationMacro>,
    buffer_size: usize,
    /// Preallocated scratch buffer to avoid per‐block allocation in update_macro.
    scratch_buffer: Vec<f32>,
}

pub struct MacroData {
    // For each macro, store its targets and a copy of its entire buffer for this block.
    // macros[i].0 = vector of targets, macros[i].1 = the macro’s buffer values.
    macros: Vec<(Vec<ModulationTarget>, Vec<f32>)>,
    buffer_size: usize,
}

impl MacroManager {
    pub fn new(num_macros: usize, buffer_pool: &mut AudioBufferPool, buffer_size: usize) -> Self {
        let mut macros = Vec::with_capacity(num_macros);
        for _ in 0..num_macros {
            let buffer_idx = buffer_pool.acquire(buffer_size);
            buffer_pool.fill(buffer_idx, 0.0);
            macros.push(ModulationMacro::new(buffer_idx));
        }
        Self {
            macros,
            buffer_size,
            scratch_buffer: vec![0.0; buffer_size],
        }
    }

    pub fn add_modulation(
        &mut self,
        macro_index: usize,
        target: ModulationTarget,
    ) -> Result<(), String> {
        self.macros
            .get_mut(macro_index)
            .ok_or_else(|| format!("Invalid macro index: {}", macro_index))?
            .add_target(target);
        Ok(())
    }

    pub fn remove_target(
        &mut self,
        macro_index: usize,
        node_id: NodeId,
        port_id: PortId,
    ) -> Result<bool, String> {
        let macro_mod = self
            .macros
            .get_mut(macro_index)
            .ok_or_else(|| format!("Invalid macro index: {}", macro_index))?;
        Ok(macro_mod.remove_target(node_id, port_id))
    }

    pub fn clear_macro(&mut self, macro_index: usize) -> Result<(), String> {
        let macro_mod = self
            .macros
            .get_mut(macro_index)
            .ok_or_else(|| format!("Invalid macro index: {}", macro_index))?;
        macro_mod.clear_targets();
        Ok(())
    }

    pub fn has_active_macros(&self) -> bool {
        self.macros.iter().any(|m| !m.get_targets().is_empty())
    }

    /// Update a macro’s buffer without per‐call allocation.
    pub fn update_macro(
        &mut self,
        macro_index: usize,
        values: &[f32],
        buffer_pool: &mut AudioBufferPool,
    ) -> Result<(), String> {
        if values.is_empty() {
            return Ok(());
        }

        let macro_mod = self
            .macros
            .get(macro_index)
            .ok_or_else(|| format!("Invalid macro index: {}", macro_index))?;
        let buffer_idx = macro_mod.get_value_buffer_idx();

        if buffer_idx >= buffer_pool.buffers.len() {
            return Err(format!("Buffer index {} out of range", buffer_idx));
        }

        let dest_buffer = &mut buffer_pool.buffers[buffer_idx];
        let dest_buffer_size = dest_buffer.len();
        if values.len() > dest_buffer_size {
            return Err(format!(
                "Source values too large: {} vs buffer size: {}",
                values.len(),
                dest_buffer_size
            ));
        }

        // Ensure the scratch buffer is the correct size.
        if self.scratch_buffer.len() != dest_buffer_size {
            self.scratch_buffer.resize(dest_buffer_size, 0.0);
        }
        // Copy provided values and zero-fill the remainder.
        self.scratch_buffer[..values.len()].copy_from_slice(values);
        for sample in &mut self.scratch_buffer[values.len()..] {
            *sample = 0.0;
        }
        buffer_pool.copy_in(buffer_idx, &self.scratch_buffer);
        Ok(())
    }

    pub fn clear(&mut self, buffer_pool: &mut AudioBufferPool) {
        for macro_mod in &self.macros {
            buffer_pool.clear(macro_mod.get_value_buffer_idx());
        }
    }

    pub fn get_macro_buffer_idx(&self, macro_index: usize) -> Option<usize> {
        self.macros
            .get(macro_index)
            .map(|m| m.get_value_buffer_idx())
    }

    /// Prepare all macro data for this block by copying it once from the buffer_pool.
    pub fn prepare_macro_data(&self, buffer_pool: &AudioBufferPool) -> MacroData {
        let mut macros_data = Vec::with_capacity(self.macros.len());
        for m in &self.macros {
            let buffer_idx = m.get_value_buffer_idx();
            let buffer = buffer_pool.copy_out(buffer_idx).to_vec();
            let targets = m.get_targets().to_vec();
            macros_data.push((targets, buffer));
        }
        MacroData {
            macros: macros_data,
            buffer_size: self.buffer_size,
        }
    }

    /// Apply modulation for the entire block starting at `offset`, processing in chunks of 4 samples.
    pub fn apply_modulation(
        &self,
        offset: usize,
        macro_data: &MacroData,
        outputs: &mut FxHashMap<PortId, &mut [f32]>,
    ) {
        let block_size = macro_data.buffer_size;
        if offset >= block_size {
            return;
        }

        // Process the block in 4-sample chunks.
        for i in (offset..block_size).step_by(4) {
            let chunk_size = (block_size - i).min(4);
            // For each macro...
            for (targets, buffer) in &macro_data.macros {
                if targets.is_empty() || i >= buffer.len() {
                    continue;
                }
                let current_chunk_size = (buffer.len() - i).min(chunk_size);
                let mut macro_chunk = [0.0; 4];
                macro_chunk[..current_chunk_size]
                    .copy_from_slice(&buffer[i..i + current_chunk_size]);
                // Apply modulation for each target in the macro.
                for target in targets {
                    // Apply modulation transformation to the macro values
                    let mut transformed = [0.0; 4];
                    for j in 0..current_chunk_size {
                        transformed[j] = target.modulation_transform.apply(macro_chunk[j]);
                    }
                    let transformed_simd = f32x4::from_array(transformed);

                    if let Some(output_buffer) = outputs.get_mut(&target.port_id) {
                        if i >= output_buffer.len() {
                            continue;
                        }
                        let out_chunk_size = (output_buffer.len() - i).min(current_chunk_size);
                        let mut current_chunk = [0.0; 4];
                        current_chunk[..out_chunk_size]
                            .copy_from_slice(&output_buffer[i..i + out_chunk_size]);
                        let current_simd = f32x4::from_array(current_chunk);
                        let amount_simd = f32x4::splat(target.amount);

                        let modulated_simd = match target.modulation_type {
                            ModulationType::Additive => {
                                current_simd + (transformed_simd * amount_simd)
                            }
                            ModulationType::VCA => current_simd * (transformed_simd * amount_simd),
                            ModulationType::Bipolar => {
                                current_simd * (f32x4::splat(1.0) + transformed_simd * amount_simd)
                            }
                        };

                        output_buffer[i..i + out_chunk_size]
                            .copy_from_slice(&modulated_simd.to_array()[..out_chunk_size]);
                    }
                }
            }
        }
    }
}
