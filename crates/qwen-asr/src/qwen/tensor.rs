// Derived from second-state/qwen3_asr_rs; Apache-2.0. See NOTICE.
//! MLX tensor operations used by the Qwen model.

// ---------------------------------------------------------------------------
// DType — data type abstraction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DType {
    Float32,
    Float16,
    BFloat16,
    Int64,
    Int32,
    Bool,
}

impl From<DType> for crate::qwen::backend::mlx::ffi::mlx_dtype {
    fn from(dt: DType) -> Self {
        use crate::qwen::backend::mlx::ffi::mlx_dtype::*;
        match dt {
            DType::Float32 => MLX_FLOAT32,
            DType::Float16 => MLX_FLOAT16,
            DType::BFloat16 => MLX_BFLOAT16,
            DType::Int64 => MLX_INT64,
            DType::Int32 => MLX_INT32,
            DType::Bool => MLX_BOOL,
        }
    }
}

impl From<crate::qwen::backend::mlx::ffi::mlx_dtype> for DType {
    fn from(dt: crate::qwen::backend::mlx::ffi::mlx_dtype) -> Self {
        use crate::qwen::backend::mlx::ffi::mlx_dtype::*;
        match dt {
            MLX_FLOAT32 | MLX_FLOAT64 => DType::Float32,
            MLX_FLOAT16 => DType::Float16,
            MLX_BFLOAT16 => DType::BFloat16,
            MLX_INT64 => DType::Int64,
            MLX_INT32 | MLX_INT8 | MLX_INT16 => DType::Int32,
            MLX_BOOL => DType::Bool,
            _ => DType::Float32,
        }
    }
}

// ---------------------------------------------------------------------------
// Device — compute device abstraction
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Device {
    Cpu,
    Gpu(usize),
}

impl Device {
    pub fn gpu() -> Self {
        Device::Gpu(0)
    }
}

// ---------------------------------------------------------------------------
// Tensor — unified tensor type
// ---------------------------------------------------------------------------

#[derive(Debug)]
pub struct Tensor {
    pub(crate) inner: crate::qwen::backend::mlx::array::MlxArray,
}

impl Clone for Tensor {
    fn clone(&self) -> Self {
        Tensor {
            inner: self.inner.clone(),
        }
    }
}

#[allow(dead_code)]
impl Tensor {
    pub fn from_mlx(a: crate::qwen::backend::mlx::array::MlxArray) -> Self {
        Tensor { inner: a }
    }

    // -- Creation --

    pub fn from_slice_f32(data: &[f32]) -> Self {
        let shape = [data.len() as i32];
        Tensor::from_mlx(crate::qwen::backend::mlx::array::MlxArray::from_f32(
            data, &shape,
        ))
    }

    pub fn from_slice_i64(data: &[i64]) -> Self {
        let shape = [data.len() as i32];
        Tensor::from_mlx(crate::qwen::backend::mlx::array::MlxArray::from_i64(
            data, &shape,
        ))
    }

    pub fn zeros(shape: &[i64], dtype: DType, _device: Device) -> Self {
        let shape_i32: Vec<i32> = shape.iter().map(|&s| s as i32).collect();
        Tensor::from_mlx(crate::qwen::backend::mlx::array::MlxArray::zeros(
            &shape_i32,
            dtype.into(),
        ))
    }

    pub fn ones(shape: &[i64], dtype: DType, _device: Device) -> Self {
        let shape_i32: Vec<i32> = shape.iter().map(|&s| s as i32).collect();
        Tensor::from_mlx(crate::qwen::backend::mlx::array::MlxArray::ones(
            &shape_i32,
            dtype.into(),
        ))
    }

    pub fn full(shape: &[i64], val: f64, dtype: DType, _device: Device) -> Self {
        let shape_i32: Vec<i32> = shape.iter().map(|&s| s as i32).collect();
        let val_arr = crate::qwen::backend::mlx::array::MlxArray::scalar_f32(val as f32);
        Tensor::from_mlx(crate::qwen::backend::mlx::array::MlxArray::full(
            &shape_i32,
            &val_arr,
            dtype.into(),
        ))
    }

