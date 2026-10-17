// use std::any::Any;
// use std::collections::FxHashMap;

// use crate::biquad::{Biquad, CascadedBiquad, Filter, FilterType};
// use crate::graph::ModulationSource;
// use crate::traits::{AudioNode, PortId};

// /// Choose between 12 dB/octave (single biquad) and 24 dB/octave (cascaded biquad) slopes.
// pub enum FilterOrder {
//     TwelveDb,
//     TwentyFourDb,
// }

// /// Helper to create a filter with the specified type, order, and parameters.
// fn create_filter(
//     filter_type: FilterType,
//     order: FilterOrder,
//     sample_rate: f32,
//     frequency: f32,
//     Q: f32,
//     gain_db: f32,
// ) -> Box<dyn Filter> {
//     match order {
//         FilterOrder::TwelveDb => {
//             Box::new(Biquad::new(filter_type, sample_rate, frequency, Q, gain_db))
//         }
//         FilterOrder::TwentyFourDb => Box::new(CascadedBiquad::new(
//             filter_type,
//             sample_rate,
//             frequency,
//             Q,
//             gain_db,
//         )),
//     }
// }

// /// A real three-band equalizer node that processes stereo audio.
// /// This EQ cascades a low-shelf, a peaking, and a high-shelf filter per channel.
// pub struct Equalizer {
//     enabled: bool,
//     sample_rate: f32,
//     // Left channel filters.
//     low_left: Box<dyn Filter>,
//     mid_left: Box<dyn Filter>,
//     high_left: Box<dyn Filter>,
//     // Right channel filters.
//     low_right: Box<dyn Filter>,
//     mid_right: Box<dyn Filter>,
//     high_right: Box<dyn Filter>,
// }

// impl Equalizer {
//     /// Creates a new three-band EQ node.
//     ///
//     /// * `sample_rate` - The audio sample rate.
//     /// * `low_gain_db`, `mid_gain_db`, `high_gain_db` - Gains (in dB) for each band.
//     /// * `order` - Select 12 dB/octave (TwelveDb) or 24 dB/octave (TwentyFourDb) slopes.
//     pub fn new(
//         sample_rate: f32,
//         low_gain_db: f32,
//         mid_gain_db: f32,
//         high_gain_db: f32,
//         order: FilterOrder,
//     ) -> Self {
//         // Default center frequencies and Q factors.
//         let low_freq = 200.0;
//         let mid_freq = 1000.0;
//         let high_freq = 5000.0;
//         let shelf_Q = 0.707;
//         let mid_Q = 1.0;

//         let low_left = create_filter(
//             FilterType::LowShelf,
//             order,
//             sample_rate,
//             low_freq,
//             shelf_Q,
//             low_gain_db,
//         );
//         let mid_left = create_filter(
//             FilterType::Peaking,
//             order,
//             sample_rate,
//             mid_freq,
//             mid_Q,
//             mid_gain_db,
//         );
//         let high_left = create_filter(
//             FilterType::HighShelf,
//             order,
//             sample_rate,
//             high_freq,
//             shelf_Q,
//             high_gain_db,
//         );

//         let low_right = create_filter(
//             FilterType::LowShelf,
//             order,
//             sample_rate,
//             low_freq,
//             shelf_Q,
//             low_gain_db,
//         );
//         let mid_right = create_filter(
//             FilterType::Peaking,
//             order,
//             sample_rate,
//             mid_freq,
//             mid_Q,
//             mid_gain_db,
//         );
//         let high_right = create_filter(
//             FilterType::HighShelf,
//             order,
//             sample_rate,
//             high_freq,
//             shelf_Q,
//             high_gain_db,
//         );

//         Self {
//             enabled: true,
//             sample_rate,
//             low_left,
//             mid_left,
//             high_left,
//             low_right,
//             mid_right,
//             high_right,
//         }
//     }

//     /// Resets all filter states.
//     pub fn reset_filters(&mut self) {
//         self.low_left.reset();
//         self.mid_left.reset();
//         self.high_left.reset();
//         self.low_right.reset();
//         self.mid_right.reset();
//         self.high_right.reset();
//     }
// }

// impl AudioNode for Equalizer {
//     fn get_ports(&self) -> FxHashMap<PortId, bool> {
//         let mut ports = FxHashMap::new();
//         // Stereo audio inputs.
//         ports.insert(PortId::AudioInput0, false);
//         ports.insert(PortId::AudioInput1, false);
//         // Stereo audio outputs.
//         ports.insert(PortId::AudioOutput0, true);
//         ports.insert(PortId::AudioOutput1, true);
//         ports
//     }

