//! Safe wrappers for MLX array operations.
#![allow(dead_code)]

use super::array::MlxArray;
use super::ffi;
use super::stream::default_stream;

// ---------------------------------------------------------------------------
// Arithmetic
// ---------------------------------------------------------------------------

pub fn add(a: &MlxArray, b: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_add(&mut res.ptr, a.ptr, b.ptr, default_stream()) },
        "mlx_add",
    );
    res
}

pub fn subtract(a: &MlxArray, b: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_subtract(&mut res.ptr, a.ptr, b.ptr, default_stream()) },
        "mlx_subtract",
    );
    res
}

pub fn multiply(a: &MlxArray, b: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_multiply(&mut res.ptr, a.ptr, b.ptr, default_stream()) },
        "mlx_multiply",
    );
    res
}

pub fn divide(a: &MlxArray, b: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_divide(&mut res.ptr, a.ptr, b.ptr, default_stream()) },
        "mlx_divide",
    );
    res
}

pub fn negative(a: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_negative(&mut res.ptr, a.ptr, default_stream()) },
        "mlx_negative",
    );
    res
}

pub fn abs(a: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_abs(&mut res.ptr, a.ptr, default_stream()) },
        "mlx_abs",
    );
    res
}

pub fn power(a: &MlxArray, b: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_power(&mut res.ptr, a.ptr, b.ptr, default_stream()) },
        "mlx_power",
    );
    res
}

pub fn maximum(a: &MlxArray, b: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_maximum(&mut res.ptr, a.ptr, b.ptr, default_stream()) },
        "mlx_maximum",
    );
    res
}

pub fn clip(a: &MlxArray, min: &MlxArray, max: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_clip(&mut res.ptr, a.ptr, min.ptr, max.ptr, default_stream()) },
        "mlx_clip",
    );
    res
}

// ---------------------------------------------------------------------------
// Matrix multiplication
// ---------------------------------------------------------------------------

pub fn matmul(a: &MlxArray, b: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_matmul(&mut res.ptr, a.ptr, b.ptr, default_stream()) },
        "mlx_matmul",
    );
    res
}

// ---------------------------------------------------------------------------
// Shape manipulation
// ---------------------------------------------------------------------------

pub fn reshape(a: &MlxArray, shape: &[i32]) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe {
            ffi::mlx_reshape(
                &mut res.ptr,
                a.ptr,
                shape.as_ptr(),
                shape.len(),
                default_stream(),
            )
        },
        "mlx_reshape",
    );
    res
}

pub fn transpose(a: &MlxArray, axes: &[i32]) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe {
            ffi::mlx_transpose_axes(
                &mut res.ptr,
                a.ptr,
                axes.as_ptr(),
                axes.len(),
                default_stream(),
            )
        },
        "mlx_transpose_axes",
    );
    res
}

pub fn swapaxes(a: &MlxArray, axis1: i32, axis2: i32) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_swapaxes(&mut res.ptr, a.ptr, axis1, axis2, default_stream()) },
        "mlx_swapaxes",
    );
    res
}

pub fn expand_dims(a: &MlxArray, axes: &[i32]) -> MlxArray {
    let mut result = a.clone();
    for &axis in axes {
        let mut res = MlxArray::empty();
        super::error::check(
            unsafe { ffi::mlx_expand_dims(&mut res.ptr, result.ptr, axis, default_stream()) },
            "mlx_expand_dims",
        );
        result = res;
    }
    result
}

pub fn squeeze(a: &MlxArray, axes: &[i32]) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe {
            ffi::mlx_squeeze_axes(
                &mut res.ptr,
                a.ptr,
                axes.as_ptr(),
                axes.len(),
                default_stream(),
            )
        },
        "mlx_squeeze_axes",
    );
    res
}

pub fn slice(a: &MlxArray, start: &[i32], stop: &[i32], strides: &[i32]) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe {
            ffi::mlx_slice(
                &mut res.ptr,
                a.ptr,
                start.as_ptr(),
                start.len(),
                stop.as_ptr(),
                stop.len(),
                strides.as_ptr(),
                strides.len(),
                default_stream(),
            )
        },
        "mlx_slice",
    );
    res
}

pub fn broadcast_to(a: &MlxArray, shape: &[i32]) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe {
            ffi::mlx_broadcast_to(
                &mut res.ptr,
                a.ptr,
                shape.as_ptr(),
                shape.len(),
                default_stream(),
            )
        },
        "mlx_broadcast_to",
    );
    res
}

// ---------------------------------------------------------------------------
// Concatenation
// ---------------------------------------------------------------------------

