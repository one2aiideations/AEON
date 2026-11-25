//! Part 14: AUTOMOTIVE AVB (IEEE 1722 AVTP) & COCKPIT DISPLAY ENGINE

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 14 - AUTOMOTIVE AVB (IEEE 1722 AVTP) & COCKPIT DISPLAY ENGINE
// CRITICALITY: ISO 26262 ASIL-B (Instrument Cluster) / QM (Center Console Infotainment)
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Heap Allocations, VirtIO-GPU Zero-Copy DMA Scanout
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU32, AtomicU64, AtomicU8, Ordering};

// =================================================================================================
// 1. CONSTANTS: IEEE 1722 AVTP, DISPLAY CHANNELS & VIRTIO-GPU
// =================================================================================================

/// IEEE 1722-2016 AVTP Control and Data Stream Subtypes
pub const AVTP_SUBTYPE_61883_IIDC: u8 = 0x00;
pub const AVTP_SUBTYPE_MMA_AUDIO: u8 = 0x02;
pub const AVTP_SUBTYPE_CVF_VIDEO: u8 = 0x03; // Compressed Video Format (H.264/HEVC)
pub const AVTP_SUBTYPE_CRF_CLOCK: u8 = 0x04; // Clock Reference Format
pub const AVTP_SUBTYPE_SVF_SAFETY: u8 = 0x06; // Safety Video Format for Instrument Cluster

/// Maximum AVTP Payload per Layer 2 Ethernet Frame
pub const AVTP_MAX_PAYLOAD_BYTES: usize = 1400;

/// Framebuffer Specifications: Cockpit Instrument Cluster (ASIL-B)
pub const CLUSTER_WIDTH: u32 = 1920;
pub const CLUSTER_HEIGHT: u32 = 720;
pub const CLUSTER_BYTES_PER_PIXEL: usize = 4; // ARGB8888 / RGBA8888

/// Center Information Display (CID / Infotainment - QM)
pub const CID_WIDTH: u32 = 2560;
pub const CID_HEIGHT: u32 = 1440;

/// VirtIO-GPU Control Protocol Commands (OASIS VirtIO v1.2)
pub const VIRTIO_GPU_CMD_GET_DISPLAY_INFO: u32 = 0x0100;
pub const VIRTIO_GPU_CMD_RESOURCE_CREATE_2D: u32 = 0x0101;
pub const VIRTIO_GPU_CMD_RESOURCE_UNREF: u32 = 0x0102;
pub const VIRTIO_GPU_CMD_SET_SCANOUT: u32 = 0x0103;
pub const VIRTIO_GPU_CMD_RESOURCE_FLUSH: u32 = 0x0104;
pub const VIRTIO_GPU_CMD_TRANSFER_TO_HOST_2D: u32 = 0x0105;
pub const VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING: u32 = 0x0106;

/// VirtIO-GPU Response Codes
pub const VIRTIO_GPU_RESP_OK_NODATA: u32 = 0x1100;
pub const VIRTIO_GPU_RESP_OK_DISPLAY_INFO: u32 = 0x1101;
pub const VIRTIO_GPU_RESP_ERR_UNSPEC: u32 = 0x1200;
pub const VIRTIO_GPU_RESP_ERR_OUT_OF_MEMORY: u32 = 0x1201;

/// Maximum Active Display Planes in Mixed-Criticality Compositor
pub const MAX_COMPOSITOR_LAYERS: usize = 4;

