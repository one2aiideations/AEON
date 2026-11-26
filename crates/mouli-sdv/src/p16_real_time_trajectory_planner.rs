//! Part 16: REAL-TIME TRAJECTORY PLANNER & MODEL PREDICTIVE CONTROL (MPC) SOLVER

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 16 - REAL-TIME TRAJECTORY PLANNER & MODEL PREDICTIVE CONTROL (MPC) SOLVER
// CRITICALITY: ISO 26262 ASIL-D / ISO 21448 (SOTIF) / Real-Time Numerical Control
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Dynamic Allocation, Bounded Iterations, Deterministic WCET
// =================================================================================================

#[allow(unused_imports)]
use crate::fmath::FloatMath;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};

// =================================================================================================
// 1. ARCHITECTURAL CONSTANTS & KINEMATIC VEHICLE GEOMETRY
// =================================================================================================

/// Model Predictive Control Prediction Horizon Steps (N = 10 steps @ dt = 50ms = 500ms horizon)
pub const MPC_HORIZON_STEPS: usize = 10;
pub const MPC_TIME_STEP_SEC: f32 = 0.050; // 50 ms loop cycle (20 Hz control rate)

/// Maximum number of trajectory waypoints generated along a planned lane maneuver
pub const MAX_TRAJECTORY_WAYPOINTS: usize = 64;

/// Local 2D Occupancy Grid Costmap Dimensions (64x64 cells @ 0.5m resolution = 32m x 32m field)
pub const COSTMAP_CELLS_DIM: usize = 64;
pub const COSTMAP_RESOLUTION_M: f32 = 0.50;

/// Maximum number of dynamic obstacles evaluated in the spatial potential field
pub const MAX_PLANNED_OBSTACLES: usize = 32;

/// Vehicle Kinematic Parameters (Standard E-Segment Premium Electric Sedan)
pub const VEHICLE_WHEELBASE_L_M: f32 = 2.95; // Distance between front and rear axles
pub const VEHICLE_WIDTH_M: f32 = 1.98;
pub const VEHICLE_LENGTH_M: f32 = 4.98;
pub const VEHICLE_MASS_KG: f32 = 2350.0;
pub const VEHICLE_MAX_STEER_RAD: f32 = 0.6108; // +/- 35.0 degrees
pub const VEHICLE_MAX_STEER_RATE_RAD_S: f32 = core::f32::consts::FRAC_PI_4; // 45 degrees / sec
pub const VEHICLE_MAX_ACCEL_MPS2: f32 = 4.50; // Maximum service acceleration
pub const VEHICLE_MAX_DECEL_MPS2: f32 = -8.50; // Emergency deceleration limit
pub const VEHICLE_MAX_JERK_MPS3: f32 = 5.0; // Maximum comfortable jerk limit
pub const GRAVITY_MPS2: f32 = 9.80665;
pub const DEFAULT_ROAD_TIRE_MU: f32 = 0.85; // Standard dry asphalt friction coefficient

// QP Active-Set Solver Bounds
pub const QP_MAX_ITERATIONS: usize = 25;
pub const QP_EPSILON: f32 = 1e-4;

// =================================================================================================
// 2. ERROR TAXONOMY
// =================================================================================================

#[repr(u16)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MotionPlanningError {
    Ok = 0,
    InvalidBoundaryConditions = 1,
    TrajectoryInfeasible = 2,
    CostmapCollisionDetected = 3,
    QpMaxIterationsExceeded = 4,
    FrictionCircleLimitExceeded = 5,
    ActuatorSlewRateViolated = 6,
    MatrixSingular = 7,
    LockContention = 8,
}

// =================================================================================================
// 3. EMBEDDED KINEMATIC STATE & TRAJECTORY STRUCTURES
// =================================================================================================

/// Vehicle 2D Pose and Kinematic State
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct VehicleState {
    pub x_m: f32,               // Longitudinal global/local frame coordinate (meters)
    pub y_m: f32,               // Lateral global/local frame coordinate (meters)
    pub yaw_rad: f32,           // Heading angle relative to positive X-axis (radians)
    pub velocity_mps: f32,      // Forward longitudinal velocity (m/s)
    pub steering_rad: f32,      // Current road wheel angle (radians)
    pub acceleration_mps2: f32, // Current longitudinal acceleration (m/s^2)
    pub timestamp_us: u64,
}

