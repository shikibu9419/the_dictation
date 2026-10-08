//! Raw FFI declarations for the mlx-c library.

#![allow(non_camel_case_types)]
#![allow(dead_code)]

use std::os::raw::{c_char, c_float, c_int, c_void};

// ---------------------------------------------------------------------------
// Opaque types
// ---------------------------------------------------------------------------

pub type mlx_array = *mut c_void;
pub type mlx_stream = *mut c_void;
pub type mlx_device = *mut c_void;
pub type mlx_map_string_to_array = *mut c_void;
pub type mlx_map_string_to_string = *mut c_void;
pub type mlx_map_string_to_array_iterator = *mut c_void;
pub type mlx_vector_array = *mut c_void;
pub type mlx_vector_int = *mut c_void;
pub type mlx_string = *mut c_void;
pub type mlx_closure = *mut c_void;

// ---------------------------------------------------------------------------
// Enums
// ---------------------------------------------------------------------------

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum mlx_dtype {
    MLX_BOOL = 0,
    MLX_UINT8 = 1,
    MLX_UINT16 = 2,
    MLX_UINT32 = 3,
    MLX_UINT64 = 4,
    MLX_INT8 = 5,
    MLX_INT16 = 6,
    MLX_INT32 = 7,
    MLX_INT64 = 8,
    MLX_FLOAT16 = 9,
    MLX_FLOAT32 = 10,
    MLX_FLOAT64 = 11,
    MLX_BFLOAT16 = 12,
    MLX_COMPLEX64 = 13,
}

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum mlx_device_type {
    MLX_CPU = 0,
    MLX_GPU = 1,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct mlx_optional_float {
    pub value: c_float,
    pub has_value: bool,
}

extern "C" {
    // -----------------------------------------------------------------------
    // Array lifecycle
    // -----------------------------------------------------------------------

    pub fn mlx_array_new() -> mlx_array;
    pub fn mlx_array_free(arr: mlx_array) -> c_int;
    pub fn mlx_array_set(arr: *mut mlx_array, src: mlx_array) -> c_int;

    // -----------------------------------------------------------------------
    // Array creation
    // -----------------------------------------------------------------------

    pub fn mlx_array_new_data(
        data: *const c_void,
        shape: *const c_int,
        ndim: c_int,
        dtype: mlx_dtype,
    ) -> mlx_array;

    pub fn mlx_array_new_float(val: c_float) -> mlx_array;

    // -----------------------------------------------------------------------
    // Array metadata
    // -----------------------------------------------------------------------

    pub fn mlx_array_ndim(arr: mlx_array) -> usize;
    pub fn mlx_array_shape(arr: mlx_array) -> *const c_int;
    pub fn mlx_array_size(arr: mlx_array) -> usize;
    pub fn mlx_array_dtype(arr: mlx_array) -> mlx_dtype;

    // -----------------------------------------------------------------------
    // Array evaluation and data access
    // -----------------------------------------------------------------------

    pub fn mlx_array_eval(arr: mlx_array) -> c_int;
    pub fn mlx_array_data_float32(arr: mlx_array) -> *const c_float;

    pub fn mlx_array_item_float32(res: *mut c_float, arr: mlx_array) -> c_int;

    pub fn mlx_array_item_int64(res: *mut i64, arr: mlx_array) -> c_int;

    // -----------------------------------------------------------------------
    // Device
    // -----------------------------------------------------------------------

    pub fn mlx_device_new_type(dtype: mlx_device_type, index: c_int) -> mlx_device;
    pub fn mlx_device_free(dev: mlx_device) -> c_int;

    pub fn mlx_set_default_device(dev: mlx_device) -> c_int;
    pub fn mlx_metal_is_available(res: *mut bool) -> c_int;

    // -----------------------------------------------------------------------
    // Stream
    // -----------------------------------------------------------------------

    pub fn mlx_stream_new_device(dev: mlx_device) -> mlx_stream;
    pub fn mlx_stream_free(stream: mlx_stream) -> c_int;

    pub fn mlx_synchronize(stream: mlx_stream) -> c_int;

    // -----------------------------------------------------------------------
    // Core ops
    // -----------------------------------------------------------------------

    // Arithmetic
    pub fn mlx_add(res: *mut mlx_array, a: mlx_array, b: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_subtract(res: *mut mlx_array, a: mlx_array, b: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_multiply(res: *mut mlx_array, a: mlx_array, b: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_divide(res: *mut mlx_array, a: mlx_array, b: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_negative(res: *mut mlx_array, a: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_abs(res: *mut mlx_array, a: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_power(res: *mut mlx_array, a: mlx_array, b: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_maximum(res: *mut mlx_array, a: mlx_array, b: mlx_array, s: mlx_stream) -> c_int;

    pub fn mlx_clip(
        res: *mut mlx_array,
        a: mlx_array,
        min: mlx_array,
        max: mlx_array,
        s: mlx_stream,
    ) -> c_int;

    // Matrix multiplication
    pub fn mlx_matmul(res: *mut mlx_array, a: mlx_array, b: mlx_array, s: mlx_stream) -> c_int;

    // Shape manipulation
    pub fn mlx_reshape(
        res: *mut mlx_array,
        a: mlx_array,
        shape: *const c_int,
        shape_num: usize,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_transpose_axes(
        res: *mut mlx_array,
        a: mlx_array,
        axes: *const c_int,
        axes_num: usize,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_swapaxes(
        res: *mut mlx_array,
        a: mlx_array,
        axis1: c_int,
        axis2: c_int,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_expand_dims(res: *mut mlx_array, a: mlx_array, axis: c_int, s: mlx_stream) -> c_int;

    pub fn mlx_squeeze_axes(
        res: *mut mlx_array,
        a: mlx_array,
        axes: *const c_int,
        axes_num: usize,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_slice(
        res: *mut mlx_array,
        a: mlx_array,
        start: *const c_int,
        start_num: usize,
        stop: *const c_int,
        stop_num: usize,
        strides: *const c_int,
        strides_num: usize,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_broadcast_to(
        res: *mut mlx_array,
        a: mlx_array,
        shape: *const c_int,
        shape_num: usize,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_contiguous(
        res: *mut mlx_array,
        a: mlx_array,
        allow_col_major: bool,
        s: mlx_stream,
    ) -> c_int;

    // Concatenation
    pub fn mlx_concatenate_axis(
        res: *mut mlx_array,
        arrays: mlx_vector_array,
        axis: c_int,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_stack_axis(
        res: *mut mlx_array,
        arrays: mlx_vector_array,
        axis: c_int,
        s: mlx_stream,
    ) -> c_int;

    // Creation ops
    pub fn mlx_zeros(
        res: *mut mlx_array,
        shape: *const c_int,
        shape_num: usize,
        dtype: mlx_dtype,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_ones(
        res: *mut mlx_array,
        shape: *const c_int,
        shape_num: usize,
        dtype: mlx_dtype,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_full(
        res: *mut mlx_array,
        shape: *const c_int,
        shape_num: usize,
        val: mlx_array,
        dtype: mlx_dtype,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_arange(
        res: *mut mlx_array,
        start: f64,
        stop: f64,
        step: f64,
        dtype: mlx_dtype,
        s: mlx_stream,
    ) -> c_int;

    // Indexing
    pub fn mlx_take_axis(
        res: *mut mlx_array,
        a: mlx_array,
        indices: mlx_array,
        axis: c_int,
        s: mlx_stream,
    ) -> c_int;

    // Reduction (with axes)
    pub fn mlx_sum_axes(
        res: *mut mlx_array,
        a: mlx_array,
        axes: *const c_int,
        axes_num: usize,
        keepdims: bool,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_mean_axes(
        res: *mut mlx_array,
        a: mlx_array,
        axes: *const c_int,
        axes_num: usize,
        keepdims: bool,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_var_axes(
        res: *mut mlx_array,
        a: mlx_array,
        axes: *const c_int,
        axes_num: usize,
        keepdims: bool,
        ddof: c_int,
        s: mlx_stream,
    ) -> c_int;

    // Reduction (all axes)

    pub fn mlx_max(res: *mut mlx_array, a: mlx_array, keepdims: bool, s: mlx_stream) -> c_int;

    pub fn mlx_argmax_axis(
        res: *mut mlx_array,
        a: mlx_array,
        axis: c_int,
        keepdims: bool,
        s: mlx_stream,
    ) -> c_int;

    // Math functions
    pub fn mlx_exp(res: *mut mlx_array, a: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_log(res: *mut mlx_array, a: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_sqrt(res: *mut mlx_array, a: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_rsqrt(res: *mut mlx_array, a: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_sin(res: *mut mlx_array, a: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_cos(res: *mut mlx_array, a: mlx_array, s: mlx_stream) -> c_int;
    pub fn mlx_sigmoid(res: *mut mlx_array, a: mlx_array, s: mlx_stream) -> c_int;

    // Activation-related
    pub fn mlx_softmax_axes(
        res: *mut mlx_array,
        a: mlx_array,
        axes: *const c_int,
        axes_num: usize,
        precise: bool,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_where(
        res: *mut mlx_array,
        condition: mlx_array,
        x: mlx_array,
        y: mlx_array,
        s: mlx_stream,
    ) -> c_int;

    // Comparison

    // Triangular
    pub fn mlx_triu(res: *mut mlx_array, a: mlx_array, k: c_int, s: mlx_stream) -> c_int;

    // Top-k

    // Sort / argsort

    // Type conversion
    pub fn mlx_astype(res: *mut mlx_array, a: mlx_array, dtype: mlx_dtype, s: mlx_stream) -> c_int;

    // Convolution

    pub fn mlx_conv2d(
        res: *mut mlx_array,
        input: mlx_array,
        weight: mlx_array,
        stride_0: c_int,
        stride_1: c_int,
        padding_0: c_int,
        padding_1: c_int,
        dilation_0: c_int,
        dilation_1: c_int,
        groups: c_int,
        s: mlx_stream,
    ) -> c_int;

    // Pad

    // -----------------------------------------------------------------------
    // Fast ML ops
    // -----------------------------------------------------------------------

    pub fn mlx_fast_rms_norm(
        res: *mut mlx_array,
        x: mlx_array,
        weight: mlx_array,
        eps: c_float,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_fast_layer_norm(
        res: *mut mlx_array,
        x: mlx_array,
        weight: mlx_array,
        bias: mlx_array,
        eps: c_float,
        s: mlx_stream,
    ) -> c_int;

    pub fn mlx_fast_scaled_dot_product_attention(
        res: *mut mlx_array,
        queries: mlx_array,
        keys: mlx_array,
        values: mlx_array,
        scale: c_float,
        mask_mode: *const c_char,
        mask_arr: mlx_array,
        sinks: mlx_array,
        s: mlx_stream,
    ) -> c_int;

    // -----------------------------------------------------------------------
    // FFT
    // -----------------------------------------------------------------------

    pub fn mlx_fft_rfft(
        res: *mut mlx_array,
        a: mlx_array,
        n: c_int,
        axis: c_int,
        s: mlx_stream,
    ) -> c_int;

    // -----------------------------------------------------------------------
    // I/O
    // -----------------------------------------------------------------------

    pub fn mlx_load_safetensors(
        data: *mut mlx_map_string_to_array,
        metadata: *mut mlx_map_string_to_string,
        path: *const c_char,
        s: mlx_stream,
    ) -> c_int;

    // -----------------------------------------------------------------------
    // Map
    // -----------------------------------------------------------------------

    pub fn mlx_map_string_to_array_free(map: mlx_map_string_to_array) -> c_int;
    pub fn mlx_map_string_to_array_iterator_new(
        map: mlx_map_string_to_array,
    ) -> mlx_map_string_to_array_iterator;
    pub fn mlx_map_string_to_array_iterator_next(
        key: *mut *const c_char,
        value: *mut mlx_array,
        it: mlx_map_string_to_array_iterator,
    ) -> c_int;
    pub fn mlx_map_string_to_array_iterator_free(it: mlx_map_string_to_array_iterator) -> c_int;

    pub fn mlx_map_string_to_string_free(map: mlx_map_string_to_string) -> c_int;

    // -----------------------------------------------------------------------
    // Vector
    // -----------------------------------------------------------------------

    pub fn mlx_vector_array_new() -> mlx_vector_array;
    pub fn mlx_vector_array_free(vec: mlx_vector_array) -> c_int;
    pub fn mlx_vector_array_append_value(vec: mlx_vector_array, val: mlx_array) -> c_int;
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct mlx_optional_int {
    pub value: c_int,
    pub has_value: bool,
}
#[repr(C)]
#[derive(Clone, Copy)]
pub struct mlx_optional_dtype {
    pub value: mlx_dtype,
    pub has_value: bool,
}
extern "C" {
    pub fn mlx_quantized_matmul(
        res: *mut mlx_array,
        x: mlx_array,
        w: mlx_array,
        scales: mlx_array,
        biases: mlx_array,
        transpose: bool,
        group_size: mlx_optional_int,
        bits: mlx_optional_int,
        mode: *const c_char,
        stream: mlx_stream,
    ) -> c_int;
    pub fn mlx_dequantize(
        res: *mut mlx_array,
        w: mlx_array,
        scales: mlx_array,
        biases: mlx_array,
        group_size: mlx_optional_int,
        bits: mlx_optional_int,
        mode: *const c_char,
        dtype: mlx_optional_dtype,
        stream: mlx_stream,
    ) -> c_int;
}

extern "C" {
    pub fn mlx_erf(res: *mut mlx_array, a: mlx_array, s: mlx_stream) -> c_int;
}

extern "C" {
    pub fn mlx_as_strided(
        res: *mut mlx_array,
        a: mlx_array,
        shape: *const c_int,
        shape_num: usize,
        strides: *const i64,
        strides_num: usize,
        offset: usize,
        s: mlx_stream,
    ) -> c_int;
}

extern "C" {
    pub fn mlx_closure_new_unary(
        fun: unsafe extern "C" fn(*mut mlx_array, mlx_array) -> c_int,
    ) -> mlx_closure;
    pub fn mlx_closure_new() -> mlx_closure;
    pub fn mlx_closure_free(closure: mlx_closure) -> c_int;
    pub fn mlx_closure_apply(
        res: *mut mlx_vector_array,
        closure: mlx_closure,
        input: mlx_vector_array,
    ) -> c_int;
    pub fn mlx_compile(res: *mut mlx_closure, fun: mlx_closure, shapeless: bool) -> c_int;
    pub fn mlx_vector_array_get(
        res: *mut mlx_array,
        vector: mlx_vector_array,
        index: usize,
    ) -> c_int;
}

extern "C" {
    pub fn mlx_set_error_handler(
        handler: unsafe extern "C" fn(*const c_char, *mut c_void),
        data: *mut c_void,
        dtor: Option<unsafe extern "C" fn(*mut c_void)>,
    );
}
