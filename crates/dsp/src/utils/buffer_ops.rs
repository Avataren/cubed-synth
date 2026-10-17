use cubed_simd::{f32x4, Simd};

pub fn multiply_buffers(buffer1: &[f32], buffer2: &[f32], output: &mut [f32]) {
    debug_assert!(buffer1.len() >= output.len());
    debug_assert!(buffer2.len() >= output.len());

    let chunks = output.len() / 4;
    let remainder = output.len() % 4;

    // Process 4 elements at a time using SIMD
    for i in 0..chunks {
        let offset = i * 4;
        let a: f32x4 = Simd::from_slice(&buffer1[offset..offset + 4]);
        let b: f32x4 = Simd::from_slice(&buffer2[offset..offset + 4]);
        let result = a * b;
        result.copy_to_slice(&mut output[offset..offset + 4]);
    }

    // Handle remaining elements
    let start = chunks * 4;
    for i in 0..remainder {
        output[start + i] = buffer1[start + i] * buffer2[start + i];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_multiply_buffers() {
        let buffer1 = vec![1.0, 2.0, 3.0, 4.0, 5.0];
        let buffer2 = vec![0.5, 0.5, 0.5, 0.5, 0.5];
        let mut output = vec![0.0; 5];

        multiply_buffers(&buffer1, &buffer2, &mut output);

        assert_eq!(output, vec![0.5, 1.0, 1.5, 2.0, 2.5]);
    }
}
