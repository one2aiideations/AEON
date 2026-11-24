//! Part 22: EV INVERTER & MOTOR DRIVE CORE, FIELD-ORIENTED CONTROL (FOC), SVPWM & SIC DRIVER

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 22 - EV INVERTER & MOTOR DRIVE CORE, FIELD-ORIENTED CONTROL (FOC), SVPWM & SIC DRIVER
// CRITICALITY: ISO 26262 ASIL-D / IEC 61508 SIL-3 / AUTOSAR MCAL Motor Control
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Heap Allocation, 20 kHz Hard Real-Time Loop (50 µs Deadline)
// =================================================================================================

#[allow(unused_imports)]
use crate::fmath::FloatMath;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

// =================================================================================================
// 1. INVERTER & TRACTION MOTOR PHYSICAL CONSTANTS
// =================================================================================================

/// Inverter Switching / Current Sampling Frequency (20 kHz = 50 µs control period)
pub const FOC_PWM_FREQUENCY_HZ: f32 = 20_000.0;
pub const FOC_CYCLE_PERIOD_S: f32 = 1.0 / FOC_PWM_FREQUENCY_HZ; // 50 µs

/// Minimum hardware dead-time in nanoseconds to prevent DC-bus shoot-through in SiC MOSFET bridge
pub const INVERTER_DEAD_TIME_NS: u32 = 450;

/// Maximum allowable DC-link bus voltage (Volts) for 800V SiC automotive powertrain
pub const MAX_DC_BUS_VOLTAGE_V: f32 = 920.0;
pub const MIN_DC_BUS_VOLTAGE_V: f32 = 450.0;

/// Absolute peak instantaneous phase current limit before hardware desaturation / overcurrent trip
pub const PEAK_PHASE_CURRENT_LIMIT_A: f32 = 750.0;

/// Permanent Magnet Synchronous Motor (PMSM) Baseline Parameters (Formula E / High-Performance EV)
pub const MOTOR_POLE_PAIRS: u8 = 4;
pub const MOTOR_STATOR_RESISTANCE_OHM: f32 = 0.0125; // Rs (Ohms)
pub const MOTOR_D_AXIS_INDUCTANCE_H: f32 = 0.000145; // Ld (Henries)
pub const MOTOR_Q_AXIS_INDUCTANCE_H: f32 = 0.000280; // Lq (Henries) - Salient PMSM
pub const MOTOR_PERMANENT_FLUX_WB: f32 = 0.0650; // Lambda_pm (Weber)
pub const MAX_ELECTRICAL_SPEED_RAD_S: f32 = 3500.0; // ~8350 RPM mechanical

/// Mathematical constants for Park/Clarke and SVPWM transforms
pub const ONE_OVER_SQRT3: f32 = 0.577_350_26;
pub const TWO_OVER_SQRT3: f32 = 1.154_700_5;
pub const SQRT3_OVER_TWO: f32 = 0.866_025_4;

// =================================================================================================
// 2. ERROR TAXONOMY & FAULT VECTORS
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InverterFault {
    None = 0,
    HardwareDesaturationTrip = 1,
    OverCurrentPhaseA = 2,
    OverCurrentPhaseB = 3,
    OverCurrentPhaseC = 4,
    DcBusOverVoltage = 5,
    DcBusUnderVoltage = 6,
    ResolverLossOfSignal = 7,
    GateDriverUndervoltageLockout = 8,
    InverterOverTemperature = 9,
    MotorOverTemperature = 10,
    CurrentSensorPlausibilityFailed = 11,
    FluxWeakeningInstability = 12,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InverterState {
    PowerOff = 0,
    Standby = 1,
    PrechargeActive = 2,
    GateCalibration = 3,
    ModulationReady = 4,
    TorqueControlActive = 5,
    ActiveShortCircuit = 6, // ASIL-D Emergency Safe State: Low-Side or High-Side FETs clamped ON
    FaultLocked = 7,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AscMode {
    LowSideActiveShortCircuit = 0,
    HighSideActiveShortCircuit = 1,
    AllPhasesHighImpedance = 2,
}

// =================================================================================================
// 3. FAST EMBEDDED TRIGONOMETRY FOR REAL-TIME FOC
// =================================================================================================

pub struct FastTrig;

impl FastTrig {
    /// Sine in radians (via `fmath`). The earlier 5th-order polynomial was only good near zero
    /// (about 0.07 off at pi) and its wrap-around loop never ended for infinite input.
    #[inline(always)]
    pub fn sin(theta: f32) -> f32 {
        crate::fmath::sin(f64::from(theta)) as f32
    }

    /// Cosine in radians (via `fmath`).
    #[inline(always)]
    pub fn cos(theta: f32) -> f32 {
        crate::fmath::cos(f64::from(theta)) as f32
    }

    /// Fast, non-allocating inverse square root
    #[inline(always)]
    pub fn inv_sqrt(x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        let i = x.to_bits();
        let i = 0x5f3759df_u32.wrapping_sub(i >> 1);
        let y = f32::from_bits(i);
        // Single Newton-Raphson iteration
        y * (1.5 - (0.5 * x * y * y))
    }
}

// =================================================================================================
// 4. COORDINATE TRANSFORMATIONS (CLARKE, PARK, INVERSE PARK)
// =================================================================================================

/// 3-Phase Natural Reference Frame ($a, b, c$) currents or voltages
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PhaseTriple {
    pub a: f32,
    pub b: f32,
    pub c: f32,
}

/// 2-Phase Stationary Orthogonal Reference Frame ($\alpha, \beta$)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AlphaBeta {
    pub alpha: f32,
    pub beta: f32,
}

/// 2-Phase Synchronously Rotating Reference Frame ($d, q$)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct DqPair {
    pub d: f32, // Direct axis (Flux aligned)
    pub q: f32, // Quadrature axis (Torque producing)
}

