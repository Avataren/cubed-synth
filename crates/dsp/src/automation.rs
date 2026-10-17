use crate::{audio_engine::WasmModulationType, graph::ModulationTransformation, traits::PortId};

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use crate::audio_engine::AudioEngine;
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use js_sys::{Float32Array, Object, Reflect};
#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
use wasm_bindgen::{prelude::*, JsCast, JsValue};

const DEFAULT_FREQUENCY: f32 = 440.0;
const DEFAULT_GAIN: f32 = 1.0;
const DEFAULT_GATE: f32 = 0.0;
const DEFAULT_VELOCITY: f32 = 0.0;

/// Frame of automation data that can be shared between wasm and native hosts.
#[cfg_attr(all(feature = "wasm", target_arch = "wasm32"), wasm_bindgen)]
#[derive(Clone, Debug)]
pub struct AutomationFrame {
    num_voices: usize,
    macro_count: usize,
    macro_buffer_len: usize,
    gate_buffer_len: usize,
    frequency_buffer_len: usize,
    gates: Vec<f32>,
    frequencies: Vec<f32>, // Per-voice, per-sample
    velocities: Vec<f32>,
    gains: Vec<f32>,
    macro_buffers: Vec<f32>,
}

impl AutomationFrame {
    pub fn with_dimensions(num_voices: usize, macro_count: usize, macro_buffer_len: usize) -> Self {
        let macro_count = macro_count.max(1);
        let gate_buffer_len = macro_buffer_len.max(1);
        let frequency_buffer_len = macro_buffer_len.max(1);
        let macro_buffer_len = macro_buffer_len.max(1);
        let gate_buffers = vec![DEFAULT_GATE; num_voices * gate_buffer_len];
        let frequency_buffers = vec![DEFAULT_FREQUENCY; num_voices * frequency_buffer_len];
        let macro_buffers = vec![0.0; num_voices * macro_count * macro_buffer_len];
        Self {
            num_voices,
            macro_count,
            macro_buffer_len,
            gate_buffer_len,
            frequency_buffer_len,
            gates: gate_buffers,
            frequencies: frequency_buffers,
            velocities: vec![DEFAULT_VELOCITY; num_voices],
            gains: vec![DEFAULT_GAIN; num_voices],
            macro_buffers,
        }
    }

    pub fn num_voices(&self) -> usize {
        self.num_voices
    }

    pub fn macro_count(&self) -> usize {
        self.macro_count
    }

    pub fn gate_buffer_len(&self) -> usize {
        self.gate_buffer_len
    }

    pub fn frequency_buffer_len(&self) -> usize {
        self.frequency_buffer_len
    }

    pub fn macro_buffer_len(&self) -> usize {
        self.macro_buffer_len
    }

    pub fn gates(&self) -> &[f32] {
        &self.gates
    }

    pub fn frequencies(&self) -> &[f32] {
        &self.frequencies
    }

    pub fn velocities(&self) -> &[f32] {
        &self.velocities
    }

    pub fn gains(&self) -> &[f32] {
        &self.gains
    }

    pub fn macro_buffers(&self) -> &[f32] {
        &self.macro_buffers
    }

    pub fn gates_mut(&mut self) -> &mut [f32] {
        &mut self.gates
    }

    pub fn frequencies_mut(&mut self) -> &mut [f32] {
        &mut self.frequencies
    }

    pub fn velocities_mut(&mut self) -> &mut [f32] {
        &mut self.velocities
    }

    pub fn gains_mut(&mut self) -> &mut [f32] {
        &mut self.gains
    }

    pub fn macro_buffers_mut(&mut self) -> &mut [f32] {
        &mut self.macro_buffers
    }

    pub fn set_voice_values(
        &mut self,
        voice_index: usize,
        gate: f32,
        frequency: f32,
        velocity: f32,
        gain: f32,
    ) {
        if voice_index >= self.num_voices {
            return;
        }
        self.set_gate_value(voice_index, gate);
        self.set_frequency_value(voice_index, frequency);
        self.velocities[voice_index] = velocity;
        self.gains[voice_index] = gain;
    }

