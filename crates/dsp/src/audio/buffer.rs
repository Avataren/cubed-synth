use cubed_simd::f32x4;

pub struct AudioInput<'a> {
    buffer: Option<&'a [f32]>,
    default_value: f32,
}

impl<'a> AudioInput<'a> {
    pub fn new(buffer: Option<&'a [f32]>, default_value: f32) -> Self {
        Self {
            buffer,
            default_value,
        }
    }

    // pub fn get(&self, index: usize) -> f32 {
    //     self.buffer
    //         .and_then(|b| b.get(index))
    //         .copied()
    //         .unwrap_or(self.default_value)
    // }

    pub fn get_simd(&self, index: usize) -> f32x4 {
        match self.buffer {
            Some(buffer) if index + 4 <= buffer.len() => {
                f32x4::from_slice(&buffer[index..index + 4])
            }
            Some(buffer) => {
                let mut values = [self.default_value; 4];
                for i in 0..4 {
                    if index + i < buffer.len() {
                        values[i] = buffer[index + i];
                    }
                }
                f32x4::from_array(values)
            }
            None => f32x4::splat(self.default_value),
        }
    }
}

pub struct AudioOutput<'a> {
    pub(crate) buffer: &'a mut [f32],
}

impl<'a> AudioOutput<'a> {
    pub fn new(buffer: &'a mut [f32]) -> Self {
        Self { buffer }
    }

    pub fn write_simd(&mut self, index: usize, values: f32x4) {
        if index + 4 <= self.buffer.len() {
            values.copy_to_slice(&mut self.buffer[index..index + 4]);
        } else {
            let array = values.to_array();
            let n = self.buffer.len().saturating_sub(index);
            if n > 0 {
                self.buffer[index..index + n].copy_from_slice(&array[..n]);
            }
        }
    }
}