pub fn concatenate(arrays: &[&MlxArray], axis: i32) -> MlxArray {
    let vec = unsafe { ffi::mlx_vector_array_new() };
    for a in arrays {
        super::error::check(
            unsafe { ffi::mlx_vector_array_append_value(vec, a.ptr) },
            "mlx_vector_array_append_value",
        );
    }
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_concatenate_axis(&mut res.ptr, vec, axis, default_stream()) },
        "mlx_concatenate_axis",
    );
    unsafe { ffi::mlx_vector_array_free(vec) };
    res
}

pub fn stack(arrays: &[&MlxArray], axis: i32) -> MlxArray {
    let vec = unsafe { ffi::mlx_vector_array_new() };
    for a in arrays {
        super::error::check(
            unsafe { ffi::mlx_vector_array_append_value(vec, a.ptr) },
            "mlx_vector_array_append_value",
        );
    }
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_stack_axis(&mut res.ptr, vec, axis, default_stream()) },
        "mlx_stack_axis",
    );
    unsafe { ffi::mlx_vector_array_free(vec) };
    res
}

// ---------------------------------------------------------------------------
// Indexing
// ---------------------------------------------------------------------------

pub fn take(a: &MlxArray, indices: &MlxArray, axis: i32) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_take_axis(&mut res.ptr, a.ptr, indices.ptr, axis, default_stream()) },
        "mlx_take_axis",
    );
    res
}

// ---------------------------------------------------------------------------
// Reduction
// ---------------------------------------------------------------------------

pub fn sum(a: &MlxArray, axes: &[i32], keepdims: bool) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe {
            ffi::mlx_sum_axes(
                &mut res.ptr,
                a.ptr,
                axes.as_ptr(),
                axes.len(),
                keepdims,
                default_stream(),
            )
        },
        "mlx_sum_axes",
    );
    res
}

pub fn mean(a: &MlxArray, axes: &[i32], keepdims: bool) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe {
            ffi::mlx_mean_axes(
                &mut res.ptr,
                a.ptr,
                axes.as_ptr(),
                axes.len(),
                keepdims,
                default_stream(),
            )
        },
        "mlx_mean_axes",
    );
    res
}

pub fn var(a: &MlxArray, axes: &[i32], keepdims: bool, ddof: i32) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe {
            ffi::mlx_var_axes(
                &mut res.ptr,
                a.ptr,
                axes.as_ptr(),
                axes.len(),
                keepdims,
                ddof,
                default_stream(),
            )
        },
        "mlx_var_axes",
    );
    res
}

pub fn max_all(a: &MlxArray, keepdims: bool) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_max(&mut res.ptr, a.ptr, keepdims, default_stream()) },
        "mlx_max",
    );
    res
}

pub fn argmax(a: &MlxArray, axis: i32, keepdims: bool) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_argmax_axis(&mut res.ptr, a.ptr, axis, keepdims, default_stream()) },
        "mlx_argmax_axis",
    );
    res
}

// ---------------------------------------------------------------------------
// Math functions
// ---------------------------------------------------------------------------

pub fn exp(a: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_exp(&mut res.ptr, a.ptr, default_stream()) },
        "mlx_exp",
    );
    res
}

pub fn log(a: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_log(&mut res.ptr, a.ptr, default_stream()) },
        "mlx_log",
    );
    res
}

pub fn sqrt(a: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_sqrt(&mut res.ptr, a.ptr, default_stream()) },
        "mlx_sqrt",
    );
    res
}

pub fn rsqrt(a: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_rsqrt(&mut res.ptr, a.ptr, default_stream()) },
        "mlx_rsqrt",
    );
    res
}

pub fn sin(a: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_sin(&mut res.ptr, a.ptr, default_stream()) },
        "mlx_sin",
    );
    res
}

pub fn cos(a: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_cos(&mut res.ptr, a.ptr, default_stream()) },
        "mlx_cos",
    );
    res
}

pub fn sigmoid(a: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_sigmoid(&mut res.ptr, a.ptr, default_stream()) },
        "mlx_sigmoid",
    );
    res
}

// ---------------------------------------------------------------------------
// Activation helpers
// ---------------------------------------------------------------------------

pub fn softmax(a: &MlxArray, axes: &[i32]) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe {
            ffi::mlx_softmax_axes(
                &mut res.ptr,
                a.ptr,
                axes.as_ptr(),
                axes.len(),
                true,
                default_stream(),
            )
        },
        "mlx_softmax_axes",
    );
    res
}

pub(crate) fn silu_uncompiled(a: &MlxArray) -> MlxArray {
    let sig = sigmoid(a);
    multiply(a, &sig)
}

