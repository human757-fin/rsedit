//! Frame cache + background decoder.
//!
//! Golden rule from the spec: the UI thread never blocks on decode. The cache
//! owns a worker thread that decodes whichever (asset,video) the playhead is on
//! and inserts frames into a small sliding-window map keyed by (asset, frame-pts).
//! The UI asks for `nearest(asset, pts_us)` and gets an `Arc`, or None while the
//! worker is still catching up to the target.

use std::collections::HashMap;
use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use crate::decoder::{VideoFrame, VideoSource};

type CacheMap = HashMap<(u64, i64), Arc<VideoFrame>>;

struct Shared {
    cache: Mutex<CacheMap>,
}

#[derive(Clone)]
pub struct FrameRequest {
    pub asset: u64,
    pub path: String,
    pub w: u32,
    pub h: u32,
    pub pts_us: i64,
}

enum Command {
    Work(FrameRequest),
    Stop,
}

pub struct FrameCache {
    shared: Arc<Shared>,
    tx: Sender<Command>,
    worker: Option<JoinHandle<()>>,
}

impl FrameCache {
    pub fn new() -> Self {
        let shared = Arc::new(Shared {
            cache: Mutex::new(HashMap::new()),
        });
        let (tx, rx) = channel();
        let worker = Some(spawn_worker(shared.clone(), rx));
        Self { shared, tx, worker }
    }

    /// Ask the worker to make frames up to `req.pts_us` available for this asset.
    pub fn set_target(&self, req: FrameRequest) {
        let _ = self.tx.send(Command::Work(req));
    }

    /// Nearest cached frame at or before `pts_us` for `asset`, else closest.
    pub fn nearest(&self, asset: u64, pts_us: i64, up_to: Option<i64>) -> Option<Arc<VideoFrame>> {
        let cache = self.shared.cache.lock().unwrap();
        let before = cache
            .iter()
            .filter(|&(k, _)| k.0 == asset && k.1 <= pts_us)
            .max_by_key(|&(k, _)| k.1)
            .map(|(_, v)| v.clone());
        if before.is_some() {
            return before;
        }
        // Fall back to the first frame after the requested point (SEO-on-miss).
        let after = cache
            .iter()
            .filter(|&(k, _)| {
                k.0 == asset
                    && (up_to.map_or(true, |u| k.1 <= u))
            })
            .min_by_key(|&(k, _)| (k.1 - pts_us).abs())
            .map(|(_, v)| v.clone());
        after
    }
}

impl Drop for FrameCache {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(h) = self.worker.take() {
            let _ = h.join();
        }
    }
}

fn spawn_worker(shared: Arc<Shared>, rx: Receiver<Command>) -> JoinHandle<()> {
    std::thread::spawn(move || {
        let mut current: Option<VideoSource> = None;
        let mut current_key: Option<(u64, String, u32, u32)> = None;
        // pts of the last frame we decoded for the current asset
        let mut last_decoded: Option<i64> = None;

        while let Ok(cmd) = rx.recv() {
            match cmd {
                Command::Stop => break,
                Command::Work(req) => {
                    let key = (req.asset, req.path.clone(), req.w, req.h);
                    if current_key.as_ref() != Some(&key) {
                        current = VideoSource::open(&req.path, req.w, req.h).ok();
                        current_key = Some(key);
                        last_decoded = None;
                    }
                    let Some(src) = current.as_mut() else { continue };

                    let target = req.pts_us.max(0);
                    let need_forward_seek = match last_decoded {
                        Some(l) => target >= l,
                        None => true,
                    };

                    // If we have a decoder open and just need a little more decode,
                    // step forward without reopening.
                    let start_pts = if need_forward_seek {
                        last_decoded.unwrap_or((target - 200_000).max(0))
                    } else {
                        (target - 200_000).max(0)
                    };
                    if need_forward_seek && last_decoded.is_some() {
                        // continue from where we were
                    } else if src.seek_to(start_pts).is_err() {
                        continue;
                    }

                    // Decode forward until we pass the requested pts.
                    let mut decided = 0;
                    loop {
                        match src.next_frame() {
                            Ok(Some(f)) => {
                                shared
                                    .cache
                                    .lock()
                                    .unwrap()
                                    .insert((req.asset, f.pts_us), Arc::new(f.clone()));
                                last_decoded = Some(f.pts_us);
                                if f.pts_us >= target || decided > 600 {
                                    break;
                                }
                                decided += 1;
                            }
                            Ok(None) => break,
                            Err(_) => break,
                        }
                    }

                    // Trim the window: keep ~3s before target, ~1.5s after.
                    let low = (target - 3_000_000).max(0);
                    let high = target + 1_500_000;
                    shared
                        .cache
                        .lock()
                        .unwrap()
                        .retain(|&(_, p), _| p >= low && p <= high);
                }
            }
        }
    })
}