//! Fuse elementwise activations as the Python MLX implementation does.
//! Closures and GPU arrays remain owned by the inference thread.
use super::{array::MlxArray, ffi, ops};
struct Unary(ffi::mlx_closure);
impl Drop for Unary {
    fn drop(&mut self) {
        unsafe {
            ffi::mlx_closure_free(self.0);
        }
    }
}
impl Unary {
    fn new(callback: unsafe extern "C" fn(*mut ffi::mlx_array, ffi::mlx_array) -> i32) -> Self {
        unsafe {
            let input = ffi::mlx_closure_new_unary(callback);
            let mut output = ffi::mlx_closure_new();
            let status = ffi::mlx_compile(&mut output, input, true);
            ffi::mlx_closure_free(input);
            assert_eq!(status, 0, "MLX activation compilation failed");
            Self(output)
        }
    }
    fn call(&self, a: &MlxArray) -> MlxArray {
        unsafe {
            let input = ffi::mlx_vector_array_new();
            assert_eq!(ffi::mlx_vector_array_append_value(input, a.ptr), 0);
            let mut output = ffi::mlx_vector_array_new();
            let status = ffi::mlx_closure_apply(&mut output, self.0, input);
            ffi::mlx_vector_array_free(input);
            let mut result = MlxArray::empty();
            let extracted = if status == 0 {
                ffi::mlx_vector_array_get(&mut result.ptr, output, 0)
            } else {
                status
            };
            ffi::mlx_vector_array_free(output);
            assert_eq!(extracted, 0, "MLX activation execution failed");
            result
        }
    }
}
unsafe fn invoke(
    res: *mut ffi::mlx_array,
    input: ffi::mlx_array,
    f: fn(&MlxArray) -> MlxArray,
) -> i32 {
    // Never unwind through C++; propagate failure to the checked closure call.
    std::panic::catch_unwind(|| {
        let mut owned = MlxArray::empty();
        let status = ffi::mlx_array_set(&mut owned.ptr, input);
        if status != 0 {
            return status;
        }
        let out = f(&owned);
        ffi::mlx_array_set(res, out.ptr)
    })
    .unwrap_or(1)
}
unsafe extern "C" fn gelu_callback(res: *mut ffi::mlx_array, a: ffi::mlx_array) -> i32 {
    invoke(res, a, ops::gelu_uncompiled)
}
unsafe extern "C" fn silu_callback(res: *mut ffi::mlx_array, a: ffi::mlx_array) -> i32 {
    invoke(res, a, ops::silu_uncompiled)
}
thread_local! {
    static GELU: Unary = Unary::new(gelu_callback);
    static SILU: Unary = Unary::new(silu_callback);
}
pub fn gelu(a: &MlxArray) -> MlxArray {
    GELU.with(|f| f.call(a))
}
pub fn silu(a: &MlxArray) -> MlxArray {
    SILU.with(|f| f.call(a))
}
