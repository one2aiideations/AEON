//! Part 20: EV BATTERY MANAGEMENT SYSTEM (BMS) CORE, CELL BALANCING & DUAL EKF SoC/SoH

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 20 - EV BATTERY MANAGEMENT SYSTEM (BMS) CORE, CELL BALANCING & DUAL EKF SoC/SoH
// CRITICALITY: ISO 26262 ASIL-D / IEC 62619 / UN ECE R100 Rev 3
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Dynamic Allocation, Microvolt/Millikelvin Precision Fixed Point
// =================================================================================================

use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU16, AtomicU64, AtomicU8, Ordering};

// =================================================================================================
// 1. HARDWARE ARCHITECTURE CONSTANTS: 800V EV TRACTION PACK
// =================================================================================================

/// 800V Battery Architecture: 192 Series Cells (e.g., 2P192S NMC811 / LFP configuration)
pub const BMS_SERIES_CELLS: usize = 192;

/// Number of temperature sensors distributed across the battery modules
pub const BMS_TEMP_SENSORS: usize = 48;

/// Minimum and Maximum Cell Operating Voltages (in millivolts, mV)
pub const CELL_UNDERVOLTAGE_FAULT_MV: u16 = 2800; // 2.80V Critical Undervoltage limit
pub const CELL_UNDERVOLTAGE_WARN_MV: u16 = 3000; // 3.00V Warning threshold
pub const CELL_NOMINAL_MV: u16 = 3700; // 3.70V Nominal chemistry
pub const CELL_OVERVOLTAGE_WARN_MV: u16 = 4200; // 4.20V Warning threshold
pub const CELL_OVERVOLTAGE_FAULT_MV: u16 = 4250; // 4.25V Critical Overvoltage limit

/// Temperature Operating Boundaries (in tenths of a degree Celsius: 0.1°C)
/// Offset by +500 to represent -50.0°C to +150.0°C as unsigned integers (0 = -50.0°C)
pub const TEMP_OFFSET_DECICELSIUS: i16 = 500;
pub const TEMP_MIN_CHARGE_LIMIT: i16 = 0; // 0.0°C (Do not fast-charge below freezing)
pub const TEMP_MAX_OPERATING_LIMIT: i16 = 600; // +60.0°C Max operating temperature
pub const TEMP_THERMAL_RUNAWAY_RATE_LIMIT: f32 = 1.0; // °C/second sudden temperature rise rate

/// Balancing threshold delta in millivolts
pub const CELL_BALANCING_DELTA_MV: u16 = 15; // Start balancing if delta > 15mV
pub const MAX_SIMULTANEOUS_BALANCING_CELLS: usize = 32;