pub(crate) fn gelu_uncompiled(a: &MlxArray) -> MlxArray {
    let scaled = multiply(
        a,
        &MlxArray::scalar_f32(std::f32::consts::FRAC_1_SQRT_2).astype(a.dtype()),
    );
    let mut erf = MlxArray::empty();
    let status = unsafe { ffi::mlx_erf(&mut erf.ptr, scaled.ptr, default_stream()) };
    assert_eq!(status, 0, "MLX erf failed");
    multiply(
        &multiply(a, &MlxArray::scalar_f32(0.5).astype(a.dtype())),
        &add(&erf, &MlxArray::scalar_f32(1.0).astype(a.dtype())),
    )
}

// ---------------------------------------------------------------------------
// Comparison / logical
// ---------------------------------------------------------------------------

pub fn where_cond(cond: &MlxArray, x: &MlxArray, y: &MlxArray) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_where(&mut res.ptr, cond.ptr, x.ptr, y.ptr, default_stream()) },
        "mlx_where",
    );
    res
}

// ---------------------------------------------------------------------------
// Triangular
// ---------------------------------------------------------------------------

pub fn triu(a: &MlxArray, k: i32) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_triu(&mut res.ptr, a.ptr, k, default_stream()) },
        "mlx_triu",
    );
    res
}

// ---------------------------------------------------------------------------
// Convolution
// ---------------------------------------------------------------------------

pub fn conv2d(
    input: &MlxArray,
    weight: &MlxArray,
    stride: [i32; 2],
    padding: [i32; 2],
    dilation: [i32; 2],
    groups: i32,
) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe {
            ffi::mlx_conv2d(
                &mut res.ptr,
                input.ptr,
                weight.ptr,
                stride[0],
                stride[1],
                padding[0],
                padding[1],
                dilation[0],
                dilation[1],
                groups,
                default_stream(),
            )
        },
        "mlx_conv2d",
    );
    res
}

// ---------------------------------------------------------------------------
// Padding
// ---------------------------------------------------------------------------

// ---------------------------------------------------------------------------
// Fast ML ops
// ---------------------------------------------------------------------------

pub fn fast_rms_norm(x: &MlxArray, weight: &MlxArray, eps: f32) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_fast_rms_norm(&mut res.ptr, x.ptr, weight.ptr, eps, default_stream()) },
        "mlx_fast_rms_norm",
    );
    res
}

pub fn fast_layer_norm(
    x: &MlxArray,
    weight: &MlxArray,
    bias: Option<&MlxArray>,
    eps: f32,
) -> MlxArray {
    let mut res = MlxArray::empty();
    let bias_ptr = bias.map_or(std::ptr::null_mut(), |b| b.ptr);
    super::error::check(
        unsafe {
            ffi::mlx_fast_layer_norm(
                &mut res.ptr,
                x.ptr,
                weight.ptr,
                bias_ptr,
                eps,
                default_stream(),
            )
        },
        "mlx_fast_layer_norm",
    );
    res
}

pub fn fast_sdpa(
    queries: &MlxArray,
    keys: &MlxArray,
    values: &MlxArray,
    scale: f32,
    mask: Option<&MlxArray>,
) -> MlxArray {
    let mut res = MlxArray::empty();
    let mask_ptr = mask.map_or(std::ptr::null_mut() as ffi::mlx_array, |m| m.ptr);
    let sinks = std::ptr::null_mut() as ffi::mlx_array;
    // mask_mode: "" (empty) when no mask, "array" when mask tensor is provided
    let mask_mode = if mask.is_some() {
        c"array".as_ptr()
    } else {
        c"".as_ptr()
    };
    super::error::check(
        unsafe {
            ffi::mlx_fast_scaled_dot_product_attention(
                &mut res.ptr,
                queries.ptr,
                keys.ptr,
                values.ptr,
                scale,
                mask_mode,
                mask_ptr,
                sinks,
                default_stream(),
            )
        },
        "mlx_fast_scaled_dot_product_attention",
    );
    res
}

// ---------------------------------------------------------------------------
// FFT
// ---------------------------------------------------------------------------

pub fn rfft(a: &MlxArray, n: i32, axis: i32) -> MlxArray {
    let mut res = MlxArray::empty();
    super::error::check(
        unsafe { ffi::mlx_fft_rfft(&mut res.ptr, a.ptr, n, axis, default_stream()) },
        "mlx_fft_rfft",
    );
    res
}

// ---------------------------------------------------------------------------
// Top-k and sorting
// ---------------------------------------------------------------------------

pub fn gelu(a: &MlxArray) -> MlxArray {
    super::compiled::gelu(a)
}
pub fn silu(a: &MlxArray) -> MlxArray {
    super::compiled::silu(a)
}