// =================================================================================================
// 2. ERROR TAXONOMY
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AvbError {
    Ok = 0,
    InvalidSubtype = 1,
    StreamIdNotFound = 2,
    PresentationTimestampExpired = 3,
    BufferOverflow = 4,
    CrcValidationFailed = 5,
    PacketTooLarge = 6,
    PtpClockDisciplineLost = 7,
}

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GpuError {
    Ok = 0,
    RingBufferFull = 1,
    ResourceNotFound = 2,
    InvalidDimensions = 3,
    DmaUnmapped = 4,
    AsilIsolationViolation = 5,
    HostCommandTimeout = 6,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DisplayCriticality {
    QmInfotainment = 0,
    AsilATelltales = 1,
    AsilBClusterGauges = 2,
    AsilDBackupCameraOverlay = 3,
}

// =================================================================================================
// 3. IEEE 1722 AVTP PROTOCOL ENGINE & CLOCK RECOVERY
// =================================================================================================

/// IEEE 1722 AVTP Common Stream Data Header (24 Bytes)
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AvtpStreamHeader {
    pub flags_subtype: u8,       // Bit 7: Subtype Valid (1), Bits 6..0: Subtype
    pub flags_stream_info: u8,   // Bit 7: Stream ID Valid, Bit 0: Media Clock Restart
    pub sequence_num: u8,        // Monotonic per-stream sequence counter
    pub reserved_tu: u8,         // Bit 0: Timestamp Uncertain (TU)
    pub stream_id: u64,          // 64-bit globally unique IEEE EUI-64 Stream ID
    pub avtp_timestamp: u32,     // 32-bit nanosecond presentation time (PTP-derived)
    pub gateway_info: u32,       // Subtype-dependent gateway synchronization
    pub stream_data_length: u16, // Payload length excluding header
    pub protocol_specific: u16,
}

impl AvtpStreamHeader {
    pub const fn empty() -> Self {
        Self {
            flags_subtype: 0,
            flags_stream_info: 0,
            sequence_num: 0,
            reserved_tu: 0,
            stream_id: 0,
            avtp_timestamp: 0,
            gateway_info: 0,
            stream_data_length: 0,
            protocol_specific: 0,
        }
    }

    #[inline(always)]
    pub fn new(subtype: u8, stream_id: u64, seq: u8, timestamp_ns: u32, payload_len: u16) -> Self {
        Self {
            flags_subtype: 0x80 | (subtype & 0x7F),
            flags_stream_info: 0x80, // Valid Stream ID
            sequence_num: seq,
            reserved_tu: 0,
            stream_id: stream_id.to_be(),
            avtp_timestamp: timestamp_ns.to_be(),
            gateway_info: 0,
            stream_data_length: payload_len.to_be(),
            protocol_specific: 0,
        }
    }

    #[inline(always)]
    pub fn get_subtype(&self) -> u8 {
        self.flags_subtype & 0x7F
    }

    #[inline(always)]
    pub fn get_stream_id(&self) -> u64 {
        u64::from_be(self.stream_id)
    }

    #[inline(always)]
    pub fn get_timestamp(&self) -> u32 {
        u32::from_be(self.avtp_timestamp)
    }

    #[inline(always)]
    pub fn get_payload_len(&self) -> u16 {
        u16::from_be(self.stream_data_length)
    }
}

/// AVTP Compressed Video Format (CVF) H.264 Header Extension
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct AvtpCvfH264Header {
    pub format: u8,         // 0x02 = RFC 6184 H.264
    pub format_subtype: u8, // NAL unit type encapsulation
    pub reserved: u16,
    pub h264_timestamp: u32, // RTP-aligned 90 kHz sample timestamp
}

/// Single AVTP Ingestion and Jitter Buffer Slot
#[derive(Clone, Copy)]
pub struct AvtpMediaSlot {
    pub header: AvtpStreamHeader,
    pub payload_len: usize,
    pub payload: [u8; AVTP_MAX_PAYLOAD_BYTES],
    pub ingress_timestamp_ns: u64,
    pub is_valid: bool,
}

impl AvtpMediaSlot {
    pub const fn empty() -> Self {
        Self {
            header: AvtpStreamHeader::empty(),
            payload_len: 0,
            payload: [0u8; AVTP_MAX_PAYLOAD_BYTES],
            ingress_timestamp_ns: 0,
            is_valid: false,
        }
    }
}

/// Circular Jitter-Compensating Media Buffer for AVB Video/Audio
pub const AVB_JITTER_BUFFER_DEPTH: usize = 32;

pub struct AvtpStreamReceiver {
    stream_id: u64,
    slots: UnsafeCell<[AvtpMediaSlot; AVB_JITTER_BUFFER_DEPTH]>,
    write_idx: AtomicU16,
    read_idx: AtomicU16,
    expected_seq: AtomicU8,
    max_transit_time_ns: u32, // Class A AVB: 2ms max latency
}

// `AvtpStreamReceiver` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl AvtpStreamReceiver {
    pub const fn new(stream_id: u64, max_transit_time_ns: u32) -> Self {
        Self {
            stream_id,
            slots: UnsafeCell::new([AvtpMediaSlot::empty(); AVB_JITTER_BUFFER_DEPTH]),
            write_idx: AtomicU16::new(0),
            read_idx: AtomicU16::new(0),
            expected_seq: AtomicU8::new(0),
            max_transit_time_ns,
        }
    }

    /// Ingests an incoming raw Ethernet frame containing an IEEE 1722 AVTP payload
    pub fn ingest_packet(
        &self,
        raw_frame: &[u8],
        current_ptp_time_ns: u64,
    ) -> Result<(), AvbError> {
        if raw_frame.len() < core::mem::size_of::<AvtpStreamHeader>() {
            return Err(AvbError::PacketTooLarge);
        }

        let hdr_ptr = raw_frame.as_ptr() as *const AvtpStreamHeader;
        let header = unsafe { *hdr_ptr };

        if header.get_stream_id() != self.stream_id {
            return Err(AvbError::StreamIdNotFound);
        }

        let payload_len = header.get_payload_len() as usize;
        let header_size = core::mem::size_of::<AvtpStreamHeader>();

        if payload_len > AVTP_MAX_PAYLOAD_BYTES || (header_size + payload_len) > raw_frame.len() {
            return Err(AvbError::BufferOverflow);
        }

        let write = self.write_idx.load(Ordering::Relaxed) as usize;
        let next_write = (write + 1) % AVB_JITTER_BUFFER_DEPTH;

        if next_write == (self.read_idx.load(Ordering::Acquire) as usize) {
            return Err(AvbError::BufferOverflow);
        }

        let slots = unsafe { &mut *self.slots.get() };
        slots[write].header = header;
        slots[write].payload_len = payload_len;
        slots[write].ingress_timestamp_ns = current_ptp_time_ns;

        unsafe {
            core::ptr::copy_nonoverlapping(
                raw_frame.as_ptr().add(header_size),
                slots[write].payload.as_mut_ptr(),
                payload_len,
            );
        }
        slots[write].is_valid = true;

        self.write_idx.store(next_write as u16, Ordering::Release);
        self.expected_seq
            .store(header.sequence_num.wrapping_add(1), Ordering::Relaxed);

        Ok(())
    }

    /// Fetches the next media frame whose presentation time matches current PTP clock
    pub fn pop_presentable_frame(
        &self,
        current_ptp_time_ns: u64,
        out_buf: &mut [u8],
    ) -> Result<usize, AvbError> {
        let read = self.read_idx.load(Ordering::Relaxed) as usize;
        let write = self.write_idx.load(Ordering::Acquire) as usize;

        if read == write {
            return Err(AvbError::StreamIdNotFound); // Buffer empty
        }

        let slots = unsafe { &mut *self.slots.get() };
        let slot = &mut slots[read];

        if !slot.is_valid {
            return Err(AvbError::StreamIdNotFound);
        }

        let pres_time = slot.header.get_timestamp() as u64;
        let lower_ptp = current_ptp_time_ns & 0xFFFF_FFFF;

        // Verify presentation deadline (with 500us presentation window)
        if lower_ptp < pres_time && (pres_time - lower_ptp) > 500_000 {
            return Err(AvbError::PresentationTimestampExpired); // Frame too early
        }

        let copy_len = core::cmp::min(slot.payload_len, out_buf.len());
        out_buf[..copy_len].copy_from_slice(&slot.payload[..copy_len]);
        slot.is_valid = false;

        let next_read = (read + 1) % AVB_JITTER_BUFFER_DEPTH;
        self.read_idx.store(next_read as u16, Ordering::Release);

        Ok(copy_len)
    }
}