impl VehicleState {
    pub const fn zero() -> Self {
        Self {
            x_m: 0.0,
            y_m: 0.0,
            yaw_rad: 0.0,
            velocity_mps: 0.0,
            steering_rad: 0.0,
            acceleration_mps2: 0.0,
            timestamp_us: 0,
        }
    }
}

/// Control Command Vector computed by the Model Predictive Controller
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MpcControlOutput {
    pub target_steering_rad: f32,
    pub target_accel_mps2: f32,
    pub expected_cost: f32,
    pub is_feasible: bool,
}

impl MpcControlOutput {
    pub const fn empty() -> Self {
        Self {
            target_steering_rad: 0.0,
            target_accel_mps2: 0.0,
            expected_cost: 0.0,
            is_feasible: false,
        }
    }
}

/// Single Waypoint on a Planned Continuous Trajectory
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrajectoryPoint {
    pub x_m: f32,
    pub y_m: f32,
    pub yaw_rad: f32,
    pub curvature_inv_m: f32,
    pub target_velocity_mps: f32,
    pub target_accel_mps2: f32,
    pub relative_time_s: f32,
}

impl TrajectoryPoint {
    pub const fn zero() -> Self {
        Self {
            x_m: 0.0,
            y_m: 0.0,
            yaw_rad: 0.0,
            curvature_inv_m: 0.0,
            target_velocity_mps: 0.0,
            target_accel_mps2: 0.0,
            relative_time_s: 0.0,
        }
    }
}

/// Full Planned Trajectory Buffer
pub struct TrajectoryProfile {
    pub points: [TrajectoryPoint; MAX_TRAJECTORY_WAYPOINTS],
    pub count: usize,
    pub is_valid: bool,
}

impl TrajectoryProfile {
    pub const fn new() -> Self {
        Self {
            points: [TrajectoryPoint::zero(); MAX_TRAJECTORY_WAYPOINTS],
            count: 0,
            is_valid: false,
        }
    }
}

impl Default for TrajectoryProfile {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 4. QUINTIC POLYNOMIAL TRAJECTORY GENERATOR (JERK-OPTIMAL SPLINES)
// =================================================================================================

/// 1D Quintic Polynomial: $s(t) = a_0 + a_1 t + a_2 t^2 + a_3 t^3 + a_4 t^4 + a_5 t^5$
/// Minimizes squared jerk: $\int_0^T (\dddot{s}(t))^2 dt$
#[derive(Debug, Clone, Copy)]
pub struct QuinticPolynomial1D {
    pub a0: f32,
    pub a1: f32,
    pub a2: f32,
    pub a3: f32,
    pub a4: f32,
    pub a5: f32,
    pub duration_t: f32,
}

impl QuinticPolynomial1D {
    /// Computes analytical coefficients matching boundary conditions:
    /// Start: (x0, v0, a0) at t = 0
    /// Target: (x1, v1, a1) at t = T
    pub fn compute(
        x0: f32,
        v0: f32,
        a0_val: f32,
        x1: f32,
        v1: f32,
        a1_val: f32,
        duration_t: f32,
    ) -> Result<Self, MotionPlanningError> {
        if duration_t <= 0.010 {
            return Err(MotionPlanningError::InvalidBoundaryConditions);
        }

        let t = duration_t;
        let t2 = t * t;
        let t3 = t2 * t;
        let t4 = t3 * t;
        let t5 = t4 * t;

        let c0 = x0;
        let c1 = v0;
        let c2 = 0.5 * a0_val;

        // Linear equation system solving for [a3, a4, a5]^T
        let dx = x1 - c0 - c1 * t - c2 * t2;
        let dv = v1 - c1 - 2.0 * c2 * t;
        let da = a1_val - 2.0 * c2;

        let a3 = (10.0 * dx - 4.0 * dv * t + 0.5 * da * t2) / t3;
        let a4 = (-15.0 * dx + 7.0 * dv * t - da * t2) / t4;
        let a5 = (6.0 * dx - 3.0 * dv * t + 0.5 * da * t2) / t5;

        Ok(Self {
            a0: c0,
            a1: c1,
            a2: c2,
            a3,
            a4,
            a5,
            duration_t,
        })
    }

    #[inline(always)]
    pub fn position(&self, t: f32) -> f32 {
        let t_clamped = if t < 0.0 {
            0.0
        } else if t > self.duration_t {
            self.duration_t
        } else {
            t
        };
        let t2 = t_clamped * t_clamped;
        let t3 = t2 * t_clamped;
        let t4 = t3 * t_clamped;
        let t5 = t4 * t_clamped;
        self.a0 + self.a1 * t_clamped + self.a2 * t2 + self.a3 * t3 + self.a4 * t4 + self.a5 * t5
    }

