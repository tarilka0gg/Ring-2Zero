/// SIMD-optimized tile extraction
/// Copies tile data from frame buffer to contiguous tile buffer

#[cfg(target_arch = "x86_64")]
use core::arch::x86_64::*;

/// Extract tile with SIMD optimization (AVX2/SSE2/scalar)
///
/// # Arguments
/// * `frame_rgba` - Source frame buffer (RGBA, row-major)
/// * `tile_buffer` - Destination tile buffer (must be pre-allocated)
/// * `tile_x`, `tile_y` - Tile position in pixels
/// * `tile_width`, `tile_height` - Tile dimensions in pixels
/// * `frame_width` - Frame width in pixels
pub fn extract_tile(
    frame_rgba: &[u8],
    tile_buffer: &mut [u8],
    tile_x: u32,
    tile_y: u32,
    tile_width: u32,
    tile_height: u32,
    frame_width: u32,
) {
    let row_bytes = (tile_width * 4) as usize;

    // Full-width tile - single contiguous copy
    if tile_width == frame_width {
        let src_offset = (tile_y * frame_width * 4) as usize;
        let len = (tile_width * tile_height * 4) as usize;
        tile_buffer[..len].copy_from_slice(&frame_rgba[src_offset..src_offset + len]);
        return;
    }

    // Partial-width tile - row-by-row copy with SIMD
    #[cfg(target_arch = "x86_64")]
    {
        if is_x86_feature_detected!("avx2") && row_bytes >= 32 {
            unsafe {
                extract_tile_rows_avx2(
                    frame_rgba,
                    tile_buffer,
                    tile_x,
                    tile_y,
                    tile_width,
                    tile_height,
                    frame_width,
                    row_bytes,
                );
            }
            return;
        }

        if is_x86_feature_detected!("sse2") && row_bytes >= 16 {
            unsafe {
                extract_tile_rows_sse2(
                    frame_rgba,
                    tile_buffer,
                    tile_x,
                    tile_y,
                    tile_width,
                    tile_height,
                    frame_width,
                    row_bytes,
                );
            }
            return;
        }
    }

    // Scalar fallback
    extract_tile_rows_scalar(
        frame_rgba,
        tile_buffer,
        tile_x,
        tile_y,
        tile_height,
        frame_width,
        row_bytes,
    );
}

/// AVX2 tile extraction - copies 32 bytes per iteration
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "avx2")]
unsafe fn extract_tile_rows_avx2(
    frame_rgba: &[u8],
    tile_buffer: &mut [u8],
    tile_x: u32,
    tile_y: u32,
    _tile_width: u32,
    tile_height: u32,
    frame_width: u32,
    row_bytes: usize,
) {
    let chunks_per_row = row_bytes / 32;
    let remainder = row_bytes % 32;

    for row in 0..tile_height {
        let src_offset = (((tile_y + row) * frame_width + tile_x) * 4) as usize;
        let dst_offset = (row * _tile_width * 4) as usize;

        // Copy 32-byte chunks with AVX2
        for chunk in 0..chunks_per_row {
            let src_ptr = frame_rgba.as_ptr().add(src_offset + chunk * 32);
            let dst_ptr = tile_buffer.as_mut_ptr().add(dst_offset + chunk * 32);

            let data = _mm256_loadu_si256(src_ptr as *const __m256i);
            _mm256_storeu_si256(dst_ptr as *mut __m256i, data);
        }

        // Copy remainder bytes (< 32)
        if remainder > 0 {
            let src_start = src_offset + chunks_per_row * 32;
            let dst_start = dst_offset + chunks_per_row * 32;
            tile_buffer[dst_start..dst_start + remainder]
                .copy_from_slice(&frame_rgba[src_start..src_start + remainder]);
        }
    }
}

/// SSE2 tile extraction - copies 16 bytes per iteration
#[cfg(target_arch = "x86_64")]
#[target_feature(enable = "sse2")]
unsafe fn extract_tile_rows_sse2(
    frame_rgba: &[u8],
    tile_buffer: &mut [u8],
    tile_x: u32,
    tile_y: u32,
    _tile_width: u32,
    tile_height: u32,
    frame_width: u32,
    row_bytes: usize,
) {
    let chunks_per_row = row_bytes / 16;
    let remainder = row_bytes % 16;

    for row in 0..tile_height {
        let src_offset = (((tile_y + row) * frame_width + tile_x) * 4) as usize;
        let dst_offset = (row * _tile_width * 4) as usize;

        // Copy 16-byte chunks with SSE2
        for chunk in 0..chunks_per_row {
            let src_ptr = frame_rgba.as_ptr().add(src_offset + chunk * 16);
            let dst_ptr = tile_buffer.as_mut_ptr().add(dst_offset + chunk * 16);

            let data = _mm_loadu_si128(src_ptr as *const __m128i);
            _mm_storeu_si128(dst_ptr as *mut __m128i, data);
        }

        // Copy remainder bytes (< 16)
        if remainder > 0 {
            let src_start = src_offset + chunks_per_row * 16;
            let dst_start = dst_offset + chunks_per_row * 16;
            tile_buffer[dst_start..dst_start + remainder]
                .copy_from_slice(&frame_rgba[src_start..src_start + remainder]);
        }
    }
}

/// Scalar fallback - row-by-row memcpy
fn extract_tile_rows_scalar(
    frame_rgba: &[u8],
    tile_buffer: &mut [u8],
    tile_x: u32,
    tile_y: u32,
    tile_height: u32,
    frame_width: u32,
    row_bytes: usize,
) {
    for row in 0..tile_height {
        let src_offset = (((tile_y + row) * frame_width + tile_x) * 4) as usize;
        let dst_offset = (row as usize * row_bytes);
        tile_buffer[dst_offset..dst_offset + row_bytes]
            .copy_from_slice(&frame_rgba[src_offset..src_offset + row_bytes]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_extract_tile_full_width() {
        let frame = vec![1u8; 1920 * 1080 * 4];
        let mut tile = vec![0u8; 1920 * 27 * 4];

        extract_tile(&frame, &mut tile, 0, 0, 1920, 27, 1920);

        assert_eq!(tile[0], 1);
        assert_eq!(tile[tile.len() - 1], 1);
    }

    #[test]
    fn test_extract_tile_partial_width() {
        let mut frame = vec![0u8; 1920 * 1080 * 4];
        // Fill test pattern
        for i in 0..frame.len() {
            frame[i] = (i % 256) as u8;
        }

        let mut tile = vec![0u8; 48 * 27 * 4];
        extract_tile(&frame, &mut tile, 100, 50, 48, 27, 1920);

        // Verify first pixel
        let src_offset = ((50 * 1920 + 100) * 4) as usize;
        assert_eq!(tile[0], frame[src_offset]);
        assert_eq!(tile[1], frame[src_offset + 1]);
    }
}
