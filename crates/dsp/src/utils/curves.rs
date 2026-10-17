// src/utils/curves.rs
pub fn get_curved_value(position: f32, curve: f32) -> f32 {
    if curve.abs() < 0.001 {
        return position;
    }

    let alpha = (curve.abs()).exp();

    if curve > 0.0 {
        // Exponential curve
        ((position * (1.0 + alpha).ln()).exp() - 1.0) / alpha
    } else {
        // Logarithmic curve
        (1.0 + position * alpha).ln() / (1.0 + alpha).ln()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const EPSILON: f32 = 1e-6;

    #[test]
    fn test_linear_curve() {
        // When curve is near 0, should return linear (identity)
        assert!((get_curved_value(0.0, 0.0) - 0.0).abs() < EPSILON);
        assert!((get_curved_value(0.5, 0.0) - 0.5).abs() < EPSILON);
        assert!((get_curved_value(1.0, 0.0) - 1.0).abs() < EPSILON);

        // Small curve values should also be nearly linear
        assert!((get_curved_value(0.5, 0.0001) - 0.5).abs() < 0.01);
    }

    #[test]
    fn test_exponential_curve() {
        // Positive curve values create exponential curves
        let result_start = get_curved_value(0.0, 1.0);
        let result_mid = get_curved_value(0.5, 1.0);
        let result_end = get_curved_value(1.0, 1.0);

        // Start should be at 0
        assert!(result_start.abs() < EPSILON);
        // End should be at 1
        assert!((result_end - 1.0).abs() < 0.01);
        // Middle should be less than 0.5 (exponential curve)
        assert!(result_mid < 0.5);
    }

    #[test]
    fn test_logarithmic_curve() {
        // Negative curve values create logarithmic curves
        let result_start = get_curved_value(0.0, -1.0);
        let result_mid = get_curved_value(0.5, -1.0);
        let result_end = get_curved_value(1.0, -1.0);

        // Start should be at 0
        assert!(result_start.abs() < EPSILON);
        // End should be at 1
        assert!((result_end - 1.0).abs() < 0.01);
        // Middle should be greater than 0.5 (logarithmic curve)
        assert!(result_mid > 0.5);
    }

    #[test]
    fn test_boundary_values() {
        // Test at boundaries for various curve values
        for curve in [-2.0, -1.0, -0.5, 0.0, 0.5, 1.0, 2.0] {
            let result_0 = get_curved_value(0.0, curve);
            let result_1 = get_curved_value(1.0, curve);

            assert!(
                result_0.abs() < 0.01,
                "Curve {} at position 0.0 should be near 0, got {}",
                curve,
                result_0
            );
            assert!(
                (result_1 - 1.0).abs() < 0.01,
                "Curve {} at position 1.0 should be near 1, got {}",
                curve,
                result_1
            );
        }
    }

    #[test]
    fn test_monotonic_increasing() {
        // Curve should be monotonically increasing
        for curve in [-2.0, -1.0, 0.0, 1.0, 2.0] {
            let mut prev = get_curved_value(0.0, curve);
            for i in 1..=10 {
                let pos = i as f32 / 10.0;
                let curr = get_curved_value(pos, curve);
                assert!(
                    curr >= prev,
                    "Curve {} not monotonic at position {}",
                    curve,
                    pos
                );
                prev = curr;
            }
        }
    }

    #[test]
    fn test_finite_output() {
        // All outputs should be finite for valid inputs
        for curve in [-5.0, -2.0, -1.0, 0.0, 1.0, 2.0, 5.0] {
            for i in 0..=10 {
                let pos = i as f32 / 10.0;
                let result = get_curved_value(pos, curve);
                assert!(
                    result.is_finite(),
                    "Curve {} at position {} produced non-finite value",
                    curve,
                    pos
                );
            }
        }
    }
}
