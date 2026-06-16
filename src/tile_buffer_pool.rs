/// Thread-safe pool of reusable tile buffers
/// Reduces allocations by reusing Vec<u8> buffers for tile extraction

use std::sync::Arc;
use crossbeam::queue::SegQueue;

pub struct TileBufferPool {
    buffers: Arc<SegQueue<Vec<u8>>>,
    buffer_size: usize,
}

impl TileBufferPool {
    /// Create a new buffer pool
    ///
    /// # Arguments
    /// * `buffer_size` - Size of each buffer in bytes (e.g., 48*27*4 for typical tile)
    /// * `initial_count` - Number of buffers to pre-allocate
    pub fn new(buffer_size: usize, initial_count: usize) -> Self {
        let buffers = SegQueue::new();

        // Pre-allocate buffers
        for _ in 0..initial_count {
            buffers.push(vec![0u8; buffer_size]);
        }

        Self {
            buffers: Arc::new(buffers),
            buffer_size,
        }
    }

    /// Get a buffer from the pool (or allocate a new one if pool is empty)
    pub fn get(&self) -> Vec<u8> {
        if let Some(mut buffer) = self.buffers.pop() {
            // Reuse existing buffer
            buffer.clear();
            buffer.resize(self.buffer_size, 0);
            buffer
        } else {
            // Pool empty - allocate new buffer
            vec![0u8; self.buffer_size]
        }
    }

    /// Return a buffer to the pool for reuse
    pub fn return_buffer(&self, buffer: Vec<u8>) {
        // Only keep buffers that match our size (avoid memory bloat)
        if buffer.capacity() >= self.buffer_size && buffer.capacity() < self.buffer_size * 2 {
            self.buffers.push(buffer);
        }
        // Otherwise drop the buffer (will be deallocated)
    }

    /// Get current pool size (for debugging/monitoring)
    pub fn available_buffers(&self) -> usize {
        self.buffers.len()
    }
}

impl Clone for TileBufferPool {
    fn clone(&self) -> Self {
        Self {
            buffers: Arc::clone(&self.buffers),
            buffer_size: self.buffer_size,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pool_basic() {
        let pool = TileBufferPool::new(1024, 5);
        assert_eq!(pool.available_buffers(), 5);

        // Get a buffer
        let buf1 = pool.get();
        assert_eq!(buf1.len(), 1024);
        assert_eq!(pool.available_buffers(), 4);

        // Return it
        pool.return_buffer(buf1);
        assert_eq!(pool.available_buffers(), 5);
    }

    #[test]
    fn test_pool_empty() {
        let pool = TileBufferPool::new(512, 0);
        assert_eq!(pool.available_buffers(), 0);

        // Should allocate new buffer when pool is empty
        let buf = pool.get();
        assert_eq!(buf.len(), 512);
        assert_eq!(pool.available_buffers(), 0);
    }

    #[test]
    fn test_pool_reuse() {
        let pool = TileBufferPool::new(2048, 2);

        let buf1 = pool.get();
        let buf2 = pool.get();
        assert_eq!(pool.available_buffers(), 0);

        pool.return_buffer(buf1);
        pool.return_buffer(buf2);
        assert_eq!(pool.available_buffers(), 2);

        // Should reuse returned buffers
        let buf3 = pool.get();
        assert_eq!(buf3.len(), 2048);
        assert_eq!(pool.available_buffers(), 1);
    }
}
