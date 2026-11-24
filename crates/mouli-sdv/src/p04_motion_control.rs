//! Part 4: MOTION CONTROL, ISO 14229 UDS SERVER, UPTANE OTA & MASTER ORCHESTRATOR

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 4 - MOTION CONTROL, ISO 14229 UDS SERVER, UPTANE OTA & MASTER ORCHESTRATOR
// CRITICALITY: ISO 26262 ASIL-D / ISO 14229-1 UDS / IEEE Uptane Automotive Security Aligned
// TARGET ARCH: aarch64-unknown-none (Cortex-A78AE / Cortex-R82) & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Heap Allocations, Deterministic Bounded Execution
// =================================================================================================

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering};

// =================================================================================================
// 1. VEHICLE KINEMATICS, DIAGNOSTICS & CRYPTOGRAPHIC CONSTANTS
// =================================================================================================

/// Vehicle Physical Kinematic Configuration
pub const VEHICLE_WHEELBASE_METERS: f32 = 2.85; // Distance between front and rear axles
pub const VEHICLE_TRACK_WIDTH_METERS: f32 = 1.62;
pub const MAX_STEER_ANGLE_RAD: f32 = 0.6108; // ~35.0 degrees in radians
pub const MAX_STEER_RATE_RAD_PER_SEC: f32 = 1.047; // ~60.0 degrees/sec dynamic limit
pub const MAX_ACCELERATION_MPS2: f32 = 4.5;
pub const MAX_DECELERATION_MPS2: f32 = 9.8; // Maximum emergency friction braking limit

/// ISO 14229-1 UDS Diagnostic Protocol Constants
pub const UDS_MAX_PAYLOAD_SIZE: usize = 256;
pub const UDS_SESSION_TIMEOUT_MICROS: u64 = 5_000_000; // 5 seconds P2/P2* timeout
pub const UDS_SECURITY_SEED_LEN: usize = 16;
pub const UDS_SECURITY_KEY_LEN: usize = 16;

/// Uptane Secure Firmware Storage Dimensions
pub const FLASH_SECTOR_SIZE_BYTES: usize = 4096;
pub const FLASH_MAX_IMAGE_SECTORS: usize = 1024; // 4 MiB Bank Size
pub const UPTANE_DIGEST_LEN: usize = 32; // SHA-256 Digest length
pub const UPTANE_SIGNATURE_LEN: usize = 64; // Ed25519 / ECDSA P-256 signature length

// =================================================================================================
// 2. EMBEDDED KINEMATIC MATH & PID CONTROL PRIMITIVES
// =================================================================================================

/// Zero-allocation PID Controller with Anti-Windup Clamping
#[derive(Debug, Clone, Copy)]
pub struct PidController {
    pub kp: f32,
    pub ki: f32,
    pub kd: f32,
    pub integral: f32,
    pub prev_error: f32,
    pub out_min: f32,
    pub out_max: f32,
}

impl PidController {
    pub const fn new(kp: f32, ki: f32, kd: f32, out_min: f32, out_max: f32) -> Self {
        Self {
            kp,
            ki,
            kd,
            integral: 0.0,
            prev_error: 0.0,
            out_min,
            out_max,
        }
    }

    pub fn compute(&mut self, setpoint: f32, measurement: f32, dt_secs: f32) -> f32 {
        if dt_secs <= 0.0001 {
            return 0.0;
        }

        let error = setpoint - measurement;
        self.integral += error * dt_secs;

        // Anti-windup clamping on integral component
        if self.integral > self.out_max {
            self.integral = self.out_max;
        } else if self.integral < self.out_min {
            self.integral = self.out_min;
        }

        let derivative = (error - self.prev_error) / dt_secs;
        self.prev_error = error;

        let output = (self.kp * error) + (self.ki * self.integral) + (self.kd * derivative);

        if output > self.out_max {
            self.out_max
        } else if output < self.out_min {
            self.out_min
        } else {
            output
        }
    }

    pub fn reset(&mut self) {
        self.integral = 0.0;
        self.prev_error = 0.0;
    }
}

// Fast trigonometric helpers for bare-metal execution
#[inline(always)]
fn fast_sin(x: f32) -> f32 {
    let x = crate::fmath::wrap_pi(x);
    let x2 = x * x;
    let x3 = x * x2;
    let x5 = x3 * x2;
    x - (x3 * 0.16666667) + (x5 * 0.00833333)
}

#[inline(always)]
fn fast_cos(x: f32) -> f32 {
    fast_sin(x + 1.570_796_4)
}