    pub fn arange(start: i64, end: i64, _device: Device) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::array::MlxArray::arange(
            start as f64,
            end as f64,
            1.0,
            crate::qwen::backend::mlx::ffi::mlx_dtype::MLX_INT64,
        ))
    }

    pub fn cat(tensors: &[Tensor], dim: i64) -> Self {
        let refs: Vec<&crate::qwen::backend::mlx::array::MlxArray> =
            tensors.iter().map(|t| &t.inner).collect();
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::concatenate(
            &refs, dim as i32,
        ))
    }

    pub fn stack(tensors: &[Tensor], dim: i64) -> Self {
        let refs: Vec<&crate::qwen::backend::mlx::array::MlxArray> =
            tensors.iter().map(|t| &t.inner).collect();
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::stack(&refs, dim as i32))
    }

    pub fn embedding(weight: &Tensor, indices: &Tensor) -> Self {
        // Embedding is just take(weight, indices, axis=0)
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::take(
            &weight.inner,
            &indices.inner,
            0,
        ))
    }

    pub fn hann_window(size: i64, _device: Device) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::signal::hann_window(size as i32))
    }

    // -- Shape --

    pub fn size(&self) -> Vec<i64> {
        self.inner.shape().iter().map(|&s| s as i64).collect()
    }

    pub fn size3(&self) -> (i64, i64, i64) {
        let s = self.size();
        (s[0], s[1], s[2])
    }

    pub fn size4(&self) -> (i64, i64, i64, i64) {
        let s = self.size();
        (s[0], s[1], s[2], s[3])
    }

    pub fn dim(&self) -> usize {
        self.inner.ndim() as usize
    }

    pub fn view(&self, shape: &[i64]) -> Self {
        let shape_i32: Vec<i32> = shape.iter().map(|&s| s as i32).collect();
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::reshape(
            &self.inner,
            &shape_i32,
        ))
    }

    pub fn reshape(&self, shape: &[i64]) -> Self {
        self.view(shape)
    }

    pub fn narrow(&self, dim: i64, start: i64, len: i64) -> Self {
        let ndim = self.inner.ndim();
        let dim = if dim < 0 { ndim as i64 + dim } else { dim } as i32;
        let shape = self.inner.shape();
        let mut starts = vec![0i32; ndim as usize];
        let mut stops: Vec<i32> = shape.clone();
        let strides = vec![1i32; ndim as usize];
        starts[dim as usize] = start as i32;
        stops[dim as usize] = (start + len) as i32;
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::slice(
            &self.inner,
            &starts,
            &stops,
            &strides,
        ))
    }

    pub fn unsqueeze(&self, dim: i64) -> Self {
        let dim = if dim < 0 {
            self.inner.ndim() as i64 + dim + 1
        } else {
            dim
        } as i32;
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::expand_dims(
            &self.inner,
            &[dim],
        ))
    }

    pub fn squeeze_dim(&self, dim: i64) -> Self {
        let dim = if dim < 0 {
            self.inner.ndim() as i64 + dim
        } else {
            dim
        } as i32;
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::squeeze(&self.inner, &[dim]))
    }

    pub fn transpose(&self, dim0: i64, dim1: i64) -> Self {
        let ndim = self.inner.ndim();
        let dim0 = if dim0 < 0 { ndim as i64 + dim0 } else { dim0 } as i32;
        let dim1 = if dim1 < 0 { ndim as i64 + dim1 } else { dim1 } as i32;
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::swapaxes(
            &self.inner,
            dim0,
            dim1,
        ))
    }

    pub fn permute(&self, dims: &[i64]) -> Self {
        let dims_i32: Vec<i32> = dims.iter().map(|&d| d as i32).collect();
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::transpose(
            &self.inner,
            &dims_i32,
        ))
    }

    pub fn tr(&self) -> Self {
        self.transpose(-2, -1)
    }

    pub fn get(&self, index: i64) -> Self {
        self.select(0, index)
    }

    pub fn select(&self, dim: i64, index: i64) -> Self {
        let idx = crate::qwen::backend::mlx::array::MlxArray::from_i32(&[index as i32], &[1]);
        let dim = if dim < 0 {
            self.inner.ndim() as i64 + dim
        } else {
            dim
        } as i32;
        let taken = crate::qwen::backend::mlx::ops::take(&self.inner, &idx, dim);
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::squeeze(&taken, &[dim]))
    }

    // -- Arithmetic --

    pub fn matmul(&self, other: &Tensor) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::matmul(
            &self.inner,
            &other.inner,
        ))
    }

    pub fn pow_scalar(&self, exp: f64) -> Self {
        let exp_arr = crate::qwen::backend::mlx::array::MlxArray::scalar_f32(exp as f32)
            .astype(self.inner.dtype());
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::power(&self.inner, &exp_arr))
    }

    pub fn clamp_min(&self, min: f64) -> Self {
        let min_arr = crate::qwen::backend::mlx::array::MlxArray::scalar_f32(min as f32)
            .astype(self.inner.dtype());
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::maximum(
            &self.inner,
            &min_arr,
        ))
    }

    pub fn maximum(&self, other: &Tensor) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::maximum(
            &self.inner,
            &other.inner,
        ))
    }

    // -- Math --

    pub fn abs(&self) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::abs(&self.inner))
    }

    pub fn square(&self) -> Self {
        self.pow_scalar(2.0)
    }

    pub fn sqrt(&self) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::sqrt(&self.inner))
    }

    pub fn rsqrt(&self) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::rsqrt(&self.inner))
    }

    pub fn log10(&self) -> Self {
        // log10(x) = ln(x) / ln(10)
        let ln_x = crate::qwen::backend::mlx::ops::log(&self.inner);
        let ln10 = crate::qwen::backend::mlx::array::MlxArray::scalar_f32(std::f32::consts::LN_10)
            .astype(self.inner.dtype());
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::divide(&ln_x, &ln10))
    }

    pub fn sin(&self) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::sin(&self.inner))
    }

    pub fn cos(&self) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::cos(&self.inner))
    }

    pub fn exp(&self) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::exp(&self.inner))
    }

    // -- Activations --

    pub fn softmax(&self, dim: i64) -> Self {
        let dim = if dim < 0 {
            self.inner.ndim() as i64 + dim
        } else {
            dim
        } as i32;
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::softmax(&self.inner, &[dim]))
    }

    pub fn gelu(&self) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::gelu(&self.inner))
    }

    pub fn silu(&self) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::silu(&self.inner))
    }

    // -- Reduction --

    pub fn mean_dim(&self, dims: &[i64], keepdim: bool) -> Self {
        let dims_i32: Vec<i32> = dims
            .iter()
            .map(|&d| {
                if d < 0 {
                    self.inner.ndim() + d as i32
                } else {
                    d as i32
                }
            })
            .collect();
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::mean(
            &self.inner,
            &dims_i32,
            keepdim,
        ))
    }

    pub fn max(&self) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::max_all(&self.inner, false))
    }

    // -- Indexing --

    pub fn argmax(&self, dim: i64, keepdim: bool) -> Self {
        let dim = if dim < 0 {
            self.inner.ndim() as i64 + dim
        } else {
            dim
        } as i32;
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::argmax(
            &self.inner,
            dim,
            keepdim,
        ))
    }

    pub fn triu(&self, diagonal: i64) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::triu(
            &self.inner,
            diagonal as i32,
        ))
    }

    // -- Normalization --

    pub fn rms_norm(&self, weight: &Tensor, eps: f64) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::fast_rms_norm(
            &self.inner,
            &weight.inner,
            eps as f32,
        ))
    }

    pub fn layer_norm(
        &self,
        _normalized_shape: &[i64],
        weight: Option<&Tensor>,
        bias: Option<&Tensor>,
        eps: f64,
    ) -> Self {
        if let Some(w) = weight {
            Tensor::from_mlx(crate::qwen::backend::mlx::ops::fast_layer_norm(
                &self.inner,
                &w.inner,
                bias.map(|b| &b.inner),
                eps as f32,
            ))
        } else {
            let mean = self.mean_dim(&[-1], true);
            let var_t = {
                let diff = self - &mean;
                (&diff * &diff).mean_dim(&[-1], true)
            };
            let normalized = &(self - &mean) / &(&var_t + eps).sqrt();
            if let Some(b) = bias {
                &normalized + b
            } else {
                normalized
            }
        }
    }

    /// Apply rotary position embeddings using cos/sin tensors.
    pub fn apply_rope(&self, cos: &Tensor, sin: &Tensor) -> Tensor {
        let cos = cos.to_dtype(self.kind()).unsqueeze(0).unsqueeze(0);
        let sin = sin.to_dtype(self.kind()).unsqueeze(0).unsqueeze(0);
        let half = self.size().last().unwrap() / 2;
        let x1 = self.narrow(-1, 0, half);
        let x2 = self.narrow(-1, half, half);
        let x_rotated = Tensor::cat(&[(-&x2), x1], -1);
        self * &cos + x_rotated * &sin
    }

    /// Scaled dot-product attention using fused MLX kernel.
    /// Q: (B, nqh, S, D), K: (B, nkvh, T, D), V: (B, nkvh, T, D)
    /// Natively handles GQA (different Q and KV head counts).
    pub fn scaled_dot_product_attention(
        q: &Tensor,
        k: &Tensor,
        v: &Tensor,
        scale: f64,
        mask: Option<&Tensor>,
    ) -> Tensor {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::fast_sdpa(
            &q.inner,
            &k.inner,
            &v.inner,
            scale as f32,
            mask.map(|m| &m.inner),
        ))
    }

    // -- Convolution --

    pub fn conv2d(
        &self,
        weight: &Tensor,
        bias: Option<&Tensor>,
        stride: &[i64],
        padding: &[i64],
        dilation: &[i64],
        groups: i64,
    ) -> Self {
        // PyTorch: input [N, C, H, W], weight [C_out, C_in, kH, kW]
        // MLX:     input [N, H, W, C], weight [C_out, kH, kW, C_in]
        let input_t = self.permute(&[0, 2, 3, 1]); // [N, C, H, W] -> [N, H, W, C]
        let weight_t = weight.shallow_clone(); // [C_out, C_in, kH, kW] -> [C_out, kH, kW, C_in]

        let result = crate::qwen::backend::mlx::ops::conv2d(
            &input_t.inner,
            &weight_t.inner,
            [stride[0] as i32, stride[1] as i32],
            [padding[0] as i32, padding[1] as i32],
            [dilation[0] as i32, dilation[1] as i32],
            groups as i32,
        );
        // Output: [N, H_out, W_out, C_out] -> [N, C_out, H_out, W_out]
        let out = Tensor::from_mlx(result).permute(&[0, 3, 1, 2]);
        if let Some(b) = bias {
            // bias is [C_out], reshape to [1, C_out, 1, 1] for broadcasting
            &out + &b.reshape(&[-1, 1, 1]).unsqueeze(0)
        } else {
            out
        }
    }

    // -- Signal --

    pub fn reflection_pad1d(&self, pad: &[i64]) -> Self {
        Tensor::from_mlx(crate::qwen::backend::mlx::signal::reflection_pad1d(
            &self.inner,
            pad[0] as i32,
            pad[1] as i32,
        ))
    }

    pub fn stft_magnitude(&self, n_fft: i64, hop_length: i64, window: &Tensor) -> Self {
        // stft_magnitude returns [n_frames, freq_bins].
        // Transpose to [freq_bins, n_frames] to match tch STFT output layout.
        let mag = crate::qwen::backend::mlx::signal::stft_magnitude(
            &self.inner,
            n_fft as i32,
            hop_length as i32,
            &window.inner,
        );
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::swapaxes(&mag, 0, 1))
    }

    // -- Type / Device --

    pub fn to_dtype(&self, dtype: DType) -> Self {
        Tensor::from_mlx(self.inner.astype(dtype.into()))
    }

    pub fn to_device(&self, _device: Device) -> Self {
        self.clone()
    }

    pub fn kind(&self) -> DType {
        DType::from(self.inner.dtype())
    }

    pub fn device(&self) -> Device {
        Device::Gpu(0)
    }

    pub fn shallow_clone(&self) -> Self {
        self.clone()
    }

    /// Force evaluation of the lazy computation graph for this tensor.
    pub fn eval(&self) {
        self.inner.eval();
    }

    // -- Data extraction --

    pub fn int64_value(&self, indices: &[i64]) -> i64 {
        if indices.is_empty() {
            return self.inner.item_i64();
        }
        let starts: Vec<i32> = indices.iter().map(|&i| i as i32).collect();
        let stops: Vec<i32> = indices.iter().map(|&i| i as i32 + 1).collect();
        let strides: Vec<i32> = vec![1; indices.len()];
        let sliced = crate::qwen::backend::mlx::ops::slice(&self.inner, &starts, &stops, &strides);
        sliced.item_i64()
    }

    pub fn f64_value(&self, indices: &[i64]) -> f64 {
        if indices.is_empty() {
            return self.inner.item_f32() as f64;
        }
        let starts: Vec<i32> = indices.iter().map(|&i| i as i32).collect();
        let stops: Vec<i32> = indices.iter().map(|&i| i as i32 + 1).collect();
        let strides: Vec<i32> = vec![1; indices.len()];
        let sliced = crate::qwen::backend::mlx::ops::slice(&self.inner, &starts, &stops, &strides);
        sliced.item_f32() as f64
    }

    pub fn to_vec_f32(&self) -> Vec<f32> {
        let f32_arr = self
            .inner
            .astype(crate::qwen::backend::mlx::ffi::mlx_dtype::MLX_FLOAT32);
        f32_arr.to_vec_f32()
    }
}

