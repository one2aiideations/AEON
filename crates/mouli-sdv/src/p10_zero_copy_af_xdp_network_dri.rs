//! Part 10: ZERO-COPY AF_XDP NETWORK DRIVER, AUTOMOTIVE CYBERSECURITY IDS/IPS (ISO 21434)

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 10 - ZERO-COPY AF_XDP NETWORK DRIVER, AUTOMOTIVE CYBERSECURITY IDS/IPS (ISO 21434)
//                   & ISO 26262 ASIL-D FAULT INJECTION & SELF-HEALING HARNESS
// CRITICALITY: ISO 26262 ASIL-D / ISO/SAE 21434 / IEEE 802.3 Zero-Copy Kernel Bypass
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Dynamic Allocation, Cacheline-Aligned Atomic Rings
// =================================================================================================

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

// =================================================================================================
// 1. CONSTANTS: AF_XDP, CYBERSECURITY IDS & FAULT INJECTION MATRICES
// =================================================================================================

/// AF_XDP UMEM Frame Size (Standard 2048-byte or 4096-byte chunk)
pub const XDP_UMEM_FRAME_SIZE: usize = 2048;
pub const XDP_RING_DEPTH: usize = 64;
pub const XDP_MAX_FRAMES: usize = 128;

/// Ethernet Frame Constraints
pub const ETH_MIN_FRAME_LEN: usize = 60;
pub const ETH_MAX_FRAME_LEN: usize = 1518;
pub const ETH_VLAN_TAG_LEN: usize = 4;

/// ISO/SAE 21434 IDS/IPS Configuration
pub const IDS_MAX_WHITELIST_RULES: usize = 32;
pub const IDS_MAX_BLOCKED_IDENTIFIERS: usize = 64;
pub const IDS_BURST_WINDOW_MS: u64 = 100;
pub const IDS_MAX_MESSAGES_PER_WINDOW: u32 = 50;
pub const IDS_PAYLOAD_MAX_ENTROPY_THRESHOLD: u32 = 780; // Scaled entropy (0-800 => 0.0 - 8.0)

/// Fault Injection Vector Constants
pub const FAULT_INJECTION_SLOTS: usize = 16;
pub const MAX_HEALTH_NODES: usize = 8;