    pub fn set_macro_value(&mut self, voice_index: usize, macro_index: usize, value: f32) {
        if voice_index >= self.num_voices || macro_index >= self.macro_count {
            return;
        }
        let start = self.macro_offset(voice_index, macro_index);
        for slot in self
            .macro_buffers
            .iter_mut()
            .skip(start)
            .take(self.macro_buffer_len)
        {
            *slot = value;
        }
    }

    pub fn macro_slice(&self, voice_index: usize, macro_index: usize) -> &[f32] {
        let start = self.macro_offset(voice_index, macro_index);
        let end = start + self.macro_buffer_len;
        &self.macro_buffers[start..end]
    }

    pub fn gate_slice(&self, voice_index: usize) -> &[f32] {
        let start = voice_index.saturating_mul(self.gate_buffer_len);
        let end = start + self.gate_buffer_len;
        if end > self.gates.len() {
            return &[];
        }
        &self.gates[start..end]
    }

    pub fn frequency_slice(&self, voice_index: usize) -> &[f32] {
        let start = voice_index.saturating_mul(self.frequency_buffer_len);
        let end = start + self.frequency_buffer_len;
        if end > self.frequencies.len() {
            return &[];
        }
        &self.frequencies[start..end]
    }

    fn macro_offset(&self, voice_index: usize, macro_index: usize) -> usize {
        (voice_index * self.macro_count + macro_index) * self.macro_buffer_len
    }

    pub fn ensure_macro_dimensions(
        &mut self,
        voice_count: usize,
        macro_count: usize,
        buffer_len: usize,
    ) {
        let voice_count = voice_count.max(1);
        let macro_count = macro_count.max(1);
        let buffer_len = buffer_len.max(1);
        let expected_len = voice_count * macro_count * buffer_len;

        if self.macro_count != macro_count
            || self.macro_buffer_len != buffer_len
            || self.macro_buffers.len() < expected_len
        {
            self.macro_count = macro_count;
            self.macro_buffer_len = buffer_len;
            self.macro_buffers = vec![0.0; expected_len];
        } else {
            self.macro_buffers.fill(0.0);
        }
    }

    fn gate_offset(&self, voice_index: usize) -> usize {
        voice_index * self.gate_buffer_len
    }

    fn frequency_offset(&self, voice_index: usize) -> usize {
        voice_index * self.frequency_buffer_len
    }

    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    fn ensure_gate_buffer_len(&mut self, target_len: usize) {
        let target_len = target_len.max(1);
        if self.gate_buffer_len != target_len || self.gates.len() < self.num_voices * target_len {
            self.gate_buffer_len = target_len;
            self.gates = vec![DEFAULT_GATE; self.num_voices * target_len];
        } else {
            self.gates.fill(DEFAULT_GATE);
        }
    }

    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    fn ensure_frequency_buffer_len(&mut self, target_len: usize) {
        let target_len = target_len.max(1);
        if self.frequency_buffer_len != target_len
            || self.frequencies.len() < self.num_voices * target_len
        {
            self.frequency_buffer_len = target_len;
            self.frequencies = vec![DEFAULT_FREQUENCY; self.num_voices * target_len];
        } else {
            self.frequencies.fill(DEFAULT_FREQUENCY);
        }
    }

    fn set_gate_value(&mut self, voice_index: usize, value: f32) {
        if voice_index >= self.num_voices {
            return;
        }
        let start = self.gate_offset(voice_index);
        let end = start + self.gate_buffer_len;
        if end > self.gates.len() {
            return;
        }
        self.gates[start..end].fill(value);
    }

    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    fn set_gate_buffer(&mut self, voice_index: usize, values: &[f32]) {
        if voice_index >= self.num_voices {
            return;
        }
        let start = self.gate_offset(voice_index);
        let end = start + self.gate_buffer_len;
        if end > self.gates.len() {
            return;
        }

        let write_len = values.len().min(self.gate_buffer_len);
        self.gates[start..start + write_len].copy_from_slice(&values[..write_len]);

        if write_len < self.gate_buffer_len {
            let last = values
                .get(write_len.saturating_sub(1))
                .copied()
                .unwrap_or(DEFAULT_GATE);
            self.gates[start + write_len..end].fill(last);
        }
    }