// ---------------------------------------------------------------------------
// Operator overloads (both backends)
// ---------------------------------------------------------------------------

// Add: Tensor + Tensor
impl std::ops::Add<&Tensor> for &Tensor {
    type Output = Tensor;
    fn add(self, rhs: &Tensor) -> Tensor {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::add(&self.inner, &rhs.inner))
    }
}

impl std::ops::Add<Tensor> for &Tensor {
    type Output = Tensor;
    fn add(self, rhs: Tensor) -> Tensor {
        self + &rhs
    }
}

impl std::ops::Add<&Tensor> for Tensor {
    type Output = Tensor;
    fn add(self, rhs: &Tensor) -> Tensor {
        &self + rhs
    }
}

impl std::ops::Add<Tensor> for Tensor {
    type Output = Tensor;
    fn add(self, rhs: Tensor) -> Tensor {
        &self + &rhs
    }
}

// Add: Tensor + f64
impl std::ops::Add<f64> for &Tensor {
    type Output = Tensor;
    fn add(self, rhs: f64) -> Tensor {
        {
            let scalar = crate::qwen::backend::mlx::array::MlxArray::scalar_f32(rhs as f32)
                .astype(self.inner.dtype());
            Tensor::from_mlx(crate::qwen::backend::mlx::ops::add(&self.inner, &scalar))
        }
    }
}