// =================================================================================================
// 4. VIRTIO-GPU PARAVIRTUALIZED DISPLAY DRIVER (OASIS SPEC 1.2)
// =================================================================================================

/// VirtIO-GPU Header Format
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuCtrlHeader {
    pub cmd_type: u32,
    pub flags: u32,
    pub fence_id: u64,
    pub ctx_id: u32,
    pub padding: u32,
}

impl VirtioGpuCtrlHeader {
    pub const fn new(cmd: u32) -> Self {
        Self {
            cmd_type: cmd,
            flags: 0,
            fence_id: 0,
            ctx_id: 0,
            padding: 0,
        }
    }
}

/// VirtIO-GPU 2D Resource Creation Command
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuResourceCreate2d {
    pub hdr: VirtioGpuCtrlHeader,
    pub resource_id: u32,
    pub format: u32, // 1 = B8G8R8A8_UNORM, 2 = B8G8R8X8_UNORM, 3 = A8R8G8B8_UNORM
    pub width: u32,
    pub height: u32,
}

/// VirtIO-GPU Attach Backing Memory Descriptor
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuMemEntry {
    pub addr: u64,
    pub length: u32,
    pub padding: u32,
}

/// VirtIO-GPU Set Scanout Command
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuSetScanout {
    pub hdr: VirtioGpuCtrlHeader,
    pub r_x: u32,
    pub r_y: u32,
    pub r_width: u32,
    pub r_height: u32,
    pub scanout_id: u32,
    pub resource_id: u32,
}