    fn set_frequency_value(&mut self, voice_index: usize, value: f32) {
        if voice_index >= self.num_voices {
            return;
        }
        let start = self.frequency_offset(voice_index);
        let end = start + self.frequency_buffer_len;
        if end > self.frequencies.len() {
            return;
        }
        self.frequencies[start..end].fill(value);
    }

    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    fn set_frequency_buffer(&mut self, voice_index: usize, values: &[f32]) {
        if voice_index >= self.num_voices {
            return;
        }
        let start = self.frequency_offset(voice_index);
        let end = start + self.frequency_buffer_len;
        if end > self.frequencies.len() {
            return;
        }

        let write_len = values.len().min(self.frequency_buffer_len);
        self.frequencies[start..start + write_len].copy_from_slice(&values[..write_len]);

        if write_len < self.frequency_buffer_len {
            let last = values
                .get(write_len.saturating_sub(1))
                .copied()
                .unwrap_or(DEFAULT_FREQUENCY);
            self.frequencies[start + write_len..end].fill(last);
        }
    }

    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    fn reset_defaults(&mut self) {
        self.gates.fill(DEFAULT_GATE);
        self.frequencies.fill(DEFAULT_FREQUENCY);
        self.velocities.fill(DEFAULT_VELOCITY);
        self.gains.fill(DEFAULT_GAIN);
        self.macro_buffers.fill(0.0);
    }

    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    fn read_parameter_scalar(
        &self,
        parameters: &Object,
        key: &str,
        default: f32,
    ) -> Result<f32, JsValue> {
        let value = Reflect::get(parameters, &JsValue::from_str(key))?;
        if value.is_undefined() || value.is_null() {
            return Ok(default);
        }

        if let Some(array) = value.dyn_ref::<Float32Array>() {
            if array.length() == 0 {
                return Ok(default);
            }
            return Ok(array.get_index(0));
        }

        if let Some(number) = value.as_f64() {
            return Ok(number as f32);
        }

        Ok(default)
    }

    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    fn read_parameter_buffer(
        &self,
        parameters: &Object,
        key: &str,
        expected_len: usize,
        default: f32,
    ) -> Result<Vec<f32>, JsValue> {
        let expected_len = expected_len.max(1);
        let value = Reflect::get(parameters, &JsValue::from_str(key))?;
        if value.is_undefined() || value.is_null() {
            return Ok(vec![default; expected_len]);
        }

        if let Some(array) = value.dyn_ref::<Float32Array>() {
            let mut values = array.to_vec();
            if values.is_empty() {
                values = vec![default; expected_len];
            } else if values.len() == 1 && expected_len > 1 {
                values = vec![values[0]; expected_len];
            } else if values.len() < expected_len {
                let last = *values.last().unwrap_or(&default);
                values.resize(expected_len, last);
            } else if values.len() > expected_len {
                values.truncate(expected_len);
            }
            return Ok(values);
        }

        if let Some(number) = value.as_f64() {
            return Ok(vec![number as f32; expected_len]);
        }

        Ok(vec![default; expected_len])
    }