impl std::ops::Add<f64> for Tensor {
    type Output = Tensor;
    fn add(self, rhs: f64) -> Tensor {
        &self + rhs
    }
}

// Sub: Tensor - Tensor
impl std::ops::Sub<&Tensor> for &Tensor {
    type Output = Tensor;
    fn sub(self, rhs: &Tensor) -> Tensor {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::subtract(
            &self.inner,
            &rhs.inner,
        ))
    }
}

impl std::ops::Sub<Tensor> for &Tensor {
    type Output = Tensor;
    fn sub(self, rhs: Tensor) -> Tensor {
        self - &rhs
    }
}

impl std::ops::Sub<&Tensor> for Tensor {
    type Output = Tensor;
    fn sub(self, rhs: &Tensor) -> Tensor {
        &self - rhs
    }
}

impl std::ops::Sub<Tensor> for Tensor {
    type Output = Tensor;
    fn sub(self, rhs: Tensor) -> Tensor {
        &self - &rhs
    }
}

impl std::ops::Sub<f64> for &Tensor {
    type Output = Tensor;
    fn sub(self, rhs: f64) -> Tensor {
        {
            let scalar = crate::qwen::backend::mlx::array::MlxArray::scalar_f32(rhs as f32)
                .astype(self.inner.dtype());
            Tensor::from_mlx(crate::qwen::backend::mlx::ops::subtract(
                &self.inner,
                &scalar,
            ))
        }
    }
}

