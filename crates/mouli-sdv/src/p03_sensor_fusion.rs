//! Part 3: SENSOR FUSION, NEURAL PPU INTERFACE, 4D KALMAN FILTER & AEB CORRIDOR GATING

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 3 - SENSOR FUSION, NEURAL PPU INTERFACE, 4D KALMAN FILTER & AEB CORRIDOR GATING
// CRITICALITY: ISO 26262 ASIL-D (Actuation Bounds & AEB) / ASIL-B (Perception Association)
// TARGET ARCH: aarch64-unknown-none (Cortex-A78AE / Cortex-R82) & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Dynamic Allocation, Constant-Time Matrix Operations
// =================================================================================================

use core::cell::UnsafeCell;
use core::hint::spin_loop;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicUsize, Ordering};

// =================================================================================================
// 1. ARCHITECTURAL LIMITS & SENSOR SYSTEM CONFIGURATION
// =================================================================================================

pub const MAX_PERCEPTION_TARGETS: usize = 64;
pub const MAX_CAMERA_OBJECTS: usize = 32;
pub const MAX_RADAR_TARGETS: usize = 64;
pub const MAX_LIDAR_CLUSTERS: usize = 32;

pub const PPU_DMA_RING_CAPACITY: usize = 128;
pub const PPU_TENSOR_DESCRIPTOR_SLOTS: usize = 16;

/// Standard ISO 8855 Vehicle Coordinate Frame Bounds:
/// X: Forward (+), Y: Left (+), Z: Up (+)
pub const VEHICLE_WIDTH_METERS: f32 = 2.10;
pub const VEHICLE_LENGTH_METERS: f32 = 4.95;
pub const SAFETY_CORRIDOR_LATERAL_HALF_WIDTH_METERS: f32 = 1.85;

/// Autonomous Emergency Braking Thresholds
pub const AEB_TTC_CRITICAL_THRESHOLD_SECONDS: f32 = 1.20;
pub const AEB_TTC_WARNING_THRESHOLD_SECONDS: f32 = 2.40;
pub const AEB_MAX_DECELERATION_MPS2: f32 = 9.81; // 1.0G hard braking limit

// =================================================================================================
// 2. EMBEDDED MATH ENGINE FOR NO_STD KALMAN FILTERING (Trigonometry & Linear Algebra)
// =================================================================================================

pub struct EmbeddedMath;

impl EmbeddedMath {
    /// Fast inverse square root (Carmack algorithm tailored for 32-bit floats)
    #[inline(always)]
    pub fn inv_sqrt(x: f32) -> f32 {
        if !(x > 0.0) || !x.is_finite() {
            return 0.0;
        }
        let i = x.to_bits();
        let i = 0x5f3759df_u32.wrapping_sub(i >> 1);
        let y = f32::from_bits(i);
        y * (1.5 - 0.5 * x * y * y)
    }

    #[inline(always)]
    pub fn sqrt(x: f32) -> f32 {
        if !(x > 0.0) {
            return 0.0;
        }
        // Accurate Newton iteration from `fmath` (the old 1/inv_sqrt was off by up to 0.2%).
        crate::fmath::sqrt(f64::from(x)) as f32
    }

    /// Four-quadrant arctangent in radians, accurate to about 1e-7 (via `fmath`). The earlier
    /// approximation was off by up to 0.07 rad (about 4 degrees).
    pub fn atan2(y: f32, x: f32) -> f32 {
        crate::fmath::atan2(f64::from(y), f64::from(x)) as f32
    }

    /// Sine in radians (via `fmath`). Non-finite input gives NaN; huge input cannot loop (the
    /// earlier version subtracted 2*pi in a loop that never ended for infinity or 1e30).
    pub fn sin(x: f32) -> f32 {
        crate::fmath::sin(f64::from(x)) as f32
    }

    /// Cosine in radians (via `fmath`).
    #[inline(always)]
    pub fn cos(x: f32) -> f32 {
        crate::fmath::cos(f64::from(x)) as f32
    }
}

// =================================================================================================
// 3. FIXED 4x4 MATRIX ALGEBRA FOR 4D TRACKING STATE
// =================================================================================================

/// State vector x = [pos_x, pos_y, vel_x, vel_y]^T
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Vector4 {
    pub data: [f32; 4],
}

impl Vector4 {
    pub const fn zero() -> Self {
        Self { data: [0.0; 4] }
    }

