use std::{env, fs, process::ExitCode};

fn main() -> ExitCode {
    let Some(path) = env::args().nth(1) else {
        eprintln!(
            "usage: sigx-cli <signature.bin> | generate <pcm_i16le> <sample_rate> <output.bin> [flush-frames] [chunk-samples] | generate-magnitude-crop <pcm_i16le> <sample_rate> <output.bin> [flush-frames] [chunk-samples] (diagnostic only) | generate-f32complex <pcm_i16le> <sample_rate> <output.bin> [flush-frames] [chunk-samples] | generate-f32complex-direct <pcm_i16le> <sample_rate> <output.bin> [flush-frames] [chunk-samples] | generate-f32complex-fma-butterfly <pcm_i16le> <sample_rate> <output.bin> [flush-frames] [chunk-samples] | generate-f32complex-fma-twiddle <pcm_i16le> <sample_rate> <output.bin> [flush-frames] [chunk-samples] | generate-f32complex-fma <pcm_i16le> <sample_rate> <output.bin> [flush-frames] [chunk-samples] | generate-f64fft <pcm_i16le> <sample_rate> <output.bin> [flush-frames] [chunk-samples] | generate-f32 <pcm_f32le> <sample_rate> <output.bin> [flush-frames] [chunk-samples]"
        );
        return ExitCode::from(2);
    };
    if path == "generate" {
        return generate_i16_signature(
            sigx_core::FftKernel::PortablePackedRealF32,
            sigx_core::CropOrder::FrequencyThenMagnitude,
        );
    }
    if path == "generate-magnitude-crop" {
        return generate_i16_signature(
            sigx_core::FftKernel::PortablePackedRealF32,
            sigx_core::CropOrder::MagnitudeThenFrequency,
        );
    }
    if path == "generate-crop-frequency-time" {
        return generate_i16_signature_with_crop_policy(
            sigx_core::FftKernel::PortablePackedRealF32,
            sigx_core::CropOrder::FrequencyThenMagnitude,
            sigx_core::CropResortOrder::FrequencyThenTime,
        );
    }
    if path == "generate-magnitude-crop-frequency-time" {
        return generate_i16_signature_with_crop_policy(
            sigx_core::FftKernel::PortablePackedRealF32,
            sigx_core::CropOrder::MagnitudeThenFrequency,
            sigx_core::CropResortOrder::FrequencyThenTime,
        );
    }
    if path == "generate-f64fft" {
        return generate_i16_signature(
            sigx_core::FftKernel::FullComplexF64Diagnostic,
            sigx_core::CropOrder::FrequencyThenMagnitude,
        );
    }
    if path == "generate-f32complex" {
        return generate_i16_signature(
            sigx_core::FftKernel::FullComplexF32Diagnostic,
            sigx_core::CropOrder::FrequencyThenMagnitude,
        );
    }
    if path == "generate-f32complex-direct" {
        return generate_i16_signature(
            sigx_core::FftKernel::FullComplexF32DirectTwiddleDiagnostic,
            sigx_core::CropOrder::FrequencyThenMagnitude,
        );
    }
    if path == "generate-f32complex-fma" {
        return generate_i16_signature(
            sigx_core::FftKernel::FullComplexF32FmaDiagnostic,
            sigx_core::CropOrder::FrequencyThenMagnitude,
        );
    }
    if path == "generate-f32complex-fma-butterfly" {
        return generate_i16_signature(
            sigx_core::FftKernel::FullComplexF32FmaButterflyDiagnostic,
            sigx_core::CropOrder::FrequencyThenMagnitude,
        );
    }
    if path == "generate-f32complex-fma-twiddle" {
        return generate_i16_signature(
            sigx_core::FftKernel::FullComplexF32FmaTwiddleDiagnostic,
            sigx_core::CropOrder::FrequencyThenMagnitude,
        );
    }
    if path == "generate-f32" {
        return generate_f32_signature();
    }
    let bytes = match fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read {path}: {error}");
            return ExitCode::from(2);
        }
    };
    match sigx_format::Signature::parse(&bytes) {
        Ok(signature) => {
            println!(
                "path={path} bytes={} payload_len={} crc32={:#010x} rate_marker_be={}",
                signature.len(),
                signature.payload_len(),
                signature.crc32(),
                signature.rate_marker_be()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("invalid signature {path}: {error}");
            ExitCode::from(1)
        }
    }
}

fn generate_i16_signature(
    fft_kernel: sigx_core::FftKernel,
    crop_order: sigx_core::CropOrder,
) -> ExitCode {
    generate_i16_signature_with_crop_policy(
        fft_kernel,
        crop_order,
        sigx_core::CropResortOrder::TimeThenFrequency,
    )
}