    #[cfg(all(feature = "wasm", target_arch = "wasm32"))]
    fn populate_from_js_object(
        &mut self,
        parameters: &Object,
        block_len_hint: usize,
    ) -> Result<(), JsValue> {
        // Determine gate buffer length from parameters (a-rate arrays) or fall back to hint.
        let mut gate_buffer_len = block_len_hint.max(1);
        let mut frequency_buffer_len = block_len_hint.max(1);
        for voice in 0..self.num_voices {
            let gate_key = format!("gate_{}", voice);
            let value = Reflect::get(parameters, &JsValue::from_str(&gate_key))?;
            if let Some(array) = value.dyn_ref::<Float32Array>() {
                let len = array.length() as usize;
                if len > 1 {
                    gate_buffer_len = len;
                }
            }

            let freq_key = format!("frequency_{}", voice);
            let freq_value = Reflect::get(parameters, &JsValue::from_str(&freq_key))?;
            if let Some(array) = freq_value.dyn_ref::<Float32Array>() {
                let len = array.length() as usize;
                if len > 1 {
                    frequency_buffer_len = len;
                }
            }
        }

        self.ensure_gate_buffer_len(gate_buffer_len);
        self.ensure_frequency_buffer_len(frequency_buffer_len);
        self.ensure_macro_dimensions(self.num_voices, self.macro_count, block_len_hint);
        self.reset_defaults();

        for voice in 0..self.num_voices {
            let gate_key = format!("gate_{}", voice);
            let freq_key = format!("frequency_{}", voice);
            let gain_key = format!("gain_{}", voice);
            let velocity_key = format!("velocity_{}", voice);

            let gate_values =
                self.read_parameter_buffer(parameters, &gate_key, gate_buffer_len, DEFAULT_GATE)?;
            let frequency_values = self.read_parameter_buffer(
                parameters,
                &freq_key,
                frequency_buffer_len,
                DEFAULT_FREQUENCY,
            )?;
            let gain = self.read_parameter_scalar(parameters, &gain_key, DEFAULT_GAIN)?;
            let velocity =
                self.read_parameter_scalar(parameters, &velocity_key, DEFAULT_VELOCITY)?;

            self.set_gate_buffer(voice, &gate_values);
            self.set_frequency_buffer(voice, &frequency_values);
            self.velocities[voice] = velocity;
            self.gains[voice] = gain;

            for macro_index in 0..self.macro_count {
                let macro_key = format!("macro_{}_{}", voice, macro_index);
                let value = self.read_parameter_scalar(parameters, &macro_key, 0.0)?;
                self.set_macro_value(voice, macro_index, value);
            }
        }

        Ok(())
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
#[cfg_attr(all(feature = "wasm", target_arch = "wasm32"), wasm_bindgen)]
impl AutomationFrame {
    #[cfg_attr(
        all(feature = "wasm", target_arch = "wasm32"),
        wasm_bindgen(constructor)
    )]
    pub fn new(num_voices: usize, macro_count: usize, macro_buffer_len: usize) -> AutomationFrame {
        AutomationFrame::with_dimensions(num_voices, macro_count, macro_buffer_len)
    }