    #[inline(always)]
    pub fn velocity(&self, t: f32) -> f32 {
        let t_clamped = if t < 0.0 {
            0.0
        } else if t > self.duration_t {
            self.duration_t
        } else {
            t
        };
        let t2 = t_clamped * t_clamped;
        let t3 = t2 * t_clamped;
        let t4 = t3 * t_clamped;
        self.a1
            + 2.0 * self.a2 * t_clamped
            + 3.0 * self.a3 * t2
            + 4.0 * self.a4 * t3
            + 5.0 * self.a5 * t4
    }

    #[inline(always)]
    pub fn acceleration(&self, t: f32) -> f32 {
        let t_clamped = if t < 0.0 {
            0.0
        } else if t > self.duration_t {
            self.duration_t
        } else {
            t
        };
        let t2 = t_clamped * t_clamped;
        let t3 = t2 * t_clamped;
        2.0 * self.a2 + 6.0 * self.a3 * t_clamped + 12.0 * self.a4 * t2 + 20.0 * self.a5 * t3
    }
}

// =================================================================================================
// 5. 2D OCCUPANCY GRID & POTENTIAL FIELD COSTMAP EVALUATOR
// =================================================================================================

/// Static 2D Local Costmap Array (64x64 cells @ 0.5m = 32m x 32m field around vehicle center)
#[repr(C, align(64))]
pub struct LocalCostmap {
    grid: UnsafeCell<[[u8; COSTMAP_CELLS_DIM]; COSTMAP_CELLS_DIM]>, // Cost values: 0 (free) to 255 (lethal)
    origin_x_m: f32,
    origin_y_m: f32,
    lock: AtomicBool,
}

// `LocalCostmap` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl LocalCostmap {
    pub const fn new() -> Self {
        Self {
            grid: UnsafeCell::new([[0u8; COSTMAP_CELLS_DIM]; COSTMAP_CELLS_DIM]),
            origin_x_m: -16.0, // Centered 16m behind front axle
            origin_y_m: -16.0, // Centered 16m to the right
            lock: AtomicBool::new(false),
        }
    }

    pub fn clear(&self) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        // SAFETY: the spin lock acquired just above is held until its release at the end of this function (no early
        // return in between), so this is the only reference to the grid.
        let grid = unsafe { &mut *self.grid.get() };
        for row in grid.iter_mut() {
            row.fill(0);
        }
        self.lock.store(false, Ordering::Release);
    }

    /// Renders an obstacle bounding circle into the costmap with repulsive Gaussian inflation
    pub fn inject_obstacle(&self, center_x: f32, center_y: f32, radius_m: f32, inflation_m: f32) {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }

        // SAFETY: the spin lock acquired just above is held until its release at the end of this function (no early
        // return in between), so this is the only reference to the grid.
        let grid = unsafe { &mut *self.grid.get() };
        let total_radius = radius_m + inflation_m;

        for r in 0..COSTMAP_CELLS_DIM {
            let cell_y = self.origin_y_m + (r as f32) * COSTMAP_RESOLUTION_M;
            for c in 0..COSTMAP_CELLS_DIM {
                let cell_x = self.origin_x_m + (c as f32) * COSTMAP_RESOLUTION_M;

                let dx = cell_x - center_x;
                let dy = cell_y - center_y;
                let dist_sq = dx * dx + dy * dy;

                if dist_sq <= radius_m * radius_m {
                    // Lethal obstacle core
                    grid[r][c] = 255;
                } else if dist_sq <= total_radius * total_radius {
                    // Exponential repulsive gradient
                    let dist = (dist_sq).sqrt();
                    let factor = 1.0 - ((dist - radius_m) / inflation_m);
                    let cost = (factor * 254.0) as u8;
                    if cost > grid[r][c] {
                        grid[r][c] = cost;
                    }
                }
            }
        }

        self.lock.store(false, Ordering::Release);
    }

    /// Checks if a 2D line trajectory point intersects any lethal cell (> 200)
    pub fn evaluate_point_cost(&self, x: f32, y: f32) -> u8 {
        let c = ((x - self.origin_x_m) / COSTMAP_RESOLUTION_M) as i32;
        let r = ((y - self.origin_y_m) / COSTMAP_RESOLUTION_M) as i32;

        if c < 0 || c >= (COSTMAP_CELLS_DIM as i32) || r < 0 || r >= (COSTMAP_CELLS_DIM as i32) {
            return 0; // Point is outside current local map bounds
        }

        // SAFETY: `LocalCostmap` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the grid is alive.
        // This takes no lock, so it must not run at the same time as the locked writers above.
        let grid = unsafe { &*self.grid.get() };
        grid[r as usize][c as usize]
    }
}