    pub const fn new(px: f32, py: f32, vx: f32, vy: f32) -> Self {
        Self {
            data: [px, py, vx, vy],
        }
    }

    #[inline(always)]
    pub fn add(&self, rhs: &Vector4) -> Vector4 {
        Vector4 {
            data: [
                self.data[0] + rhs.data[0],
                self.data[1] + rhs.data[1],
                self.data[2] + rhs.data[2],
                self.data[3] + rhs.data[3],
            ],
        }
    }

    #[inline(always)]
    pub fn sub(&self, rhs: &Vector4) -> Vector4 {
        Vector4 {
            data: [
                self.data[0] - rhs.data[0],
                self.data[1] - rhs.data[1],
                self.data[2] - rhs.data[2],
                self.data[3] - rhs.data[3],
            ],
        }
    }
}

/// 4x4 Covariance and Transition Matrix
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Matrix4x4 {
    pub data: [[f32; 4]; 4],
}

impl Matrix4x4 {
    pub const fn identity() -> Self {
        Self {
            data: [
                [1.0, 0.0, 0.0, 0.0],
                [0.0, 1.0, 0.0, 0.0],
                [0.0, 0.0, 1.0, 0.0],
                [0.0, 0.0, 0.0, 1.0],
            ],
        }
    }

    pub const fn zero() -> Self {
        Self {
            data: [[0.0; 4]; 4],
        }
    }

    pub fn multiply_vector(&self, vec: &Vector4) -> Vector4 {
        let mut out = [0.0f32; 4];
        let mut i = 0;
        while i < 4 {
            out[i] = self.data[i][0] * vec.data[0]
                + self.data[i][1] * vec.data[1]
                + self.data[i][2] * vec.data[2]
                + self.data[i][3] * vec.data[3];
            i += 1;
        }
        Vector4 { data: out }
    }

    pub fn multiply_matrix(&self, rhs: &Matrix4x4) -> Matrix4x4 {
        let mut out = [[0.0f32; 4]; 4];
        let mut i = 0;
        while i < 4 {
            let mut j = 0;
            while j < 4 {
                out[i][j] = self.data[i][0] * rhs.data[0][j]
                    + self.data[i][1] * rhs.data[1][j]
                    + self.data[i][2] * rhs.data[2][j]
                    + self.data[i][3] * rhs.data[3][j];
                j += 1;
            }
            i += 1;
        }
        Matrix4x4 { data: out }
    }

    pub fn transpose(&self) -> Matrix4x4 {
        let mut out = [[0.0f32; 4]; 4];
        let mut i = 0;
        while i < 4 {
            let mut j = 0;
            while j < 4 {
                out[i][j] = self.data[j][i];
                j += 1;
            }
            i += 1;
        }
        Matrix4x4 { data: out }
    }

    pub fn add(&self, rhs: &Matrix4x4) -> Matrix4x4 {
        let mut out = [[0.0f32; 4]; 4];
        let mut i = 0;
        while i < 4 {
            let mut j = 0;
            while j < 4 {
                out[i][j] = self.data[i][j] + rhs.data[i][j];
                j += 1;
            }
            i += 1;
        }
        Matrix4x4 { data: out }
    }

    pub fn sub(&self, rhs: &Matrix4x4) -> Matrix4x4 {
        let mut out = [[0.0f32; 4]; 4];
        let mut i = 0;
        while i < 4 {
            let mut j = 0;
            while j < 4 {
                out[i][j] = self.data[i][j] - rhs.data[i][j];
                j += 1;
            }
            i += 1;
        }
        Matrix4x4 { data: out }
    }
}