//     fn process<'a>(
//         &mut self,
//         inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
//         outputs: &mut FxHashMap<PortId, &mut [f32]>,
//         buffer_size: usize,
//     ) {
//         // Retrieve input buffers.
//         let left_in = inputs.get(&PortId::AudioInput0).unwrap()[0]
//             .buffer
//             .as_slice();
//         let right_in = inputs.get(&PortId::AudioInput1).unwrap()[0]
//             .buffer
//             .as_slice();
//         // Retrieve output buffers using nightly's get_many_mut.
//         let outs = outputs.get_many_mut([&PortId::AudioOutput0, &PortId::AudioOutput1]);
//         let [Some(out_left), Some(out_right)] = outs else {
//             panic!("Missing stereo output buffers");
//         };
//         let out_left: &mut [f32] = *out_left;
//         let out_right: &mut [f32] = *out_right;

//         // Process each sample through the cascaded filters.
//         for i in 0..buffer_size {
//             // Left channel.
//             let sample_left = left_in[i];
//             let low_out_left = self.low_left.process(sample_left);
//             let mid_out_left = self.mid_left.process(low_out_left);
//             let eq_sample_left = self.high_left.process(mid_out_left);
//             // Right channel.
//             let sample_right = right_in[i];
//             let low_out_right = self.low_right.process(sample_right);
//             let mid_out_right = self.mid_right.process(low_out_right);
//             let eq_sample_right = self.high_right.process(mid_out_right);

//             out_left[i] = eq_sample_left;
//             out_right[i] = eq_sample_right;
//         }
//     }

//     fn reset(&mut self) {
//         self.reset_filters();
//     }

//     fn as_any_mut(&mut self) -> &mut dyn Any {
//         self
//     }

//     fn as_any(&self) -> &dyn Any {
//         self
//     }

//     fn is_active(&self) -> bool {
//         self.enabled
//     }

//     fn set_active(&mut self, active: bool) {
//         self.enabled = active;
//         if active {
//             self.reset();
//         }
//     }

//     fn node_type(&self) -> &str {
//         "equalizer"
//     }
// }

// /// A notch filter node implemented using a biquad (or cascaded biquad) with FilterType::Notch.
// pub struct NotchFilter {
//     enabled: bool,
//     sample_rate: f32,
//     // Left and right channel filters.
//     left: Box<dyn Filter>,
//     right: Box<dyn Filter>,
// }

// impl NotchFilter {
//     /// Creates a new notch filter node.
//     ///
//     /// * `sample_rate` - The audio sample rate.
//     /// * `notch_freq` - The center frequency to notch out.
//     /// * `Q` - The quality factor of the notch.
//     /// * `order` - Choose between 12 dB/octave (TwelveDb) or 24 dB/octave (TwentyFourDb).
//     pub fn new(sample_rate: f32, notch_freq: f32, Q: f32, order: FilterOrder) -> Self {
//         let left = create_filter(FilterType::Notch, order, sample_rate, notch_freq, Q, 0.0);
//         let right = create_filter(FilterType::Notch, order, sample_rate, notch_freq, Q, 0.0);
//         Self {
//             enabled: true,
//             sample_rate,
//             left,
//             right,
//         }
//     }

//     pub fn reset_filters(&mut self) {
//         self.left.reset();
//         self.right.reset();
//     }
// }

// impl AudioNode for NotchFilter {
//     fn get_ports(&self) -> FxHashMap<PortId, bool> {
//         let mut ports = FxHashMap::new();
//         ports.insert(PortId::AudioInput0, false);
//         ports.insert(PortId::AudioInput1, false);
//         ports.insert(PortId::AudioOutput0, true);
//         ports.insert(PortId::AudioOutput1, true);
//         ports
//     }

//     fn process<'a>(
//         &mut self,
//         inputs: &FxHashMap<PortId, Vec<ModulationSource<'a>>>,
//         outputs: &mut FxHashMap<PortId, &mut [f32]>,
//         buffer_size: usize,
//     ) {
//         let left_in = inputs.get(&PortId::AudioInput0).unwrap()[0]
//             .buffer
//             .as_slice();
//         let right_in = inputs.get(&PortId::AudioInput1).unwrap()[0]
//             .buffer
//             .as_slice();
//         let outs = outputs.get_many_mut([&PortId::AudioOutput0, &PortId::AudioOutput1]);
//         let [Some(out_left), Some(out_right)] = outs else {
//             panic!("Missing stereo output buffers");
//         };
//         let out_left: &mut [f32] = *out_left;
//         let out_right: &mut [f32] = *out_right;

//         for i in 0..buffer_size {
//             let processed_left = self.left.process(left_in[i]);
//             let processed_right = self.right.process(right_in[i]);
//             out_left[i] = processed_left;
//             out_right[i] = processed_right;
//         }
//     }

//     fn reset(&mut self) {
//         self.reset_filters();
//     }

//     fn as_any_mut(&mut self) -> &mut dyn Any {
//         self
//     }

//     fn as_any(&self) -> &dyn Any {
//         self
//     }

//     fn is_active(&self) -> bool {
//         self.enabled
//     }

//     fn set_active(&mut self, active: bool) {
//         self.enabled = active;
//         if active {
//             self.reset();
//         }
//     }

//     fn node_type(&self) -> &str {
//         "notch_filter"
//     }
// }
