//! Effects on a bus.

use cubed_dsp::AudioNode;

/// An effect in a bus's insert chain. It processes the bus's stereo block in
/// place and must not allocate, lock or block: it runs on the audio thread.
pub trait Insert: Send {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]);

    /// Clears delay lines and envelopes, e.g. when the insert is re-enabled.
    fn reset(&mut self) {}
}

/// One of `cubed-dsp`'s effect nodes (reverb, delay, chorus, compressor,
/// limiter, ...) as an insert, through its `StereoEffect` path. A node that
/// has no stereo path passes the signal through.
pub struct DspInsert(Box<dyn AudioNode + Send>);

impl DspInsert {
    pub fn new(node: impl AudioNode + Send + 'static) -> DspInsert {
        DspInsert(Box::new(node))
    }

    /// The node, e.g. to downcast it and change its parameters before the
    /// insert is sent to the mixer.
    pub fn node_mut(&mut self) -> &mut dyn AudioNode {
        self.0.as_mut()
    }
}

impl Insert for DspInsert {
    fn process(&mut self, left: &mut [f32], right: &mut [f32]) {
        if let Some(effect) = self.0.as_stereo_effect() {
            effect.process_stereo(left, right);
        }
    }

    fn reset(&mut self) {
        self.0.reset();
    }
}
