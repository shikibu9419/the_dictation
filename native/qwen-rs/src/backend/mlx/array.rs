//! Safe Rust wrapper around MLX arrays.

use super::ffi;
use super::stream::default_stream;
use std::fmt;

/// Safe wrapper around an MLX array handle.
pub struct MlxArray {
    pub(crate) ptr: ffi::mlx_array,
}

impl Drop for MlxArray {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { ffi::mlx_array_free(self.ptr) };
        }
    }
}

impl Clone for MlxArray {
    fn clone(&self) -> Self {
        let mut new_ptr = unsafe { ffi::mlx_array_new() };
        super::error::check(
            unsafe { ffi::mlx_array_set(&mut new_ptr, self.ptr) },
            "mlx_array_set",
        );
        Self { ptr: new_ptr }
    }
}

impl fmt::Debug for MlxArray {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let shape = self.shape();
        let dtype = self.dtype();
        write!(f, "MlxArray(shape={:?}, dtype={:?})", shape, dtype)
    }
}

impl MlxArray {
    pub(crate) fn empty() -> Self {
        let ptr = unsafe { ffi::mlx_array_new() };
        Self { ptr }
    }

    pub(crate) fn from_raw(ptr: ffi::mlx_array) -> Self {
        assert!(!ptr.is_null(), "MLX array construction failed");
        Self { ptr }
    }

    // -- Creation --

    pub fn from_f32(data: &[f32], shape: &[i32]) -> Self {
        assert!(shape.len() <= i32::MAX as usize);
        let count = shape.iter().try_fold(1usize, |n, &d| {
            usize::try_from(d).ok().and_then(|d| n.checked_mul(d))
        });
        assert_eq!(
            count,
            Some(data.len()),
            "MLX shape does not match supplied data"
        );
        let ptr = unsafe {
            ffi::mlx_array_new_data(
                data.as_ptr() as *const _,
                shape.as_ptr(),
                shape.len() as i32,
                ffi::mlx_dtype::MLX_FLOAT32,
            )
        };
        Self::from_raw(ptr)
    }

    pub fn from_i64(data: &[i64], shape: &[i32]) -> Self {
        assert!(shape.len() <= i32::MAX as usize);
        let count = shape.iter().try_fold(1usize, |n, &d| {
            usize::try_from(d).ok().and_then(|d| n.checked_mul(d))
        });
        assert_eq!(
            count,
            Some(data.len()),
            "MLX shape does not match supplied data"
        );
        let ptr = unsafe {
            ffi::mlx_array_new_data(
                data.as_ptr() as *const _,
                shape.as_ptr(),
                shape.len() as i32,
                ffi::mlx_dtype::MLX_INT64,
            )
        };
        Self::from_raw(ptr)
    }

    pub fn from_i32(data: &[i32], shape: &[i32]) -> Self {
        assert!(shape.len() <= i32::MAX as usize);
        let count = shape.iter().try_fold(1usize, |n, &d| {
            usize::try_from(d).ok().and_then(|d| n.checked_mul(d))
        });
        assert_eq!(
            count,
            Some(data.len()),
            "MLX shape does not match supplied data"
        );
        let ptr = unsafe {
            ffi::mlx_array_new_data(
                data.as_ptr() as *const _,
                shape.as_ptr(),
                shape.len() as i32,
                ffi::mlx_dtype::MLX_INT32,
            )
        };
        Self::from_raw(ptr)
    }

    pub fn scalar_f32(val: f32) -> Self {
        let ptr = unsafe { ffi::mlx_array_new_float(val) };
        Self::from_raw(ptr)
    }

    pub fn zeros(shape: &[i32], dtype: ffi::mlx_dtype) -> Self {
        let mut res = Self::empty();
        let s = default_stream();
        super::error::check(
            unsafe { ffi::mlx_zeros(&mut res.ptr, shape.as_ptr(), shape.len(), dtype, s) },
            "mlx_zeros",
        );
        res
    }

