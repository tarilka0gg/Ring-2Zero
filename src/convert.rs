use rayon::prelude::*;

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

/// AVX2 SIMD conversion: processes 8 pixels (32 bytes) at once
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn convert_bgrx_to_rgba_avx2(src: &[u8], dst: &mut [u8]) {
    // Shuffle mask: BGRX → RGBA
    // Input bytes:  [B0 G0 R0 X0 | B1 G1 R1 X1 | B2 G2 R2 X2 | B3 G3 R3 X3 | ...]
    // Output bytes: [R0 G0 B0 FF | R1 G1 B1 FF | R2 G2 B2 FF | R3 G3 B3 FF | ...]
    //
    // Shuffle indices (per 128-bit lane):
    //   Pixel 0: R=byte2, G=byte1, B=byte0, A=0xFF
    //   Pixel 1: R=byte6, G=byte5, B=byte4, A=0xFF
    //   ...
    let shuffle = _mm256_setr_epi8(
        2, 1, 0, -1,  6, 5, 4, -1,   // Lane 0: pixels 0-1
        10, 9, 8, -1, 14, 13, 12, -1, // Lane 0: pixels 2-3
        2, 1, 0, -1,  6, 5, 4, -1,   // Lane 1: pixels 4-5
        10, 9, 8, -1, 14, 13, 12, -1  // Lane 1: pixels 6-7
    );

    // Alpha channel (0xFF in the 4th byte of each pixel)
    let alpha = _mm256_set1_epi32(0xFF000000u32 as i32);

    let chunks = src.len() / 32;

    for i in 0..chunks {
        let src_ptr = src.as_ptr().add(i * 32) as *const __m256i;
        let dst_ptr = dst.as_mut_ptr().add(i * 32) as *mut __m256i;

        let pixels = _mm256_loadu_si256(src_ptr);
        let shuffled = _mm256_shuffle_epi8(pixels, shuffle);
        let with_alpha = _mm256_or_si256(shuffled, alpha);
        _mm256_storeu_si256(dst_ptr, with_alpha);
    }
}

/// SSE2 SIMD conversion: processes 4 pixels (16 bytes) at once
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn convert_bgrx_to_rgba_sse2(src: &[u8], dst: &mut [u8]) {
    let shuffle = _mm_setr_epi8(
        2, 1, 0, -1,  6, 5, 4, -1,   // Pixels 0-1
        10, 9, 8, -1, 14, 13, 12, -1  // Pixels 2-3
    );

    let alpha = _mm_set1_epi32(0xFF000000u32 as i32);

    let chunks = src.len() / 16;

    for i in 0..chunks {
        let src_ptr = src.as_ptr().add(i * 16) as *const __m128i;
        let dst_ptr = dst.as_mut_ptr().add(i * 16) as *mut __m128i;

        let pixels = _mm_loadu_si128(src_ptr);
        let shuffled = _mm_shuffle_epi8(pixels, shuffle);
        let with_alpha = _mm_or_si128(shuffled, alpha);
        _mm_storeu_si128(dst_ptr, with_alpha);
    }
}

