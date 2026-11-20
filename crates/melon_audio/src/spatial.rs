//! Distance attenuation and stereo panning: pure curves, no device.

use std::time::Duration;

use glam::Vec3;

/// Clips shorter than this decode to memory; longer ones stream from disk.
pub const STREAM_THRESHOLD: Duration = Duration::from_secs(5);

pub fn should_stream(duration: Duration) -> bool {
    duration >= STREAM_THRESHOLD
}

/// Gain 0..1 at `distance`: 1 inside `min`, 0 at `max` and beyond, in
/// between the inverse-distance falloff `min / d`, shifted so it reaches 0
/// exactly at `max`.
pub fn attenuation(distance: f32, min: f32, max: f32) -> f32 {
    if distance.is_nan() || max.is_nan() {
        return 1.0;
    }
    let min = min.max(0.01);
    if distance <= min {
        return 1.0;
    }
    if distance >= max {
        return 0.0;
    }
    let floor = min / max;
    ((min / distance - floor) / (1.0 - floor)).clamp(0.0, 1.0)
}

/// Pan -1 (left) to 1 (right): the sine of the angle between the listener's
/// facing and the source, so straight ahead or behind is 0. `right` is the
/// listener's right vector.
pub fn pan(listener: Vec3, right: Vec3, source: Vec3) -> f32 {
    let to_source = source - listener;
    if to_source.length_squared().is_nan() || to_source.length_squared() < 1e-8 {
        return 0.0;
    }
    to_source
        .normalize()
        .dot(right.normalize_or_zero())
        .clamp(-1.0, 1.0)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn full_inside_min_silent_at_max() {
        assert_eq!(attenuation(0.0, 2.0, 20.0), 1.0);
        assert_eq!(attenuation(2.0, 2.0, 20.0), 1.0);
        assert_eq!(attenuation(20.0, 2.0, 20.0), 0.0);
        assert_eq!(attenuation(99.0, 2.0, 20.0), 0.0);
    }

    #[test]
    fn falls_off_monotonically_in_between() {
        // min 1, max 10: d = 2 -> (0.5 - 0.1) / 0.9
        assert!((attenuation(2.0, 1.0, 10.0) - 0.4 / 0.9).abs() < 1e-6);
        let mut last = 1.0;
        for i in 1..=20 {
            let g = attenuation(1.0 + i as f32 * 0.45, 1.0, 10.0);
            assert!(g <= last && (0.0..=1.0).contains(&g), "{g}");
            last = g;
        }
    }

    #[test]
    fn degenerate_ranges_do_not_blow_up() {
        assert_eq!(
            attenuation(5.0, 10.0, 3.0),
            1.0,
            "max under min: inside min"
        );
        assert_eq!(attenuation(11.0, 10.0, 3.0), 0.0);
        assert!(attenuation(0.5, 0.0, 10.0).is_finite());
    }

    #[test]
    fn nan_and_negative_inputs_stay_finite() {
        for (d, min, max) in [
            (f32::NAN, 1.0, 10.0),
            (5.0, f32::NAN, 10.0),
            (5.0, 1.0, f32::NAN),
            (-3.0, -1.0, 10.0),
            (5.0, 1.0, -2.0),
            (f32::INFINITY, 1.0, 10.0),
        ] {
            let g = attenuation(d, min, max);
            assert!((0.0..=1.0).contains(&g), "{d} {min} {max} -> {g}");
        }
        assert!(pan(Vec3::NAN, Vec3::X, Vec3::ONE).is_finite());
        assert!(pan(Vec3::ZERO, Vec3::NAN, Vec3::ONE).is_finite());
    }

    #[test]
    fn pan_follows_side() {
        let (at, right) = (Vec3::ZERO, Vec3::X);
        assert!((pan(at, right, Vec3::new(5.0, 0.0, 0.0)) - 1.0).abs() < 1e-6);
        assert!((pan(at, right, Vec3::new(-5.0, 0.0, 0.0)) + 1.0).abs() < 1e-6);
        assert_eq!(pan(at, right, Vec3::new(0.0, 0.0, -5.0)), 0.0);
        assert_eq!(pan(at, right, Vec3::new(0.0, 0.0, 5.0)), 0.0);
        let half = pan(at, right, Vec3::new(1.0, 0.0, -1.0));
        assert!((half - 0.5f32.sqrt()).abs() < 1e-6);
    }

    #[test]
    fn pan_on_top_of_listener_is_centre() {
        assert_eq!(pan(Vec3::ONE, Vec3::X, Vec3::ONE), 0.0);
    }

    #[test]
    fn streaming_threshold() {
        assert!(!should_stream(Duration::from_millis(4999)));
        assert!(should_stream(Duration::from_secs(5)));
    }
}