impl Default for LocalCostmap {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 6. NUMERICAL QUADRATIC PROGRAMMING (QP) ACTIVE-SET SOLVER (NO_STD)
// =================================================================================================

/// Solves strictly convex QP problems of the form:
/// minimize   0.5 * u^T * H * u + f^T * u
/// subject to u_min <= u <= u_max
///
/// Specialized for 2-Control Dimension MPC (Acceleration, Steering Angle) across Horizon N
pub struct ActiveSetQp2D {
    pub h_accel: f32, // Diagonal weight for acceleration
    pub h_steer: f32, // Diagonal weight for steering
    pub u_min_accel: f32,
    pub u_max_accel: f32,
    pub u_min_steer: f32,
    pub u_max_steer: f32,
}

impl ActiveSetQp2D {
    pub const fn new(w_accel: f32, w_steer: f32) -> Self {
        Self {
            h_accel: w_accel,
            h_steer: w_steer,
            u_min_accel: VEHICLE_MAX_DECEL_MPS2,
            u_max_accel: VEHICLE_MAX_ACCEL_MPS2,
            u_min_steer: -VEHICLE_MAX_STEER_RAD,
            u_max_steer: VEHICLE_MAX_STEER_RAD,
        }
    }

    /// Projected gradient descent active-set minimization with guaranteed convergence within N iterations
    pub fn solve(&self, f_accel: f32, f_steer: f32) -> Result<(f32, f32), MotionPlanningError> {
        if self.h_accel <= 1e-6 || self.h_steer <= 1e-6 {
            return Err(MotionPlanningError::MatrixSingular);
        }

        // Unconstrained unclipped minimum: u* = -H^{-1} * f
        let unconstrained_u_accel = -f_accel / self.h_accel;
        let unconstrained_u_steer = -f_steer / self.h_steer;

        // Project against bound constraints (Box clipping)
        let clamped_accel = if unconstrained_u_accel < self.u_min_accel {
            self.u_min_accel
        } else if unconstrained_u_accel > self.u_max_accel {
            self.u_max_accel
        } else {
            unconstrained_u_accel
        };

        let clamped_steer = if unconstrained_u_steer < self.u_min_steer {
            self.u_min_steer
        } else if unconstrained_u_steer > self.u_max_steer {
            self.u_max_steer
        } else {
            unconstrained_u_steer
        };

        Ok((clamped_accel, clamped_steer))
    }
}

// =================================================================================================
// 7. KINEMATIC BICYCLE MODEL PREDICTIVE CONTROLLER
// =================================================================================================

/// Real-Time Model Predictive Controller tracking reference trajectories
pub struct ModelPredictiveController {
    qp_solver: ActiveSetQp2D,
    last_steer_rad: UnsafeCell<f32>,
    last_accel_mps2: UnsafeCell<f32>,
    road_friction_mu: UnsafeCell<f32>,
    cycle_counter: AtomicU64,
}

// `ModelPredictiveController` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl ModelPredictiveController {
    pub const fn new() -> Self {
        Self {
            qp_solver: ActiveSetQp2D::new(1.0, 15.0), // Higher penalty on aggressive steering excursions
            last_steer_rad: UnsafeCell::new(0.0),
            last_accel_mps2: UnsafeCell::new(0.0),
            road_friction_mu: UnsafeCell::new(DEFAULT_ROAD_TIRE_MU),
            cycle_counter: AtomicU64::new(0),
        }
    }

    pub fn set_surface_friction(&self, mu: f32) {
        let mu_clamped = if mu < 0.15 {
            0.15
        } else if mu > 1.20 {
            1.20
        } else {
            mu
        };
        // SAFETY: `ModelPredictiveController` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the friction value is alive.
        // The reference is used for one store and ends.
        let ptr = unsafe { &mut *self.road_friction_mu.get() };
        *ptr = mu_clamped;
    }