pub struct FocTransforms;

impl FocTransforms {
    /// Forward Clarke Transform: 3-phase ($a, b, c$) to stationary 2-phase ($\alpha, \beta$)
    /// Assumes balanced 3-wire neutral: $i_a + i_b + i_c = 0$
    #[inline(always)]
    pub fn clarke(abc: &PhaseTriple) -> AlphaBeta {
        AlphaBeta {
            alpha: abc.a,
            beta: ONE_OVER_SQRT3 * (abc.a + 2.0 * abc.b),
        }
    }

    /// Forward Park Transform: Stationary 2-phase ($\alpha, \beta$) to rotating ($d, q$)
    #[inline(always)]
    pub fn park(ab: &AlphaBeta, sin_theta: f32, cos_theta: f32) -> DqPair {
        DqPair {
            d: ab.alpha * cos_theta + ab.beta * sin_theta,
            q: -ab.alpha * sin_theta + ab.beta * cos_theta,
        }
    }

    /// Inverse Park Transform: Rotating ($d, q$) to stationary 2-phase ($\alpha, \beta$)
    #[inline(always)]
    pub fn inv_park(dq: &DqPair, sin_theta: f32, cos_theta: f32) -> AlphaBeta {
        AlphaBeta {
            alpha: dq.d * cos_theta - dq.q * sin_theta,
            beta: dq.d * sin_theta + dq.q * cos_theta,
        }
    }
}

// =================================================================================================
// 5. RESOLVER / ENCODER PHASE-LOCKED LOOP (PLL) OBSERVER
// =================================================================================================

/// High-precision tracking PLL extracting continuous rotor electrical angle and angular velocity
pub struct ResolverPllObserver {
    theta_est_rad: f32,
    omega_est_rad_s: f32,
    kp: f32,
    ki: f32,
    loss_of_signal_counter: u32,
}

impl ResolverPllObserver {
    pub const fn new() -> Self {
        Self {
            theta_est_rad: 0.0,
            omega_est_rad_s: 0.0,
            kp: 1200.0, // High-bandwidth natural frequency
            ki: 180_000.0,
            loss_of_signal_counter: 0,
        }
    }

    /// Evaluates raw sinusoidal resolver carrier envelopes (Sin/Cos tracking resolver)
    pub fn update_resolver(
        &mut self,
        sin_carrier: f32,
        cos_carrier: f32,
        dt: f32,
    ) -> Result<(f32, f32), InverterFault> {
        // Amplitude plausibility check: sin^2 + cos^2 should approximate 1.0
        let magnitude_sq = sin_carrier * sin_carrier + cos_carrier * cos_carrier;
        if magnitude_sq < 0.25 || magnitude_sq > 2.25 {
            self.loss_of_signal_counter += 1;
            if self.loss_of_signal_counter > 10 {
                return Err(InverterFault::ResolverLossOfSignal);
            }
        } else {
            self.loss_of_signal_counter = 0;
        }

        let sin_est = FastTrig::sin(self.theta_est_rad);
        let cos_est = FastTrig::cos(self.theta_est_rad);

        // Phase detector: sin(theta - theta_est) = sin(theta)*cos(theta_est) - cos(theta)*sin(theta_est)
        let phase_err = sin_carrier * cos_est - cos_carrier * sin_est;

        // PI loop filter for angular velocity tracking
        self.omega_est_rad_s += (self.ki * phase_err) * dt;
        let corrected_omega = self.omega_est_rad_s + self.kp * phase_err;

        // Mechanical to Electrical angle scaling
        self.theta_est_rad += corrected_omega * dt;

        // Wrap electrical angle to [0, 2*PI)
        while self.theta_est_rad >= core::f32::consts::TAU {
            self.theta_est_rad -= core::f32::consts::TAU;
        }
        while self.theta_est_rad < 0.0 {
            self.theta_est_rad += core::f32::consts::TAU;
        }

        Ok((self.theta_est_rad, self.omega_est_rad_s))
    }
}

