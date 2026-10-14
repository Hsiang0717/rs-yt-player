use std::sync::{Arc, Mutex};

pub const MAX_BYTE_CACHE_BYTES: usize = 32 * 1024 * 1024; // 32 MB capacity bound

/// Sparse store of already-downloaded byte segments, shared across seeks of the same stream
/// so that seeking into a previously downloaded region needs no new HTTP request.
#[derive(Default)]
pub struct ByteCache {
    segments: std::collections::BTreeMap<u64, Vec<u8>>,
}

pub type SharedByteCache = Arc<Mutex<ByteCache>>;

impl ByteCache {
    /// Copies up to `max` cached bytes starting exactly at `pos`, if `pos` is cached.
    pub fn read_at(&self, pos: u64, max: usize) -> Option<Vec<u8>> {
        let (start, seg) = self.segments.range(..=pos).next_back()?;
        let offset = (pos - start) as usize;
        if offset >= seg.len() {
            return None;
        }
        let end = seg.len().min(offset + max);
        Some(seg[offset..end].to_vec())
    }

    pub fn total_bytes(&self) -> usize {
        self.segments.values().map(|s| s.len()).sum()
    }

    pub fn insert(&mut self, pos: u64, data: &[u8]) {
        if data.is_empty() {
            return;
        }
        let mut key = pos;
        let prev = self
            .segments
            .range(..=pos)
            .next_back()
            .map(|(s, v)| (*s, v.len() as u64));
        match prev {
            Some((start, len)) if start + len >= pos => {
                key = start;
                let seg = self.segments.get_mut(&start).unwrap();
                let overlap = (start + len - pos) as usize;
                if overlap < data.len() {
                    seg.extend_from_slice(&data[overlap..]);
                }
            }
            _ => {
                self.segments.insert(pos, data.to_vec());
            }
        }
        // Merge following segments that now touch or overlap.
        loop {
            let seg_end = key + self.segments[&key].len() as u64;
            let next = self.segments.range(key + 1..).next().map(|(s, _)| *s);
            match next {
                Some(next_start) if next_start <= seg_end => {
                    let next_seg = self.segments.remove(&next_start).unwrap();
                    let skip = (seg_end - next_start) as usize;
                    if skip < next_seg.len() {
                        self.segments
                            .get_mut(&key)
                            .unwrap()
                            .extend_from_slice(&next_seg[skip..]);
                    }
                }
                _ => break,
            }
        }

        // Enforce maximum memory bound for multi-hour audio
        let mut total = self.total_bytes();
        while total > MAX_BYTE_CACHE_BYTES && self.segments.len() > 1 {
            let min_key = *self.segments.keys().next().unwrap();
            let max_key = *self.segments.keys().next_back().unwrap();
            let evict_key = if pos.abs_diff(min_key) >= pos.abs_diff(max_key) {
                min_key
            } else {
                max_key
            };
            if let Some(removed) = self.segments.remove(&evict_key) {
                total = total.saturating_sub(removed.len());
            } else {
                break;
            }
        }

        if total > MAX_BYTE_CACHE_BYTES
            && let Some((&first_key, seg)) = self.segments.iter_mut().next()
                && seg.len() > MAX_BYTE_CACHE_BYTES {
                    let excess = seg.len() - MAX_BYTE_CACHE_BYTES;
                    if pos > first_key + excess as u64 {
                        seg.drain(..excess);
                        let new_key = first_key + excess as u64;
                        let moved_seg = std::mem::take(seg);
                        self.segments.remove(&first_key);
                        self.segments.insert(new_key, moved_seg);
                    } else {
                        seg.truncate(MAX_BYTE_CACHE_BYTES);
                    }
                }
    }
}