pub fn convert_bgrx_to_rgba_inplace(src: &[u8], width: u32, height: u32, dst: &mut Vec<u8>) {
    let pixels = (width * height) as usize;
    dst.clear();
    dst.resize(pixels * 4, 0);

    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") {
            unsafe {
                // Process with AVX2 (8 pixels at once)
                let avx2_pixels = (pixels / 8) * 8;
                let avx2_bytes = avx2_pixels * 4;

                convert_bgrx_to_rgba_avx2(
                    &src[..avx2_bytes],
                    &mut dst[..avx2_bytes]
                );

                // Handle remainder (scalar)
                for i in avx2_pixels..pixels {
                    let s = i * 4;
                    dst[s] = src[s + 2];     // R
                    dst[s + 1] = src[s + 1]; // G
                    dst[s + 2] = src[s];     // B
                    dst[s + 3] = 255;        // A
                }

                return;
            }
        }

        if is_x86_feature_detected!("sse2") {
            unsafe {
                // Process with SSE2 (4 pixels at once)
                let sse2_pixels = (pixels / 4) * 4;
                let sse2_bytes = sse2_pixels * 4;

                convert_bgrx_to_rgba_sse2(
                    &src[..sse2_bytes],
                    &mut dst[..sse2_bytes]
                );

                // Handle remainder (scalar)
                for i in sse2_pixels..pixels {
                    let s = i * 4;
                    dst[s] = src[s + 2];
                    dst[s + 1] = src[s + 1];
                    dst[s + 2] = src[s];
                    dst[s + 3] = 255;
                }

                return;
            }
        }
    }

    // Scalar fallback (original parallel implementation)
    let full_chunks = pixels / 4 * 4;

    dst[..full_chunks * 4]
        .par_chunks_exact_mut(16)
        .zip(src[..full_chunks * 4].par_chunks_exact(16))
        .for_each(|(dst, src)| {
            dst[0] = src[2];
            dst[1] = src[1];
            dst[2] = src[0];
            dst[3] = 255;
            dst[4] = src[6];
            dst[5] = src[5];
            dst[6] = src[4];
            dst[7] = 255;
            dst[8] = src[10];
            dst[9] = src[9];
            dst[10] = src[8];
            dst[11] = 255;
            dst[12] = src[14];
            dst[13] = src[13];
            dst[14] = src[12];
            dst[15] = 255;
        });

    for i in full_chunks..pixels {
        let s = i * 4;
        dst[s] = src[s + 2];
        dst[s + 1] = src[s + 1];
        dst[s + 2] = src[s];
        dst[s + 3] = 255;
    }
}

// Keep old function for backward compatibility
pub fn convert_bgrx_to_rgba(src: &[u8], width: u32, height: u32) -> Vec<u8> {
    let mut rgba = Vec::new();
    convert_bgrx_to_rgba_inplace(src, width, height, &mut rgba);
    rgba
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_a_single_pixel_correctly() {
        let src = [10u8, 20, 30, 99]; // B=10 G=20 R=30 X=ignored
        let dst = convert_bgrx_to_rgba(&src, 1, 1);
        assert_eq!(dst, vec![30, 20, 10, 255]); // R,G,B,A
    }

    #[test]
    fn converts_various_pixel_counts_correctly() {
        // Exercise the AVX2 (8px/iter), SSE2 (4px/iter), and scalar
        // remainder paths at their boundaries.
        for &pixels in &[1usize, 3, 4, 7, 8, 9, 15, 16, 17, 33, 100] {
            let mut src = Vec::with_capacity(pixels * 4);
            for i in 0..pixels {
                src.extend_from_slice(&[(i * 3) as u8, (i * 5) as u8, (i * 7) as u8, 0xAA]);
            }
            let dst = convert_bgrx_to_rgba(&src, pixels as u32, 1);
            assert_eq!(dst.len(), pixels * 4);
            for i in 0..pixels {
                let s = i * 4;
                assert_eq!(dst[s], src[s + 2], "R mismatch at pixel {i} for {pixels} pixels");
                assert_eq!(dst[s + 1], src[s + 1], "G mismatch at pixel {i} for {pixels} pixels");
                assert_eq!(dst[s + 2], src[s], "B mismatch at pixel {i} for {pixels} pixels");
                assert_eq!(dst[s + 3], 255, "alpha must always be opaque at pixel {i} for {pixels} pixels");
            }
        }
    }

    #[test]
    fn inplace_variant_resizes_a_stale_destination_buffer() {
        let src = [1u8, 2, 3, 4, 5, 6, 7, 8]; // 2 pixels
        let mut dst = vec![0u8; 999]; // stale, oversized buffer
        convert_bgrx_to_rgba_inplace(&src, 2, 1, &mut dst);
        assert_eq!(dst, vec![3, 2, 1, 255, 7, 6, 5, 255]);
    }
}