// =================================================================================================
// 4. SENSOR INPUT ABSTRACTIONS & CALIBRATED PHYSICAL FRAMES
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PerceptionClassification {
    Unknown = 0,
    PassengerCar = 1,
    HeavyTruck = 2,
    Motorcycle = 3,
    Pedestrian = 4,
    Cyclist = 5,
    Debris = 6,
    TrafficSign = 7,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct CameraObjectDetection {
    pub object_id: u16,
    pub class: PerceptionClassification,
    pub confidence: f32,     // 0.0 .. 1.0
    pub lateral_m: f32,      // Y
    pub longitudinal_m: f32, // X
    pub length_m: f32,
    pub width_m: f32,
    pub time_stamp_micros: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct RadarTargetDetection {
    pub target_id: u16,
    pub radial_distance_m: f32,   // Range (r)
    pub azimuth_rad: f32,         // Angle (theta)
    pub radial_velocity_mps: f32, // Doppler (r_dot)
    pub rcs_db: f32,              // Radar Cross Section
    pub snr_linear: f32,
    pub time_stamp_micros: u64,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct LidarClusterDetection {
    pub cluster_id: u16,
    pub centroid_x: f32,
    pub centroid_y: f32,
    pub centroid_z: f32,
    pub point_count: u32,
    pub time_stamp_micros: u64,
}

// =================================================================================================
// 5. HARDWARE NEURAL PARALLEL PROCESSING UNIT (PPU) INTERFACE
// =================================================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct PpuDmaTensorDescriptor {
    pub input_frame_id: u64,
    pub physical_address_src: u64,
    pub physical_address_dst: u64,
    pub buffer_length_bytes: u32,
    pub quant_zero_point: i32,
    pub quant_scale_factor: f32,
    pub valid_flag: bool,
}

pub struct PpuAcceleratorDriver {
    descriptors: [UnsafeCell<PpuDmaTensorDescriptor>; PPU_TENSOR_DESCRIPTOR_SLOTS],
    hardware_doorbell_reg: *mut u32,
    job_head: AtomicUsize,
    job_tail: AtomicUsize,
    is_busy: AtomicBool,
}

// SAFETY: the interior-mutable descriptors form a ring with `job_head` and `job_tail` as atomics.
// Sound only under the single-producer, single-consumer rule: one thread at a time calls
// `dispatch_inference_job` (writes the slot at `head`, then publishes it with a Release store) and
// one calls `poll_inference_complete` (reads the slot at `tail`, only below the Acquire-loaded
// `head`). Nothing in the types enforces that; two producers would write the same slot.
// `hardware_doorbell_reg` is a raw pointer that is only written through (see its use).
unsafe impl Sync for PpuAcceleratorDriver {}

impl PpuAcceleratorDriver {
    pub const fn new(doorbell_address: usize) -> Self {
        const INIT_DESC: UnsafeCell<PpuDmaTensorDescriptor> =
            UnsafeCell::new(PpuDmaTensorDescriptor {
                input_frame_id: 0,
                physical_address_src: 0,
                physical_address_dst: 0,
                buffer_length_bytes: 0,
                quant_zero_point: 0,
                quant_scale_factor: 1.0,
                valid_flag: false,
            });

        Self {
            descriptors: [INIT_DESC; PPU_TENSOR_DESCRIPTOR_SLOTS],
            hardware_doorbell_reg: doorbell_address as *mut u32,
            job_head: AtomicUsize::new(0),
            job_tail: AtomicUsize::new(0),
            is_busy: AtomicBool::new(false),
        }
    }

    pub fn dispatch_inference_job(
        &self,
        frame_id: u64,
        src: u64,
        dst: u64,
        len: u32,
    ) -> Result<usize, &'static str> {
        let head = self.job_head.load(Ordering::Relaxed);
        let tail = self.job_tail.load(Ordering::Acquire);

        if head.wrapping_sub(tail) >= PPU_TENSOR_DESCRIPTOR_SLOTS {
            return Err("PPU DMA Command Queue Full");
        }

        let slot = head & (PPU_TENSOR_DESCRIPTOR_SLOTS - 1);
        // SAFETY: the check above shows head - tail < slots, so the consumer cannot be reading this
        // slot, and by the single-producer rule nobody else is writing it. It is not published
        // until the Release store below.
        let desc = unsafe { &mut *self.descriptors[slot].get() };
        desc.input_frame_id = frame_id;
        desc.physical_address_src = src;
        desc.physical_address_dst = dst;
        desc.buffer_length_bytes = len;
        desc.valid_flag = true;

        self.job_head.store(head.wrapping_add(1), Ordering::Release);

        // Ring MMIO doorbell to trigger DMA engine on PPU coprocessor
        if !self.hardware_doorbell_reg.is_null() {
            // SAFETY: the pointer is non-null (checked) and was built from the doorbell address
            // given to `new`; the integrator must have given the address of a writable 32-bit
            // device register, which this code cannot check. Volatile so the write is kept.
            unsafe {
                core::ptr::write_volatile(self.hardware_doorbell_reg, slot as u32);
            }
        }

        Ok(slot)
    }

    pub fn poll_inference_complete(&self) -> bool {
        let tail = self.job_tail.load(Ordering::Relaxed);
        let head = self.job_head.load(Ordering::Acquire);
        if tail == head {
            return true;
        }

        let slot = tail & (PPU_TENSOR_DESCRIPTOR_SLOTS - 1);
        // SAFETY: `tail != head` was checked with `head` loaded as Acquire, so this slot was fully
        // written and published by the producer, which does not write it again until `tail`
        // moves past it.
        let desc = unsafe { &*self.descriptors[slot].get() };

        // Emulate completion when descriptor invalidation is processed
        if desc.valid_flag {
            self.job_tail.store(tail.wrapping_add(1), Ordering::Release);
            true
        } else {
            false
        }
    }
}

// =================================================================================================
// 6. 4D EXTENDED KALMAN FILTER (EKF) TRACKING PIPELINE
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrackStatus {
    Free = 0,
    Tentative = 1,
    Confirmed = 2,
    Coated = 3,
    Lost = 4,
}

#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct TrackedObstacle {
    pub track_id: u32,
    pub status: TrackStatus,
    pub class: PerceptionClassification,
    pub state: Vector4,        // [px, py, vx, vy]
    pub covariance: Matrix4x4, // P
    pub hits: u32,
    pub misses: u32,
    pub last_updated_micros: u64,
}