    /// Forward integration of kinematic bicycle model over horizon dt:
    /// $\dot{x} = v \cos(\psi)$
    /// $\dot{y} = v \sin(\psi)$
    /// $\dot{\psi} = \frac{v}{L} \tan(\delta)$
    /// $\dot{v} = a$
    #[inline(always)]
    pub fn step_bicycle_model(
        current: &VehicleState,
        accel: f32,
        steer_angle: f32,
        dt: f32,
    ) -> VehicleState {
        let beta = (steer_angle * 0.5).tan(); // Center of gravity slip angle approx
        let next_v = (current.velocity_mps + accel * dt).max(0.0);
        let next_yaw = current.yaw_rad
            + (current.velocity_mps / VEHICLE_WHEELBASE_L_M) * steer_angle.tan() * dt;
        let next_x = current.x_m + current.velocity_mps * (current.yaw_rad).cos() * dt;
        let next_y = current.y_m + current.velocity_mps * (current.yaw_rad).sin() * dt;

        VehicleState {
            x_m: next_x,
            y_m: next_y,
            yaw_rad: next_yaw,
            velocity_mps: next_v,
            steering_rad: steer_angle,
            acceleration_mps2: accel,
            timestamp_us: current.timestamp_us + (dt * 1_000_000.0) as u64,
        }
    }

    /// Evaluates lateral acceleration: $a_y = \frac{v^2}{R} = \frac{v^2 \tan(\delta)}{L}$
    /// Asserts combined acceleration lies inside Kamm's friction circle:
    /// $a_{total}^2 = a_x^2 + a_y^2 \le (\mu \cdot g)^2$
    pub fn verify_friction_circle(
        &self,
        velocity_mps: f32,
        accel_mps2: f32,
        steer_rad: f32,
    ) -> bool {
        // SAFETY: `ModelPredictiveController` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the friction value is alive.
        // This reads a `Copy` `f32` by value.
        let mu = unsafe { *self.road_friction_mu.get() };
        let max_total_accel = mu * GRAVITY_MPS2;

        let lateral_accel =
            (velocity_mps * velocity_mps * steer_rad.abs().tan()) / VEHICLE_WHEELBASE_L_M;
        let total_accel_sq = accel_mps2 * accel_mps2 + lateral_accel * lateral_accel;

        total_accel_sq <= (max_total_accel * max_total_accel)
    }