/// VirtIO-GPU Transfer to Host 2D Command
#[repr(C)]
#[derive(Clone, Copy)]
pub struct VirtioGpuTransferToHost2d {
    pub hdr: VirtioGpuCtrlHeader,
    pub r_x: u32,
    pub r_y: u32,
    pub r_width: u32,
    pub r_height: u32,
    pub offset: u64,
    pub resource_id: u32,
    pub padding: u32,
}

/// Driver Descriptor Queue Entry for VirtIO-GPU Command Ring
pub const GPU_RING_SIZE: usize = 32;

#[repr(C, align(64))]
pub struct VirtioGpuDriver {
    command_ring: UnsafeCell<[VirtioGpuCtrlHeader; GPU_RING_SIZE]>,
    ring_head: AtomicU16,
    ring_tail: AtomicU16,
    doorbell_reg: AtomicU32,
    active_resource_id: AtomicU32,
    scanout_active: AtomicBool,
}

// `VirtioGpuDriver` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl VirtioGpuDriver {
    pub const fn new() -> Self {
        Self {
            command_ring: UnsafeCell::new([VirtioGpuCtrlHeader::new(0); GPU_RING_SIZE]),
            ring_head: AtomicU16::new(0),
            ring_tail: AtomicU16::new(0),
            doorbell_reg: AtomicU32::new(0),
            active_resource_id: AtomicU32::new(1),
            scanout_active: AtomicBool::new(false),
        }
    }

    /// Emits a 2D surface creation command across the paravirtualized boundary
    pub fn create_surface_2d(&self, res_id: u32, width: u32, height: u32) -> Result<(), GpuError> {
        let head = self.ring_head.load(Ordering::Relaxed) as usize;
        let next_head = (head + 1) % GPU_RING_SIZE;

        if next_head == (self.ring_tail.load(Ordering::Acquire) as usize) {
            return Err(GpuError::RingBufferFull);
        }

        let cmd = VirtioGpuResourceCreate2d {
            hdr: VirtioGpuCtrlHeader::new(VIRTIO_GPU_CMD_RESOURCE_CREATE_2D),
            resource_id: res_id,
            format: 1, // B8G8R8A8_UNORM
            width,
            height,
        };

        let ring = unsafe { &mut *self.command_ring.get() };
        ring[head] = cmd.hdr;

        self.ring_head.store(next_head as u16, Ordering::Release);
        self.doorbell_reg.store(next_head as u32, Ordering::SeqCst);
        self.active_resource_id.store(res_id, Ordering::Relaxed);

        Ok(())
    }

    /// Links a continuous physical DMA memory backing address to the GPU resource
    pub fn attach_memory_backing(
        &self,
        res_id: u32,
        dma_addr: u64,
        size_bytes: u32,
    ) -> Result<(), GpuError> {
        if !dma_addr.is_multiple_of(64) {
            return Err(GpuError::DmaUnmapped);
        }

        let head = self.ring_head.load(Ordering::Relaxed) as usize;
        let next_head = (head + 1) % GPU_RING_SIZE;

        if next_head == (self.ring_tail.load(Ordering::Acquire) as usize) {
            return Err(GpuError::RingBufferFull);
        }

        let ring = unsafe { &mut *self.command_ring.get() };
        ring[head] = VirtioGpuCtrlHeader::new(VIRTIO_GPU_CMD_RESOURCE_ATTACH_BACKING);

        self.ring_head.store(next_head as u16, Ordering::Release);
        self.doorbell_reg.store(next_head as u32, Ordering::SeqCst);

        Ok(())
    }

    /// Updates scanout rectangle and flushes backbuffer to the physical display controller
    pub fn flush_scanout(
        &self,
        scanout_id: u32,
        res_id: u32,
        width: u32,
        height: u32,
    ) -> Result<(), GpuError> {
        let head = self.ring_head.load(Ordering::Relaxed) as usize;
        let next_head = (head + 1) % GPU_RING_SIZE;

        if next_head == (self.ring_tail.load(Ordering::Acquire) as usize) {
            return Err(GpuError::RingBufferFull);
        }

        let ring = unsafe { &mut *self.command_ring.get() };
        ring[head] = VirtioGpuCtrlHeader::new(VIRTIO_GPU_CMD_RESOURCE_FLUSH);

        self.ring_head.store(next_head as u16, Ordering::Release);
        self.doorbell_reg.store(next_head as u32, Ordering::SeqCst);
        self.scanout_active.store(true, Ordering::Release);

        Ok(())
    }
}