impl TrackedObstacle {
    pub const fn empty() -> Self {
        Self {
            track_id: 0,
            status: TrackStatus::Free,
            class: PerceptionClassification::Unknown,
            state: Vector4::zero(),
            covariance: Matrix4x4::identity(),
            hits: 0,
            misses: 0,
            last_updated_micros: 0,
        }
    }

    /// Predicts target state forward by dt seconds (Constant Velocity Kinematic Model)
    pub fn predict(&mut self, dt_seconds: f32) {
        // State transition matrix F
        let mut f = Matrix4x4::identity();
        f.data[0][2] = dt_seconds;
        f.data[1][3] = dt_seconds;

        // x' = F * x
        self.state = f.multiply_vector(&self.state);

        // Process noise covariance Q (discrete continuous-time white noise model)
        const SIGMA_A: f32 = 2.5; // Acceleration noise spectral density (m/s^2)
        let q_val = SIGMA_A * SIGMA_A;
        let dt2 = dt_seconds * dt_seconds;
        let dt3 = dt2 * dt_seconds / 2.0;
        let dt4 = dt2 * dt2 / 4.0;

        let mut q = Matrix4x4::zero();
        q.data[0][0] = dt4 * q_val;
        q.data[0][2] = dt3 * q_val;
        q.data[2][0] = dt3 * q_val;
        q.data[2][2] = dt2 * q_val;

        q.data[1][1] = dt4 * q_val;
        q.data[1][3] = dt3 * q_val;
        q.data[3][1] = dt3 * q_val;
        q.data[3][3] = dt2 * q_val;

        // P' = F * P * F^T + Q
        let fp = f.multiply_matrix(&self.covariance);
        let ft = f.transpose();
        self.covariance = fp.multiply_matrix(&ft).add(&q);
    }

    /// Update step using Camera Cartesian Measurement [meas_x, meas_y]
    pub fn update_camera(&mut self, meas_x: f32, meas_y: f32, r_variance: f32) {
        // H = [[1, 0, 0, 0], [0, 1, 0, 0]]
        let px = self.state.data[0];
        let py = self.state.data[1];

        let y_res_x = meas_x - px;
        let y_res_y = meas_y - py;

        // Innovation covariance S = H * P * H^T + R
        let s00 = self.covariance.data[0][0] + r_variance;
        let s01 = self.covariance.data[0][1];
        let s10 = self.covariance.data[1][0];
        let s11 = self.covariance.data[1][1] + r_variance;

        // Invert 2x2 matrix S
        let det = s00 * s11 - s01 * s10;
        if det.abs() < 1e-6 {
            return;
        }
        let inv_det = 1.0 / det;
        let inv_s00 = s11 * inv_det;
        let inv_s01 = -s01 * inv_det;
        let inv_s10 = -s10 * inv_det;
        let inv_s11 = s00 * inv_det;

        // Kalman Gain K = P * H^T * S^-1 (4x2 Matrix)
        let mut k = [[0.0f32; 2]; 4];
        let mut i = 0;
        while i < 4 {
            let p_h_t_0 = self.covariance.data[i][0];
            let p_h_t_1 = self.covariance.data[i][1];
            k[i][0] = p_h_t_0 * inv_s00 + p_h_t_1 * inv_s10;
            k[i][1] = p_h_t_0 * inv_s01 + p_h_t_1 * inv_s11;
            i += 1;
        }

        // State update x = x + K * y_res
        self.state.data[0] += k[0][0] * y_res_x + k[0][1] * y_res_y;
        self.state.data[1] += k[1][0] * y_res_x + k[1][1] * y_res_y;
        self.state.data[2] += k[2][0] * y_res_x + k[2][1] * y_res_y;
        self.state.data[3] += k[3][0] * y_res_x + k[3][1] * y_res_y;

        // Covariance update P = (I - K * H) * P
        let mut kh = Matrix4x4::zero();
        i = 0;
        while i < 4 {
            kh.data[i][0] = k[i][0];
            kh.data[i][1] = k[i][1];
            i += 1;
        }
        let i_kh = Matrix4x4::identity().sub(&kh);
        self.covariance = i_kh.multiply_matrix(&self.covariance);

        self.hits += 1;
        if self.hits > 3 && self.status == TrackStatus::Tentative {
            self.status = TrackStatus::Confirmed;
        }
    }

