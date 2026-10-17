//! The maps a node's `process` gets: what reaches each input port and where
//! each output port writes. They are fixed arrays indexed by [`PortId`], so
//! building them for every node in every block never allocates.

use super::PortId;
use crate::graph::ModulationSource;

/// How many ports there are.
pub const PORT_COUNT: usize = PortId::ALL.len();

impl PortId {
    /// Every port, in declaration order.
    pub const ALL: [PortId; 28] = [
        PortId::AudioInput0,
        PortId::AudioInput1,
        PortId::AudioInput2,
        PortId::AudioInput3,
        PortId::AudioOutput0,
        PortId::AudioOutput1,
        PortId::AudioOutput2,
        PortId::AudioOutput3,
        PortId::GlobalGate,
        PortId::GlobalFrequency,
        PortId::GlobalVelocity,
        PortId::Frequency,
        PortId::FrequencyMod,
        PortId::PhaseMod,
        PortId::ModIndex,
        PortId::CutoffMod,
        PortId::ResonanceMod,
        PortId::GainMod,
        PortId::EnvelopeMod,
        PortId::StereoPan,
        PortId::FeedbackMod,
        PortId::DetuneMod,
        PortId::WavetableIndex,
        PortId::WetDryMix,
        PortId::AttackMod,
        PortId::ArpGate,
        PortId::CombinedGate,
        PortId::SampleOffset,
    ];

    /// The port's slot in a [`PortMap`].
    #[inline]
    pub fn index(self) -> usize {
        self as usize
    }
}

/// A map from ports to values with the API of the hash maps it replaces,
/// stored in a fixed array.
#[derive(Clone, Debug)]
pub struct PortMap<V> {
    slots: [Option<V>; PORT_COUNT],
    len: usize,
}

impl<V> Default for PortMap<V> {
    fn default() -> Self {
        Self {
            slots: std::array::from_fn(|_| None),
            len: 0,
        }
    }
}

impl<V> PortMap<V> {
    pub fn new() -> Self {
        Self::default()
    }

    #[inline]
    pub fn get(&self, port: &PortId) -> Option<&V> {
        self.slots[port.index()].as_ref()
    }

    #[inline]
    pub fn get_mut(&mut self, port: &PortId) -> Option<&mut V> {
        self.slots[port.index()].as_mut()
    }

    #[inline]
    pub fn contains_key(&self, port: &PortId) -> bool {
        self.slots[port.index()].is_some()
    }

    pub fn insert(&mut self, port: PortId, value: V) -> Option<V> {
        let old = self.slots[port.index()].replace(value);
        if old.is_none() {
            self.len += 1;
        }
        old
    }

    pub fn remove(&mut self, port: &PortId) -> Option<V> {
        let old = self.slots[port.index()].take();
        if old.is_some() {
            self.len -= 1;
        }
        old
    }

    /// The value at `port`, inserted from `make` if there is none.
    pub fn get_or_insert_with(&mut self, port: PortId, make: impl FnOnce() -> V) -> &mut V {
        let slot = &mut self.slots[port.index()];
        if slot.is_none() {
            self.len += 1;
        }
        slot.get_or_insert_with(make)
    }

    pub fn len(&self) -> usize {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }

    pub fn clear(&mut self) {
        for slot in &mut self.slots {
            *slot = None;
        }
        self.len = 0;
    }

    /// The ports that have values, in port order.
    pub fn iter(&self) -> impl Iterator<Item = (&PortId, &V)> {
        PortId::ALL
            .iter()
            .zip(&self.slots)
            .filter_map(|(port, slot)| slot.as_ref().map(|v| (port, v)))
    }

    pub fn iter_mut(&mut self) -> impl Iterator<Item = (&PortId, &mut V)> {
        PortId::ALL
            .iter()
            .zip(&mut self.slots)
            .filter_map(|(port, slot)| slot.as_mut().map(|v| (port, v)))
    }

    pub fn keys(&self) -> impl Iterator<Item = &PortId> {
        self.iter().map(|(port, _)| port)
    }

    pub fn values(&self) -> impl Iterator<Item = &V> {
        self.slots.iter().flatten()
    }

    pub fn values_mut(&mut self) -> impl Iterator<Item = &mut V> {
        self.slots.iter_mut().flatten()
    }