    #[cfg_attr(
        all(feature = "wasm", target_arch = "wasm32"),
        wasm_bindgen(js_name = "populateFromParameters")
    )]
    pub fn populate_from_parameters(&mut self, parameters: &JsValue) -> Result<(), JsValue> {
        let object = parameters
            .dyn_ref::<Object>()
            .ok_or_else(|| JsValue::from_str("Expected parameter map object"))?;
        self.populate_from_js_object(object, self.gate_buffer_len)
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
pub(crate) trait ModulationEndpoint {
    fn connect_nodes_adapter(
        &mut self,
        from_node: &str,
        from_port: PortId,
        to_node: &str,
        to_port: PortId,
        amount: f32,
        modulation_type: Option<WasmModulationType>,
        modulation_transform: ModulationTransformation,
    ) -> Result<(), JsValue>;

    fn remove_specific_connection_adapter(
        &mut self,
        from_node: &str,
        to_node: &str,
        to_port: PortId,
    ) -> Result<(), JsValue>;
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl ModulationEndpoint for AudioEngine {
    fn connect_nodes_adapter(
        &mut self,
        from_node: &str,
        from_port: PortId,
        to_node: &str,
        to_port: PortId,
        amount: f32,
        modulation_type: Option<WasmModulationType>,
        modulation_transform: ModulationTransformation,
    ) -> Result<(), JsValue> {
        self.connect_nodes(
            from_node,
            from_port,
            to_node,
            to_port,
            amount,
            modulation_type,
            modulation_transform,
        )
    }

    fn remove_specific_connection_adapter(
        &mut self,
        from_node: &str,
        to_node: &str,
        to_port: PortId,
    ) -> Result<(), JsValue> {
        self.remove_specific_connection(from_node, to_node, to_port)
    }
}

#[cfg_attr(all(feature = "wasm", target_arch = "wasm32"), wasm_bindgen)]
#[derive(Debug, Clone, PartialEq)]
pub struct ConnectionUpdate {
    from_id: String,
    to_id: String,
    target: PortId,
    amount: f32,
    modulation_transformation: ModulationTransformation,
    is_removing: bool,
    modulation_type: Option<WasmModulationType>,
}

impl ConnectionUpdate {
    pub fn new(
        from_id: String,
        to_id: String,
        target: PortId,
        amount: f32,
        modulation_transformation: ModulationTransformation,
    ) -> Self {
        Self {
            from_id,
            to_id,
            target,
            amount,
            modulation_transformation,
            is_removing: false,
            modulation_type: None,
        }
    }

    pub fn with_modulation_type(mut self, modulation_type: WasmModulationType) -> Self {
        self.modulation_type = Some(modulation_type);
        self
    }

    pub fn mark_removal(mut self) -> Self {
        self.is_removing = true;
        self
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
fn apply_connection_update_internal<E: ModulationEndpoint>(
    engine: &mut E,
    update: &ConnectionUpdate,
) -> Result<(), JsValue> {
    if update.is_removing {
        engine.remove_specific_connection_adapter(&update.from_id, &update.to_id, update.target)
    } else {
        let modulation_type = update.modulation_type.or(Some(WasmModulationType::VCA));
        engine.connect_nodes_adapter(
            &update.from_id,
            PortId::AudioOutput0,
            &update.to_id,
            update.target,
            update.amount,
            modulation_type,
            update.modulation_transformation,
        )
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
#[cfg_attr(all(feature = "wasm", target_arch = "wasm32"), wasm_bindgen)]
pub struct AutomationAdapter {
    frame: AutomationFrame,
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
impl AutomationAdapter {
    pub fn with_frame(frame: AutomationFrame) -> Self {
        Self { frame }
    }

    pub fn frame_mut(&mut self) -> &mut AutomationFrame {
        &mut self.frame
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
#[cfg_attr(all(feature = "wasm", target_arch = "wasm32"), wasm_bindgen)]
impl AutomationAdapter {
    #[cfg_attr(
        all(feature = "wasm", target_arch = "wasm32"),
        wasm_bindgen(constructor)
    )]
    pub fn new(
        num_voices: usize,
        macro_count: usize,
        macro_buffer_len: usize,
    ) -> AutomationAdapter {
        AutomationAdapter {
            frame: AutomationFrame::with_dimensions(num_voices, macro_count, macro_buffer_len),
        }
    }

    #[cfg_attr(
        all(feature = "wasm", target_arch = "wasm32"),
        wasm_bindgen(js_name = "processBlock")
    )]
    pub fn process_block(
        &mut self,
        engine: &mut AudioEngine,
        parameters: &JsValue,
        master_gain: f32,
        output_left: &mut [f32],
        output_right: &mut [f32],
    ) -> Result<(), JsValue> {
        let object = parameters
            .dyn_ref::<Object>()
            .ok_or_else(|| JsValue::from_str("Expected parameter map object"))?;
        self.frame
            .populate_from_js_object(object, output_left.len())?;
        engine.process_with_frame(&self.frame, master_gain, output_left, output_right);
        Ok(())
    }

    #[cfg_attr(
        all(feature = "wasm", target_arch = "wasm32"),
        wasm_bindgen(js_name = "applyConnectionUpdate")
    )]
    pub fn apply_connection_update(
        &mut self,
        engine: &mut AudioEngine,
        update: &ConnectionUpdate,
    ) -> Result<(), JsValue> {
        apply_connection_update_internal(engine, update)
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
#[cfg_attr(all(feature = "wasm", target_arch = "wasm32"), wasm_bindgen)]
impl ConnectionUpdate {
    #[cfg_attr(
        all(feature = "wasm", target_arch = "wasm32"),
        wasm_bindgen(constructor)
    )]
    pub fn new_wasm(
        from_id: String,
        to_id: String,
        target: PortId,
        amount: f32,
        modulation_transformation: ModulationTransformation,
        is_removing: bool,
        modulation_type: Option<WasmModulationType>,
    ) -> ConnectionUpdate {
        ConnectionUpdate {
            from_id,
            to_id,
            target,
            amount,
            modulation_transformation,
            is_removing,
            modulation_type,
        }
    }

    #[cfg_attr(
        all(feature = "wasm", target_arch = "wasm32"),
        wasm_bindgen(getter, js_name = "fromId")
    )]
    pub fn from_id(&self) -> String {
        self.from_id.clone()
    }

    #[cfg_attr(
        all(feature = "wasm", target_arch = "wasm32"),
        wasm_bindgen(getter, js_name = "toId")
    )]
    pub fn to_id(&self) -> String {
        self.to_id.clone()
    }

    #[cfg_attr(all(feature = "wasm", target_arch = "wasm32"), wasm_bindgen(getter))]
    pub fn target(&self) -> PortId {
        self.target
    }

    #[cfg_attr(all(feature = "wasm", target_arch = "wasm32"), wasm_bindgen(getter))]
    pub fn amount(&self) -> f32 {
        self.amount
    }

    #[cfg_attr(
        feature = "wasm",
        wasm_bindgen(getter, js_name = "modulationTransformation")
    )]
    pub fn modulation_transformation(&self) -> ModulationTransformation {
        self.modulation_transformation
    }

    #[cfg_attr(
        all(feature = "wasm", target_arch = "wasm32"),
        wasm_bindgen(getter, js_name = "isRemoving")
    )]
    pub fn is_removing(&self) -> bool {
        self.is_removing
    }

    #[cfg_attr(
        all(feature = "wasm", target_arch = "wasm32"),
        wasm_bindgen(getter, js_name = "modulationType")
    )]
    pub fn modulation_type(&self) -> Option<WasmModulationType> {
        self.modulation_type
    }
}