    /// Update step using Radar Polar Measurement [range, azimuth, radial_velocity]
    pub fn update_radar(&mut self, range: f32, azimuth: f32, radial_vel: f32) {
        let px = self.state.data[0];
        let py = self.state.data[1];
        let vx = self.state.data[2];
        let vy = self.state.data[3];

        let rho = EmbeddedMath::sqrt(px * px + py * py);
        if rho < 0.1 {
            return;
        }

        let phi = EmbeddedMath::atan2(py, px);
        let rho_dot = (px * vx + py * vy) / rho;

        let y_res_0 = range - rho;
        let y_res_1 = crate::fmath::wrap_pi(azimuth - phi);
        let y_res_2 = radial_vel - rho_dot;

        // Normalize azimuth error to [-PI, PI]

        // Radar measurement noise variances
        const R_RHO: f32 = 0.25;
        const R_PHI: f32 = 0.015;
        const R_RHODOT: f32 = 0.40;

        // Simplified Sequential Scalar EKF Update for Radar (Avoiding 3x3 Inverse in Embedded Runtime)
        let s0 = self.covariance.data[0][0] + R_RHO;
        if s0 > 1e-4 {
            let k0 = self.covariance.data[0][0] / s0;
            self.state.data[0] += k0 * y_res_0;
            self.covariance.data[0][0] *= 1.0 - k0;
        }

        let s1 = self.covariance.data[1][1] + R_PHI * rho * rho;
        if s1 > 1e-4 {
            let k1 = self.covariance.data[1][1] / s1;
            self.state.data[1] += k1 * (y_res_1 * rho);
            self.covariance.data[1][1] *= 1.0 - k1;
        }

        let s2 = self.covariance.data[2][2] + R_RHODOT;
        if s2 > 1e-4 {
            let k2 = self.covariance.data[2][2] / s2;
            self.state.data[2] += k2 * y_res_2;
            self.covariance.data[2][2] *= 1.0 - k2;
        }

        self.hits += 1;
        if self.hits > 3 && self.status == TrackStatus::Tentative {
            self.status = TrackStatus::Confirmed;
        }
    }
}

// =================================================================================================
// 7. MULTI-SENSOR DATA ASSOCIATION & TRACK MANAGEMENT POOL
// =================================================================================================

pub struct SensorFusionEngine {
    track_pool: [UnsafeCell<TrackedObstacle>; MAX_PERCEPTION_TARGETS],
    track_count: AtomicU32,
    next_track_id: AtomicU32,
    engine_lock: AtomicBool,
}

// SAFETY: the interior-mutable `track_pool` is only written between `acquire_lock` and
// `release_lock` of `engine_lock` (in `update`), which makes writers mutually exclusive. Readers
// that take no lock (`evaluate_aeb_intervention`) are only sound if they do not run at the same
// time as `update`, as in a single control loop; nothing in the types enforces that.
unsafe impl Sync for SensorFusionEngine {}

impl SensorFusionEngine {
    pub const fn new() -> Self {
        const INIT_OBSTACLE: UnsafeCell<TrackedObstacle> =
            UnsafeCell::new(TrackedObstacle::empty());

        Self {
            track_pool: [INIT_OBSTACLE; MAX_PERCEPTION_TARGETS],
            track_count: AtomicU32::new(0),
            next_track_id: AtomicU32::new(1),
            engine_lock: AtomicBool::new(false),
        }
    }