    /// Mutable references to several ports' values at once. Panics if a port
    /// is named twice, as `HashMap::get_disjoint_mut` does.
    pub fn get_disjoint_mut<const N: usize>(&mut self, ports: [&PortId; N]) -> [Option<&mut V>; N] {
        let indices = ports.map(|p| p.index());
        for (i, a) in indices.iter().enumerate() {
            assert!(
                !indices[..i].contains(a),
                "get_disjoint_mut: port {:?} named twice",
                ports[i]
            );
        }
        let mut out: [Option<&mut V>; N] = std::array::from_fn(|_| None);
        for (i, slot) in self.slots.iter_mut().enumerate() {
            if let Some(k) = indices.iter().position(|&index| index == i) {
                out[k] = slot.as_mut();
            }
        }
        out
    }
}

impl<V> FromIterator<(PortId, V)> for PortMap<V> {
    fn from_iter<I: IntoIterator<Item = (PortId, V)>>(iter: I) -> Self {
        let mut map = PortMap::new();
        map.extend(iter);
        map
    }
}

impl<V> Extend<(PortId, V)> for PortMap<V> {
    fn extend<I: IntoIterator<Item = (PortId, V)>>(&mut self, iter: I) {
        for (port, value) in iter {
            self.insert(port, value);
        }
    }
}

/// Where a node writes each output port for one block.
pub type Outputs<'a> = PortMap<&'a mut [f32]>;

/// The modulation sources reaching each of a node's input ports for one
/// block, in the order they were connected.
#[derive(Default)]
pub struct Inputs<'a> {
    ports: PortMap<&'a [ModulationSource<'a>]>,
}

impl<'a> Inputs<'a> {
    pub fn new() -> Self {
        Self::default()
    }

    /// The sources reaching `port`, if any are connected.
    #[inline]
    pub fn get(&self, port: &PortId) -> Option<&'a [ModulationSource<'a>]> {
        self.ports.get(port).copied()
    }

    #[inline]
    pub fn contains_key(&self, port: &PortId) -> bool {
        self.ports.contains_key(port)
    }

    pub fn insert(&mut self, port: PortId, sources: &'a [ModulationSource<'a>]) {
        self.ports.insert(port, sources);
    }

    pub fn len(&self) -> usize {
        self.ports.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ports.is_empty()
    }

    /// The connected ports and their sources, in port order.
    pub fn iter(&self) -> impl Iterator<Item = (&PortId, &'a [ModulationSource<'a>])> {
        self.ports.iter().map(|(port, sources)| (port, *sources))
    }
}

impl<'a> PortMap<Vec<ModulationSource<'a>>> {
    /// The sources as a node reads them, for maps built by hand (tests,
    /// one-off renders).
    pub fn as_inputs(&self) -> Inputs<'_> {
        let mut inputs = Inputs::new();
        for (port, sources) in self.iter() {
            inputs.insert(*port, sources);
        }
        inputs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ports_index_their_slots() {
        for (i, port) in PortId::ALL.iter().enumerate() {
            assert_eq!(port.index(), i, "{port:?}");
        }
    }

    #[test]
    fn a_port_map_behaves_like_the_hash_map() {
        let mut map: PortMap<u32> = PortMap::new();
        assert!(map.is_empty());
        assert_eq!(map.insert(PortId::GainMod, 1), None);
        assert_eq!(map.insert(PortId::GainMod, 2), Some(1));
        map.insert(PortId::AudioOutput0, 3);
        assert_eq!(map.len(), 2);
        assert_eq!(map.get(&PortId::GainMod), Some(&2));
        let [a, b, c] =
            map.get_disjoint_mut([&PortId::AudioOutput0, &PortId::GainMod, &PortId::PhaseMod]);
        *a.unwrap() += 10;
        *b.unwrap() += 20;
        assert!(c.is_none());
        let pairs: Vec<_> = map.iter().map(|(p, v)| (*p, *v)).collect();
        assert_eq!(pairs, [(PortId::AudioOutput0, 13), (PortId::GainMod, 22)]);
        assert_eq!(map.remove(&PortId::GainMod), Some(22));
        assert_eq!(map.len(), 1);
    }
}