impl Default for ResolverPllObserver {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. CURRENT LOOP PI CONTROLLERS WITH ANTI-WINDUP & CROSS-COUPLING DECOUPLING
// =================================================================================================

#[derive(Clone, Copy)]
pub struct CurrentPiRegulator {
    kp: f32,
    ki: f32,
    integrator: f32,
    anti_windup_limit: f32,
}

impl CurrentPiRegulator {
    pub const fn new(bandwidth_rad_s: f32, inductance_h: f32, resistance_ohm: f32) -> Self {
        Self {
            kp: bandwidth_rad_s * inductance_h,
            ki: bandwidth_rad_s * resistance_ohm,
            integrator: 0.0,
            anti_windup_limit: 500.0, // Initial voltage clamp
        }
    }

    pub fn set_limits(&mut self, max_voltage: f32) {
        self.anti_windup_limit = max_voltage;
    }

    #[inline(always)]
    pub fn step(&mut self, target: f32, measured: f32, feedforward_v: f32, dt: f32) -> f32 {
        let err = target - measured;

        // Proportional term
        let p_out = self.kp * err;

        // Integration with conditional clamp (Anti-windup)
        let tentative_i = self.integrator + (self.ki * err) * dt;
        let tentative_total = p_out + tentative_i + feedforward_v;

        if tentative_total.abs() <= self.anti_windup_limit {
            self.integrator = tentative_i;
        }

        (p_out + self.integrator + feedforward_v)
            .clamp(-self.anti_windup_limit, self.anti_windup_limit)
    }

    pub fn reset(&mut self) {
        self.integrator = 0.0;
    }
}

// =================================================================================================
// 7. MAXIMUM TORQUE PER AMPERE (MTPA) & FLUX-WEAKENING CONTROLLER
// =================================================================================================

pub struct FluxWeakeningEngine {
    v_dc_bus: f32,
    max_phase_voltage: f32,
    fw_integrator: f32,
    fw_kp: f32,
    fw_ki: f32,
}

impl FluxWeakeningEngine {
    pub const fn new() -> Self {
        Self {
            v_dc_bus: 800.0,
            max_phase_voltage: 800.0 * ONE_OVER_SQRT3,
            fw_integrator: 0.0,
            fw_kp: 0.05,
            fw_ki: 1.25,
        }
    }

    pub fn update_dc_bus(&mut self, v_dc: f32) {
        self.v_dc_bus = v_dc;
        // Inverter linear modulation voltage ceiling = Vdc / sqrt(3)
        self.max_phase_voltage = v_dc * ONE_OVER_SQRT3;
    }