    /// Primary MPC Optimization Cycle: Computes optimal $(a, \delta)$ commands matching the trajectory
    pub fn compute_control(
        &self,
        current_state: &VehicleState,
        trajectory: &TrajectoryProfile,
    ) -> Result<MpcControlOutput, MotionPlanningError> {
        if !trajectory.is_valid || trajectory.count == 0 {
            return Err(MotionPlanningError::TrajectoryInfeasible);
        }

        self.cycle_counter.fetch_add(1, Ordering::Relaxed);

        // Extract target waypoint closest to lookahead horizon
        let lookahead_idx = core::cmp::min(MPC_HORIZON_STEPS, trajectory.count.saturating_sub(1));
        let ref_pt = &trajectory.points[lookahead_idx];

        // 1. Longitudinal tracking error: e_v = v - v_ref
        let error_v = current_state.velocity_mps - ref_pt.target_velocity_mps;
        // Cost gradient term: f_accel = 2.0 * error_v
        let f_accel = 2.0 * error_v;

        // 2. Lateral & Heading Tracking Error (Frenet-Serret approx)
        let dx = ref_pt.x_m - current_state.x_m;
        let dy = ref_pt.y_m - current_state.y_m;

        // Heading error
        let mut error_yaw = ref_pt.yaw_rad - current_state.yaw_rad;
        while error_yaw > core::f32::consts::PI {
            error_yaw -= 2.0 * core::f32::consts::PI;
        }
        while error_yaw < -core::f32::consts::PI {
            error_yaw += 2.0 * core::f32::consts::PI;
        }

        // Cross-track error (projection of [dx, dy] orthogonal to current heading)
        let cross_track_error =
            -dx * (current_state.yaw_rad).sin() + dy * (current_state.yaw_rad).cos();

        // Target feedforward steering from road curvature: delta_ff = arctan(L * kappa)
        let steer_feedforward = (VEHICLE_WHEELBASE_L_M * ref_pt.curvature_inv_m).atan();

        // Combined lateral gradient
        let f_steer = -2.0 * (cross_track_error * 0.45 + error_yaw * 1.20) - steer_feedforward;

        // 3. Solve Quadratic Program
        let (raw_accel, raw_steer) = self.qp_solver.solve(f_accel, f_steer)?;

        // 4. Rate-Limiting Actuator Enforcements
        // SAFETY: `ModelPredictiveController` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the last steering command is alive.
        // This reads a `Copy` `f32` by value.
        let prev_steer = unsafe { *self.last_steer_rad.get() };
        // SAFETY: `ModelPredictiveController` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the last acceleration command is alive.
        // This reads a `Copy` `f32` by value.
        let prev_accel = unsafe { *self.last_accel_mps2.get() };

        let max_steer_delta = VEHICLE_MAX_STEER_RATE_RAD_S * MPC_TIME_STEP_SEC;
        let clamped_steer =
            raw_steer.clamp(prev_steer - max_steer_delta, prev_steer + max_steer_delta);

        let max_accel_delta = VEHICLE_MAX_JERK_MPS3 * MPC_TIME_STEP_SEC;
        let clamped_accel =
            raw_accel.clamp(prev_accel - max_accel_delta, prev_accel + max_accel_delta);

        // 5. ASIL-D Friction Circle Protection
        let is_within_grip =
            self.verify_friction_circle(current_state.velocity_mps, clamped_accel, clamped_steer);

        let final_accel = if !is_within_grip {
            // Priority safety reduction: clamp acceleration to maintain tire lateral grip
            clamped_accel.min(0.0)
        } else {
            clamped_accel
        };

        // Cache persistent state
        // SAFETY: `ModelPredictiveController` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the cached commands is alive.
        // These are two separate cells, each written once by value.
        unsafe {
            *self.last_steer_rad.get() = clamped_steer;
            *self.last_accel_mps2.get() = final_accel;
        }

        Ok(MpcControlOutput {
            target_steering_rad: clamped_steer,
            target_accel_mps2: final_accel,
            expected_cost: (error_v * error_v) + (cross_track_error * cross_track_error),
            is_feasible: is_within_grip,
        })
    }
}

impl Default for ModelPredictiveController {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 8. UNIFIED REAL-TIME MOTION PLANNER ORCHESTRATOR
// =================================================================================================

pub struct RealTimeMotionPlanner {
    pub costmap: LocalCostmap,
    pub mpc: ModelPredictiveController,
    active_trajectory: UnsafeCell<TrajectoryProfile>,
}

// `RealTimeMotionPlanner` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl RealTimeMotionPlanner {
    pub const fn new() -> Self {
        Self {
            costmap: LocalCostmap::new(),
            mpc: ModelPredictiveController::new(),
            active_trajectory: UnsafeCell::new(TrajectoryProfile::new()),
        }
    }

    /// Plans a smooth Quintic polynomial lane-change trajectory from current pose to target lane
    pub fn plan_lane_change(
        &self,
        start: &VehicleState,
        target_x: f32,
        target_y: f32,
        target_speed: f32,
        maneuver_time_s: f32,
    ) -> Result<(), MotionPlanningError> {
        let poly_x = QuinticPolynomial1D::compute(
            start.x_m,
            start.velocity_mps * (start.yaw_rad).cos(),
            start.acceleration_mps2 * (start.yaw_rad).cos(),
            target_x,
            target_speed,
            0.0,
            maneuver_time_s,
        )?;

        let poly_y = QuinticPolynomial1D::compute(
            start.y_m,
            start.velocity_mps * (start.yaw_rad).sin(),
            start.acceleration_mps2 * (start.yaw_rad).sin(),
            target_y,
            0.0,
            0.0,
            maneuver_time_s,
        )?;

        // SAFETY: `RealTimeMotionPlanner` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the active trajectory is alive.
        // The reference is used to fill the trajectory and ends with this function.
        let traj = unsafe { &mut *self.active_trajectory.get() };
        let dt = maneuver_time_s / (MAX_TRAJECTORY_WAYPOINTS as f32);

        for i in 0..MAX_TRAJECTORY_WAYPOINTS {
            let t = (i as f32) * dt;
            let px = poly_x.position(t);
            let py = poly_y.position(t);

            // Collision evaluation against the local costmap
            let cost = self.costmap.evaluate_point_cost(px, py);
            if cost >= 200 {
                traj.is_valid = false;
                return Err(MotionPlanningError::CostmapCollisionDetected);
            }

            let vx = poly_x.velocity(t);
            let vy = poly_y.velocity(t);
            let speed = (vx * vx + vy * vy).sqrt();
            let yaw = vy.atan2(vx);

            let ax = poly_x.acceleration(t);
            let ay = poly_y.acceleration(t);
            // Curvature: kappa = (vx * ay - vy * ax) / (vx^2 + vy^2)^(3/2)
            let speed_cubed = speed * speed * speed;
            let kappa = if speed_cubed > 1e-3 {
                (vx * ay - vy * ax) / speed_cubed
            } else {
                0.0
            };

            traj.points[i] = TrajectoryPoint {
                x_m: px,
                y_m: py,
                yaw_rad: yaw,
                curvature_inv_m: kappa,
                target_velocity_mps: speed,
                target_accel_mps2: (ax * (yaw).cos() + ay * (yaw).sin()),
                relative_time_s: t,
            };
        }

        traj.count = MAX_TRAJECTORY_WAYPOINTS;
        traj.is_valid = true;
        Ok(())
    }