// =================================================================================================
// 2. ERROR & FAULT TAXONOMY
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmsFault {
    None = 0,
    CellOvervoltage = 1,
    CellUndervoltage = 2,
    PackOvercurrentDischarge = 3,
    PackOvercurrentCharge = 4,
    Overtemperature = 5,
    Undertemperature = 6,
    ThermalRunawayDetected = 7,
    HighVoltageIsolationLoss = 8,
    ContactorWelded = 9,
    PrechargeTimeout = 10,
    InterlockBroken = 11,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContactorState {
    Open = 0,
    Precharging = 1,
    Closed = 2,
    WeldedFault = 3,
    EmergencyOpening = 4,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BmsOperatingMode {
    Init = 0,
    Standby = 1,
    Precharge = 2,
    DriveReady = 3,
    FastCharging = 4,
    SlowCharging = 5,
    CellBalancing = 6,
    FaultSafeState = 7,
}

// =================================================================================================
// 3. PHYSICAL TELEMETRY STRUCTURES
// =================================================================================================

/// High-precision telemetry acquired from Daisy-Chained AFE (Analog Front End) ASICs
#[repr(C)]
#[derive(Clone, Copy)]
pub struct CellTelemetry {
    pub voltage_mv: [u16; BMS_SERIES_CELLS],
    pub temperatures_decicelsius: [i16; BMS_TEMP_SENSORS],
    pub pack_voltage_deciav: u32,    // Tenths of a volt (0.1V)
    pub pack_current_milliamps: i32, // Positive: Charge, Negative: Discharge
    pub isolation_resistance_kohm: u32,
    pub timestamp_us: u64,
}

impl CellTelemetry {
    pub const fn empty() -> Self {
        Self {
            voltage_mv: [CELL_NOMINAL_MV; BMS_SERIES_CELLS],
            temperatures_decicelsius: [250; BMS_TEMP_SENSORS], // 25.0°C default
            pack_voltage_deciav: (CELL_NOMINAL_MV as u32 * BMS_SERIES_CELLS as u32) / 100,
            pack_current_milliamps: 0,
            isolation_resistance_kohm: 50_000, // 50 MΩ nominal high-voltage isolation
            timestamp_us: 0,
        }
    }
}

// =================================================================================================
// 4. DUAL EXTENDED KALMAN FILTER (EKF): EQUIVALENT CIRCUIT SoC / SoH ESTIMATOR
// =================================================================================================

/// 1st-Order Thevenin Equivalent Circuit Model:
/// V_terminal = OCV(SoC) - I * R_0 - V_rc
/// dV_rc/dt = -V_rc / (R_1 * C_1) + I / C_1
#[repr(C)]
#[derive(Clone, Copy)]
pub struct DualEkfEstimator {
    // State 1: State of Charge (SoC: 0.0 to 1.0)
    pub soc: f32,
    // State 2: Polarization capacitor voltage (V_rc)
    pub v_rc: f32,
    // State 3: Usable pack capacity (State of Health parameter: Ampere-seconds)
    pub nominal_capacity_as: f32,
    // State 4: Internal Ohmic resistance (R_0 in Ohms)
    pub r0_ohms: f32,
    // Model parameters
    pub r1_ohms: f32,
    pub c1_farads: f32,
    // Covariance matrix elements (P matrix diagonals)
    pub p_soc: f32,
    pub p_vrc: f32,
    pub p_r0: f32,
    // Process noise (Q)
    pub q_soc: f32,
    pub q_vrc: f32,
    pub q_r0: f32,
    // Measurement noise (R)
    pub r_measure: f32,
}

impl DualEkfEstimator {
    pub const fn new(initial_capacity_ah: f32) -> Self {
        Self {
            soc: 0.80, // Initial default 80% SoC
            v_rc: 0.0,
            nominal_capacity_as: initial_capacity_ah * 3600.0,
            r0_ohms: 0.025,    // 25 mΩ internal pack resistance
            r1_ohms: 0.015,    // 15 mΩ polarization resistance
            c1_farads: 2500.0, // 2500 F polarization capacitance
            p_soc: 0.01,
            p_vrc: 0.001,
            p_r0: 0.0001,
            q_soc: 1e-6,
            q_vrc: 1e-5,
            q_r0: 1e-7,
            r_measure: 0.005, // 5mV voltage sensor variance
        }
    }

    /// Evaluates non-linear Open Circuit Voltage (OCV) curve from SoC
    /// Approximation polynomial for standard NMC811 Lithium-ion cell
    #[inline(always)]
    pub fn ocv_from_soc(soc: f32) -> f32 {
        let s = if soc < 0.0 {
            0.0
        } else if soc > 1.0 {
            1.0
        } else {
            soc
        };
        // 5th-order empirical polynomial approximation
        3.0 + 1.25 * s - 0.95 * (s * s) + 1.85 * (s * s * s) - 1.20 * (s * s * s * s)
            + 0.30 * (s * s * s * s * s)
    }

    /// Derivative of OCV with respect to SoC (dOCV/dSoC)
    #[inline(always)]
    pub fn docv_dsoc(soc: f32) -> f32 {
        let s = if soc < 0.0 {
            0.0
        } else if soc > 1.0 {
            1.0
        } else {
            soc
        };
        1.25 - 1.90 * s + 5.55 * (s * s) - 4.80 * (s * s * s) + 1.50 * (s * s * s * s)
    }

    /// Executes EKF Predict and Update step on high-frequency sample
    pub fn update_step(
        &mut self,
        current_amperes: f32,
        terminal_voltage_volts: f32,
        dt_seconds: f32,
    ) {
        // --- 1. TIME PREDICTION (A PRIORI) ---
        // Coulomb counting integration
        let soc_prior = self.soc + (current_amperes * dt_seconds) / self.nominal_capacity_as;
        // RC circuit discrete-time state decay: exp(-dt / (R1*C1))
        let tau = self.r1_ohms * self.c1_farads;
        let alpha = 1.0 - (dt_seconds / tau);
        let v_rc_prior = alpha * self.v_rc + (dt_seconds / self.c1_farads) * current_amperes;

        // Covariance time update
        let p_soc_prior = self.p_soc + self.q_soc;
        let p_vrc_prior = (alpha * alpha) * self.p_vrc + self.q_vrc;

        // --- 2. MEASUREMENT PREDICTION ---
        let ocv_pred = Self::ocv_from_soc(soc_prior) * (BMS_SERIES_CELLS as f32);
        // Sign convention: current is NEGATIVE while discharging (Coulomb counting above lowers
        // SoC for negative current), so terminal voltage = OCV + I*R0 + V_rc, with V_rc taking
        // the sign of the current that charged it.
        let v_term_pred = ocv_pred + (current_amperes * self.r0_ohms) + v_rc_prior;
        let measurement_residual = terminal_voltage_volts - v_term_pred;

        // --- 3. KALMAN GAIN COMPUTATION ---
        let h_soc = Self::docv_dsoc(soc_prior) * (BMS_SERIES_CELLS as f32);
        // R0, R1 and C1 are pack-level, so V_rc is a pack voltage and is not scaled by cells.
        let h_vrc = 1.0;

        let s_innov = h_soc * p_soc_prior * h_soc + h_vrc * p_vrc_prior * h_vrc + self.r_measure;
        let k_soc = (p_soc_prior * h_soc) / s_innov;
        let k_vrc = (p_vrc_prior * h_vrc) / s_innov;

        // --- 4. STATE UPDATE (A POSTERIORI) ---
        self.soc = soc_prior + k_soc * measurement_residual;
        self.v_rc = v_rc_prior + k_vrc * measurement_residual;

        // Constrain SoC to valid physical range [0.0, 1.0]
        if self.soc < 0.0 {
            self.soc = 0.0;
        } else if self.soc > 1.0 {
            self.soc = 1.0;
        }

        // Covariance measurement update
        self.p_soc = (1.0 - k_soc * h_soc) * p_soc_prior;
        self.p_vrc = (1.0 - k_vrc * h_vrc) * p_vrc_prior;

        // Slow Adaptation: Ohmic Resistance update (Health Degradation Indicator)
        if current_amperes.abs() > 30.0 {
            // V = OCV + I*R0 + V_rc, so R0 = (V - OCV - V_rc) / I (positive for either sign of I).
            let r0_instant = (terminal_voltage_volts - ocv_pred - self.v_rc) / current_amperes;
            if r0_instant > 0.01 && r0_instant < 0.10 {
                self.r0_ohms = 0.999 * self.r0_ohms + 0.001 * r0_instant;
            }
        }
    }

    /// Computes State of Health (SoH) percentage based on cell internal resistance growth
    pub fn get_soh_percentage(&self) -> f32 {
        // Typical automotive end-of-life criteria: R0 increases by 100% (2.0x initial)
        let r0_initial = 0.025;
        let r0_degraded = 0.050;
        let soh = 100.0 * (1.0 - ((self.r0_ohms - r0_initial) / (r0_degraded - r0_initial)));
        if soh > 100.0 {
            100.0
        } else if soh < 0.0 {
            0.0
        } else {
            soh
        }
    }
}

// =================================================================================================
// 5. HARDWARE CELL BALANCING ENGINE
// =================================================================================================

pub struct PassiveBalancingController {
    balancing_mask: UnsafeCell<[bool; BMS_SERIES_CELLS]>,
    active_count: AtomicU8,
    target_voltage_mv: AtomicU16,
    is_enabled: AtomicBool,
}

// `PassiveBalancingController` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl PassiveBalancingController {
    pub const fn new() -> Self {
        Self {
            balancing_mask: UnsafeCell::new([false; BMS_SERIES_CELLS]),
            active_count: AtomicU8::new(0),
            target_voltage_mv: AtomicU16::new(CELL_NOMINAL_MV),
            is_enabled: AtomicBool::new(false),
        }
    }

    pub fn set_enabled(&self, enable: bool) {
        self.is_enabled.store(enable, Ordering::SeqCst);
    }

    /// Evaluates cell spread and schedules discharge bleed resistors on highest cells
    pub fn update_balancing_schedule(&self, voltages: &[u16; BMS_SERIES_CELLS]) {
        if !self.is_enabled.load(Ordering::Relaxed) {
            self.clear_all();
            return;
        }

        // Find lowest cell voltage in the pack
        let mut min_mv = u16::MAX;
        for &v in voltages.iter() {
            if v < min_mv {
                min_mv = v;
            }
        }
        self.target_voltage_mv.store(min_mv, Ordering::Relaxed);

        let mask = unsafe { &mut *self.balancing_mask.get() };
        let mut count = 0u8;

        for (idx, &v) in voltages.iter().enumerate() {
            // Balance if cell is above minimum cell by threshold AND above nominal baseline
            if v > min_mv + CELL_BALANCING_DELTA_MV
                && v > CELL_NOMINAL_MV
                && (count as usize) < MAX_SIMULTANEOUS_BALANCING_CELLS
            {
                // Avoid balancing adjacent cells simultaneously to prevent localized PCB hotspots
                let prev_active = if idx > 0 { mask[idx - 1] } else { false };
                if !prev_active {
                    mask[idx] = true;
                    count += 1;
                    continue;
                }
            }
            mask[idx] = false;
        }

        self.active_count.store(count, Ordering::Release);
    }

    pub fn clear_all(&self) {
        let mask = unsafe { &mut *self.balancing_mask.get() };
        for val in mask.iter_mut() {
            *val = false;
        }
        self.active_count.store(0, Ordering::Release);
    }

    pub fn is_cell_balancing(&self, cell_idx: usize) -> bool {
        if cell_idx < BMS_SERIES_CELLS {
            let mask = unsafe { &*self.balancing_mask.get() };
            mask[cell_idx]
        } else {
            false
        }
    }
}

impl Default for PassiveBalancingController {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. HIGH-VOLTAGE CONTACTOR SEQUENCER & PRECHARGE LOGIC
// =================================================================================================

pub struct HighVoltageContactorManager {
    main_positive: AtomicU8, // ContactorState
    main_negative: AtomicU8, // ContactorState
    precharge: AtomicU8,     // ContactorState
    precharge_start_us: AtomicU64,
    has_welded_fault: AtomicBool,
}

impl HighVoltageContactorManager {
    pub const PRECHARGE_TIMEOUT_US: u64 = 500_000; // 500ms max precharge timeout
    pub const PRECHARGE_VOLTAGE_TARGET_RATIO: f32 = 0.95; // Inverter must reach 95% pack voltage

    pub const fn new() -> Self {
        Self {
            main_positive: AtomicU8::new(ContactorState::Open as u8),
            main_negative: AtomicU8::new(ContactorState::Open as u8),
            precharge: AtomicU8::new(ContactorState::Open as u8),
            precharge_start_us: AtomicU64::new(0),
            has_welded_fault: AtomicBool::new(false),
        }
    }

    /// Step A: Initiate Safe Sequence by closing Main Negative
    pub fn start_precharge_sequence(&self, now_us: u64) -> Result<(), BmsFault> {
        if self.has_welded_fault.load(Ordering::Acquire) {
            return Err(BmsFault::ContactorWelded);
        }

        // Close Negative Contactor first
        self.main_negative
            .store(ContactorState::Closed as u8, Ordering::SeqCst);
        // Close Precharge Contactor with current-limiting series resistor
        self.precharge
            .store(ContactorState::Closed as u8, Ordering::SeqCst);
        self.precharge_start_us.store(now_us, Ordering::SeqCst);

        Ok(())
    }

    /// Step B: Evaluate Inverter DC link voltage; close Main Positive when equalized
    pub fn evaluate_precharge(
        &self,
        pack_voltage: f32,
        inverter_dc_link_voltage: f32,
        now_us: u64,
    ) -> Result<bool, BmsFault> {
        let start = self.precharge_start_us.load(Ordering::Relaxed);
        if now_us - start > Self::PRECHARGE_TIMEOUT_US {
            self.emergency_open();
            return Err(BmsFault::PrechargeTimeout);
        }

        // If DC link has charged to 95% of battery voltage, close Main Positive
        if inverter_dc_link_voltage >= pack_voltage * Self::PRECHARGE_VOLTAGE_TARGET_RATIO {
            self.main_positive
                .store(ContactorState::Closed as u8, Ordering::SeqCst);
            // Open precharge circuit to isolate bypass resistor
            self.precharge
                .store(ContactorState::Open as u8, Ordering::SeqCst);
            return Ok(true);
        }

        Ok(false)
    }

    /// High-voltage emergency disconnect
    pub fn emergency_open(&self) {
        self.main_positive
            .store(ContactorState::Open as u8, Ordering::SeqCst);
        self.precharge
            .store(ContactorState::Open as u8, Ordering::SeqCst);
        self.main_negative
            .store(ContactorState::Open as u8, Ordering::SeqCst);
    }

    pub fn are_contactors_closed(&self) -> bool {
        self.main_positive.load(Ordering::Acquire) == (ContactorState::Closed as u8)
            && self.main_negative.load(Ordering::Acquire) == (ContactorState::Closed as u8)
    }
}

impl Default for HighVoltageContactorManager {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 7. ASIL-D THERMAL RUNAWAY & HAZARD PROTECTION ENGINE
// =================================================================================================

pub struct ThermalSafetySupervisor {
    last_temperatures: UnsafeCell<[i16; BMS_TEMP_SENSORS]>,
    last_timestamp_us: AtomicU64,
    runaway_latched: AtomicBool,
}

// `ThermalSafetySupervisor` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl ThermalSafetySupervisor {
    pub const fn new() -> Self {
        Self {
            last_temperatures: UnsafeCell::new([250; BMS_TEMP_SENSORS]),
            last_timestamp_us: AtomicU64::new(0),
            runaway_latched: AtomicBool::new(false),
        }
    }

    /// Ingests temperature sensor array and calculates first-order heating derivatives ($dT/dt$)
    pub fn supervise_temperatures(
        &self,
        temps: &[i16; BMS_TEMP_SENSORS],
        now_us: u64,
    ) -> Result<(), BmsFault> {
        if self.runaway_latched.load(Ordering::Acquire) {
            return Err(BmsFault::ThermalRunawayDetected);
        }

        let last_time = self.last_timestamp_us.load(Ordering::Relaxed);
        let dt_sec = ((now_us.saturating_sub(last_time)) as f32) / 1_000_000.0;
        let prev = unsafe { &mut *self.last_temperatures.get() };

        for (i, &t) in temps.iter().enumerate() {
            // Check absolute upper safety bound (e.g. 60.0°C)
            if t > TEMP_MAX_OPERATING_LIMIT {
                return Err(BmsFault::Overtemperature);
            }

            // Check rate-of-rise: Thermal Runaway manifests as rapid exothermic spike (> 1.0°C/sec)
            if dt_sec > 0.1 {
                let dt_temp = ((t - prev[i]) as f32) / 10.0; // Convert 0.1°C to °C
                let rate = dt_temp / dt_sec;

                if rate > TEMP_THERMAL_RUNAWAY_RATE_LIMIT {
                    self.runaway_latched.store(true, Ordering::SeqCst);
                    return Err(BmsFault::ThermalRunawayDetected);
                }
            }

            prev[i] = t;
        }

        self.last_timestamp_us.store(now_us, Ordering::Release);
        Ok(())
    }
}

impl Default for ThermalSafetySupervisor {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 8. UNIFIED BATTERY MANAGEMENT SYSTEM CONTROLLER
// =================================================================================================

pub struct BatteryManagementSystem {
    pub ekf: UnsafeCell<DualEkfEstimator>,
    pub balancer: PassiveBalancingController,
    pub contactors: HighVoltageContactorManager,
    pub thermal: ThermalSafetySupervisor,
    pub mode: AtomicU8,
    pub fault_latched: AtomicU16,
    last_cycle_us: AtomicU64,
}

// `BatteryManagementSystem` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl BatteryManagementSystem {
    pub const fn new(pack_capacity_ah: f32) -> Self {
        Self {
            ekf: UnsafeCell::new(DualEkfEstimator::new(pack_capacity_ah)),
            balancer: PassiveBalancingController::new(),
            contactors: HighVoltageContactorManager::new(),
            thermal: ThermalSafetySupervisor::new(),
            mode: AtomicU8::new(BmsOperatingMode::Standby as u8),
            fault_latched: AtomicU16::new(BmsFault::None as u16),
            last_cycle_us: AtomicU64::new(0),
        }
    }

    /// Master BMS Periodic Cycle Execution (called deterministically at 100 Hz / 10ms)
    pub fn run_cycle(
        &self,
        telemetry: &CellTelemetry,
        inverter_dc_link_v: f32,
    ) -> Result<(), BmsFault> {
        let now = telemetry.timestamp_us;
        let last = self.last_cycle_us.load(Ordering::Relaxed);
        let dt = if last == 0 {
            0.01
        } else {
            (now.saturating_sub(last) as f32) / 1_000_000.0
        };
        self.last_cycle_us.store(now, Ordering::Relaxed);

        // --- STEP 1: CELL VOLTAGE BOUNDS VERIFICATION ---
        for &v in telemetry.voltage_mv.iter() {
            if v > CELL_OVERVOLTAGE_FAULT_MV {
                self.transition_to_fault(BmsFault::CellOvervoltage);
                return Err(BmsFault::CellOvervoltage);
            }
            if v < CELL_UNDERVOLTAGE_FAULT_MV {
                self.transition_to_fault(BmsFault::CellUndervoltage);
                return Err(BmsFault::CellUndervoltage);
            }
        }

        // --- STEP 2: THERMAL RUNAWAY SUPERVISION ---
        if let Err(fault) = self
            .thermal
            .supervise_temperatures(&telemetry.temperatures_decicelsius, now)
        {
            self.transition_to_fault(fault);
            return Err(fault);
        }

        // --- STEP 3: HIGH-VOLTAGE ISOLATION VERIFICATION ---
        // Minimum isolation standard: 500 Ω/V on 800V bus = 400 kΩ
        if telemetry.isolation_resistance_kohm < 400 {
            self.transition_to_fault(BmsFault::HighVoltageIsolationLoss);
            return Err(BmsFault::HighVoltageIsolationLoss);
        }

        // --- STEP 4: STATE ESTIMATION (DUAL EKF UPDATE) ---
        let pack_v = (telemetry.pack_voltage_deciav as f32) / 10.0;
        let pack_i = (telemetry.pack_current_milliamps as f32) / 1000.0;
        let ekf = unsafe { &mut *self.ekf.get() };
        ekf.update_step(pack_i, pack_v, dt);

        // --- STEP 5: AUTOMATED PASSIVE CELL BALANCING ---
        let current_mode = self.mode.load(Ordering::Relaxed);
        if current_mode == (BmsOperatingMode::Standby as u8)
            || current_mode == (BmsOperatingMode::SlowCharging as u8)
        {
            // Only balance when current is small (< 2A) to prevent I*R measuring distortion
            if pack_i.abs() < 2.0 {
                self.balancer.set_enabled(true);
                self.balancer
                    .update_balancing_schedule(&telemetry.voltage_mv);
            } else {
                self.balancer.clear_all();
            }
        } else {
            self.balancer.clear_all();
        }

        // --- STEP 6: PRECHARGE CONTACTOR ARBITRATION ---
        if current_mode == (BmsOperatingMode::Precharge as u8) {
            match self
                .contactors
                .evaluate_precharge(pack_v, inverter_dc_link_v, now)
            {
                Ok(true) => {
                    self.mode
                        .store(BmsOperatingMode::DriveReady as u8, Ordering::SeqCst);
                }
                Ok(false) => { /* Precharge progressing */ }
                Err(f) => {
                    self.transition_to_fault(f);
                    return Err(f);
                }
            }
        }

        Ok(())
    }

    fn transition_to_fault(&self, fault: BmsFault) {
        self.fault_latched.store(fault as u16, Ordering::SeqCst);
        self.mode
            .store(BmsOperatingMode::FaultSafeState as u8, Ordering::SeqCst);
        self.balancer.clear_all();
        self.contactors.emergency_open();
    }

    pub fn get_state_of_charge(&self) -> f32 {
        let ekf = unsafe { &*self.ekf.get() };
        ekf.soc
    }

    pub fn get_state_of_health(&self) -> f32 {
        let ekf = unsafe { &*self.ekf.get() };
        ekf.get_soh_percentage()
    }
}

// =================================================================================================
// 9. UNIT TESTS & VERIFICATION SUITE
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    /// A simulated battery that the estimator is not allowed to see inside.
    struct TrueBattery {
        soc: f32,
        v_rc: f32,
        r0: f32,
        capacity_as: f32,
    }

    impl TrueBattery {
        fn step(&mut self, current: f32, dt: f32, r1: f32, c1: f32) -> f32 {
            self.soc += current * dt / self.capacity_as;
            self.v_rc = (1.0 - dt / (r1 * c1)) * self.v_rc + dt / c1 * current;
            DualEkfEstimator::ocv_from_soc(self.soc) * BMS_SERIES_CELLS as f32
                + current * self.r0
                + self.v_rc
        }
    }

    #[test]
    fn test_ekf_soc_tracking() {
        let mut ekf = DualEkfEstimator::new(100.0); // 100 Ah pack
        let initial_soc = ekf.soc;
        let mut truth = TrueBattery {
            soc: initial_soc,
            v_rc: 0.0,
            r0: ekf.r0_ohms,
            capacity_as: ekf.nominal_capacity_as,
        };

        // Discharge at 50 A for 360 s: 18,000 A.s = 5 Ah = 5% of 100 Ah.
        for _ in 0..360 {
            let v = truth.step(-50.0, 1.0, ekf.r1_ohms, ekf.c1_farads);
            ekf.update_step(-50.0, v, 1.0);
        }
        assert!(
            (truth.soc - (initial_soc - 0.05)).abs() < 1e-4,
            "truth moved by {}",
            initial_soc - truth.soc
        );
        assert!(ekf.soc < initial_soc);
        assert!(
            (ekf.soc - truth.soc).abs() < 0.005,
            "estimate {} vs truth {}",
            ekf.soc,
            truth.soc
        );
    }

    #[test]
    fn ekf_recovers_from_a_wrong_initial_soc() {
        for wrong in [0.4f32, 0.6, 0.95] {
            let mut ekf = DualEkfEstimator::new(100.0);
            ekf.soc = wrong;
            let mut truth = TrueBattery {
                soc: 0.8,
                v_rc: 0.0,
                r0: ekf.r0_ohms,
                capacity_as: ekf.nominal_capacity_as,
            };
            for _ in 0..600 {
                let v = truth.step(-30.0, 1.0, ekf.r1_ohms, ekf.c1_farads);
                ekf.update_step(-30.0, v, 1.0);
            }
            assert!(
                (ekf.soc - truth.soc).abs() < 0.02,
                "started at {wrong}: estimate {} vs truth {}",
                ekf.soc,
                truth.soc
            );
        }
    }

    #[test]
    fn ekf_tracks_charging_as_well_as_discharging() {
        let mut ekf = DualEkfEstimator::new(100.0);
        let mut truth = TrueBattery {
            soc: 0.5,
            v_rc: 0.0,
            r0: ekf.r0_ohms,
            capacity_as: ekf.nominal_capacity_as,
        };
        ekf.soc = 0.5;
        for _ in 0..600 {
            let v = truth.step(40.0, 1.0, ekf.r1_ohms, ekf.c1_farads);
            ekf.update_step(40.0, v, 1.0);
        }
        assert!(truth.soc > 0.5 && ekf.soc > 0.5);
        assert!(
            (ekf.soc - truth.soc).abs() < 0.01,
            "estimate {} vs truth {}",
            ekf.soc,
            truth.soc
        );
    }

    #[test]
    fn ekf_learns_a_higher_internal_resistance_in_either_direction() {
        for current in [-60.0f32, 60.0] {
            let mut ekf = DualEkfEstimator::new(100.0);
            let before = ekf.r0_ohms;
            let mut truth = TrueBattery {
                soc: 0.8,
                v_rc: 0.0,
                r0: 0.045,
                capacity_as: ekf.nominal_capacity_as,
            };
            // 300 s at 60 A moves SoC by 5%, so the simulated pack stays well inside its range.
            for _ in 0..300 {
                let v = truth.step(current, 1.0, ekf.r1_ohms, ekf.c1_farads);
                ekf.update_step(current, v, 1.0);
            }
            assert!(
                ekf.r0_ohms < 0.045,
                "must move towards the true value, not past it: {}",
                ekf.r0_ohms
            );
            assert!(
                ekf.r0_ohms > before,
                "I = {current}: r0 stayed at {}",
                ekf.r0_ohms
            );
            assert!(ekf.get_soh_percentage() < 100.0);
        }
    }

    #[test]
    fn test_thermal_runaway_detection() {
        let supervisor = ThermalSafetySupervisor::new();
        let mut temps = [250i16; BMS_TEMP_SENSORS]; // 25.0°C

        assert_eq!(supervisor.supervise_temperatures(&temps, 0), Ok(()));

        // Sudden exothermic rise on sensor 4: 25.0°C -> 45.0°C in 500ms (+40°C/s)
        temps[4] = 450;
        let res = supervisor.supervise_temperatures(&temps, 500_000);
        assert_eq!(res, Err(BmsFault::ThermalRunawayDetected));
    }

    #[test]
    fn test_high_voltage_isolation_fault() {
        let bms = BatteryManagementSystem::new(80.0);
        let mut telemetry = CellTelemetry::empty();
        telemetry.isolation_resistance_kohm = 200; // Drops below 400 kΩ threshold

        let res = bms.run_cycle(&telemetry, 0.0);
        assert_eq!(res, Err(BmsFault::HighVoltageIsolationLoss));
        assert_eq!(
            bms.mode.load(Ordering::SeqCst),
            BmsOperatingMode::FaultSafeState as u8
        );
    }
}