    /// Evaluates voltage limit circle and computes negative direct-axis current injection ($I_d$)
    pub fn compute_current_references(
        &mut self,
        target_torque_nm: f32,
        omega_e_rad_s: f32,
        dt: f32,
    ) -> DqPair {
        // Base MTPA reference for salient PMSM:
        // Torque = 1.5 * p * (Lambda_pm * Iq + (Ld - Lq) * Id * Iq)
        let torque_constant = 1.5 * (MOTOR_POLE_PAIRS as f32) * MOTOR_PERMANENT_FLUX_WB;
        let mut i_q_ref = (target_torque_nm / torque_constant)
            .clamp(-PEAK_PHASE_CURRENT_LIMIT_A, PEAK_PHASE_CURRENT_LIMIT_A);

        // Calculate back-EMF magnitude: V_bemf ~= omega_e * sqrt((Ld*Id + Lambda_pm)^2 + (Lq*Iq)^2)
        let nominal_bemf = omega_e_rad_s.abs() * MOTOR_PERMANENT_FLUX_WB;
        let voltage_headroom = self.max_phase_voltage - nominal_bemf;

        let mut i_d_ref = 0.0f32;

        if voltage_headroom < 0.0 {
            // Speed exceeds base speed: Engage Flux-Weakening (inject negative d-axis current)
            let v_error = -voltage_headroom;
            self.fw_integrator += (self.fw_ki * v_error) * dt;
            self.fw_integrator = self
                .fw_integrator
                .clamp(0.0, PEAK_PHASE_CURRENT_LIMIT_A * 0.85);

            let fw_correction = -(self.fw_kp * v_error + self.fw_integrator);
            i_d_ref = fw_correction.clamp(-PEAK_PHASE_CURRENT_LIMIT_A * 0.85, 0.0);

            // Re-enforce current limit circle: Id^2 + Iq^2 <= I_max^2
            let max_iq_allowed_sq = PEAK_PHASE_CURRENT_LIMIT_A.powi(2) - i_d_ref.powi(2);
            if max_iq_allowed_sq > 0.0 {
                let max_iq = max_iq_allowed_sq.sqrt();
                i_q_ref = i_q_ref.clamp(-max_iq, max_iq);
            } else {
                i_q_ref = 0.0;
            }
        } else {
            self.fw_integrator = (self.fw_integrator - (self.fw_ki * 10.0) * dt).max(0.0);
        }

        DqPair {
            d: i_d_ref,
            q: i_q_ref,
        }
    }
}

impl Default for FluxWeakeningEngine {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 8. SPACE VECTOR PULSE WIDTH MODULATION (SVPWM) 6-SECTOR ENGINE
// =================================================================================================

#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PwmDutyCycles {
    pub duty_a: f32, // Phase A normalized duty cycle [0.0, 1.0]
    pub duty_b: f32, // Phase B normalized duty cycle [0.0, 1.0]
    pub duty_c: f32, // Phase C normalized duty cycle [0.0, 1.0]
    pub sector: u8,  // Active Space Vector Sector [1..6]
}

pub struct SvpwmEngine;

impl SvpwmEngine {
    /// Computes center-aligned symmetrical 3-phase duty cycles using Space Vector Hexagon
    pub fn compute_duties(voltage_ab: &AlphaBeta, v_dc: f32) -> PwmDutyCycles {
        // `!(v_dc > 10.0)` also catches NaN, which `v_dc <= 10.0` would let through.
        if !(v_dc > 10.0) {
            return PwmDutyCycles {
                duty_a: 0.0,
                duty_b: 0.0,
                duty_c: 0.0,
                sector: 0,
            };
        }

        let v_alpha = voltage_ab.alpha;
        let v_beta = voltage_ab.beta;

        // A non-finite command must never reach the PWM registers: output the zero-voltage
        // vector (all legs at 50%) and flag it with sector 0.
        if !v_alpha.is_finite() || !v_beta.is_finite() || !v_dc.is_finite() {
            return PwmDutyCycles {
                duty_a: 0.5,
                duty_b: 0.5,
                duty_c: 0.5,
                sector: 0,
            };
        }

        // Sector from the signs of three projections of the commanded vector: with
        // N = [V_beta > 0] + 2*[V2 > 0] + 4*[V3 > 0], where V2 = (sqrt(3)*V_alpha - V_beta) / 2
        // and V3 = (-sqrt(3)*V_alpha - V_beta) / 2, N maps to sectors 3->1, 1->2, 5->3, 4->4,
        // 6->5, 2->6 (sector k lies between active vectors k and k+1, 60 degrees apart).
        const SQRT_3: f32 = 1.732_050_8;
        let v1 = v_beta;
        let v2 = 0.5 * (SQRT_3 * v_alpha - v_beta);
        let v3 = 0.5 * (-SQRT_3 * v_alpha - v_beta);
        let n = u8::from(v1 > 0.0) + 2 * u8::from(v2 > 0.0) + 4 * u8::from(v3 > 0.0);
        let sector: u8 = match n {
            3 => 1,
            1 => 2,
            5 => 3,
            4 => 4,
            6 => 5,
            2 => 6,
            _ => 0, // all three projections non-positive or non-negative: only at zero/NaN
        };
        if sector == 0 {
            return PwmDutyCycles {
                duty_a: 0.5,
                duty_b: 0.5,
                duty_c: 0.5,
                sector: 0,
            };
        }

        // Normalised projections (switching period = 1). The scale is sqrt(3)/V_dc, derived by
        // solving V = T1*V_k + T2*V_(k+1) for active vectors of magnitude 2/3*V_dc.
        let t_pwm = 1.0f32;
        let k = SQRT_3 / v_dc;
        let x = k * v_beta;
        let y = k * (0.5 * SQRT_3 * v_alpha + 0.5 * v_beta);
        let z = k * (-0.5 * SQRT_3 * v_alpha + 0.5 * v_beta);

        // Dwell times (T1 on active vector k, T2 on k+1). Even sectors take their two
        // projections in the opposite order, which an earlier version of this table had swapped.
        let (mut t_a, mut t_b) = match sector {
            1 => (-z, x),
            2 => (y, z),
            3 => (x, -y),
            4 => (z, -x),
            5 => (-y, -z),
            6 => (-x, y),
            _ => (0.0, 0.0),
        };

        // Over-modulation saturation clamp
        let sum_t = t_a + t_b;
        if sum_t > t_pwm {
            t_a /= sum_t;
            t_b /= sum_t;
        }

        let t_0 = (t_pwm - (t_a + t_b)) * 0.5; // Null vector allocation (V0 / V7)

        // Symmetrical duty assignment mapping to phases A, B, C
        let (duty_a, duty_b, duty_c) = match sector {
            1 => (t_a + t_b + t_0, t_b + t_0, t_0),
            2 => (t_a + t_0, t_a + t_b + t_0, t_0),
            3 => (t_0, t_a + t_b + t_0, t_b + t_0),
            4 => (t_0, t_a + t_0, t_a + t_b + t_0),
            5 => (t_b + t_0, t_0, t_a + t_b + t_0),
            6 => (t_a + t_b + t_0, t_0, t_a + t_0),
            _ => (0.5, 0.5, 0.5),
        };

        PwmDutyCycles {
            duty_a: duty_a.clamp(0.0, 1.0),
            duty_b: duty_b.clamp(0.0, 1.0),
            duty_c: duty_c.clamp(0.0, 1.0),
            sector,
        }
    }
}

// =================================================================================================
// 9. HARDWARE GATE DRIVER INTERFACE & HARD FAULT INTERLOCKS
// =================================================================================================

#[repr(C)]
pub struct InverterHwRegisters {
    pub pwm_period_ticks: u32,
    pub duty_cmp_a: u32,
    pub duty_cmp_b: u32,
    pub duty_cmp_c: u32,
    pub deadtime_ticks: u32,
    pub gate_enable: u32,
    pub asc_control: u32,
    pub fault_status: u32,
}

pub struct GateDriverInterface {
    raw_regs: UnsafeCell<InverterHwRegisters>,
    clock_freq_hz: u32,
    is_armed: AtomicBool,
}

// `GateDriverInterface` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl GateDriverInterface {
    pub const fn new(clock_freq_hz: u32) -> Self {
        Self {
            raw_regs: UnsafeCell::new(InverterHwRegisters {
                pwm_period_ticks: 0,
                duty_cmp_a: 0,
                duty_cmp_b: 0,
                duty_cmp_c: 0,
                deadtime_ticks: 0,
                gate_enable: 0,
                asc_control: 0,
                fault_status: 0,
            }),
            clock_freq_hz,
            is_armed: AtomicBool::new(false),
        }
    }