#[cfg(all(feature = "wasm", target_arch = "wasm32"))]
#[cfg_attr(
    all(feature = "wasm", target_arch = "wasm32"),
    wasm_bindgen(js_name = "apply_modulation_update")
)]
pub fn apply_connection_update(
    engine: &mut AudioEngine,
    update: &ConnectionUpdate,
) -> Result<(), JsValue> {
    apply_connection_update_internal(engine, update)
}

#[cfg(all(test, feature = "wasm", target_arch = "wasm32"))]
mod tests {
    use super::*;
    use crate::graph::ModulationTransformation;
    #[test]
    fn frame_resizes_macro_buffers_for_block_length_changes() {
        let mut frame = AutomationFrame::with_dimensions(1, 4, 8);
        assert_eq!(frame.macro_buffer_len(), 8);

        // Simulate an audio host whose quantum is larger than the adapter's
        // initial buffer size. This must resize every per-voice dimension.
        frame.ensure_macro_dimensions(1, 4, 128);

        assert_eq!(frame.macro_buffer_len(), 128);
        assert_eq!(frame.macro_slice(0, 3).len(), 128);
        assert_eq!(frame.macro_buffers.len(), 512);

        frame.set_macro_value(0, 0, 0.5);
        assert!(frame.macro_slice(0, 0).iter().all(|&v| v == 0.5));
    }

    #[test]
    fn macro_expansion_matches_worklet_behavior() {
        let mut frame = AutomationFrame::with_dimensions(2, 4, 8);
        frame.set_voice_values(0, 1.0, 330.0, 0.5, 0.75);
        frame.set_macro_value(0, 0, 0.25);
        frame.set_macro_value(0, 1, 0.5);
        frame.set_macro_value(0, 2, 0.75);
        frame.set_macro_value(0, 3, 1.0);

        let gate_slice = frame.gate_slice(0);
        assert_eq!(gate_slice.len(), frame.gate_buffer_len());
        assert!(gate_slice.iter().all(|&value| (value - 1.0).abs() < 1e-6));

        let frequency_slice = frame.frequency_slice(0);
        assert_eq!(frequency_slice.len(), frame.frequency_buffer_len());
        assert!(frequency_slice
            .iter()
            .all(|&value| (value - 330.0).abs() < 1e-6));

        let macro_slice = frame.macro_slice(0, 2);
        assert_eq!(macro_slice.len(), 8);
        assert!(macro_slice.iter().all(|&value| (value - 0.75).abs() < 1e-6));

        // Ensure other voices remain at defaults
        assert!(frame.macro_slice(1, 3).iter().all(|&value| value == 0.0));
        assert_eq!(frame.frequencies()[0], 330.0);
        assert_eq!(frame.gains()[0], 0.75);
        assert_eq!(frame.velocities()[0], 0.5);
        assert_eq!(frame.gates()[0], 1.0);
        assert_eq!(
            frame.frequencies()[frame.frequency_buffer_len()],
            DEFAULT_FREQUENCY
        );
        assert_eq!(frame.gains()[1], DEFAULT_GAIN);
    }

