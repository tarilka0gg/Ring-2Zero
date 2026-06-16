// Encoding Thread Pool Implementation
// Provides persistent worker threads with warm encoder cache

use std::thread;
use std::sync::Arc;
use crossbeam::channel::{bounded, Sender, Receiver};
use crate::tile::Tile;

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
    pub fn new(num_workers: usize) -> Self {
        let (task_tx, task_rx) = bounded(num_workers * 2);
        let (result_tx, result_rx) = bounded(num_workers * 2);

        let workers: Vec<_> = (0..num_workers)
            .map(|_worker_id| {
                let task_rx: Receiver<EncodingTask> = task_rx.clone();
                let result_tx: Sender<EncodedResult> = result_tx.clone();

                thread::spawn(move || {
                    // Worker loop
                    while let Ok(task) = task_rx.recv() {
                        let encoded = webp::Encoder::from_rgba(
                            &task.tile_data,
                            task.tile.width,
                            task.tile.height,
                        )
                        .encode(task.tile.quality);

                        let _ = result_tx.send(EncodedResult {
                            tile_idx: task.tile_idx,
                            data: encoded.to_vec(),
                        });
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
        for _ in 0..count {
            if let Ok(result) = self.result_rx.recv() {
                results.push(result);
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
