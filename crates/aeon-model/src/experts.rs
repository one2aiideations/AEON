//! Tiered mixture-of-experts store, routing and a SwiGLU expert kernel.
//!
//! Every routed expert of a model has the same packed size, so the store
//! keeps two arrays of fixed-size slots:
//!
//! * **fast**: the hot set (pinned or GPU-visible memory), never evicted;
//! * **ram**: an LRU cache;
//! * **disk**: the model source, read only when the router picks an expert.
//!
//! An optional per-layer allow mask restricts routing to a domain hot set.

use crate::source::ModelSource;
use crate::ModelError;

/// Upper bound on experts per layer.
pub const MAX_EXPERTS_PER_LAYER: u32 = 256;
const MASK_WORDS: usize = (MAX_EXPERTS_PER_LAYER as usize) / 64;

/// Identifies one expert.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ExpertKey {
    /// Layer index.
    pub layer: u32,
    /// Expert index within the layer.
    pub expert: u32,
}

/// Where an expert was served from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Tier {
    /// Fast (pinned) tier.
    Fast,
    /// RAM cache hit.
    Ram,
    /// Just loaded from the source into RAM.
    Disk,
}

/// Counters.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExpertStats {
    /// Fast-tier hits.
    pub hit_fast: u64,
    /// RAM-tier hits.
    pub hit_ram: u64,
    /// Loads from the source.
    pub disk_loads: u64,
    /// Bytes loaded from the source.
    pub disk_bytes: u64,
    /// RAM evictions.
    pub evictions: u64,
    /// Requests refused by the allow mask.
    pub denied: u64,
}

/// Store sizing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExpertConfig {
    /// Packed bytes per expert.
    pub expert_bytes: usize,
    /// Number of layers.
    pub layers: u32,
    /// Experts per layer (at most [`MAX_EXPERTS_PER_LAYER`]).
    pub experts_per_layer: u32,
    /// Slots in the fast tier.
    pub fast_slots: usize,
    /// Slots in the RAM tier.
    pub ram_slots: usize,
}

#[derive(Debug, Clone, Copy, Default)]
struct Slot {
    key: Option<ExpertKey>,
    pinned: bool,
    age: u64,
}

#[derive(Debug)]
struct TierBuf {
    data: Vec<u8>,
    slots: Vec<Slot>,
}

impl TierBuf {
    fn new(n: usize, expert_bytes: usize) -> Result<Self, ModelError> {
        let len = n.checked_mul(expert_bytes).ok_or(ModelError::BadConfig)?;
        Ok(Self {
            data: vec![0; len],
            slots: vec![Slot::default(); n],
        })
    }

    fn find(&self, k: ExpertKey) -> Option<usize> {
        self.slots.iter().position(|s| s.key == Some(k))
    }

    fn free(&self) -> Option<usize> {
        self.slots.iter().position(|s| s.key.is_none())
    }

    /// Free slot, else least-recently-used unpinned slot (flagged as eviction).
    fn victim(&self) -> Option<(usize, bool)> {
        if let Some(i) = self.free() {
            return Some((i, false));
        }
        self.slots
            .iter()
            .enumerate()
            .filter(|(_, s)| !s.pinned)
            .min_by_key(|(_, s)| s.age)
            .map(|(i, _)| (i, true))
    }
}

/// Tiered expert store over a model source.
///
/// `locate` maps an expert to the byte offset of its packed weights.
pub struct ExpertStore<S, L> {
    cfg: ExpertConfig,
    src: S,
    locate: L,
    fast: TierBuf,
    ram: TierBuf,
    masks: Option<Vec<[u64; MASK_WORDS]>>,
    clock: u64,
    stats: ExpertStats,
}

impl<S, L> core::fmt::Debug for ExpertStore<S, L> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.debug_struct("ExpertStore")
            .field("cfg", &self.cfg)
            .field("stats", &self.stats)
            .finish_non_exhaustive()
    }
}