    #[derive(Default)]
    struct MockEngine {
        connections: Vec<(
            String,
            PortId,
            String,
            PortId,
            f32,
            Option<WasmModulationType>,
            ModulationTransformation,
        )>,
        removals: Vec<(String, String, PortId)>,
    }

    impl ModulationEndpoint for MockEngine {
        fn connect_nodes_adapter(
            &mut self,
            from_node: &str,
            from_port: PortId,
            to_node: &str,
            to_port: PortId,
            amount: f32,
            modulation_type: Option<WasmModulationType>,
            modulation_transform: ModulationTransformation,
        ) -> Result<(), JsValue> {
            self.connections.push((
                from_node.to_string(),
                from_port,
                to_node.to_string(),
                to_port,
                amount,
                modulation_type,
                modulation_transform,
            ));
            Ok(())
        }

        fn remove_specific_connection_adapter(
            &mut self,
            from_node: &str,
            to_node: &str,
            to_port: PortId,
        ) -> Result<(), JsValue> {
            self.removals
                .push((from_node.to_string(), to_node.to_string(), to_port));
            Ok(())
        }
    }

    #[test]
    fn apply_connection_update_adds_connection_with_defaults() {
        let mut engine = MockEngine::default();
        let update = ConnectionUpdate::new(
            "1".to_string(),
            "2".to_string(),
            PortId::GainMod,
            0.75,
            ModulationTransformation::Square,
        );

        apply_connection_update_internal(&mut engine, &update).unwrap();

        assert_eq!(engine.removals.len(), 0);
        assert_eq!(engine.connections.len(), 1);
        let (from, from_port, to, to_port, amount, modulation_type, transform) =
            engine.connections[0].clone();
        assert_eq!(from, "1");
        assert_eq!(from_port, PortId::AudioOutput0);
        assert_eq!(to, "2");
        assert_eq!(to_port, PortId::GainMod);
        assert_eq!(amount, 0.75);
        assert_eq!(modulation_type, Some(WasmModulationType::VCA));
        assert_eq!(transform, ModulationTransformation::Square);
    }

    #[test]
    fn apply_connection_update_removes_connection() {
        let mut engine = MockEngine::default();
        let update = ConnectionUpdate::new(
            "4".to_string(),
            "7".to_string(),
            PortId::FrequencyMod,
            1.0,
            ModulationTransformation::None,
        )
        .mark_removal();

        apply_connection_update_internal(&mut engine, &update).unwrap();

        assert_eq!(engine.connections.len(), 0);
        assert_eq!(engine.removals.len(), 1);
        let (from, to, target) = engine.removals[0].clone();
        assert_eq!(from, "4");
        assert_eq!(to, "7");
        assert_eq!(target, PortId::FrequencyMod);
    }

    #[test]
    fn apply_connection_update_uses_custom_modulation_type() {
        let mut engine = MockEngine::default();
        let update = ConnectionUpdate::new(
            "9".to_string(),
            "12".to_string(),
            PortId::ResonanceMod,
            0.25,
            ModulationTransformation::Cube,
        )
        .with_modulation_type(WasmModulationType::Bipolar);

        apply_connection_update_internal(&mut engine, &update).unwrap();

        assert_eq!(engine.connections.len(), 1);
        let (_, _, _, _, _, modulation_type, _) = engine.connections[0];
        assert_eq!(modulation_type, Some(WasmModulationType::Bipolar));
    }
}