    /// Single periodic control step (20 Hz)
    pub fn step_planning_cycle(
        &self,
        current: &VehicleState,
    ) -> Result<MpcControlOutput, MotionPlanningError> {
        // SAFETY: `RealTimeMotionPlanner` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the active trajectory is alive.
        // It is only read here, by `compute_control`, and no writer runs meanwhile.
        let traj = unsafe { &*self.active_trajectory.get() };
        self.mpc.compute_control(current, traj)
    }
}

impl Default for RealTimeMotionPlanner {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 9. VERIFICATION & UNIT TEST SUITE
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_quintic_polynomial_boundary_match() {
        let x0 = 0.0;
        let v0 = 10.0;
        let a0 = 0.0;
        let x1 = 50.0;
        let v1 = 15.0;
        let a1 = 0.0;
        let duration = 5.0;

        let poly = QuinticPolynomial1D::compute(x0, v0, a0, x1, v1, a1, duration)
            .expect("Spline generation must succeed");

        // Verify t = 0 matches start conditions
        assert!((poly.position(0.0) - x0).abs() < 1e-3);
        assert!((poly.velocity(0.0) - v0).abs() < 1e-3);
        assert!((poly.acceleration(0.0) - a0).abs() < 1e-3);

        // Verify t = T matches terminal target conditions
        assert!((poly.position(duration) - x1).abs() < 1e-3);
        assert!((poly.velocity(duration) - v1).abs() < 1e-3);
        assert!((poly.acceleration(duration) - a1).abs() < 1e-3);
    }

    #[test]
    fn test_costmap_obstacle_injection_and_detection() {
        let map = LocalCostmap::new();
        map.clear();

        // Inject an obstacle at (0, 0) with 2m radius and 1m inflation
        map.inject_obstacle(0.0, 0.0, 2.0, 1.0);

        // Core must be lethal (255)
        let core_cost = map.evaluate_point_cost(0.0, 0.0);
        assert_eq!(core_cost, 255);

        // Point outside the 3m perimeter must be free (0)
        let free_cost = map.evaluate_point_cost(5.0, 5.0);
        assert_eq!(free_cost, 0);
    }

    #[test]
    fn test_mpc_friction_circle_limiter() {
        let mpc = ModelPredictiveController::new();
        mpc.set_surface_friction(0.85); // mu = 0.85

        // Extreme speed (50 m/s = 180 km/h) with sharp steering (0.3 rad = 17 deg)
        // must trigger friction circle violation
        let is_safe = mpc.verify_friction_circle(50.0, 2.0, 0.30);
        assert!(!is_safe, "High speed cornering must exceed friction circle");

        // Mild speed (10 m/s = 36 km/h) with light steering must be safe
        let is_mild_safe = mpc.verify_friction_circle(10.0, 1.0, 0.05);
        assert!(
            is_mild_safe,
            "Moderate driving must remain safely within friction circle"
        );
    }

    #[test]
    fn test_qp_solver_box_clamping() {
        let qp = ActiveSetQp2D::new(1.0, 1.0);

        // Target demanding huge acceleration (-20 m/s^2) and extreme steering (2.0 rad)
        let (accel, steer) = qp.solve(20.0, -2.0).expect("QP must solve");

        assert_eq!(accel, VEHICLE_MAX_DECEL_MPS2);
        assert_eq!(steer, VEHICLE_MAX_STEER_RAD);
    }
}
