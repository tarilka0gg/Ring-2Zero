// Encoding Thread Pool Implementation
// Provides persistent worker threads with warm encoder cache

use std::thread;
use std::sync::Arc;
use std::time::Duration;
use crossbeam::channel::{bounded, Sender, Receiver};
use crate::tile::Tile;
use crate::tile_buffer_pool::TileBufferPool;

pub struct EncodingTask {
    pub tile: Tile,
    pub tile_data: Vec<u8>,  // RGB data for this tile
    pub tile_idx: usize,     // Original index
}

pub struct EncodedResult {
    pub tile_idx: usize,
    pub data: Vec<u8>,
}

pub struct EncodingPool {
    task_tx: Sender<EncodingTask>,
    result_rx: Receiver<EncodedResult>,
    workers: Vec<thread::JoinHandle<()>>,
}

impl EncodingPool {
    pub fn new(num_workers: usize, buffer_pool: TileBufferPool) -> Self {
        let (task_tx, task_rx) = bounded(num_workers * 2);
        let (result_tx, result_rx) = bounded(num_workers * 2);

        let workers: Vec<_> = (0..num_workers)
            .map(|_worker_id| {
                let task_rx: Receiver<EncodingTask> = task_rx.clone();
                let result_tx: Sender<EncodedResult> = result_tx.clone();
                let pool = buffer_pool.clone();  // Clone Arc (cheap)

                thread::spawn(move || {
                    // Worker loop
                    while let Ok(task) = task_rx.recv() {
                        let encoded = fast_webp::encode_rgba(
                            &task.tile_data,
                            task.tile.width,
                            task.tile.height,
                            fast_webp::WebpOptions {
                                quality: task.tile.quality,
                                ..Default::default()
                            },
                        ).unwrap_or_else(|e| {
                            eprintln!("WebP encoding error: {:?}", e);
                            Vec::new()
                        });

                        let _ = result_tx.send(EncodedResult {
                            tile_idx: task.tile_idx,
                            data: encoded,
                        });

                        // Return buffer to pool immediately after encoding
                        pool.return_buffer(task.tile_data);
                    }
                })
            })
            .collect();

        Self {
            task_tx,
            result_rx,
            workers,
        }
    }

    pub fn submit(&self, task: EncodingTask) -> Result<(), crossbeam::channel::SendError<EncodingTask>> {
        self.task_tx.send(task)
    }

    pub fn collect_results(&self, count: usize) -> Vec<EncodedResult> {
        let mut results = Vec::with_capacity(count);
        let timeout = Duration::from_secs(5);

        for _ in 0..count {
            match self.result_rx.recv_timeout(timeout) {
                Ok(result) => results.push(result),
                Err(_) => {
                    // Timeout or disconnect - worker likely panicked
                    eprintln!("Warning: encoding_pool.collect_results() timeout after {} results (expected {})", results.len(), count);
                    break;
                }
            }
        }
        results
    }
}

impl Drop for EncodingPool {
    fn drop(&mut self) {
        // Rust automatically drops self.task_tx when EncodingPool is dropped
        // This closes the channel and signals workers to exit
        // No need to explicitly drop - the clone() was causing the leak!
    }
}