    pub fn ones(shape: &[i32], dtype: ffi::mlx_dtype) -> Self {
        let mut res = Self::empty();
        let s = default_stream();
        super::error::check(
            unsafe { ffi::mlx_ones(&mut res.ptr, shape.as_ptr(), shape.len(), dtype, s) },
            "mlx_ones",
        );
        res
    }

    pub fn arange(start: f64, stop: f64, step: f64, dtype: ffi::mlx_dtype) -> Self {
        let mut res = Self::empty();
        let s = default_stream();
        super::error::check(
            unsafe { ffi::mlx_arange(&mut res.ptr, start, stop, step, dtype, s) },
            "mlx_arange",
        );
        res
    }

    pub fn full(shape: &[i32], val: &MlxArray, dtype: ffi::mlx_dtype) -> Self {
        let mut res = Self::empty();
        let s = default_stream();
        super::error::check(
            unsafe { ffi::mlx_full(&mut res.ptr, shape.as_ptr(), shape.len(), val.ptr, dtype, s) },
            "mlx_full",
        );
        res
    }

    // -- Metadata --

    pub fn ndim(&self) -> i32 {
        unsafe { ffi::mlx_array_ndim(self.ptr) as i32 }
    }

    pub fn shape(&self) -> Vec<i32> {
        let ndim = self.ndim();
        if ndim == 0 {
            return Vec::new();
        }
        let shape_ptr = unsafe { ffi::mlx_array_shape(self.ptr) };
        assert!(!shape_ptr.is_null());
        unsafe { std::slice::from_raw_parts(shape_ptr, ndim as usize).to_vec() }
    }

    pub fn size(&self) -> i32 {
        unsafe { ffi::mlx_array_size(self.ptr) as i32 }
    }

    pub fn dtype(&self) -> ffi::mlx_dtype {
        unsafe { ffi::mlx_array_dtype(self.ptr) }
    }

    // -- Evaluation and data access --

    pub fn eval(&self) {
        let status = unsafe { ffi::mlx_array_eval(self.ptr) };
        assert_eq!(status, 0, "mlx_array_eval failed");
    }

    pub fn to_vec_f32(&self) -> Vec<f32> {
        let array = self.astype(ffi::mlx_dtype::MLX_FLOAT32);
        let mut contiguous = Self::empty();
        let status =
            unsafe { ffi::mlx_contiguous(&mut contiguous.ptr, array.ptr, false, default_stream()) };
        assert_eq!(status, 0, "MLX contiguous failed");
        contiguous.eval();
        let size = contiguous.size() as usize;
        if size == 0 {
            return Vec::new();
        }
        let ptr = unsafe { ffi::mlx_array_data_float32(contiguous.ptr) };
        assert!(!ptr.is_null(), "MLX data access failed");
        unsafe { std::slice::from_raw_parts(ptr, size).to_vec() }
    }

    pub fn item_f32(&self) -> f32 {
        let array = self.astype(ffi::mlx_dtype::MLX_FLOAT32);
        array.eval();
        let mut val: f32 = Default::default();
        let status = unsafe { ffi::mlx_array_item_float32(&mut val, array.ptr) };
        assert_eq!(status, 0, "MLX scalar access failed");
        val
    }

    pub fn item_i64(&self) -> i64 {
        let array = self.astype(ffi::mlx_dtype::MLX_INT64);
        array.eval();
        let mut val: i64 = Default::default();
        let status = unsafe { ffi::mlx_array_item_int64(&mut val, array.ptr) };
        assert_eq!(status, 0, "MLX scalar access failed");
        val
    }

    // -- Type conversion --

    pub fn astype(&self, dtype: ffi::mlx_dtype) -> Self {
        let mut res = Self::empty();
        let s = default_stream();
        super::error::check(
            unsafe { ffi::mlx_astype(&mut res.ptr, self.ptr, dtype, s) },
            "mlx_astype",
        );
        res
    }
}