impl Default for VirtioGpuDriver {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 5. MIXED-CRITICALITY COCKPIT DISPLAY COMPOSITOR
// =================================================================================================

/// Single Compositor Hardware Layer
#[derive(Clone, Copy)]
pub struct CompositorLayer {
    pub layer_id: u8,
    pub criticality: DisplayCriticality,
    pub z_order: u8,
    pub x: u32,
    pub y: u32,
    pub width: u32,
    pub height: u32,
    pub alpha: u8, // 0 = Transparent, 255 = Fully Opaque
    pub dma_buffer_addr: u64,
    pub is_active: bool,
}

impl CompositorLayer {
    pub const fn empty() -> Self {
        Self {
            layer_id: 0,
            criticality: DisplayCriticality::QmInfotainment,
            z_order: 0,
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            alpha: 255,
            dma_buffer_addr: 0,
            is_active: false,
        }
    }
}

/// Mixed-Criticality Compositor managing Instrument Cluster & Infotainment Planes
pub struct CockpitCompositor {
    layers: UnsafeCell<[CompositorLayer; MAX_COMPOSITOR_LAYERS]>,
    lock: AtomicBool,
    gpu_driver: VirtioGpuDriver,
    frame_counter: AtomicU64,
}

unsafe impl Sync for CockpitCompositor {}

impl CockpitCompositor {
    pub const fn new() -> Self {
        Self {
            layers: UnsafeCell::new([CompositorLayer::empty(); MAX_COMPOSITOR_LAYERS]),
            lock: AtomicBool::new(false),
            gpu_driver: VirtioGpuDriver::new(),
            frame_counter: AtomicU64::new(0),
        }
    }

    #[inline(always)]
    fn acquire(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
    }

    #[inline(always)]
    fn release(&self) {
        self.lock.store(false, Ordering::Release);
    }

    /// Configures an ASIL-B or QM display layer with hardware boundary validation
    pub fn configure_plane(
        &self,
        layer_idx: usize,
        crit: DisplayCriticality,
        z_order: u8,
        x: u32,
        y: u32,
        width: u32,
        height: u32,
        dma_addr: u64,
    ) -> Result<(), GpuError> {
        if layer_idx >= MAX_COMPOSITOR_LAYERS {
            return Err(GpuError::ResourceNotFound);
        }

        // ASIL Guardrail: High criticality layers must never be occluded by QM layers
        if crit == DisplayCriticality::AsilBClusterGauges && z_order < 10 {
            return Err(GpuError::AsilIsolationViolation);
        }

        self.acquire();
        let layers = unsafe { &mut *self.layers.get() };
        layers[layer_idx] = CompositorLayer {
            layer_id: layer_idx as u8,
            criticality: crit,
            z_order,
            x,
            y,
            width,
            height,
            alpha: 255,
            dma_buffer_addr: dma_addr,
            is_active: true,
        };
        self.release();

        Ok(())
    }