// Mul: Tensor * Tensor
impl std::ops::Mul<&Tensor> for &Tensor {
    type Output = Tensor;
    fn mul(self, rhs: &Tensor) -> Tensor {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::multiply(
            &self.inner,
            &rhs.inner,
        ))
    }
}

impl std::ops::Mul<Tensor> for &Tensor {
    type Output = Tensor;
    fn mul(self, rhs: Tensor) -> Tensor {
        self * &rhs
    }
}

impl std::ops::Mul<&Tensor> for Tensor {
    type Output = Tensor;
    fn mul(self, rhs: &Tensor) -> Tensor {
        &self * rhs
    }
}

impl std::ops::Mul<Tensor> for Tensor {
    type Output = Tensor;
    fn mul(self, rhs: Tensor) -> Tensor {
        &self * &rhs
    }
}

// Mul: Tensor * f64
impl std::ops::Mul<f64> for &Tensor {
    type Output = Tensor;
    fn mul(self, rhs: f64) -> Tensor {
        {
            let scalar = crate::qwen::backend::mlx::array::MlxArray::scalar_f32(rhs as f32)
                .astype(self.inner.dtype());
            Tensor::from_mlx(crate::qwen::backend::mlx::ops::multiply(
                &self.inner,
                &scalar,
            ))
        }
    }
}