    pub fn arm_inverter(&self) {
        let period = self.clock_freq_hz / (FOC_PWM_FREQUENCY_HZ as u32);
        let dt_ticks =
            (self.clock_freq_hz as u64 * INVERTER_DEAD_TIME_NS as u64 / 1_000_000_000) as u32;

        let regs = unsafe { &mut *self.raw_regs.get() };
        regs.pwm_period_ticks = period;
        regs.deadtime_ticks = dt_ticks;
        regs.duty_cmp_a = period / 2;
        regs.duty_cmp_b = period / 2;
        regs.duty_cmp_c = period / 2;
        regs.asc_control = 0;
        regs.gate_enable = 1;

        self.is_armed.store(true, Ordering::SeqCst);
    }

    /// Writes new phase comparator thresholds directly to hardware PWM timer registers
    #[inline(always)]
    pub fn write_pwm_duties(&self, duties: &PwmDutyCycles) {
        if !self.is_armed.load(Ordering::Relaxed) {
            return;
        }

        let regs = unsafe { &mut *self.raw_regs.get() };
        let period = regs.pwm_period_ticks as f32;

        regs.duty_cmp_a = (duties.duty_a * period) as u32;
        regs.duty_cmp_b = (duties.duty_b * period) as u32;
        regs.duty_cmp_c = (duties.duty_c * period) as u32;
    }

    /// Triggers ASIL-D Hardware Active Short Circuit (ASC) clamping all low-side MOSFETs
    pub fn trigger_emergency_asc(&self, mode: AscMode) {
        let regs = unsafe { &mut *self.raw_regs.get() };
        regs.gate_enable = 0; // Disable standard PWM modulation

        match mode {
            AscMode::LowSideActiveShortCircuit => {
                regs.asc_control = 0x01; // Hardware latch: Phase A,B,C low-side gates ON
            }
            AscMode::HighSideActiveShortCircuit => {
                regs.asc_control = 0x02; // Hardware latch: Phase A,B,C high-side gates ON
            }
            AscMode::AllPhasesHighImpedance => {
                regs.asc_control = 0x00; // All gates disabled (High-Z)
            }
        }

        self.is_armed.store(false, Ordering::SeqCst);
    }
}

// =================================================================================================
// 10. UNIFIED FIELD-ORIENTED MOTOR CONTROLLER SUBSYSTEM
// =================================================================================================

pub struct MotorDriveSubsystem {
    pub resolver: ResolverPllObserver,
    pub pi_d: CurrentPiRegulator,
    pub pi_q: CurrentPiRegulator,
    pub fw_engine: FluxWeakeningEngine,
    pub gate_driver: GateDriverInterface,
    state: AtomicU8,
    active_fault: AtomicU8,
    last_phase_a_current: UnsafeCell<f32>,
    last_phase_b_current: UnsafeCell<f32>,
    last_phase_c_current: UnsafeCell<f32>,
    last_torque_feedback_nm: UnsafeCell<f32>,
}

unsafe impl Sync for MotorDriveSubsystem {}