impl<S: ModelSource, L: Fn(ExpertKey) -> Option<u64>> ExpertStore<S, L> {
    /// Build a store; all buffers are allocated up front.
    pub fn new(cfg: ExpertConfig, src: S, locate: L) -> Result<Self, ModelError> {
        if cfg.expert_bytes == 0
            || cfg.layers == 0
            || cfg.experts_per_layer == 0
            || cfg.experts_per_layer > MAX_EXPERTS_PER_LAYER
        {
            return Err(ModelError::BadConfig);
        }
        Ok(Self {
            fast: TierBuf::new(cfg.fast_slots, cfg.expert_bytes)?,
            ram: TierBuf::new(cfg.ram_slots, cfg.expert_bytes)?,
            cfg,
            src,
            locate,
            masks: None,
            clock: 0,
            stats: ExpertStats::default(),
        })
    }

    fn check(&self, k: ExpertKey) -> Result<(), ModelError> {
        if k.layer < self.cfg.layers && k.expert < self.cfg.experts_per_layer {
            Ok(())
        } else {
            Err(ModelError::BadExpert)
        }
    }

    fn tick(&mut self) -> u64 {
        self.clock += 1;
        self.clock
    }

    fn load(&mut self, k: ExpertKey, ram: bool, slot: usize) -> Result<(), ModelError> {
        let off = (self.locate)(k).ok_or(ModelError::BadExpert)?;
        let n = self.cfg.expert_bytes;
        let tier = if ram { &mut self.ram } else { &mut self.fast };
        let dst = &mut tier.data[slot * n..(slot + 1) * n];
        self.src.read_at(off, dst)?;
        self.stats.disk_loads += 1;
        self.stats.disk_bytes += n as u64;
        Ok(())
    }

    fn slice(&self, ram: bool, slot: usize) -> &[u8] {
        let n = self.cfg.expert_bytes;
        let tier = if ram { &self.ram } else { &self.fast };
        &tier.data[slot * n..(slot + 1) * n]
    }

    /// Load `k` into the fast tier, or pin it in RAM if fast is full.
    pub fn pin(&mut self, k: ExpertKey) -> Result<Tier, ModelError> {
        self.check(k)?;
        if self.fast.find(k).is_some() {
            return Ok(Tier::Fast);
        }
        if let Some(i) = self.ram.find(k) {
            self.ram.slots[i].pinned = true;
            return Ok(Tier::Ram);
        }
        if let Some(i) = self.fast.free() {
            self.load(k, false, i)?;
            let age = self.tick();
            self.fast.slots[i] = Slot {
                key: Some(k),
                pinned: true,
                age,
            };
            return Ok(Tier::Fast);
        }
        let (i, evict) = self.ram.victim().ok_or(ModelError::NoSlot)?;
        self.ram.slots[i].key = None;
        self.load(k, true, i)?;
        if evict {
            self.stats.evictions += 1;
        }
        let age = self.tick();
        self.ram.slots[i] = Slot {
            key: Some(k),
            pinned: true,
            age,
        };
        Ok(Tier::Ram)
    }

    /// Packed weights of `k`, loading from the source on a miss.
    pub fn get(&mut self, k: ExpertKey) -> Result<(&[u8], Tier), ModelError> {
        self.check(k)?;
        if !self.is_allowed(k) {
            self.stats.denied += 1;
            return Err(ModelError::Denied);
        }
        if let Some(i) = self.fast.find(k) {
            self.stats.hit_fast += 1;
            let age = self.tick();
            self.fast.slots[i].age = age;
            return Ok((self.slice(false, i), Tier::Fast));
        }
        if let Some(i) = self.ram.find(k) {
            self.stats.hit_ram += 1;
            let age = self.tick();
            self.ram.slots[i].age = age;
            return Ok((self.slice(true, i), Tier::Ram));
        }
        let (i, evict) = self.ram.victim().ok_or(ModelError::NoSlot)?;
        self.ram.slots[i].key = None;
        self.load(k, true, i)?;
        if evict {
            self.stats.evictions += 1;
        }
        let age = self.tick();
        self.ram.slots[i] = Slot {
            key: Some(k),
            pinned: false,
            age,
        };
        Ok((self.slice(true, i), Tier::Disk))
    }