impl std::ops::Mul<f64> for Tensor {
    type Output = Tensor;
    fn mul(self, rhs: f64) -> Tensor {
        &self * rhs
    }
}

// Div: Tensor / Tensor
impl std::ops::Div<&Tensor> for &Tensor {
    type Output = Tensor;
    fn div(self, rhs: &Tensor) -> Tensor {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::divide(
            &self.inner,
            &rhs.inner,
        ))
    }
}

impl std::ops::Div<Tensor> for &Tensor {
    type Output = Tensor;
    fn div(self, rhs: Tensor) -> Tensor {
        self / &rhs
    }
}

impl std::ops::Div<&Tensor> for Tensor {
    type Output = Tensor;
    fn div(self, rhs: &Tensor) -> Tensor {
        &self / rhs
    }
}

impl std::ops::Div<Tensor> for Tensor {
    type Output = Tensor;
    fn div(self, rhs: Tensor) -> Tensor {
        &self / &rhs
    }
}

// Div: Tensor / f64
impl std::ops::Div<f64> for &Tensor {
    type Output = Tensor;
    fn div(self, rhs: f64) -> Tensor {
        {
            let scalar = crate::qwen::backend::mlx::array::MlxArray::scalar_f32(rhs as f32)
                .astype(self.inner.dtype());
            Tensor::from_mlx(crate::qwen::backend::mlx::ops::divide(&self.inner, &scalar))
        }
    }
}

impl std::ops::Div<f64> for Tensor {
    type Output = Tensor;
    fn div(self, rhs: f64) -> Tensor {
        &self / rhs
    }
}

// Neg: -Tensor
impl std::ops::Neg for &Tensor {
    type Output = Tensor;
    fn neg(self) -> Tensor {
        Tensor::from_mlx(crate::qwen::backend::mlx::ops::negative(&self.inner))
    }
}

impl std::ops::Neg for Tensor {
    type Output = Tensor;
    fn neg(self) -> Tensor {
        -&self
    }
}

// AddAssign
impl std::ops::AddAssign<&Tensor> for Tensor {
    fn add_assign(&mut self, rhs: &Tensor) {
        *self = &*self + rhs;
    }
}

impl std::ops::AddAssign<Tensor> for Tensor {
    fn add_assign(&mut self, rhs: Tensor) {
        *self = &*self + &rhs;
    }
}