fn generate_i16_signature_with_crop_policy(
    fft_kernel: sigx_core::FftKernel,
    crop_order: sigx_core::CropOrder,
    resort_order: sigx_core::CropResortOrder,
) -> ExitCode {
    let mut args = env::args().skip(2);
    let Some(pcm_path) = args.next() else {
        eprintln!("usage: sigx-cli generate <pcm_i16le> <sample_rate> <output.bin> [flush-frames] [chunk-samples]");
        return ExitCode::from(2);
    };
    let Some(sample_rate) = args.next().and_then(|value| value.parse::<u32>().ok()) else {
        eprintln!("sample rate must be an integer in Hz");
        return ExitCode::from(2);
    };
    let Some(output_path) = args.next() else {
        eprintln!("usage: sigx-cli generate <pcm_i16le> <sample_rate> <output.bin> [flush-frames] [chunk-samples]");
        return ExitCode::from(2);
    };
    let flush_frames = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("flush-frames must be a nonnegative integer");
            std::process::exit(2);
        });
    let chunk_samples = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("chunk-samples must be a positive integer");
            std::process::exit(2);
        });
    if chunk_samples == Some(0) {
        eprintln!("chunk-samples must be a positive integer");
        return ExitCode::from(2);
    }
    let bytes = match fs::read(&pcm_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read {pcm_path}: {error}");
            return ExitCode::from(2);
        }
    };
    if bytes.len() % 2 != 0 {
        eprintln!("PCM input must contain an even number of bytes");
        return ExitCode::from(2);
    }
    let samples: Vec<i16> = bytes
        .chunks_exact(2)
        .map(|chunk| i16::from_le_bytes([chunk[0], chunk[1]]))
        .collect();
    let mut sigx = match sigx_core::SigX::new_with_fft_kernel(
        sigx_core::Config {
            sample_rate,
            channels: 1,
            signature_type: 2,
            signature_options: 0,
            max_frames: samples.len().max(1),
        },
        fft_kernel,
    ) {
        Ok(sigx) => sigx,
        Err(error) => {
            eprintln!("initialize SigX: {error:?}");
            return ExitCode::from(1);
        }
    };
    if let Some(chunk_size) = chunk_samples {
        for chunk in samples.chunks(chunk_size) {
            if let Err(error) = sigx.push_i16(chunk) {
                eprintln!("process PCM: {error:?}");
                return ExitCode::from(1);
            }
        }
    } else if let Err(error) = sigx.push_i16(&samples) {
        eprintln!("process PCM: {error:?}");
        return ExitCode::from(1);
    }
    let signature = match sigx.signature_with_flush_frames_and_crop_policy(
        flush_frames.unwrap_or(61),
        crop_order,
        resort_order,
    ) {
        Ok(signature) => signature,
        Err(error) => {
            eprintln!("pack signature: {error:?}");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = fs::write(&output_path, &signature) {
        eprintln!("write {output_path}: {error}");
        return ExitCode::from(2);
    }
    match sigx_format::Signature::parse(&signature) {
        Ok(parsed) => {
            println!(
                "generated path={output_path} bytes={} payload_len={} crc32={:#010x} rate_marker_be={}",
                parsed.len(),
                parsed.payload_len(),
                parsed.crc32(),
                parsed.rate_marker_be()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("generated signature failed self-parse: {error}");
            ExitCode::from(1)
        }
    }
}

fn generate_f32_signature() -> ExitCode {
    let mut args = env::args().skip(2);
    let Some(pcm_path) = args.next() else {
        eprintln!(
            "usage: sigx-cli generate-f32 <pcm_f32le> <sample_rate> <output.bin> [flush-frames] [chunk-samples]"
        );
        return ExitCode::from(2);
    };
    let Some(sample_rate) = args.next().and_then(|value| value.parse::<u32>().ok()) else {
        eprintln!("sample rate must be an integer in Hz");
        return ExitCode::from(2);
    };
    let Some(output_path) = args.next() else {
        eprintln!(
            "usage: sigx-cli generate-f32 <pcm_f32le> <sample_rate> <output.bin> [flush-frames] [chunk-samples]"
        );
        return ExitCode::from(2);
    };
    let flush_frames = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("flush-frames must be a nonnegative integer");
            std::process::exit(2);
        });
    let chunk_samples = args
        .next()
        .map(|value| value.parse::<usize>())
        .transpose()
        .unwrap_or_else(|_| {
            eprintln!("chunk-samples must be a positive integer");
            std::process::exit(2);
        });
    if chunk_samples == Some(0) {
        eprintln!("chunk-samples must be a positive integer");
        return ExitCode::from(2);
    }
    let bytes = match fs::read(&pcm_path) {
        Ok(bytes) => bytes,
        Err(error) => {
            eprintln!("read {pcm_path}: {error}");
            return ExitCode::from(2);
        }
    };
    if bytes.len() % 4 != 0 {
        eprintln!("PCM input must contain a multiple of four bytes");
        return ExitCode::from(2);
    }
    let samples: Vec<f32> = bytes
        .chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]))
        .collect();
    let mut sigx = match sigx_core::SigX::new(sigx_core::Config {
        sample_rate,
        channels: 1,
        signature_type: 2,
        signature_options: 0,
        max_frames: samples.len().max(1),
    }) {
        Ok(sigx) => sigx,
        Err(error) => {
            eprintln!("initialize SigX: {error:?}");
            return ExitCode::from(1);
        }
    };
    if let Some(chunk_size) = chunk_samples {
        for chunk in samples.chunks(chunk_size) {
            if let Err(error) = sigx.push_f32(chunk) {
                eprintln!("process PCM: {error:?}");
                return ExitCode::from(1);
            }
        }
    } else if let Err(error) = sigx.push_f32(&samples) {
        eprintln!("process PCM: {error:?}");
        return ExitCode::from(1);
    }
    let signature = match sigx.signature_with_flush_frames(flush_frames.unwrap_or(61)) {
        Ok(signature) => signature,
        Err(error) => {
            eprintln!("pack signature: {error:?}");
            return ExitCode::from(1);
        }
    };
    if let Err(error) = fs::write(&output_path, &signature) {
        eprintln!("write {output_path}: {error}");
        return ExitCode::from(2);
    }
    match sigx_format::Signature::parse(&signature) {
        Ok(parsed) => {
            println!(
                "generated path={output_path} bytes={} payload_len={} crc32={:#010x} rate_marker_be={}",
                parsed.len(),
                parsed.payload_len(),
                parsed.crc32(),
                parsed.rate_marker_be()
            );
            ExitCode::SUCCESS
        }
        Err(error) => {
            eprintln!("generated signature failed self-parse: {error}");
            ExitCode::from(1)
        }
    }
}