    /// Restrict `layer` to `experts` (enables masking).
    pub fn allow_only(&mut self, layer: u32, experts: &[u32]) -> Result<(), ModelError> {
        if layer >= self.cfg.layers || experts.iter().any(|&e| e >= self.cfg.experts_per_layer) {
            return Err(ModelError::BadExpert);
        }
        let layers = self.cfg.layers as usize;
        let masks = self
            .masks
            .get_or_insert_with(|| vec![[u64::MAX; MASK_WORDS]; layers]);
        let m = &mut masks[layer as usize];
        *m = [0; MASK_WORDS];
        for &e in experts {
            m[(e / 64) as usize] |= 1u64 << (e % 64);
        }
        Ok(())
    }

    /// Remove every mask.
    pub fn allow_all(&mut self) {
        self.masks = None;
    }

    /// Whether routing may use `k`.
    #[must_use]
    pub fn is_allowed(&self, k: ExpertKey) -> bool {
        if self.check(k).is_err() {
            return false;
        }
        match &self.masks {
            None => true,
            Some(m) => (m[k.layer as usize][(k.expert / 64) as usize] >> (k.expert % 64)) & 1 == 1,
        }
    }

    /// Counters so far.
    #[must_use]
    pub fn stats(&self) -> ExpertStats {
        self.stats
    }

    /// Bytes held across both tiers.
    #[must_use]
    pub fn resident_bytes(&self) -> usize {
        let n = self
            .fast
            .slots
            .iter()
            .chain(&self.ram.slots)
            .filter(|s| s.key.is_some())
            .count();
        n * self.cfg.expert_bytes
    }
}

/// Softmax over `logits` (disallowed experts excluded), then top-k.
///
/// Returns `(expert, weight)` pairs in descending weight order. With
/// `renormalise`, the selected weights sum to 1.
pub fn route(
    logits: &[f32],
    allowed: impl Fn(usize) -> bool,
    top_k: usize,
    renormalise: bool,
) -> Result<Vec<(usize, f32)>, ModelError> {
    if logits.is_empty()
        || logits.len() > MAX_EXPERTS_PER_LAYER as usize
        || top_k == 0
        || top_k > logits.len()
    {
        return Err(ModelError::BadConfig);
    }
    let live: Vec<usize> = (0..logits.len()).filter(|&e| allowed(e)).collect();
    let max = live
        .iter()
        .map(|&e| logits[e])
        .fold(f32::NEG_INFINITY, f32::max);
    if live.len() < top_k || !max.is_finite() {
        return Err(ModelError::Denied);
    }
    let mut probs: Vec<(usize, f32)> = live.iter().map(|&e| (e, (logits[e] - max).exp())).collect();
    let sum: f32 = probs.iter().map(|p| p.1).sum();
    for p in &mut probs {
        p.1 /= sum;
    }
    probs.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
    probs.truncate(top_k);
    if renormalise {
        let sel: f32 = probs.iter().map(|p| p.1).sum();
        if sel > 0.0 {
            for p in &mut probs {
                p.1 /= sel;
            }
        }
    }
    Ok(probs)
}

