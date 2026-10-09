//! MLX device and stream management.

use super::ffi;
use std::cell::OnceCell;

thread_local! { static DEFAULT_STREAM: OnceCell<MlxStream> = const { OnceCell::new() }; }

pub struct MlxStream {
    pub(crate) ptr: ffi::mlx_stream,
}

impl Drop for MlxStream {
    fn drop(&mut self) {
        if !self.ptr.is_null() {
            unsafe { ffi::mlx_stream_free(self.ptr) };
        }
    }
}

pub fn init_mlx(use_gpu: bool) {
    super::error::install();
    DEFAULT_STREAM.with(|cell| {
        cell.get_or_init(|| {
            let device_type = if use_gpu {
                let mut available = false;
                super::error::check(
                    unsafe { ffi::mlx_metal_is_available(&mut available) },
                    "mlx_metal_is_available",
                );
                if available {
                    ffi::mlx_device_type::MLX_GPU
                } else {
                    eprintln!("Warning: Metal GPU not available, falling back to CPU");
                    ffi::mlx_device_type::MLX_CPU
                }
            } else {
                ffi::mlx_device_type::MLX_CPU
            };

            let device = unsafe { ffi::mlx_device_new_type(device_type, 0) };
            super::error::check(
                unsafe { ffi::mlx_set_default_device(device) },
                "mlx_set_default_device",
            );

            let stream = unsafe { ffi::mlx_stream_new_device(device) };
            unsafe { ffi::mlx_device_free(device) };

            assert!(!stream.is_null(), "MLX stream initialization failed");
            MlxStream { ptr: stream }
        })
        .ptr
    });
}

pub fn default_stream() -> ffi::mlx_stream {
    DEFAULT_STREAM.with(|cell| {
        cell.get()
            .expect("MLX not initialized. Call init_mlx() first.")
            .ptr
    })
}

pub fn synchronize() {
    let stream = default_stream();
    super::error::check(unsafe { ffi::mlx_synchronize(stream) }, "mlx_synchronize");
}
