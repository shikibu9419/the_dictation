//! Signal processing operations implemented via MLX primitives.

use super::array::MlxArray;
use super::ops;
use std::f64::consts::PI;

/// Create a Hann window of the given size.
pub fn hann_window(size: i32) -> MlxArray {
    assert!(size > 0);
    let values: Vec<f32> = (0..size)
        .map(|i| (0.5 - 0.5 * (2.0 * PI * i as f64 / size as f64).cos()) as f32)
        .collect();
    MlxArray::from_f32(&values, &[size])
}

/// Reflection padding for 1D signals.
pub fn reflection_pad1d(x: &MlxArray, pad_left: i32, pad_right: i32) -> MlxArray {
    let shape = x.shape();
    let ndim = shape.len() as i32;
    let t = *shape.last().unwrap();
    let last_axis = ndim - 1;

    let mut parts = Vec::new();

    if pad_left > 0 {
        let indices: Vec<i32> = (1..=pad_left).rev().collect();
        let idx = MlxArray::from_i32(&indices, &[pad_left]);
        let left = ops::take(x, &idx, last_axis);
        parts.push(left);
    }

    parts.push(x.clone());

    if pad_right > 0 {
        let indices: Vec<i32> = (0..pad_right).map(|i| t - 2 - i).collect();
        let idx = MlxArray::from_i32(&indices, &[pad_right]);
        let right = ops::take(x, &idx, last_axis);
        parts.push(right);
    }

    let refs: Vec<&MlxArray> = parts.iter().collect();
    ops::concatenate(&refs, last_axis)
}

/// Short-Time Fourier Transform magnitude.
///
/// Returns shape (n_frames, n_fft/2+1) as float32 abs values.
pub fn stft_magnitude(
    signal: &MlxArray,
    n_fft: i32,
    hop_length: i32,
    window: &MlxArray,
) -> MlxArray {
    let padded_len = signal.shape()[0];
    let n_frames = (padded_len - n_fft) / hop_length + 1;

    assert!(n_frames > 0 && n_fft > 0 && hop_length > 0);
    let mut frames = MlxArray::empty();
    let shape = [n_frames, n_fft];
    let strides = [hop_length as i64, 1];
    let status = unsafe {
        super::ffi::mlx_as_strided(
            &mut frames.ptr,
            signal.ptr,
            shape.as_ptr(),
            shape.len(),
            strides.as_ptr(),
            strides.len(),
            0,
            super::stream::default_stream(),
        )
    };
    assert_eq!(status, 0, "MLX STFT framing failed");
    let windowed = ops::multiply(&frames, window);
    ops::abs(&ops::rfft(&windowed, n_fft, -1))
}