/// One SwiGLU expert on one token, accumulated into `out`.
///
/// `packed` holds row-major f32 `gate[inter][hidden] | up[inter][hidden] |
/// down[hidden][inter]`. Computes `out += weight * down(silu(gate x) * up x)`.
pub fn expert_accumulate(
    packed: &[f32],
    hidden: usize,
    inter: usize,
    x: &[f32],
    weight: f32,
    out: &mut [f32],
) -> Result<(), ModelError> {
    let block = hidden.checked_mul(inter).ok_or(ModelError::BadConfig)?;
    let need = block.checked_mul(3).ok_or(ModelError::BadConfig)?;
    if hidden == 0 || inter == 0 || packed.len() < need || x.len() != hidden || out.len() != hidden
    {
        return Err(ModelError::BadConfig);
    }
    let (gate, rest) = packed.split_at(block);
    let (up, down) = rest.split_at(block);
    let dot = |row: &[f32]| row.iter().zip(x).map(|(w, v)| w * v).sum::<f32>();
    let act: Vec<f32> = gate
        .chunks_exact(hidden)
        .zip(up.chunks_exact(hidden))
        .map(|(g, u)| {
            let a = dot(g);
            (a / (1.0 + (-a).exp())) * dot(u)
        })
        .collect();
    for (o, row) in out.iter_mut().zip(down.chunks_exact(inter)) {
        *o += weight * row.iter().zip(&act).map(|(w, a)| w * a).sum::<f32>();
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn store(fast: usize, ram: usize) -> ExpertStore<Vec<u8>, impl Fn(ExpertKey) -> Option<u64>> {
        // 2 layers x 4 experts x 2 bytes; expert bytes = [layer, expert].
        let mut data = Vec::new();
        for l in 0..2u8 {
            for e in 0..4u8 {
                data.extend_from_slice(&[l, e]);
            }
        }
        let cfg = ExpertConfig {
            expert_bytes: 2,
            layers: 2,
            experts_per_layer: 4,
            fast_slots: fast,
            ram_slots: ram,
        };
        ExpertStore::new(cfg, data, |k: ExpertKey| {
            Some(u64::from(k.layer * 4 + k.expert) * 2)
        })
        .unwrap()
    }

    fn key(layer: u32, expert: u32) -> ExpertKey {
        ExpertKey { layer, expert }
    }

    #[test]
    fn tiers_and_lru() {
        let mut s = store(1, 2);
        assert_eq!(s.pin(key(0, 0)).unwrap(), Tier::Fast);
        assert_eq!(s.get(key(0, 0)).unwrap(), (&[0u8, 0][..], Tier::Fast));
        assert_eq!(s.get(key(1, 2)).unwrap(), (&[1u8, 2][..], Tier::Disk));
        assert_eq!(s.get(key(1, 3)).unwrap().1, Tier::Disk);
        assert_eq!(s.get(key(1, 2)).unwrap().1, Tier::Ram);
        // (1,3) is now LRU and gets evicted.
        assert_eq!(s.get(key(0, 1)).unwrap().1, Tier::Disk);
        assert_eq!(s.get(key(1, 3)).unwrap().1, Tier::Disk);
        let st = s.stats();
        assert_eq!(st.evictions, 2);
        assert_eq!(st.hit_fast, 1);
        assert_eq!(s.resident_bytes(), 6);
    }

    #[test]
    fn pinned_ram_is_kept() {
        let mut s = store(0, 1);
        assert_eq!(s.pin(key(0, 1)).unwrap(), Tier::Ram);
        assert_eq!(s.get(key(0, 2)), Err(ModelError::NoSlot));
        assert_eq!(s.get(key(0, 1)).unwrap().1, Tier::Ram);
    }

    #[test]
    fn masks_deny() {
        let mut s = store(0, 2);
        s.allow_only(0, &[1, 3]).unwrap();
        assert!(s.is_allowed(key(0, 3)));
        assert!(!s.is_allowed(key(0, 0)));
        assert!(s.is_allowed(key(1, 0)));
        assert_eq!(s.get(key(0, 0)), Err(ModelError::Denied));
        assert_eq!(s.stats().denied, 1);
        s.allow_all();
        assert!(s.get(key(0, 0)).is_ok());
        assert_eq!(s.get(key(5, 0)), Err(ModelError::BadExpert));
    }

    #[test]
    fn routing_top_k() {
        let logits = [0.0f32, 2.0, 1.0, 3.0];
        let r = route(&logits, |_| true, 2, true).unwrap();
        assert_eq!(r[0].0, 3);
        assert_eq!(r[1].0, 1);
        assert!((r[0].1 + r[1].1 - 1.0).abs() < 1e-6);
        let r = route(&logits, |e| e != 3, 1, false).unwrap();
        assert_eq!(r[0].0, 1);
        assert_eq!(route(&logits, |e| e == 0, 2, true), Err(ModelError::Denied));
    }

    #[test]
    fn swiglu_expert() {
        // hidden = 1, inter = 1: gate = 1, up = 2, down = 3, x = 1.
        let packed = [1.0f32, 2.0, 3.0];
        let mut out = [0.5f32];
        expert_accumulate(&packed, 1, 1, &[1.0], 0.5, &mut out).unwrap();
        let silu = 1.0 / (1.0 + (-1.0f32).exp());
        let want = 0.5 + 0.5 * 3.0 * silu * 2.0;
        assert!((out[0] - want).abs() < 1e-6);
        assert!(expert_accumulate(&packed, 2, 1, &[1.0], 1.0, &mut out).is_err());
    }
}