    #[inline(always)]
    fn acquire_lock(&self) {
        while self
            .engine_lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            spin_loop();
        }
    }

    #[inline(always)]
    fn release_lock(&self) {
        self.engine_lock.store(false, Ordering::Release);
    }

    /// Primary sensor ingestion pipeline cycle
    pub fn process_fusion_cycle(
        &self,
        dt_seconds: f32,
        cameras: &[CameraObjectDetection],
        radars: &[RadarTargetDetection],
        timestamp_micros: u64,
    ) {
        self.acquire_lock();

        // 1. Predict all active tracks
        for slot in self.track_pool.iter() {
            // SAFETY: `engine_lock` is held (taken at the top of this function), and this is the
            // only reference to this slot: the reference ends each iteration.
            let track = unsafe { &mut *slot.get() };
            if track.status != TrackStatus::Free {
                track.predict(dt_seconds);
                track.last_updated_micros = timestamp_micros;
            }
        }

        // 2. Associate Camera detections (Nearest Neighbor on Euclidean Spatial Gating)
        for cam in cameras {
            let mut best_idx = None;
            let mut min_distance_sq = 16.0f32; // 4.0m gating threshold

            for (idx, slot) in self.track_pool.iter().enumerate() {
                // SAFETY: `engine_lock` is held and no `&mut` to this slot is alive here.
                let track = unsafe { &*slot.get() };
                if track.status != TrackStatus::Free {
                    let dx = track.state.data[0] - cam.longitudinal_m;
                    let dy = track.state.data[1] - cam.lateral_m;
                    let dist_sq = dx * dx + dy * dy;
                    if dist_sq < min_distance_sq {
                        min_distance_sq = dist_sq;
                        best_idx = Some(idx);
                    }
                }
            }

            if let Some(idx) = best_idx {
                // SAFETY: `engine_lock` is held; the shared references from the search loop above
                // ended with it, so this is the only reference to slot `idx`.
                let track = unsafe { &mut *self.track_pool[idx].get() };
                track.update_camera(cam.longitudinal_m, cam.lateral_m, 0.35);
                track.class = cam.class;
            } else {
                // Initialize new track in first free slot
                self.allocate_new_track(
                    cam.longitudinal_m,
                    cam.lateral_m,
                    0.0,
                    0.0,
                    cam.class,
                    timestamp_micros,
                );
            }
        }

        // 3. Associate Radar detections
        for rad in radars {
            let cart_x = rad.radial_distance_m * EmbeddedMath::cos(rad.azimuth_rad);
            let cart_y = rad.radial_distance_m * EmbeddedMath::sin(rad.azimuth_rad);

            let mut best_idx = None;
            let mut min_dist_sq = 25.0f32; // 5.0m gating threshold

            for (idx, slot) in self.track_pool.iter().enumerate() {
                // SAFETY: `engine_lock` is held and no `&mut` to this slot is alive here.
                let track = unsafe { &*slot.get() };
                if track.status != TrackStatus::Free {
                    let dx = track.state.data[0] - cart_x;
                    let dy = track.state.data[1] - cart_y;
                    let dist_sq = dx * dx + dy * dy;
                    if dist_sq < min_dist_sq {
                        min_dist_sq = dist_sq;
                        best_idx = Some(idx);
                    }
                }
            }

            if let Some(idx) = best_idx {
                // SAFETY: `engine_lock` is held; the search loop's shared references ended, so
                // this is the only reference to slot `idx`.
                let track = unsafe { &mut *self.track_pool[idx].get() };
                track.update_radar(
                    rad.radial_distance_m,
                    rad.azimuth_rad,
                    rad.radial_velocity_mps,
                );
            }
        }

        // 4. Track pruning & garbage collection for missed cycles
        for slot in self.track_pool.iter() {
            // SAFETY: `engine_lock` is held and the reference ends each iteration.
            let track = unsafe { &mut *slot.get() };
            if track.status != TrackStatus::Free
                && timestamp_micros.saturating_sub(track.last_updated_micros) > 500_000
            {
                // 500ms timeout
                track.status = TrackStatus::Free;
                self.track_count.fetch_sub(1, Ordering::Relaxed);
            }
        }

        self.release_lock();
    }

    fn allocate_new_track(
        &self,
        px: f32,
        py: f32,
        vx: f32,
        vy: f32,
        class: PerceptionClassification,
        ts: u64,
    ) {
        for slot in self.track_pool.iter() {
            // SAFETY: this private function is called from `update`, which holds `engine_lock` and
            // has no reference into the pool alive at the call, and from a single-threaded test.
            // The reference ends at `break` or the end of the iteration.
            let track = unsafe { &mut *slot.get() };
            if track.status == TrackStatus::Free {
                let new_id = self.next_track_id.fetch_add(1, Ordering::Relaxed);
                track.track_id = new_id;
                track.status = TrackStatus::Tentative;
                track.class = class;
                track.state = Vector4::new(px, py, vx, vy);
                track.covariance = Matrix4x4::identity();
                track.hits = 1;
                track.misses = 0;
                track.last_updated_micros = ts;
                self.track_count.fetch_add(1, Ordering::Relaxed);
                break;
            }
        }
    }
}

