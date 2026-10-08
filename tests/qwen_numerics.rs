use pebble_index::qwen::{
    backend::mlx::{signal, stream::init_mlx},
    mel::WhisperFeatureExtractor,
    tensor::{Device, Tensor},
};
// All GPU assertions run on one owner thread, like the worker.
#[test]
fn frontend_and_scalar_semantics() {
    init_mlx(true);
    let window = signal::hann_window(400).to_vec_f32();
    assert_eq!(window.len(), 400);
    for (i, value) in window.iter().enumerate() {
        let expected = 0.5 - 0.5 * (2.0 * std::f64::consts::PI * i as f64 / 400.0).cos();
        assert!((*value as f64 - expected).abs() < 2e-7);
    }
    let x = Tensor::from_slice_f32(&[-3.0, -1.0, 0.0, 1.0, 3.0]);
    assert_eq!(
        x.to_dtype(pebble_index::qwen::tensor::DType::Float16)
            .gelu()
            .kind(),
        pebble_index::qwen::tensor::DType::Float16
    );
    let gelu = x.gelu().to_vec_f32();
    for (v, expected) in
        gelu.iter()
            .zip([-0.00404969_f64, -0.15865525, 0.0, 0.84134475, 2.99595031])
    {
        assert!((*v as f64 - expected).abs() < 1e-6);
    }
    assert_eq!(x.argmax(0, false).int64_value(&[]), 4);
    let values = Tensor::from_slice_f32(&[1.0, 2.0, 3.0, 4.0])
        .reshape(&[2, 2])
        .transpose(0, 1)
        .to_vec_f32();
    assert_eq!(values, vec![1.0, 3.0, 2.0, 4.0]);
    // Invalid shapes/FFI failures must become catchable errors, not stdout or UB.
    assert!(
        std::panic::catch_unwind(|| {
            pebble_index::qwen::backend::mlx::array::MlxArray::from_f32(&[1.0], &[2]);
        })
        .is_err()
    );
    assert!(
        std::panic::catch_unwind(|| {
            x.reshape(&[2, 2]);
        })
        .is_err()
    );
    packed_quantization();
    let extractor = WhisperFeatureExtractor::new(400, 160, 128, 16000, Device::gpu());
    compare_frontend_reference(&extractor);
    for n in [400, 401, 15999, 16000, 16001, 16319] {
        let mel = extractor.extract(&vec![0.0; n], Device::gpu()).unwrap();
        assert_eq!(mel.size(), vec![128, (n / 160) as i64]);
        assert!(mel.to_vec_f32().iter().all(|v| (*v + 1.5).abs() < 1e-6));
    }
    assert!(extractor.extract(&[0.0; 399], Device::gpu()).is_err());
    assert!(extractor.extract(&[f32::NAN; 400], Device::gpu()).is_err());
}

fn compare_frontend_reference(extractor: &WhisperFeatureExtractor) {
    let reference: serde_json::Value =
        serde_json::from_str(include_str!("data/qwen-mel-reference.json")).unwrap();
    let pcm: Vec<f32> = (0..16123)
        .map(|i| {
            let phase = 2.0 * std::f64::consts::PI * i as f64 / 16000.0;
            (0.2 * (440.0 * phase).sin() + 0.1 * (920.0 * phase).cos()) as f32
        })
        .collect();
    let mel = extractor.extract(&pcm, Device::gpu()).unwrap();
    assert_eq!(mel.size(), vec![128, 100]);
    let values = mel.to_vec_f32();
    for point in reference["values"].as_array().unwrap() {
        let m = point[0].as_u64().unwrap() as usize;
        let t = point[1].as_u64().unwrap() as usize;
        let expected = point[2].as_f64().unwrap() as f32;
        assert!(
            (values[m * 100 + t] - expected).abs() < 2e-5,
            "mel[{m},{t}]={} expected={expected}",
            values[m * 100 + t]
        );
    }
}
fn packed_quantization() {
    use pebble_index::qwen::{
        backend::mlx::{array::MlxArray, ffi::mlx_dtype},
        layers::Linear,
    };
    let packed: Vec<i32> = [vec![0x01020304; 16], vec![0x05060708; 16]].concat();
    let linear = Linear {
        weight: Tensor::from_mlx(
            MlxArray::from_i32(&packed, &[2, 16]).astype(mlx_dtype::MLX_UINT32),
        ),
        bias: None,
        quantization: Some((
            Tensor::from_slice_f32(&[0.5, 0.25]).reshape(&[2, 1]),
            Tensor::from_slice_f32(&[-1.0, -0.5]).reshape(&[2, 1]),
        )),
    };
    let x = Tensor::from_slice_f32(&vec![1.0; 64]).reshape(&[1, 64]);
    assert_eq!(linear.forward(&x).to_vec_f32(), vec![16.0, 72.0]);
    let embedded = linear
        .embedding(&Tensor::from_slice_i64(&[1, 0]))
        .to_vec_f32();
    assert_eq!(&embedded[..4], &[1.5, 1.25, 1.0, 0.75]);
    assert_eq!(&embedded[64..68], &[1.0, 0.5, 0.0, -0.5]);
}