#[inline(always)]
fn fast_atan2(y: f32, x: f32) -> f32 {
    if x == 0.0 {
        return if y > 0.0 {
            1.570_796_4
        } else if y < 0.0 {
            -1.570_796_4
        } else {
            0.0
        };
    }
    let r = y / x;
    let r2 = r * r;
    let mut angle = r / (1.0 + 0.28 * r2);
    if x < 0.0 {
        if y >= 0.0 {
            angle += core::f32::consts::PI;
        } else {
            angle -= core::f32::consts::PI;
        }
    }
    angle
}

// =================================================================================================
// 3. MOTION CONTROL: STANLEY LATERAL STEERING & LONGITUDINAL ARBITRATION
// =================================================================================================

#[derive(Debug, Clone, Copy)]
pub struct TrajectoryWaypoint {
    pub x: f32,
    pub y: f32,
    pub target_heading_rad: f32,
    pub target_velocity_mps: f32,
    pub curvature: f32,
}

pub struct StanleyMotionController {
    cross_track_gain_k: f32,
    softening_gain_k_soft: f32,
    last_commanded_steer_rad: f32,
    longitudinal_pid: PidController,
}

impl StanleyMotionController {
    pub const fn new(k: f32, k_soft: f32) -> Self {
        Self {
            cross_track_gain_k: k,
            softening_gain_k_soft: k_soft,
            last_commanded_steer_rad: 0.0,
            longitudinal_pid: PidController::new(
                150.0,
                15.0,
                5.0,
                -MAX_DECELERATION_MPS2,
                MAX_ACCELERATION_MPS2,
            ),
        }
    }

    /// Computes front wheel steering angle based on the Stanley geometric tracking algorithm
    pub fn compute_lateral_steering(
        &mut self,
        current_x: f32,
        current_y: f32,
        current_heading_rad: f32,
        current_velocity_mps: f32,
        waypoint: &TrajectoryWaypoint,
        dt_secs: f32,
    ) -> Result<f32, u16> {
        // 1. Calculate front-axle position from center reference
        let front_axle_x =
            current_x + (VEHICLE_WHEELBASE_METERS * 0.5 * fast_cos(current_heading_rad));
        let front_axle_y =
            current_y + (VEHICLE_WHEELBASE_METERS * 0.5 * fast_sin(current_heading_rad));

        // 2. Vector from target waypoint to front axle
        let dx = front_axle_x - waypoint.x;
        let dy = front_axle_y - waypoint.y;

        // Heading unit vector normal
        let nx = -fast_sin(waypoint.target_heading_rad);
        let ny = fast_cos(waypoint.target_heading_rad);

        // Cross-track error (signed distance to path)
        let cross_track_error = (dx * nx) + (dy * ny);

        // 3. Heading alignment error
        let heading_error =
            crate::fmath::wrap_pi(waypoint.target_heading_rad - current_heading_rad);

        // 4. Stanley control law: delta = heading_error + atan(k * e / (v + k_soft))
        let denom = current_velocity_mps.abs() + self.softening_gain_k_soft;
        let cross_track_correction =
            fast_atan2(self.cross_track_gain_k * -cross_track_error, denom);

        let mut raw_steer_target = heading_error + cross_track_correction;

        // Hard steering angle clamp
        if raw_steer_target > MAX_STEER_ANGLE_RAD {
            raw_steer_target = MAX_STEER_ANGLE_RAD;
        } else if raw_steer_target < -MAX_STEER_ANGLE_RAD {
            raw_steer_target = -MAX_STEER_ANGLE_RAD;
        }

        // Slew rate limiting to prevent actuator snap (ISO 26262 ASIL-D guardrail)
        let max_delta = MAX_STEER_RATE_RAD_PER_SEC * dt_secs;
        let rate_limited_steer = if (raw_steer_target - self.last_commanded_steer_rad) > max_delta {
            self.last_commanded_steer_rad + max_delta
        } else if (raw_steer_target - self.last_commanded_steer_rad) < -max_delta {
            self.last_commanded_steer_rad - max_delta
        } else {
            raw_steer_target
        };

        self.last_commanded_steer_rad = rate_limited_steer;
        Ok(rate_limited_steer)
    }

    /// Computes longitudinal acceleration or braking command
    pub fn compute_longitudinal_effort(
        &mut self,
        target_speed_mps: f32,
        current_speed_mps: f32,
        dt_secs: f32,
    ) -> f32 {
        self.longitudinal_pid
            .compute(target_speed_mps, current_speed_mps, dt_secs)
    }
}