impl Default for SensorFusionEngine {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 8. AUTONOMOUS EMERGENCY BRAKING (AEB) & FORWARD COLLISION WARNING (FCW)
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AebActionLevel {
    None = 0,
    VisualAudioWarning = 1,
    HapticBrakeJerk = 2,
    PartialBraking50Pct = 3,
    FullEmergencyStop100Pct = 4,
}

pub struct CollisionAvoidanceArbitrator;

impl CollisionAvoidanceArbitrator {
    /// Evaluates current active tracks against ego driving corridor and computes intervention level
    pub fn evaluate_aeb_intervention(
        fusion: &SensorFusionEngine,
        ego_speed_mps: f32,
        steering_angle_rad: f32,
    ) -> (AebActionLevel, f32) {
        let mut highest_threat = AebActionLevel::None;
        let mut min_calculated_ttc = 99.0f32;

        for slot in fusion.track_pool.iter() {
            // SAFETY: this takes no lock, so it is only sound if `update` is not running at the
            // same time (the single control loop of the design); see the `Sync` impl.
            let track = unsafe { &*slot.get() };
            if track.status != TrackStatus::Confirmed {
                continue;
            }

            let px = track.state.data[0];
            let py = track.state.data[1];
            let vx_rel = track.state.data[2]; // Relative velocity (+ = opening, - = closing)

            // Only evaluate forward-facing targets
            if px <= 0.5 || px > 120.0 {
                continue;
            }

            // Path Prediction & Lateral Corridor Gating
            // Compute expected lateral path offset based on curvature: y_path = py - 0.5 * curvature * x^2
            let curvature = steering_angle_rad / 2.80; // 2.80m wheelbase estimate
            let predicted_lateral_error = py - 0.5 * curvature * px * px;

            if predicted_lateral_error.abs() <= SAFETY_CORRIDOR_LATERAL_HALF_WIDTH_METERS {
                // Object resides directly in ego vehicle's travel lane
                let closing_velocity = -vx_rel; // Positive closing speed

                if closing_velocity > 0.5 {
                    let ttc = px / (ego_speed_mps + closing_velocity);
                    if ttc < min_calculated_ttc {
                        min_calculated_ttc = ttc;
                    }

                    if ttc <= AEB_TTC_CRITICAL_THRESHOLD_SECONDS {
                        highest_threat = AebActionLevel::FullEmergencyStop100Pct;
                        break;
                    } else if ttc <= (AEB_TTC_CRITICAL_THRESHOLD_SECONDS * 1.5) {
                        if (highest_threat as u8) < (AebActionLevel::PartialBraking50Pct as u8) {
                            highest_threat = AebActionLevel::PartialBraking50Pct;
                        }
                    } else if ttc <= AEB_TTC_WARNING_THRESHOLD_SECONDS
                        && (highest_threat as u8) < (AebActionLevel::VisualAudioWarning as u8)
                    {
                        highest_threat = AebActionLevel::VisualAudioWarning;
                    }
                }
            }
        }

        (highest_threat, min_calculated_ttc)
    }
}

// =================================================================================================
// 9. PART 3 INTEGRATION TEST HARNESS & MATH VERIFICATION
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_embedded_math_trig_and_sqrt() {
        let root = EmbeddedMath::sqrt(16.0);
        assert!((root - 4.0).abs() < 0.05);

        let s = EmbeddedMath::sin(0.0);
        assert!(s.abs() < 1e-4);

        let c = EmbeddedMath::cos(0.0);
        assert!((c - 1.0).abs() < 0.05);

        let angle = EmbeddedMath::atan2(1.0, 1.0);
        assert!((angle - core::f32::consts::FRAC_PI_4).abs() < 1e-6);
    }