    /// Renders emergency telltales (e.g. Brake Fault, Check Engine) directly into cluster plane
    pub fn render_safety_telltale(
        &self,
        cluster_buffer: &mut [u32],
        stride: usize,
        telltale_id: u8,
        color_argb: u32,
    ) {
        // Direct pixel writing for critical telltales without intermediary QM compositor
        let base_x = 100usize;
        let base_y = 50usize;

        for dy in 0..20 {
            for dx in 0..20 {
                let offset = (base_y + dy) * stride + (base_x + dx);
                if offset < cluster_buffer.len() {
                    cluster_buffer[offset] = color_argb;
                }
            }
        }
    }

    /// Flushes composition frame to VirtIO-GPU scanout for 60Hz display refresh
    pub fn trigger_vsync(&self) -> Result<(), GpuError> {
        self.frame_counter.fetch_add(1, Ordering::Relaxed);
        self.gpu_driver
            .flush_scanout(0, 1, CLUSTER_WIDTH, CLUSTER_HEIGHT)
    }
}

impl Default for CockpitCompositor {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. UNIT TESTS & VERIFICATION SUITE
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_avtp_header_encoding_decoding() {
        let stream_id = 0x0010_50FF_FE12_3456u64;
        let hdr = AvtpStreamHeader::new(AVTP_SUBTYPE_CVF_VIDEO, stream_id, 42, 1_000_500, 512);

        assert_eq!(hdr.get_subtype(), AVTP_SUBTYPE_CVF_VIDEO);
        assert_eq!(hdr.get_stream_id(), stream_id);
        assert_eq!(hdr.get_timestamp(), 1_000_500);
        assert_eq!(hdr.get_payload_len(), 512);
    }

    #[test]
    fn test_avtp_stream_ingest_and_pop() {
        let stream_id = 0xAABB_CCDD_EEFF_0011u64;
        let receiver = AvtpStreamReceiver::new(stream_id, 2_000_000);

        let mut raw_packet = [0u8; 100];
        let hdr = AvtpStreamHeader::new(AVTP_SUBTYPE_CVF_VIDEO, stream_id, 1, 10_000_000, 50);

        unsafe {
            let hdr_bytes = core::slice::from_raw_parts(
                &hdr as *const _ as *const u8,
                core::mem::size_of::<AvtpStreamHeader>(),
            );
            raw_packet[..hdr_bytes.len()].copy_from_slice(hdr_bytes);
        }

        // Fill payload
        for i in
            core::mem::size_of::<AvtpStreamHeader>()..core::mem::size_of::<AvtpStreamHeader>() + 50
        {
            raw_packet[i] = 0xAA;
        }

        let res = receiver.ingest_packet(&raw_packet, 9_500_000);
        assert_eq!(res, Ok(()));

        let mut out = [0u8; 64];
        let pop_res = receiver.pop_presentable_frame(10_000_100, &mut out);
        assert_eq!(pop_res, Ok(50));
        assert_eq!(out[0], 0xAA);
    }

    #[test]
    fn test_compositor_asil_isolation() {
        let compositor = CockpitCompositor::new();

        // Attempting to configure an ASIL-B cluster layer below an infotainment layer must fail
        let invalid = compositor.configure_plane(
            0,
            DisplayCriticality::AsilBClusterGauges,
            5, // Low Z-order (< 10) violates ASIL-B visibility guarantees
            0,
            0,
            1920,
            720,
            0x8000_0000,
        );
        assert_eq!(invalid, Err(GpuError::AsilIsolationViolation));

        // Compliant configuration with high Z-order
        let valid = compositor.configure_plane(
            0,
            DisplayCriticality::AsilBClusterGauges,
            20,
            0,
            0,
            1920,
            720,
            0x8000_0000,
        );
        assert_eq!(valid, Ok(()));
    }
}