// =================================================================================================
// 2. ERROR & STATUS TAXONOMY
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XdpError {
    Ok = 0,
    RingFull = 1,
    RingEmpty = 2,
    InvalidChunkOffset = 3,
    DescriptorInvalid = 4,
    FrameTooLarge = 5,
    UmemExhausted = 6,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SecurityAnomalyType {
    None = 0,
    RateAnomalyBurst = 1,
    UnauthorizedSourceMac = 2,
    UnauthorizedServiceId = 3,
    MalformedSomeIpHeader = 4,
    EntropyFuzzingDetected = 5,
    ReplayCounterRollback = 6,
    VlanTagViolation = 7,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FaultInjectionType {
    None = 0,
    MemoryBitFlip = 1,      // Single Event Upset (SEU)
    ClockDriftJitter = 2,   // Time synchronization drift
    WatchdogStarvation = 3, // Heartbeat pause to trigger safe state
    CrcCorruption = 4,      // Data integrity failure
    DroppedFrameBurst = 5,  // Communication loss
    SensorStuckValue = 6,   // Sensor freeze failure
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SystemSafeStateDegradation {
    Nominal = 0,               // Full operational availability
    DegradedLevel1 = 1,        // Minor comfort feature cutoff (Infotainment off)
    DegradedLevel2 = 2,        // ADAS speed limit clamped to 40 km/h
    DegradedLevel3 = 3,        // Emergency pull-over / Safe stop maneuver
    FatalLockstepShutdown = 4, // Immediate actuator zero-torque fallback
}

// =================================================================================================
// 3. ZERO-COPY AF_XDP (XSK) KERNEL-BYPASS NETWORKING PRIMITIVES
// =================================================================================================

/// AF_XDP Descriptor describing packet memory within shared UMEM
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct XdpDesc {
    pub addr: u64,  // Byte offset into UMEM
    pub len: u32,   // Packet payload length
    pub flags: u32, // Driver-specific descriptor flags
}

impl XdpDesc {
    pub const fn empty() -> Self {
        Self {
            addr: 0,
            len: 0,
            flags: 0,
        }
    }
}

/// Cacheline-Aligned Lock-Free Circular Ring for AF_XDP Queues
#[repr(C, align(64))]
pub struct XdpRing {
    entries: UnsafeCell<[XdpDesc; XDP_RING_DEPTH]>,
    producer: AtomicU32,
    cached_consumer: AtomicU32,
    consumer: AtomicU32,
    cached_producer: AtomicU32,
}

unsafe impl Sync for XdpRing {}

impl XdpRing {
    pub const fn new() -> Self {
        Self {
            entries: UnsafeCell::new([XdpDesc::empty(); XDP_RING_DEPTH]),
            producer: AtomicU32::new(0),
            cached_consumer: AtomicU32::new(0),
            consumer: AtomicU32::new(0),
            cached_producer: AtomicU32::new(0),
        }
    }

    /// Enqueue a descriptor into the ring (Producer API)
    pub fn produce(&self, desc: &XdpDesc) -> Result<(), XdpError> {
        let prod = self.producer.load(Ordering::Relaxed);
        let cons = self.consumer.load(Ordering::Acquire);

        if prod.wrapping_sub(cons) >= (XDP_RING_DEPTH as u32) {
            return Err(XdpError::RingFull);
        }

        let idx = (prod as usize) % XDP_RING_DEPTH;
        let ring = unsafe { &mut *self.entries.get() };
        ring[idx] = *desc;

        self.producer.store(prod.wrapping_add(1), Ordering::Release);
        Ok(())
    }

    /// Dequeue a descriptor from the ring (Consumer API)
    pub fn consume(&self) -> Result<XdpDesc, XdpError> {
        let cons = self.consumer.load(Ordering::Relaxed);
        let prod = self.producer.load(Ordering::Acquire);

        if cons == prod {
            return Err(XdpError::RingEmpty);
        }

        let idx = (cons as usize) % XDP_RING_DEPTH;
        let ring = unsafe { &*self.entries.get() };
        let desc = ring[idx];

        self.consumer.store(cons.wrapping_add(1), Ordering::Release);
        Ok(desc)
    }
}

impl Default for XdpRing {
    fn default() -> Self {
        Self::new()
    }
}

/// Fixed-size Memory Pool (UMEM) for Zero-Copy Networking
#[repr(C, align(64))]
pub struct XdpUmemPool {
    storage: UnsafeCell<[u8; XDP_MAX_FRAMES * XDP_UMEM_FRAME_SIZE]>,
    allocated_bitmap: AtomicU64, // 64 frames tracked per word (tracks 128 frames across 2 words)
    allocated_bitmap_hi: AtomicU64,
}

unsafe impl Sync for XdpUmemPool {}

impl XdpUmemPool {
    pub const fn new() -> Self {
        Self {
            storage: UnsafeCell::new([0u8; XDP_MAX_FRAMES * XDP_UMEM_FRAME_SIZE]),
            allocated_bitmap: AtomicU64::new(0),
            allocated_bitmap_hi: AtomicU64::new(0),
        }
    }

    /// Allocates an aligned UMEM frame offset
    pub fn allocate_frame(&self) -> Result<u64, XdpError> {
        // Search lower 64 frames
        let mut curr = self.allocated_bitmap.load(Ordering::Relaxed);
        for i in 0..64 {
            if (curr & (1 << i)) == 0 {
                let next = curr | (1 << i);
                if self
                    .allocated_bitmap
                    .compare_exchange_weak(curr, next, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return Ok((i * XDP_UMEM_FRAME_SIZE) as u64);
                }
                curr = self.allocated_bitmap.load(Ordering::Relaxed);
            }
        }

        // Search upper 64 frames
        let mut curr_hi = self.allocated_bitmap_hi.load(Ordering::Relaxed);
        for i in 0..64 {
            if (curr_hi & (1 << i)) == 0 {
                let next = curr_hi | (1 << i);
                if self
                    .allocated_bitmap_hi
                    .compare_exchange_weak(curr_hi, next, Ordering::Acquire, Ordering::Relaxed)
                    .is_ok()
                {
                    return Ok(((i + 64) * XDP_UMEM_FRAME_SIZE) as u64);
                }
                curr_hi = self.allocated_bitmap_hi.load(Ordering::Relaxed);
            }
        }

        Err(XdpError::UmemExhausted)
    }

    /// Releases an allocated frame back to the UMEM pool
    pub fn free_frame(&self, addr: u64) {
        let frame_idx = (addr as usize) / XDP_UMEM_FRAME_SIZE;
        if frame_idx < 64 {
            self.allocated_bitmap
                .fetch_and(!(1 << frame_idx), Ordering::Release);
        } else if frame_idx < XDP_MAX_FRAMES {
            self.allocated_bitmap_hi
                .fetch_and(!(1 << (frame_idx - 64)), Ordering::Release);
        }
    }

    /// Read access to `len` bytes of UMEM at `addr`, or `None` if the range is not inside it.
    ///
    /// Descriptors arrive from the network ring and so are untrusted; an unchecked slice here
    /// would let a malformed descriptor panic the packet loop.
    pub fn get_slice(&self, addr: u64, len: usize) -> Option<&[u8]> {
        let start = usize::try_from(addr).ok()?;
        let end = start.checked_add(len)?;
        // SAFETY: shared read of the pool; writers use `get_slice_mut`, whose contract requires
        // that no reader of the same range is active.
        let buf = unsafe { &*self.storage.get() };
        buf.get(start..end)
    }

    /// Write access to `len` bytes of UMEM at `addr`, or `None` if the range is not inside it.
    ///
    /// # Safety
    /// The caller must guarantee that no other reference to any part of this range exists for
    /// as long as the returned slice lives (for example because it owns the frame at `addr`).
    #[allow(clippy::mut_from_ref)]
    pub unsafe fn get_slice_mut(&self, addr: u64, len: usize) -> Option<&mut [u8]> {
        let start = usize::try_from(addr).ok()?;
        let end = start.checked_add(len)?;
        // SAFETY: exclusivity of the range is the caller's obligation (see above).
        let buf = unsafe { &mut *self.storage.get() };
        buf.get_mut(start..end)
    }
}

impl Default for XdpUmemPool {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 4. ISO/SAE 21434 AUTOMOTIVE CYBERSECURITY INTRUSION DETECTION & PREVENTION SYSTEM (IDS/IPS)
// =================================================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IdsWhitelistRule {
    pub allowed_source_mac: [u8; 6],
    pub allowed_service_id: u16,
    pub max_expected_bytes: u16,
    pub is_active: bool,
}

impl IdsWhitelistRule {
    pub const fn empty() -> Self {
        Self {
            allowed_source_mac: [0u8; 6],
            allowed_service_id: 0,
            max_expected_bytes: 0,
            is_active: false,
        }
    }
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct SecurityIncidentRecord {
    pub timestamp_ms: u64,
    pub anomaly: SecurityAnomalyType,
    pub offending_source_mac: [u8; 6],
    pub offending_service_id: u16,
    pub mitigation_applied: bool,
}

impl SecurityIncidentRecord {
    pub const fn empty() -> Self {
        Self {
            timestamp_ms: 0,
            anomaly: SecurityAnomalyType::None,
            offending_source_mac: [0u8; 6],
            offending_service_id: 0,
            mitigation_applied: false,
        }
    }
}

/// Real-Time Automotive IDS/IPS Engine with Deep Packet Inspection (DPI)
pub struct AutomotiveIdsEngine {
    whitelist_rules: UnsafeCell<[IdsWhitelistRule; IDS_MAX_WHITELIST_RULES]>,
    blocked_macs: UnsafeCell<[[u8; 6]; IDS_MAX_BLOCKED_IDENTIFIERS]>,
    blocked_count: AtomicU16,
    rate_window_start_ms: AtomicU64,
    rate_packet_counter: AtomicU32,
    last_incident: UnsafeCell<SecurityIncidentRecord>,
    incident_count: AtomicU32,
    ips_drop_counter: AtomicU64,
}

unsafe impl Sync for AutomotiveIdsEngine {}

impl AutomotiveIdsEngine {
    pub const fn new() -> Self {
        Self {
            whitelist_rules: UnsafeCell::new([IdsWhitelistRule::empty(); IDS_MAX_WHITELIST_RULES]),
            blocked_macs: UnsafeCell::new([[0u8; 6]; IDS_MAX_BLOCKED_IDENTIFIERS]),
            blocked_count: AtomicU16::new(0),
            rate_window_start_ms: AtomicU64::new(0),
            rate_packet_counter: AtomicU32::new(0),
            last_incident: UnsafeCell::new(SecurityIncidentRecord::empty()),
            incident_count: AtomicU32::new(0),
            ips_drop_counter: AtomicU64::new(0),
        }
    }

    /// Registers a certified ECU communication whitelist rule
    pub fn add_rule(&self, rule: IdsWhitelistRule) -> Result<(), SecurityAnomalyType> {
        let rules = unsafe { &mut *self.whitelist_rules.get() };
        for slot in rules.iter_mut() {
            if !slot.is_active {
                *slot = rule;
                return Ok(());
            }
        }
        Err(SecurityAnomalyType::UnauthorizedSourceMac)
    }

    /// Approximate Shannon Entropy calculation (Scaled 0..800) to detect payload fuzzing
    pub fn calculate_payload_entropy(&self, payload: &[u8]) -> u32 {
        if payload.is_empty() {
            return 0;
        }

        let mut counts = [0u16; 256];
        for &byte in payload.iter() {
            counts[byte as usize] = counts[byte as usize].saturating_add(1);
        }

        // Integer approximation of -sum(p * log2(p))
        let len = payload.len() as u32;
        let mut entropy_accum = 0u32;

        for &c in counts.iter() {
            if c > 0 {
                let p_scaled = ((c as u32) * 256) / len; // Scaled probability
                                                         // Fast bitwise log2 approximation
                let lz = p_scaled.leading_zeros();
                let log2_approx = 31 - lz;
                entropy_accum += p_scaled * (8 - log2_approx);
            }
        }

        entropy_accum / 32
    }

    /// Deep Packet Inspection (DPI) & Anomaly Detection Pipeline
    pub fn inspect_ethernet_packet(
        &self,
        src_mac: &[u8; 6],
        service_id: u16,
        payload: &[u8],
        now_ms: u64,
    ) -> Result<(), SecurityAnomalyType> {
        // 1. Check if source MAC is currently blacklisted by IPS
        let count = self.blocked_count.load(Ordering::Relaxed) as usize;
        let blocked = unsafe { &*self.blocked_macs.get() };
        for i in 0..count {
            if blocked[i] == *src_mac {
                self.ips_drop_counter.fetch_add(1, Ordering::Relaxed);
                return Err(SecurityAnomalyType::UnauthorizedSourceMac);
            }
        }

        // 2. Rate Anomaly Detection: Enforce burst rate limit
        let window_start = self.rate_window_start_ms.load(Ordering::Relaxed);
        if now_ms.saturating_sub(window_start) > IDS_BURST_WINDOW_MS {
            self.rate_window_start_ms.store(now_ms, Ordering::Relaxed);
            self.rate_packet_counter.store(1, Ordering::Relaxed);
        } else {
            let packets = self.rate_packet_counter.fetch_add(1, Ordering::SeqCst);
            if packets > IDS_MAX_MESSAGES_PER_WINDOW {
                self.record_incident(
                    SecurityAnomalyType::RateAnomalyBurst,
                    src_mac,
                    service_id,
                    now_ms,
                    true,
                );
                self.blacklist_source(src_mac);
                return Err(SecurityAnomalyType::RateAnomalyBurst);
            }
        }

        // 3. Whitelist Policy Verification
        let rules = unsafe { &*self.whitelist_rules.get() };
        let mut matched = false;
        for rule in rules.iter() {
            if rule.is_active
                && rule.allowed_source_mac == *src_mac
                && rule.allowed_service_id == service_id
                && payload.len() <= (rule.max_expected_bytes as usize)
            {
                matched = true;
                break;
            }
        }

        if !matched {
            self.record_incident(
                SecurityAnomalyType::UnauthorizedServiceId,
                src_mac,
                service_id,
                now_ms,
                false,
            );
            return Err(SecurityAnomalyType::UnauthorizedServiceId);
        }

        // 4. Entropy Fuzzing Verification
        let entropy = self.calculate_payload_entropy(payload);
        if entropy > IDS_PAYLOAD_MAX_ENTROPY_THRESHOLD && payload.len() > 64 {
            self.record_incident(
                SecurityAnomalyType::EntropyFuzzingDetected,
                src_mac,
                service_id,
                now_ms,
                true,
            );
            return Err(SecurityAnomalyType::EntropyFuzzingDetected);
        }

        Ok(())
    }

    fn blacklist_source(&self, mac: &[u8; 6]) {
        let count = self.blocked_count.load(Ordering::Relaxed) as usize;
        if count < IDS_MAX_BLOCKED_IDENTIFIERS {
            let blocked = unsafe { &mut *self.blocked_macs.get() };
            blocked[count] = *mac;
            self.blocked_count
                .store((count + 1) as u16, Ordering::Release);
        }
    }

    fn record_incident(
        &self,
        anomaly: SecurityAnomalyType,
        mac: &[u8; 6],
        service: u16,
        now_ms: u64,
        mitigated: bool,
    ) {
        self.incident_count.fetch_add(1, Ordering::Relaxed);
        let inc = unsafe { &mut *self.last_incident.get() };
        inc.timestamp_ms = now_ms;
        inc.anomaly = anomaly;
        inc.offending_source_mac = *mac;
        inc.offending_service_id = service;
        inc.mitigation_applied = mitigated;
    }

    pub fn get_last_incident(&self) -> SecurityIncidentRecord {
        unsafe { *self.last_incident.get() }
    }
}

impl Default for AutomotiveIdsEngine {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 5. ISO 26262 ASIL-D FAULT INJECTION & SELF-HEALING HARNESS
// =================================================================================================

/// Single Active Fault Injection Configuration
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct FaultInjectionConfig {
    pub target_node_id: u8,
    pub fault_type: FaultInjectionType,
    pub injection_duration_ms: u32,
    pub is_enabled: bool,
}

impl FaultInjectionConfig {
    pub const fn empty() -> Self {
        Self {
            target_node_id: 0,
            fault_type: FaultInjectionType::None,
            injection_duration_ms: 0,
            is_enabled: false,
        }
    }
}

/// Dynamic Health Status across Central Vehicle Nodes
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct NodeHealthRecord {
    pub node_id: u8,
    pub missed_heartbeats: u16,
    pub crc_error_counter: u16,
    pub bitflip_detected: bool,
    pub is_isolated: bool,
}

impl NodeHealthRecord {
    pub const fn new(id: u8) -> Self {
        Self {
            node_id: id,
            missed_heartbeats: 0,
            crc_error_counter: 0,
            bitflip_detected: false,
            is_isolated: false,
        }
    }
}

/// ASIL-D Autonomous Fault Injection Arbitrator & Self-Healing Machine
pub struct SafetyFaultHarness {
    injection_slots: UnsafeCell<[FaultInjectionConfig; FAULT_INJECTION_SLOTS]>,
    node_health: UnsafeCell<[NodeHealthRecord; MAX_HEALTH_NODES]>,
    current_safe_state: AtomicU8, // SystemSafeStateDegradation
    watchdog_suppression: AtomicBool,
}

unsafe impl Sync for SafetyFaultHarness {}

impl SafetyFaultHarness {
    pub const fn new() -> Self {
        Self {
            injection_slots: UnsafeCell::new(
                [FaultInjectionConfig::empty(); FAULT_INJECTION_SLOTS],
            ),
            node_health: UnsafeCell::new([
                NodeHealthRecord::new(0),
                NodeHealthRecord::new(1),
                NodeHealthRecord::new(2),
                NodeHealthRecord::new(3),
                NodeHealthRecord::new(4),
                NodeHealthRecord::new(5),
                NodeHealthRecord::new(6),
                NodeHealthRecord::new(7),
            ]),
            current_safe_state: AtomicU8::new(SystemSafeStateDegradation::Nominal as u8),
            watchdog_suppression: AtomicBool::new(false),
        }
    }

    /// Programs a test fault injection to evaluate ISO 26262 diagnostic coverage
    pub fn arm_fault_injection(&self, slot: usize, config: FaultInjectionConfig) -> bool {
        if slot >= FAULT_INJECTION_SLOTS {
            return false;
        }
        let slots = unsafe { &mut *self.injection_slots.get() };
        slots[slot] = config;

        if config.fault_type == FaultInjectionType::WatchdogStarvation {
            self.watchdog_suppression.store(true, Ordering::SeqCst);
        }
        true
    }

    /// Ingests node diagnostic events and triggers systematic self-healing transitions
    pub fn report_node_anomaly(&self, node_id: u8, crc_fail: bool, missed_hb: bool) {
        if (node_id as usize) >= MAX_HEALTH_NODES {
            return;
        }

        let nodes = unsafe { &mut *self.node_health.get() };
        let node = &mut nodes[node_id as usize];

        if crc_fail {
            node.crc_error_counter = node.crc_error_counter.saturating_add(1);
        }
        if missed_hb {
            node.missed_heartbeats = node.missed_heartbeats.saturating_add(1);
        }

        // Evaluate fail-operational escalation policy
        if node.missed_heartbeats >= 3 || node.crc_error_counter >= 5 {
            node.is_isolated = true; // Isolate faulting node
            self.escalate_safe_state(SystemSafeStateDegradation::DegradedLevel2);
        }

        if node.missed_heartbeats >= 10 {
            // Unrecoverable loss of safety-critical node
            self.escalate_safe_state(SystemSafeStateDegradation::DegradedLevel3);
        }
    }

    fn escalate_safe_state(&self, target: SystemSafeStateDegradation) {
        let mut curr = self.current_safe_state.load(Ordering::Relaxed);
        while (target as u8) > curr {
            match self.current_safe_state.compare_exchange_weak(
                curr,
                target as u8,
                Ordering::SeqCst,
                Ordering::Relaxed,
            ) {
                Ok(_) => break,
                Err(actual) => curr = actual,
            }
        }
    }

    pub fn get_safe_state(&self) -> SystemSafeStateDegradation {
        match self.current_safe_state.load(Ordering::Acquire) {
            0 => SystemSafeStateDegradation::Nominal,
            1 => SystemSafeStateDegradation::DegradedLevel1,
            2 => SystemSafeStateDegradation::DegradedLevel2,
            3 => SystemSafeStateDegradation::DegradedLevel3,
            _ => SystemSafeStateDegradation::FatalLockstepShutdown,
        }
    }
}

impl Default for SafetyFaultHarness {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. MASTER PART 10 VEHICLE OS INTEGRATION FACADE
// =================================================================================================

pub struct Part10VehicleOsSubsystem {
    pub xdp_pool: XdpUmemPool,
    pub rx_ring: XdpRing,
    pub tx_ring: XdpRing,
    pub ids_engine: AutomotiveIdsEngine,
    pub safety_harness: SafetyFaultHarness,
}

impl Part10VehicleOsSubsystem {
    pub const fn new() -> Self {
        Self {
            xdp_pool: XdpUmemPool::new(),
            rx_ring: XdpRing::new(),
            tx_ring: XdpRing::new(),
            ids_engine: AutomotiveIdsEngine::new(),
            safety_harness: SafetyFaultHarness::new(),
        }
    }

    /// Process zero-copy RX queue frames through automotive cybersecurity filter
    pub fn process_rx_cycle(&self, now_ms: u64) -> Result<u32, XdpError> {
        let mut frames_processed = 0u32;

        while let Ok(desc) = self.rx_ring.consume() {
            // A descriptor pointing outside UMEM is malformed (or hostile): drop it, never panic.
            let Some(slice) = self.xdp_pool.get_slice(desc.addr, desc.len as usize) else {
                continue;
            };

            if slice.len() >= 14 {
                let mut src_mac = [0u8; 6];
                src_mac.copy_from_slice(&slice[6..12]);
                let ethertype = u16::from_be_bytes([slice[12], slice[13]]);

                // Mock SOME/IP or IP service inspection (Service ID at offset 14..16)
                let service_id = if slice.len() >= 16 {
                    u16::from_be_bytes([slice[14], slice[15]])
                } else {
                    0
                };

                let payload = if slice.len() > 16 { &slice[16..] } else { &[] };

                // Inspect through IDS engine
                if let Err(anomaly) = self
                    .ids_engine
                    .inspect_ethernet_packet(&src_mac, service_id, payload, now_ms)
                {
                    // Packet dropped by IDS/IPS; free frame back to pool
                    self.xdp_pool.free_frame(desc.addr);
                    continue;
                }
            }

            // Normal packet consumption path
            self.xdp_pool.free_frame(desc.addr);
            frames_processed += 1;
        }

        Ok(frames_processed)
    }
}

impl Default for Part10VehicleOsSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 7. VERIFICATION SUITE & UNIT TESTS
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_xdp_ring_produce_and_consume() {
        let ring = XdpRing::new();
        let desc = XdpDesc {
            addr: 0x4000,
            len: 128,
            flags: 0,
        };

        assert!(ring.produce(&desc).is_ok());
        let popped = ring
            .consume()
            .expect("Consumer must return queued descriptor");
        assert_eq!(popped.addr, 0x4000);
        assert_eq!(popped.len, 128);
        assert_eq!(ring.consume(), Err(XdpError::RingEmpty));
    }

    #[test]
    fn test_ids_whitelist_enforcement() {
        let ids = AutomotiveIdsEngine::new();
        let trusted_mac = [0x00, 0x1A, 0x2B, 0x3C, 0x4D, 0x5E];
        let untrusted_mac = [0xAA, 0xBB, 0xCC, 0xDD, 0xEE, 0xFF];

        let rule = IdsWhitelistRule {
            allowed_source_mac: trusted_mac,
            allowed_service_id: 0x1001,
            max_expected_bytes: 64,
            is_active: true,
        };
        assert!(ids.add_rule(rule).is_ok());

        // Whitelisted packet should pass
        let dummy_payload = [0x01u8; 32];
        let res = ids.inspect_ethernet_packet(&trusted_mac, 0x1001, &dummy_payload, 1000);
        assert_eq!(res, Ok(()));

        // Untrusted MAC should be rejected
        let res_untrusted =
            ids.inspect_ethernet_packet(&untrusted_mac, 0x1001, &dummy_payload, 1000);
        assert_eq!(
            res_untrusted,
            Err(SecurityAnomalyType::UnauthorizedServiceId)
        );
    }

    #[test]
    fn test_safety_fault_harness_safe_state_escalation() {
        let harness = SafetyFaultHarness::new();
        assert_eq!(
            harness.get_safe_state(),
            SystemSafeStateDegradation::Nominal
        );

        // Report persistent heartbeats failure on Node 1
        for _ in 0..3 {
            harness.report_node_anomaly(1, false, true);
        }
        assert_eq!(
            harness.get_safe_state(),
            SystemSafeStateDegradation::DegradedLevel2
        );

        // Catastrophic failure trigger
        for _ in 0..7 {
            harness.report_node_anomaly(1, false, true);
        }
        assert_eq!(
            harness.get_safe_state(),
            SystemSafeStateDegradation::DegradedLevel3
        );
    }

    #[test]
    fn umem_slices_are_bounds_checked() {
        let pool = XdpUmemPool::new();
        let total = XDP_MAX_FRAMES * XDP_UMEM_FRAME_SIZE;
        assert_eq!(pool.get_slice(0, 14).map(<[u8]>::len), Some(14));
        assert_eq!(pool.get_slice(0, total).map(<[u8]>::len), Some(total));
        assert!(
            pool.get_slice(0, total + 1).is_none(),
            "one byte past the end"
        );
        assert!(
            pool.get_slice(total as u64, 1).is_none(),
            "start at the end"
        );
        assert!(
            pool.get_slice(total as u64 - 1, 2).is_none(),
            "straddling the end"
        );
        assert!(pool.get_slice(u64::MAX, 1).is_none(), "huge address");
        assert!(
            pool.get_slice(1, usize::MAX).is_none(),
            "address plus length overflows"
        );
        // SAFETY: nothing else references the pool in this test.
        unsafe {
            assert!(pool.get_slice_mut(0, total + 1).is_none());
            assert!(pool.get_slice_mut(u64::MAX, usize::MAX).is_none());
            assert_eq!(pool.get_slice_mut(10, 4).map(|s| s.len()), Some(4));
        }
    }

    #[test]
    fn a_hostile_descriptor_cannot_panic_the_receive_loop() {
        let engine = Part10VehicleOsSubsystem::new();
        let evil = [
            XdpDesc {
                addr: u64::MAX,
                len: 64,
                flags: 0,
            },
            XdpDesc {
                addr: 0,
                len: u32::MAX,
                flags: 0,
            },
            XdpDesc {
                addr: (XDP_MAX_FRAMES * XDP_UMEM_FRAME_SIZE) as u64,
                len: 1,
                flags: 0,
            },
            XdpDesc {
                addr: 1,
                len: u32::MAX,
                flags: 0,
            },
        ];
        for d in evil {
            assert!(engine.rx_ring.produce(&d).is_ok());
        }
        // Must return normally and consume every queued descriptor.
        let processed = engine.process_rx_cycle(1_000);
        assert!(processed.is_ok());
        assert_eq!(engine.rx_ring.consume(), Err(XdpError::RingEmpty));
    }
}