// =================================================================================================
// 4. ISO 14229-1 UNIFIED DIAGNOSTIC SERVICES (UDS) PROTOCOL STACK
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdsSessionType {
    Default = 0x01,
    Programming = 0x02,
    ExtendedDiagnostic = 0x03,
    SafetySystemDiagnostic = 0x04,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UdsNegativeResponseCode {
    GeneralReject = 0x10,
    ServiceNotSupported = 0x11,
    SubFunctionNotSupported = 0x12,
    IncorrectMessageLengthOrInvalidFormat = 0x13,
    ResponseTooLong = 0x14,
    BusyRepeatRequest = 0x21,
    ConditionsNotCorrect = 0x22,
    RequestSequenceError = 0x24,
    RequestOutOfRange = 0x31,
    SecurityAccessDenied = 0x33,
    InvalidKey = 0x35,
    ExceededNumberOfAttempts = 0x36,
    RequiredTimeDelayNotExpired = 0x37,
    UploadDownloadNotAccepted = 0x70,
    GeneralProgrammingFailure = 0x72,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UdsRequest {
    pub service_id: u8,
    pub sub_function: u8,
    pub data_len: usize,
    pub data: [u8; UDS_MAX_PAYLOAD_SIZE],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UdsResponse {
    pub service_id: u8,
    pub is_negative_response: bool,
    pub nrc: UdsNegativeResponseCode,
    pub data_len: usize,
    pub data: [u8; UDS_MAX_PAYLOAD_SIZE],
}

impl UdsResponse {
    pub const fn positive(service_id: u8) -> Self {
        Self {
            service_id: service_id | 0x40,
            is_negative_response: false,
            nrc: UdsNegativeResponseCode::GeneralReject,
            data_len: 0,
            data: [0u8; UDS_MAX_PAYLOAD_SIZE],
        }
    }

    pub const fn negative(service_id: u8, nrc: UdsNegativeResponseCode) -> Self {
        Self {
            service_id: 0x7F,
            is_negative_response: true,
            nrc,
            data_len: 2,
            data: [
                service_id, nrc as u8, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
                0, 0, 0, 0, 0, 0, 0, 0, 0, 0,
            ],
        }
    }
}

pub struct UdsDiagnosticServer {
    current_session: AtomicU8,
    security_unlocked: AtomicBool,
    last_activity_micros: AtomicU64,
    active_seed: [AtomicU8; UDS_SECURITY_SEED_LEN],
    failed_attempts: AtomicU8,
}

impl UdsDiagnosticServer {
    pub const fn new() -> Self {
        Self {
            current_session: AtomicU8::new(UdsSessionType::Default as u8),
            security_unlocked: AtomicBool::new(false),
            last_activity_micros: AtomicU64::new(0),
            active_seed: [const { AtomicU8::new(0) }; UDS_SECURITY_SEED_LEN],
            failed_attempts: AtomicU8::new(0),
        }
    }

    pub fn process_uds_pdu(&self, req: &UdsRequest, current_time_micros: u64) -> UdsResponse {
        self.last_activity_micros
            .store(current_time_micros, Ordering::Relaxed);

        match req.service_id {
            // 0x10: DiagnosticSessionControl
            0x10 => self.handle_session_control(req),

            // 0x27: SecurityAccess
            0x27 => self.handle_security_access(req, current_time_micros),

            // 0x22: ReadDataByIdentifier
            0x22 => self.handle_read_did(req),

            // 0x3E: TesterPresent
            0x3E => {
                let mut resp = UdsResponse::positive(0x3E);
                resp.data[0] = req.sub_function & 0x7F; // ZeroSubFunction echo
                resp.data_len = 1;
                resp
            }

            // 0x11: ECUReset
            0x11 => {
                let mut resp = UdsResponse::positive(0x11);
                resp.data[0] = req.sub_function; // Echo reset type (e.g. 0x01 HardReset)
                resp.data_len = 1;
                resp
            }

            _ => {
                UdsResponse::negative(req.service_id, UdsNegativeResponseCode::ServiceNotSupported)
            }
        }
    }

    fn handle_session_control(&self, req: &UdsRequest) -> UdsResponse {
        let target_session = req.sub_function & 0x7F;
        match target_session {
            0x01 => self
                .current_session
                .store(UdsSessionType::Default as u8, Ordering::SeqCst),
            0x02 => self
                .current_session
                .store(UdsSessionType::Programming as u8, Ordering::SeqCst),
            0x03 => self
                .current_session
                .store(UdsSessionType::ExtendedDiagnostic as u8, Ordering::SeqCst),
            0x04 => self.current_session.store(
                UdsSessionType::SafetySystemDiagnostic as u8,
                Ordering::SeqCst,
            ),
            _ => {
                return UdsResponse::negative(
                    0x10,
                    UdsNegativeResponseCode::SubFunctionNotSupported,
                )
            }
        }

        let mut resp = UdsResponse::positive(0x10);
        resp.data[0] = target_session;
        // P2 Server Max: 50ms (0x0032), P2* Server Max: 5000ms (0x01F4)
        resp.data[1] = 0x00;
        resp.data[2] = 0x32;
        resp.data[3] = 0x01;
        resp.data[4] = 0xF4;
        resp.data_len = 5;
        resp
    }

    fn handle_security_access(&self, req: &UdsRequest, current_time: u64) -> UdsResponse {
        let sub = req.sub_function & 0x7F;

        if sub == 0x01 {
            // Request Seed
            if self.failed_attempts.load(Ordering::Relaxed) >= 3 {
                return UdsResponse::negative(
                    0x27,
                    UdsNegativeResponseCode::RequiredTimeDelayNotExpired,
                );
            }

            let mut resp = UdsResponse::positive(0x27);
            resp.data[0] = 0x01; // Echo sub-function

            // Generate deterministic pseudorandom seed based on cycle counters
            let mut seed_acc = current_time;
            for i in 0..UDS_SECURITY_SEED_LEN {
                seed_acc = seed_acc
                    .wrapping_mul(6364136223846793005)
                    .wrapping_add(1442695040888963407);
                let byte_val = ((seed_acc >> 24) & 0xFF) as u8;
                self.active_seed[i].store(byte_val, Ordering::Relaxed);
                resp.data[1 + i] = byte_val;
            }
            resp.data_len = 1 + UDS_SECURITY_SEED_LEN;
            resp
        } else if sub == 0x02 {
            // Send Key verification
            if req.data_len < UDS_SECURITY_KEY_LEN {
                return UdsResponse::negative(
                    0x27,
                    UdsNegativeResponseCode::IncorrectMessageLengthOrInvalidFormat,
                );
            }

            // Simple automotive XOR-mask rolling cipher for key verification
            let mut key_valid = true;
            for i in 0..UDS_SECURITY_KEY_LEN {
                let seed_byte = self.active_seed[i].load(Ordering::Relaxed);
                let expected_key = seed_byte ^ 0xA5;
                if req.data[i] != expected_key {
                    key_valid = false;
                    break;
                }
            }

            if key_valid {
                self.security_unlocked.store(true, Ordering::SeqCst);
                self.failed_attempts.store(0, Ordering::Relaxed);
                let mut resp = UdsResponse::positive(0x27);
                resp.data[0] = 0x02;
                resp.data_len = 1;
                resp
            } else {
                self.failed_attempts.fetch_add(1, Ordering::Relaxed);
                UdsResponse::negative(0x27, UdsNegativeResponseCode::InvalidKey)
            }
        } else {
            UdsResponse::negative(0x27, UdsNegativeResponseCode::SubFunctionNotSupported)
        }
    }

    fn handle_read_did(&self, req: &UdsRequest) -> UdsResponse {
        if req.data_len < 2 {
            return UdsResponse::negative(
                0x22,
                UdsNegativeResponseCode::IncorrectMessageLengthOrInvalidFormat,
            );
        }

        let did = ((req.data[0] as u16) << 8) | (req.data[1] as u16);
        let mut resp = UdsResponse::positive(0x22);
        resp.data[0] = req.data[0];
        resp.data[1] = req.data[1];

        match did {
            // 0xF190: Vehicle Identification Number (VIN)
            0xF190 => {
                let vin = b"WAUZZZmouliOS00124";
                resp.data[2..2 + vin.len()].copy_from_slice(vin);
                resp.data_len = 2 + vin.len();
                resp
            }
            // 0xF189: Active Firmware Version
            0xF189 => {
                let version = b"mouli-OS-v4.2.0-ASIL-D";
                resp.data[2..2 + version.len()].copy_from_slice(version);
                resp.data_len = 2 + version.len();
                resp
            }
            _ => UdsResponse::negative(0x22, UdsNegativeResponseCode::RequestOutOfRange),
        }
    }

    pub fn is_security_authorized(&self) -> bool {
        self.security_unlocked.load(Ordering::Acquire)
    }
}

impl Default for UdsDiagnosticServer {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 5. UPTANE-INSPIRED DUAL-BANK A/B SECURE FIRMWARE FLASHER & STORAGE CONTROLLER
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PartitionBank {
    BankA = 0,
    BankB = 1,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OtaLifecycleStatus {
    Idle = 0,
    VerifyingMetadata = 1,
    ErasingInactiveBank = 2,
    WritingSectors = 3,
    IntegrityVerified = 4,
    MarkedForReboot = 5,
    QuarantineRollback = 6,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct UptaneTargetMetadata {
    pub target_version: u32,
    pub image_size_bytes: usize,
    pub sha256_digest: [u8; UPTANE_DIGEST_LEN],
    pub director_signature: [u8; UPTANE_SIGNATURE_LEN],
    pub image_root_signature: [u8; UPTANE_SIGNATURE_LEN],
}

/// Domain separator mixed into everything the OTA roles sign, so a signature made for another
/// purpose can never be replayed as an update signature.
const UPTANE_DOMAIN: &[u8] = b"MOULI-SDV-UPTANE-1\0";
/// Size of the signed message: domain, version (4), image size (8), SHA-256 digest.
const UPTANE_SIGNED_LEN: usize = 19 + 4 + 8 + UPTANE_DIGEST_LEN;

impl UptaneTargetMetadata {
    /// The exact bytes both roles sign: domain, then version, image size and digest, each in a
    /// fixed-width big-endian encoding so no two different targets share an encoding.
    pub fn signed_bytes(&self) -> [u8; UPTANE_SIGNED_LEN] {
        let mut out = [0u8; UPTANE_SIGNED_LEN];
        let mut at = 0;
        for part in [
            UPTANE_DOMAIN,
            &self.target_version.to_be_bytes()[..],
            &(self.image_size_bytes as u64).to_be_bytes()[..],
            &self.sha256_digest[..],
        ] {
            out[at..at + part.len()].copy_from_slice(part);
            at += part.len();
        }
        out
    }
}

/// Error codes returned by [`UptaneDualBankController::verify_manifest_metadata`].
pub mod ota_error {
    /// The version is not newer than the installed one.
    pub const REPLAY_ATTACK: u16 = 0x7001;
    /// The Director role's signature is invalid.
    pub const DIRECTOR_SIGNATURE_INVALID: u16 = 0x7002;
    /// The Image repository role's signature is invalid.
    pub const IMAGE_ROOT_SIGNATURE_INVALID: u16 = 0x7003;
    /// No role keys were provisioned, so nothing can be verified (fails closed).
    pub const KEYS_NOT_PROVISIONED: u16 = 0x7004;
}

pub struct UptaneDualBankController {
    active_bank: AtomicU8,
    boot_successful: AtomicBool,
    status: AtomicU8,
    active_monotonic_counter: AtomicU32,
    sectors_written: AtomicUsize,
    director_key: Option<[u8; 32]>,
    image_root_key: Option<[u8; 32]>,
    flash_storage_mock: UnsafeCell<[u8; FLASH_SECTOR_SIZE_BYTES * 4]>,
}

unsafe impl Sync for UptaneDualBankController {}

impl UptaneDualBankController {
    pub const fn new(initial_bank: PartitionBank, initial_version: u32) -> Self {
        Self {
            active_bank: AtomicU8::new(initial_bank as u8),
            boot_successful: AtomicBool::new(true),
            status: AtomicU8::new(OtaLifecycleStatus::Idle as u8),
            active_monotonic_counter: AtomicU32::new(initial_version),
            sectors_written: AtomicUsize::new(0),
            director_key: None,
            image_root_key: None,
            flash_storage_mock: UnsafeCell::new([0xFF; FLASH_SECTOR_SIZE_BYTES * 4]),
        }
    }

    /// Provisions the Ed25519 public keys of the Director and Image-repository roles. Without
    /// them every update is refused.
    #[must_use]
    pub const fn with_role_keys(mut self, director: [u8; 32], image_root: [u8; 32]) -> Self {
        self.director_key = Some(director);
        self.image_root_key = Some(image_root);
        self
    }

    pub fn get_active_bank(&self) -> PartitionBank {
        match self.active_bank.load(Ordering::Acquire) {
            0 => PartitionBank::BankA,
            _ => PartitionBank::BankB,
        }
    }

    pub fn get_inactive_bank(&self) -> PartitionBank {
        match self.get_active_bank() {
            PartitionBank::BankA => PartitionBank::BankB,
            PartitionBank::BankB => PartitionBank::BankA,
        }
    }

    /// Validates metadata against replay attacks and verifies cryptographic signatures
    pub fn verify_manifest_metadata(&self, meta: &UptaneTargetMetadata) -> Result<(), u16> {
        self.status.store(
            OtaLifecycleStatus::VerifyingMetadata as u8,
            Ordering::SeqCst,
        );

        // Anti-rollback protection: new version must strictly exceed active monotonic counter
        let current_version = self.active_monotonic_counter.load(Ordering::Relaxed);
        if meta.target_version <= current_version {
            self.status.store(
                OtaLifecycleStatus::QuarantineRollback as u8,
                Ordering::SeqCst,
            );
            return Err(ota_error::REPLAY_ATTACK);
        }

        // Uptane: BOTH the Director and the Image repository must have signed this exact target.
        let (Some(director), Some(image_root)) = (self.director_key, self.image_root_key) else {
            self.status.store(
                OtaLifecycleStatus::QuarantineRollback as u8,
                Ordering::SeqCst,
            );
            return Err(ota_error::KEYS_NOT_PROVISIONED);
        };
        let signed = meta.signed_bytes();
        if crate::p13_automotive_hsm::Ed25519Verifier::verify(
            &director,
            &signed,
            &meta.director_signature,
        )
        .is_err()
        {
            self.status.store(
                OtaLifecycleStatus::QuarantineRollback as u8,
                Ordering::SeqCst,
            );
            return Err(ota_error::DIRECTOR_SIGNATURE_INVALID);
        }
        if crate::p13_automotive_hsm::Ed25519Verifier::verify(
            &image_root,
            &signed,
            &meta.image_root_signature,
        )
        .is_err()
        {
            self.status.store(
                OtaLifecycleStatus::QuarantineRollback as u8,
                Ordering::SeqCst,
            );
            return Err(ota_error::IMAGE_ROOT_SIGNATURE_INVALID);
        }

        Ok(())
    }

    /// Flashes raw firmware chunks into the inactive partition bank
    pub fn write_firmware_sector(
        &self,
        sector_index: usize,
        sector_data: &[u8],
    ) -> Result<(), u16> {
        if sector_data.len() != FLASH_SECTOR_SIZE_BYTES || sector_index >= FLASH_MAX_IMAGE_SECTORS {
            return Err(0x7003); // SectorBoundExceeded
        }

        self.status
            .store(OtaLifecycleStatus::WritingSectors as u8, Ordering::Relaxed);

        // Write directly to non-volatile mock flash
        let flash = unsafe { &mut *self.flash_storage_mock.get() };
        let offset = (sector_index % 4) * FLASH_SECTOR_SIZE_BYTES;
        flash[offset..offset + FLASH_SECTOR_SIZE_BYTES].copy_from_slice(sector_data);

        self.sectors_written.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }

    /// Commits the partition bank swap and flags next hardware reset to boot inactive partition
    pub fn commit_and_swap_bank(&self, target_version: u32) -> Result<(), u16> {
        let new_bank = self.get_inactive_bank();
        self.active_bank.store(new_bank as u8, Ordering::SeqCst);
        self.active_monotonic_counter
            .store(target_version, Ordering::SeqCst);
        self.status
            .store(OtaLifecycleStatus::MarkedForReboot as u8, Ordering::SeqCst);
        Ok(())
    }

    /// Safety monitor: if the newly booted bank fails self-test, revert immediately to golden bank
    pub fn assert_boot_integrity(&self, is_healthy: bool) {
        if is_healthy {
            self.boot_successful.store(true, Ordering::SeqCst);
            self.status
                .store(OtaLifecycleStatus::Idle as u8, Ordering::SeqCst);
        } else {
            // Immediate rollback to alternative bank
            let fallback = self.get_inactive_bank();
            self.active_bank.store(fallback as u8, Ordering::SeqCst);
            self.status.store(
                OtaLifecycleStatus::QuarantineRollback as u8,
                Ordering::SeqCst,
            );
        }
    }
}

// =================================================================================================
// 6. MASTER VEHICLE OS MONOLITH ORCHESTRATOR
// =================================================================================================

pub struct MouliVehicleOsMasterMonolith {
    pub motion: StanleyMotionController,
    pub uds_server: UdsDiagnosticServer,
    pub ota_manager: UptaneDualBankController,
    pub cycle_counter: AtomicU64,
    pub system_healthy: AtomicBool,
}

impl MouliVehicleOsMasterMonolith {
    pub const fn new() -> Self {
        Self {
            motion: StanleyMotionController::new(0.75, 1.2),
            uds_server: UdsDiagnosticServer::new(),
            ota_manager: UptaneDualBankController::new(PartitionBank::BankA, 100),
            cycle_counter: AtomicU64::new(0),
            system_healthy: AtomicBool::new(true),
        }
    }

    /// High-frequency real-time orchestrator cycle running at 100 Hz (10ms)
    pub fn step_100hz_control_loop(
        &mut self,
        current_time_micros: u64,
        cur_x: f32,
        cur_y: f32,
        cur_heading: f32,
        cur_velocity: f32,
        target_path: &TrajectoryWaypoint,
    ) -> Result<(f32, f32), u16> {
        let cycle = self.cycle_counter.fetch_add(1, Ordering::Relaxed);
        let dt_secs = 0.010; // 10ms fixed cyclic step

        // 1. Compute Stanley lateral steering command
        let steer_rad = self.motion.compute_lateral_steering(
            cur_x,
            cur_y,
            cur_heading,
            cur_velocity,
            target_path,
            dt_secs,
        )?;

        // 2. Compute longitudinal drive effort
        let effort_mps2 = self.motion.compute_longitudinal_effort(
            target_path.target_velocity_mps,
            cur_velocity,
            dt_secs,
        );

        Ok((steer_rad, effort_mps2))
    }

    /// Diagnostic gateway handling ISO 14229 requests
    pub fn dispatch_diagnostic_request(&self, req: &UdsRequest, current_time: u64) -> UdsResponse {
        self.uds_server.process_uds_pdu(req, current_time)
    }
}

impl Default for MouliVehicleOsMasterMonolith {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 7. PART 4 UNIT TESTS & INTEGRATION VERIFICATION
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_stanley_motion_convergence() {
        let mut controller = StanleyMotionController::new(0.5, 1.0);
        let wp = TrajectoryWaypoint {
            x: 10.0,
            y: 0.0,
            target_heading_rad: 0.0,
            target_velocity_mps: 15.0,
            curvature: 0.0,
        };

        // Vehicle is offset laterally 1 meter to the right of path (y = -1.0)
        let steer = controller
            .compute_lateral_steering(0.0, -1.0, 0.0, 10.0, &wp, 0.010)
            .expect("Motion compute failed");

        // Stanley should steer left (positive steering angle) to correct the negative cross-track error
        assert!(steer > 0.0);
        assert!(steer <= MAX_STEER_ANGLE_RAD);
    }

    #[test]
    fn test_uds_session_and_seed_key_flow() {
        let server = UdsDiagnosticServer::new();

        // 1. Enter Extended Session (0x10 0x03)
        let req_sess = UdsRequest {
            service_id: 0x10,
            sub_function: 0x03,
            data_len: 0,
            data: [0u8; UDS_MAX_PAYLOAD_SIZE],
        };
        let resp_sess = server.process_uds_pdu(&req_sess, 1000);
        assert_eq!(resp_sess.service_id, 0x50);
        assert_eq!(resp_sess.data[0], 0x03);

        // 2. Request Security Seed (0x27 0x01)
        let req_seed = UdsRequest {
            service_id: 0x27,
            sub_function: 0x01,
            data_len: 0,
            data: [0u8; UDS_MAX_PAYLOAD_SIZE],
        };
        let resp_seed = server.process_uds_pdu(&req_seed, 2000);
        assert_eq!(resp_seed.service_id, 0x67);
        assert_eq!(resp_seed.data[0], 0x01);

        // 3. Send Calculated Valid Key (Seed XOR 0xA5)
        let mut key_data = [0u8; UDS_MAX_PAYLOAD_SIZE];
        for i in 0..UDS_SECURITY_KEY_LEN {
            key_data[i] = resp_seed.data[1 + i] ^ 0xA5;
        }

        let req_key = UdsRequest {
            service_id: 0x27,
            sub_function: 0x02,
            data_len: UDS_SECURITY_KEY_LEN,
            data: key_data,
        };
        let resp_key = server.process_uds_pdu(&req_key, 3000);
        assert_eq!(resp_key.service_id, 0x67);
        assert!(server.is_security_authorized());
    }

    use ed25519_dalek::{Signer, SigningKey};

    fn role(seed: u8) -> SigningKey {
        SigningKey::from_bytes(&[seed; 32])
    }

    fn controller(version: u32) -> UptaneDualBankController {
        UptaneDualBankController::new(PartitionBank::BankA, version).with_role_keys(
            role(1).verifying_key().to_bytes(),
            role(2).verifying_key().to_bytes(),
        )
    }

    fn signed_target(version: u32) -> UptaneTargetMetadata {
        let mut m = UptaneTargetMetadata {
            target_version: version,
            image_size_bytes: 4096,
            sha256_digest: [0xAA; UPTANE_DIGEST_LEN],
            director_signature: [0; UPTANE_SIGNATURE_LEN],
            image_root_signature: [0; UPTANE_SIGNATURE_LEN],
        };
        let bytes = m.signed_bytes();
        m.director_signature = role(1).sign(&bytes).to_bytes();
        m.image_root_signature = role(2).sign(&bytes).to_bytes();
        m
    }

    #[test]
    fn test_uptane_dual_bank_rollback_guard() {
        let ota = controller(200);
        assert_eq!(ota.get_active_bank(), PartitionBank::BankA);

        // Properly signed, but not newer than the installed 200: anti-rollback rejects it.
        assert_eq!(
            ota.verify_manifest_metadata(&signed_target(150)),
            Err(ota_error::REPLAY_ATTACK)
        );
        assert_eq!(
            ota.verify_manifest_metadata(&signed_target(200)),
            Err(ota_error::REPLAY_ATTACK)
        );

        // Properly signed and newer: accepted, and the bank swap goes through.
        assert!(ota.verify_manifest_metadata(&signed_target(201)).is_ok());
        assert!(ota.commit_and_swap_bank(201).is_ok());
        assert_eq!(ota.get_active_bank(), PartitionBank::BankB);
    }

    #[test]
    fn fake_signatures_are_rejected() {
        let ota = controller(100);
        // These are exactly the kinds of "signatures" the earlier placeholder accepted.
        for fill in [0x55u8, 0x01, 0xFF] {
            let mut m = signed_target(101);
            m.director_signature = [fill; UPTANE_SIGNATURE_LEN];
            m.image_root_signature = [fill; UPTANE_SIGNATURE_LEN];
            assert_eq!(
                ota.verify_manifest_metadata(&m),
                Err(ota_error::DIRECTOR_SIGNATURE_INVALID),
                "fill {fill:#x}"
            );
        }
    }

    #[test]
    fn both_roles_must_sign() {
        let ota = controller(100);
        let mut only_director = signed_target(101);
        only_director.image_root_signature = [0x55; UPTANE_SIGNATURE_LEN];
        assert_eq!(
            ota.verify_manifest_metadata(&only_director),
            Err(ota_error::IMAGE_ROOT_SIGNATURE_INVALID)
        );
        let mut only_root = signed_target(101);
        only_root.director_signature = [0x55; UPTANE_SIGNATURE_LEN];
        assert_eq!(
            ota.verify_manifest_metadata(&only_root),
            Err(ota_error::DIRECTOR_SIGNATURE_INVALID)
        );
        // Swapping the two roles' signatures must fail: each role has its own key.
        let mut swapped = signed_target(101);
        core::mem::swap(
            &mut swapped.director_signature,
            &mut swapped.image_root_signature,
        );
        assert_eq!(
            ota.verify_manifest_metadata(&swapped),
            Err(ota_error::DIRECTOR_SIGNATURE_INVALID)
        );
    }

    #[test]
    fn a_signature_covers_every_field_of_the_target() {
        let ota = controller(100);
        let good = signed_target(101);
        assert!(ota.verify_manifest_metadata(&good).is_ok());
        let mut other_digest = good;
        other_digest.sha256_digest[0] ^= 1;
        assert_eq!(
            ota.verify_manifest_metadata(&other_digest),
            Err(ota_error::DIRECTOR_SIGNATURE_INVALID)
        );
        let mut other_size = good;
        other_size.image_size_bytes += 1;
        assert_eq!(
            ota.verify_manifest_metadata(&other_size),
            Err(ota_error::DIRECTOR_SIGNATURE_INVALID)
        );
        // Re-labelling a signed older/newer image with a different version also fails.
        let mut other_version = good;
        other_version.target_version = 102;
        assert_eq!(
            ota.verify_manifest_metadata(&other_version),
            Err(ota_error::DIRECTOR_SIGNATURE_INVALID)
        );
    }

    #[test]
    fn signatures_from_the_wrong_keys_are_rejected() {
        let ota = controller(100);
        let mut m = signed_target(101);
        let bytes = m.signed_bytes();
        m.director_signature = role(9).sign(&bytes).to_bytes();
        assert_eq!(
            ota.verify_manifest_metadata(&m),
            Err(ota_error::DIRECTOR_SIGNATURE_INVALID)
        );
    }

    #[test]
    fn without_provisioned_keys_nothing_verifies() {
        let ota = UptaneDualBankController::new(PartitionBank::BankA, 100);
        assert_eq!(
            ota.verify_manifest_metadata(&signed_target(101)),
            Err(ota_error::KEYS_NOT_PROVISIONED)
        );
    }

    #[test]
    fn a_domain_separated_message_is_required() {
        // A signature over the bare fields, without the domain prefix, must not be accepted.
        let ota = controller(100);
        let mut m = signed_target(101);
        let mut raw = [0u8; 4 + 8 + UPTANE_DIGEST_LEN];
        raw[..4].copy_from_slice(&101u32.to_be_bytes());
        raw[4..12].copy_from_slice(&4096u64.to_be_bytes());
        raw[12..].copy_from_slice(&[0xAA; UPTANE_DIGEST_LEN]);
        m.director_signature = role(1).sign(&raw).to_bytes();
        m.image_root_signature = role(2).sign(&raw).to_bytes();
        assert_eq!(
            ota.verify_manifest_metadata(&m),
            Err(ota_error::DIRECTOR_SIGNATURE_INVALID)
        );
    }
}