impl MotorDriveSubsystem {
    pub const fn new(clock_freq_hz: u32) -> Self {
        Self {
            resolver: ResolverPllObserver::new(),
            pi_d: CurrentPiRegulator::new(
                2000.0, // 2000 rad/s current loop bandwidth
                MOTOR_D_AXIS_INDUCTANCE_H,
                MOTOR_STATOR_RESISTANCE_OHM,
            ),
            pi_q: CurrentPiRegulator::new(
                2000.0,
                MOTOR_Q_AXIS_INDUCTANCE_H,
                MOTOR_STATOR_RESISTANCE_OHM,
            ),
            fw_engine: FluxWeakeningEngine::new(),
            gate_driver: GateDriverInterface::new(clock_freq_hz),
            state: AtomicU8::new(InverterState::Standby as u8),
            active_fault: AtomicU8::new(InverterFault::None as u8),
            last_phase_a_current: UnsafeCell::new(0.0),
            last_phase_b_current: UnsafeCell::new(0.0),
            last_phase_c_current: UnsafeCell::new(0.0),
            last_torque_feedback_nm: UnsafeCell::new(0.0),
        }
    }

    pub fn arm_and_start(&mut self) {
        self.pi_d.reset();
        self.pi_q.reset();
        self.gate_driver.arm_inverter();
        self.state
            .store(InverterState::TorqueControlActive as u8, Ordering::SeqCst);
    }

    /// Hard Real-Time 20 kHz FOC Control Cycle ($50\,\mu\text{s}$ execution window)
    pub fn step_foc_cycle(
        &mut self,
        current_abc: PhaseTriple,
        sin_carrier: f32,
        cos_carrier: f32,
        v_dc_bus: f32,
        torque_request_nm: f32,
    ) -> Result<PwmDutyCycles, InverterFault> {
        // Step 1: Physical overcurrent and DC-bus voltage verification
        if current_abc.a.abs() > PEAK_PHASE_CURRENT_LIMIT_A {
            self.trip_safety_fault(InverterFault::OverCurrentPhaseA);
            return Err(InverterFault::OverCurrentPhaseA);
        }
        if current_abc.b.abs() > PEAK_PHASE_CURRENT_LIMIT_A {
            self.trip_safety_fault(InverterFault::OverCurrentPhaseB);
            return Err(InverterFault::OverCurrentPhaseB);
        }
        if current_abc.c.abs() > PEAK_PHASE_CURRENT_LIMIT_A {
            self.trip_safety_fault(InverterFault::OverCurrentPhaseC);
            return Err(InverterFault::OverCurrentPhaseC);
        }
        if v_dc_bus > MAX_DC_BUS_VOLTAGE_V {
            self.trip_safety_fault(InverterFault::DcBusOverVoltage);
            return Err(InverterFault::DcBusOverVoltage);
        }
        if v_dc_bus < MIN_DC_BUS_VOLTAGE_V {
            self.trip_safety_fault(InverterFault::DcBusUnderVoltage);
            return Err(InverterFault::DcBusUnderVoltage);
        }

        unsafe {
            *self.last_phase_a_current.get() = current_abc.a;
            *self.last_phase_b_current.get() = current_abc.b;
            *self.last_phase_c_current.get() = current_abc.c;
        }

        // Step 2: Extract rotor angle and angular velocity via tracking PLL
        let (theta_e, omega_e) =
            match self
                .resolver
                .update_resolver(sin_carrier, cos_carrier, FOC_CYCLE_PERIOD_S)
            {
                Ok(values) => values,
                Err(e) => {
                    self.trip_safety_fault(e);
                    return Err(e);
                }
            };

        let sin_theta = FastTrig::sin(theta_e);
        let cos_theta = FastTrig::cos(theta_e);

        // Step 3: Forward Clarke & Park Transforms
        let i_ab = FocTransforms::clarke(&current_abc);
        let i_dq = FocTransforms::park(&i_ab, sin_theta, cos_theta);

        // Compute actual motor torque feedback:
        // T_em = 1.5 * p * [ Lambda_pm * Iq + (Ld - Lq) * Id * Iq ]
        let actual_torque = 1.5
            * (MOTOR_POLE_PAIRS as f32)
            * (MOTOR_PERMANENT_FLUX_WB * i_dq.q
                + (MOTOR_D_AXIS_INDUCTANCE_H - MOTOR_Q_AXIS_INDUCTANCE_H) * i_dq.d * i_dq.q);
        unsafe {
            *self.last_torque_feedback_nm.get() = actual_torque;
        }

        // Step 4: MTPA & Flux-Weakening current reference generation
        self.fw_engine.update_dc_bus(v_dc_bus);
        let target_i_dq = self.fw_engine.compute_current_references(
            torque_request_nm,
            omega_e,
            FOC_CYCLE_PERIOD_S,
        );

        // Step 5: Decoupled cross-coupling voltage feedforward compensation:
        // Vd_ff = -omega_e * Lq * Iq
        // Vq_ff =  omega_e * (Ld * Id + Lambda_pm)
        let v_d_ff = -omega_e * MOTOR_Q_AXIS_INDUCTANCE_H * i_dq.q;
        let v_q_ff = omega_e * (MOTOR_D_AXIS_INDUCTANCE_H * i_dq.d + MOTOR_PERMANENT_FLUX_WB);

        // Update dynamic voltage ceilings on PI regulators
        let max_phase_v = v_dc_bus * ONE_OVER_SQRT3;
        self.pi_d.set_limits(max_phase_v);
        self.pi_q.set_limits(max_phase_v);

        // Step 6: Step Current PI Loops to calculate Vd* and Vq*
        let v_d_star = self
            .pi_d
            .step(target_i_dq.d, i_dq.d, v_d_ff, FOC_CYCLE_PERIOD_S);
        let v_q_star = self
            .pi_q
            .step(target_i_dq.q, i_dq.q, v_q_ff, FOC_CYCLE_PERIOD_S);

        let v_dq_star = DqPair {
            d: v_d_star,
            q: v_q_star,
        };

        // Step 7: Inverse Park Transform back to stationary (alpha, beta)
        let v_ab_star = FocTransforms::inv_park(&v_dq_star, sin_theta, cos_theta);

        // Step 8: Calculate Space Vector PWM (SVPWM) Duty Cycles
        let pwm_duties = SvpwmEngine::compute_duties(&v_ab_star, v_dc_bus);

        // Step 9: Commit duty cycles to hardware timer compare registers
        self.gate_driver.write_pwm_duties(&pwm_duties);

        Ok(pwm_duties)
    }