    #[test]
    fn test_matrix_vector_multiplication() {
        let m = Matrix4x4::identity();
        let v = Vector4::new(1.0, 2.0, 3.0, 4.0);
        let res = m.multiply_vector(&v);
        assert_eq!(res, v);

        let mut m_scale = Matrix4x4::zero();
        m_scale.data[0][0] = 2.0;
        m_scale.data[1][1] = 2.0;
        m_scale.data[2][2] = 2.0;
        m_scale.data[3][3] = 2.0;
        let scaled_v = m_scale.multiply_vector(&v);
        assert_eq!(scaled_v.data, [2.0, 4.0, 6.0, 8.0]);
    }

    #[test]
    fn test_ekf_constant_velocity_prediction() {
        let mut track = TrackedObstacle::empty();
        track.status = TrackStatus::Confirmed;
        track.state = Vector4::new(10.0, 0.0, 5.0, 0.0); // 10m ahead moving at 5m/s

        track.predict(0.1); // 100ms dt
        assert!((track.state.data[0] - 10.5).abs() < 1e-4);
        assert!((track.state.data[1] - 0.0).abs() < 1e-4);
    }

    #[test]
    fn test_camera_measurement_update() {
        let mut track = TrackedObstacle::empty();
        track.status = TrackStatus::Tentative;
        track.state = Vector4::new(20.0, 0.0, 0.0, 0.0);

        // Measured slightly to the left at Y = 1.0
        track.update_camera(20.2, 1.0, 0.2);
        assert!(track.state.data[1] > 0.0); // State pulled toward observation
        assert_eq!(track.hits, 1);
    }

    #[test]
    fn test_aeb_critical_threat_trigger() {
        let fusion = SensorFusionEngine::new();
        // Allocate confirmed track 8m ahead closing fast
        fusion.allocate_new_track(
            8.0,
            0.0,
            -15.0,
            0.0,
            PerceptionClassification::PassengerCar,
            100_000,
        );

        // Force confirmation
        // SAFETY: single-threaded test; no other reference to the pool exists.
        unsafe {
            let t = &mut *fusion.track_pool[0].get();
            t.status = TrackStatus::Confirmed;
        }

        let (action, ttc) =
            CollisionAvoidanceArbitrator::evaluate_aeb_intervention(&fusion, 0.0, 0.0);
        assert_eq!(action, AebActionLevel::FullEmergencyStop100Pct);
        assert!(ttc < 1.0);
    }

    #[test]
    fn embedded_trig_matches_the_standard_library() {
        let mut x = -20.0f32;
        while x <= 20.0 {
            assert!((EmbeddedMath::sin(x) - x.sin()).abs() < 1e-6, "sin({x})");
            assert!((EmbeddedMath::cos(x) - x.cos()).abs() < 1e-6, "cos({x})");
            x += 0.013;
        }
    }

    #[test]
    fn embedded_atan2_is_accurate_in_every_quadrant() {
        let mut worst = 0.0f32;
        for yi in -20..=20 {
            for xi in -20..=20 {
                let (y, x) = (yi as f32 * 0.7, xi as f32 * 0.7);
                if y == 0.0 && x == 0.0 {
                    continue;
                }
                worst = worst.max((EmbeddedMath::atan2(y, x) - y.atan2(x)).abs());
            }
        }
        assert!(worst < 1e-6, "worst error {worst} rad");
    }

    #[test]
    fn embedded_math_never_hangs_or_panics_on_bad_input() {
        for bad in [
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NAN,
            1e30,
            -1e30,
            f32::MAX,
            f32::MIN,
        ] {
            assert!(!EmbeddedMath::sin(bad).is_infinite());
            let _ = EmbeddedMath::cos(bad);
            let _ = EmbeddedMath::atan2(bad, 1.0);
            let _ = EmbeddedMath::atan2(1.0, bad);
            let _ = EmbeddedMath::inv_sqrt(bad);
            let _ = EmbeddedMath::sqrt(bad);
        }
        assert_eq!(EmbeddedMath::inv_sqrt(-4.0), 0.0);
        assert_eq!(EmbeddedMath::sqrt(-4.0), 0.0);
        assert_eq!(EmbeddedMath::sqrt(0.0), 0.0);
        assert!((EmbeddedMath::sqrt(16.0) - 4.0).abs() < 1e-6);
    }
}