    fn trip_safety_fault(&mut self, fault: InverterFault) {
        self.active_fault.store(fault as u8, Ordering::SeqCst);
        self.state
            .store(InverterState::ActiveShortCircuit as u8, Ordering::SeqCst);
        // Instant hardware fail-safe clamp (ASC low-side)
        self.gate_driver
            .trigger_emergency_asc(AscMode::LowSideActiveShortCircuit);
    }
}

// =================================================================================================
// 11. UNIT TESTS & VERIFICATION SUITE
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_clarke_park_transformation_roundtrip() {
        let abc = PhaseTriple {
            a: 100.0,
            b: -50.0,
            c: -50.0,
        };

        // Forward Clarke
        let ab = FocTransforms::clarke(&abc);
        assert!((ab.alpha - 100.0).abs() < 1e-4);
        assert!(ab.beta.abs() < 1e-4);

        // Park at theta = 0
        let dq = FocTransforms::park(&ab, 0.0, 1.0);
        assert!((dq.d - 100.0).abs() < 1e-4);
        assert!(dq.q.abs() < 1e-4);

        // Inverse Park at theta = 0
        let ab_inv = FocTransforms::inv_park(&dq, 0.0, 1.0);
        assert!((ab_inv.alpha - 100.0).abs() < 1e-4);
        assert!(ab_inv.beta.abs() < 1e-4);
    }

    #[test]
    fn test_svpwm_duty_generation_sector_1() {
        let v_ab = AlphaBeta {
            alpha: 200.0,
            beta: 100.0,
        };
        let v_dc = 800.0;

        let duties = SvpwmEngine::compute_duties(&v_ab, v_dc);
        assert!(duties.sector == 1 || duties.sector == 2);
        assert!(duties.duty_a >= 0.0 && duties.duty_a <= 1.0);
        assert!(duties.duty_b >= 0.0 && duties.duty_b <= 1.0);
        assert!(duties.duty_c >= 0.0 && duties.duty_c <= 1.0);
    }

    #[test]
    fn test_overcurrent_safety_interlock() {
        let mut motor_subsystem = MotorDriveSubsystem::new(100_000_000); // 100 MHz timer
        motor_subsystem.arm_and_start();

        let faulted_currents = PhaseTriple {
            a: 850.0, // Exceeds PEAK_PHASE_CURRENT_LIMIT_A (750A)
            b: -425.0,
            c: -425.0,
        };

        let result = motor_subsystem.step_foc_cycle(faulted_currents, 0.0, 1.0, 800.0, 50.0);
        assert_eq!(result, Err(InverterFault::OverCurrentPhaseA));
        assert_eq!(
            motor_subsystem.state.load(Ordering::SeqCst),
            InverterState::ActiveShortCircuit as u8
        );
    }

    #[test]
    fn fast_trig_matches_the_standard_library_over_a_full_turn_and_beyond() {
        let mut t = -15.0f32;
        while t <= 15.0 {
            assert!((FastTrig::sin(t) - t.sin()).abs() < 1e-6, "sin({t})");
            assert!((FastTrig::cos(t) - t.cos()).abs() < 1e-6, "cos({t})");
            t += 0.011;
        }
    }

    #[test]
    fn fast_trig_survives_a_failed_encoder() {
        for bad in [f32::INFINITY, f32::NEG_INFINITY, f32::NAN, 1e30, -1e30] {
            let (s, c) = (FastTrig::sin(bad), FastTrig::cos(bad));
            assert!(!s.is_infinite() && !c.is_infinite());
        }
        assert_eq!(FastTrig::inv_sqrt(-1.0), 0.0);
        assert_eq!(FastTrig::inv_sqrt(0.0), 0.0);
    }

    /// Phase voltages implied by duty cycles (centred on their mean), turned back into an
    /// alpha/beta vector with the amplitude-invariant Clarke transform.
    fn reconstruct(d: &PwmDutyCycles, v_dc: f32) -> (f32, f32) {
        let mean = (d.duty_a + d.duty_b + d.duty_c) / 3.0;
        let (va, vb, vc) = (
            v_dc * (d.duty_a - mean),
            v_dc * (d.duty_b - mean),
            v_dc * (d.duty_c - mean),
        );
        let alpha = (2.0 / 3.0) * (va - 0.5 * vb - 0.5 * vc);
        let beta = (vb - vc) / 3.0f32.sqrt();
        (alpha, beta)
    }

    #[test]
    fn svpwm_duties_reproduce_the_commanded_voltage_vector() {
        let v_dc = 400.0f32;
        let linear_limit = v_dc / 3.0f32.sqrt();
        let mut worst = 0.0f32;
        let mut sectors = [false; 7];
        for fraction in [0.1f32, 0.4, 0.7, 0.95] {
            for deg in 0..360 {
                let angle = (deg as f32 + 0.5).to_radians();
                let (alpha, beta) = (
                    fraction * linear_limit * angle.cos(),
                    fraction * linear_limit * angle.sin(),
                );
                let d = SvpwmEngine::compute_duties(&AlphaBeta { alpha, beta }, v_dc);
                sectors[usize::from(d.sector)] = true;
                for duty in [d.duty_a, d.duty_b, d.duty_c] {
                    assert!(
                        (0.0..=1.0).contains(&duty),
                        "duty {duty} out of range at {deg} deg"
                    );
                }
                let (ra, rb) = reconstruct(&d, v_dc);
                worst = worst.max((ra - alpha).abs()).max((rb - beta).abs());
            }
        }
        assert!(
            sectors[1..].iter().all(|&s| s),
            "all six sectors must be used: {sectors:?}"
        );
        // Error as a fraction of the DC bus; f32 rounding alone gives well under 0.5%.
        assert!(
            worst < 0.005 * v_dc,
            "worst reconstruction error {worst} V on a {v_dc} V bus"
        );
    }

    #[test]
    fn svpwm_dwell_times_are_never_negative_and_stay_within_the_period() {
        let v_dc = 600.0f32;
        for fraction in [0.2f32, 0.8, 1.0] {
            for deg in 0..720 {
                let angle = (deg as f32 * 0.5 + 0.1).to_radians();
                let m = fraction * v_dc / 3.0f32.sqrt();
                let d = SvpwmEngine::compute_duties(
                    &AlphaBeta {
                        alpha: m * angle.cos(),
                        beta: m * angle.sin(),
                    },
                    v_dc,
                );
                let lowest = d.duty_a.min(d.duty_b).min(d.duty_c);
                let highest = d.duty_a.max(d.duty_b).max(d.duty_c);
                assert!(lowest >= -1e-6 && highest <= 1.0 + 1e-6, "{deg}: {d:?}");
                // Centred modulation: the null-vector time is shared equally top and bottom.
                assert!(
                    (lowest - (1.0 - highest)).abs() < 1e-4,
                    "not centred at {deg}: {lowest} vs {highest}"
                );
            }
        }
    }

    #[test]
    fn svpwm_handles_zero_overmodulation_and_bad_input() {
        let zero = SvpwmEngine::compute_duties(
            &AlphaBeta {
                alpha: 0.0,
                beta: 0.0,
            },
            400.0,
        );
        for duty in [zero.duty_a, zero.duty_b, zero.duty_c] {
            assert!((0.0..=1.0).contains(&duty));
        }
        // Far beyond the hexagon: duties are clamped and still valid.
        let over = SvpwmEngine::compute_duties(
            &AlphaBeta {
                alpha: 5_000.0,
                beta: 3_000.0,
            },
            400.0,
        );
        for duty in [over.duty_a, over.duty_b, over.duty_c] {
            assert!((0.0..=1.0).contains(&duty), "{over:?}");
        }
        for bad in [f32::NAN, f32::INFINITY] {
            let d = SvpwmEngine::compute_duties(
                &AlphaBeta {
                    alpha: bad,
                    beta: 1.0,
                },
                400.0,
            );
            for duty in [d.duty_a, d.duty_b, d.duty_c] {
                assert!(duty.is_finite() && (0.0..=1.0).contains(&duty), "{d:?}");
            }
        }
        // A collapsed DC bus is refused (all duties zero).
        let dead = SvpwmEngine::compute_duties(
            &AlphaBeta {
                alpha: 10.0,
                beta: 10.0,
            },
            5.0,
        );
        assert_eq!(
            (dead.duty_a, dead.duty_b, dead.duty_c, dead.sector),
            (0.0, 0.0, 0.0, 0)
        );
    }
}
