//! Part 21: HIGH-DEFINITION (HD) MAP & SPATIAL TOPOLOGY ENGINE, LANELET2 / OPENDRIVE ROUTING

// =================================================================================================
// PROJECT: mouli-OS (AI-Native, High-Assurance Rust Automotive Operating System Monolith)
// MODULE: PART 21 - HIGH-DEFINITION (HD) MAP & SPATIAL TOPOLOGY ENGINE, LANELET2 / OPENDRIVE ROUTING
// CRITICALITY: ISO 26262 ASIL-B / ISO 23793-1 / OGC Lanelet2 / ASAM OpenDRIVE Alignment
// TARGET ARCH: aarch64-unknown-none & riscv64gc-unknown-none
// CONSTRAINTS: 100% no_std, Zero Dynamic Allocation, Bounded Graph Traversal, Micro-Degree Resolution
// =================================================================================================

#[allow(unused_imports)]
use crate::fmath::FloatMath;
use core::cell::UnsafeCell;
use core::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, Ordering};

// =================================================================================================
// 1. TOPOLOGICAL & SPATIAL CONSTANTS
// =================================================================================================

/// Maximum localized lanelet elements stored in the active vehicle cache
pub const MAX_LOCAL_LANELETS: usize = 128;

/// Maximum waypoints forming a single lanelet boundary linestring
pub const MAX_WAYPOINTS_PER_BOUNDARY: usize = 32;

/// Maximum successor/predecessor and adjacent lane transition links per node
pub const MAX_LANE_TRANSITIONS: usize = 4;

/// Maximum regulatory elements (Traffic Lights, Stop Signs, Speed Limits) per lanelet
pub const MAX_REGULATORY_ELEMENTS_PER_LANELET: usize = 4;

/// Earth ellipsoid constants (WGS-84 standard)
pub const WGS84_A: f64 = 6378137.0; // Semi-major axis (meters)
pub const WGS84_F: f64 = 1.0 / 298.257223563; // Flattening factor
pub const WGS84_E2: f64 = 2.0 * WGS84_F - WGS84_F * WGS84_F; // First eccentricity squared

// =================================================================================================
// 2. ERROR & STATUS TAXONOMY
// =================================================================================================

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MapError {
    None = 0,
    LaneletNotFound = 1,
    OutOfMemoryBounds = 2,
    RouteUnreachable = 3,
    VehicleOffRoad = 4,
    RtkFixLost = 5,
    InvalidGeodeticCoordinates = 6,
    BoundaryDegenerate = 7,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RtkFixStatus {
    NoFix = 0,
    AutonomousGnss = 1,
    DifferentialGnss = 2,
    RtkFloat = 3,
    RtkFixed = 4, // Centimeter-accurate carrier-phase locked
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BoundaryMarking {
    Virtual = 0,
    SingleDashedWhite = 1,
    SingleSolidWhite = 2,
    DoubleSolidWhite = 3,
    SingleSolidYellow = 4,
    DoubleSolidYellow = 5,
    Curb = 6,
    Guardrail = 7,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RegulatoryType {
    SpeedLimit = 0,
    TrafficLight = 1,
    StopSign = 2,
    YieldSign = 3,
    PedestrianCrosswalk = 4,
}

#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrafficLightSignal {
    Unknown = 0,
    Red = 1,
    Yellow = 2,
    Green = 3,
    FlashingYellow = 4,
}

// =================================================================================================
// 3. GEODETIC & LOCAL CARTESIAN COORDINATE MODELS
// =================================================================================================

/// WGS-84 Geographic Coordinate (tenth-microdegree resolution: $10^{-7\circ} \approx 1.1\,\text{cm}$)
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Wgs84Point {
    pub lat_deg_e7: i32, // Latitude * 10^7
    pub lon_deg_e7: i32, // Longitude * 10^7
    pub alt_mm: i32,     // Altitude above WGS84 ellipsoid in millimeters
}

impl Wgs84Point {
    pub const fn new(lat_deg_e7: i32, lon_deg_e7: i32, alt_mm: i32) -> Self {
        Self {
            lat_deg_e7,
            lon_deg_e7,
            alt_mm,
        }
    }

    #[inline(always)]
    pub fn lat_f64(&self) -> f64 {
        (self.lat_deg_e7 as f64) * 1e-7
    }

    #[inline(always)]
    pub fn lon_f64(&self) -> f64 {
        (self.lon_deg_e7 as f64) * 1e-7
    }

    #[inline(always)]
    pub fn alt_f64(&self) -> f64 {
        (self.alt_mm as f64) * 1e-3
    }
}

/// Local Tangent Plane: East-North-Up (ENU) coordinates in meters relative to map origin
#[repr(C)]
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct EnuPoint {
    pub east_m: f32,
    pub north_m: f32,
    pub up_m: f32,
}

impl EnuPoint {
    pub const fn zero() -> Self {
        Self {
            east_m: 0.0,
            north_m: 0.0,
            up_m: 0.0,
        }
    }

    pub const fn new(east_m: f32, north_m: f32, up_m: f32) -> Self {
        Self {
            east_m,
            north_m,
            up_m,
        }
    }

    #[inline(always)]
    pub fn distance_2d_squared(&self, other: &EnuPoint) -> f32 {
        let de = self.east_m - other.east_m;
        let dn = self.north_m - other.north_m;
        de * de + dn * dn
    }
}

/// Fast Bare-Metal Geodetic Coordinate Transformer
pub struct GeodeticTransform;

impl GeodeticTransform {
    /// Converts Geodetic (Lat, Lon, Alt) to Local East-North-Up (ENU) around an anchor origin
    pub fn wgs84_to_enu(origin: &Wgs84Point, target: &Wgs84Point) -> EnuPoint {
        let lat_rad = origin.lat_f64() * (core::f64::consts::PI / 180.0);
        let lon_rad = origin.lon_f64() * (core::f64::consts::PI / 180.0);

        let d_lat = (target.lat_f64() - origin.lat_f64()) * (core::f64::consts::PI / 180.0);
        let d_lon = (target.lon_f64() - origin.lon_f64()) * (core::f64::consts::PI / 180.0);
        let d_alt = target.alt_f64() - origin.alt_f64();

        // Radii of curvature
        let sin_lat = lat_rad.sin();
        let cos_lat = lat_rad.cos();
        let n_lat = WGS84_A / (1.0 - WGS84_E2 * sin_lat * sin_lat).sqrt();
        let m_lat =
            (WGS84_A * (1.0 - WGS84_E2)) / ((1.0 - WGS84_E2 * sin_lat * sin_lat).powi(3)).sqrt();

        let north = (m_lat + origin.alt_f64()) * d_lat;
        let east = (n_lat + origin.alt_f64()) * cos_lat * d_lon;
        let up = d_alt;

        EnuPoint {
            east_m: east as f32,
            north_m: north as f32,
            up_m: up as f32,
        }
    }
}

// =================================================================================================
// 4. LANELET2 / OPENDRIVE MAP STRUCTURES
// =================================================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LineString {
    pub points: [EnuPoint; MAX_WAYPOINTS_PER_BOUNDARY],
    pub count: usize,
    pub marking: BoundaryMarking,
}

impl LineString {
    pub const fn empty(marking: BoundaryMarking) -> Self {
        Self {
            points: [EnuPoint::zero(); MAX_WAYPOINTS_PER_BOUNDARY],
            count: 0,
            marking,
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RegulatoryElement {
    pub element_id: u32,
    pub reg_type: RegulatoryType,
    pub value_u32: u32, // Speed limit in km/h or stop line point index
    pub dynamic_signal: TrafficLightSignal,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct LaneletNode {
    pub id: u32,
    pub left_bound: LineString,
    pub right_bound: LineString,
    pub centerline: LineString,
    pub length_m: f32,
    pub speed_limit_kph: u8,
    // Routing graph links
    pub successor_ids: [u32; MAX_LANE_TRANSITIONS],
    pub predecessor_ids: [u32; MAX_LANE_TRANSITIONS],
    pub adjacent_left_id: u32,
    pub adjacent_right_id: u32,
    pub regulatory: [RegulatoryElement; MAX_REGULATORY_ELEMENTS_PER_LANELET],
    pub regulatory_count: usize,
    pub is_active: bool,
}

impl LaneletNode {
    pub const fn empty() -> Self {
        Self {
            id: 0,
            left_bound: LineString::empty(BoundaryMarking::SingleSolidWhite),
            right_bound: LineString::empty(BoundaryMarking::SingleSolidWhite),
            centerline: LineString::empty(BoundaryMarking::Virtual),
            length_m: 0.0,
            speed_limit_kph: 50,
            successor_ids: [0; MAX_LANE_TRANSITIONS],
            predecessor_ids: [0; MAX_LANE_TRANSITIONS],
            adjacent_left_id: 0,
            adjacent_right_id: 0,
            regulatory: [RegulatoryElement {
                element_id: 0,
                reg_type: RegulatoryType::SpeedLimit,
                value_u32: 50,
                dynamic_signal: TrafficLightSignal::Unknown,
            }; MAX_REGULATORY_ELEMENTS_PER_LANELET],
            regulatory_count: 0,
            is_active: false,
        }
    }

    /// Computes the lanelet centerline from left and right boundaries
    pub fn compute_centerline(&mut self) {
        let count = if self.left_bound.count < self.right_bound.count {
            self.left_bound.count
        } else {
            self.right_bound.count
        };

        if count < 2 {
            return;
        }

        self.centerline.count = count;
        let mut total_len = 0.0f32;

        for i in 0..count {
            let lp = &self.left_bound.points[i];
            let rp = &self.right_bound.points[i];
            let cp = EnuPoint::new(
                (lp.east_m + rp.east_m) * 0.5,
                (lp.north_m + rp.north_m) * 0.5,
                (lp.up_m + rp.up_m) * 0.5,
            );
            self.centerline.points[i] = cp;

            if i > 0 {
                let prev = &self.centerline.points[i - 1];
                let dist_sq = cp.distance_2d_squared(prev);
                total_len += dist_sq.sqrt();
            }
        }
        self.length_m = total_len;
    }
}

// =================================================================================================
// 5. SPATIAL GEOMETRY & BOUNDARY CONTAINMENT ENGINE
// =================================================================================================

pub struct SpatialGeometry;

impl SpatialGeometry {
    /// Point-In-Polygon (PIP) ray-casting test for vehicle position inside lanelet polygon
    /// Polygon boundary formed by traversing left_bound forward, then right_bound backward
    pub fn is_point_in_lanelet(point: &EnuPoint, lanelet: &LaneletNode) -> bool {
        let left_n = lanelet.left_bound.count;
        let right_n = lanelet.right_bound.count;
        let total_verts = left_n + right_n;

        // A polygon needs at least 3 vertices. (The simplest real lane, a straight segment with
        // two points per side, has 4; requiring 6 made every such lane contain nothing.)
        if left_n < 2 || right_n < 2 || total_verts < 3 {
            return false;
        }

        let mut inside = false;
        let mut j = total_verts - 1;

        let get_vert = |idx: usize| -> EnuPoint {
            if idx < left_n {
                lanelet.left_bound.points[idx]
            } else {
                let r_idx = right_n - 1 - (idx - left_n);
                lanelet.right_bound.points[r_idx]
            }
        };

        for i in 0..total_verts {
            let vi = get_vert(i);
            let vj = get_vert(j);

            let intersect = ((vi.north_m > point.north_m) != (vj.north_m > point.north_m))
                && (point.east_m
                    < (vj.east_m - vi.east_m) * (point.north_m - vi.north_m)
                        / (vj.north_m - vi.north_m)
                        + vi.east_m);

            if intersect {
                inside = !inside;
            }
            j = i;
        }

        inside
    }

    /// Computes minimum lateral orthogonal distance and projection onto lane centerline
    pub fn distance_to_centerline(point: &EnuPoint, centerline: &LineString) -> (f32, usize) {
        if centerline.count < 2 {
            return (f32::MAX, 0);
        }

        let mut min_dist_sq = f32::MAX;
        let mut closest_idx = 0;

        for i in 0..(centerline.count - 1) {
            let a = &centerline.points[i];
            let b = &centerline.points[i + 1];

            let ab_e = b.east_m - a.east_m;
            let ab_n = b.north_m - a.north_m;
            let ap_e = point.east_m - a.east_m;
            let ap_n = point.north_m - a.north_m;

            let ab_len_sq = ab_e * ab_e + ab_n * ab_n;
            if ab_len_sq < 1e-6 {
                continue;
            }

            let t = ((ap_e * ab_e + ap_n * ab_n) / ab_len_sq).clamp(0.0, 1.0);
            let proj_e = a.east_m + t * ab_e;
            let proj_n = a.north_m + t * ab_n;

            let dist_sq = (point.east_m - proj_e).powi(2) + (point.north_m - proj_n).powi(2);
            if dist_sq < min_dist_sq {
                min_dist_sq = dist_sq;
                closest_idx = i;
            }
        }

        (min_dist_sq.sqrt(), closest_idx)
    }
}

// =================================================================================================
// 6. TOPOLOGICAL ROUTING GRAPH ENGINE (ZERO-ALLOCATION A* / DIJKSTRA)
// =================================================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RoutePlan {
    pub lanelet_ids: [u32; 32],
    pub count: usize,
    pub total_cost_m: f32,
}

pub struct TopologyRouter {
    map_nodes: UnsafeCell<[LaneletNode; MAX_LOCAL_LANELETS]>,
    node_count: AtomicU32,
    lock: AtomicBool,
}

// `TopologyRouter` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl TopologyRouter {
    pub const fn new() -> Self {
        Self {
            map_nodes: UnsafeCell::new([LaneletNode::empty(); MAX_LOCAL_LANELETS]),
            node_count: AtomicU32::new(0),
            lock: AtomicBool::new(false),
        }
    }

    pub fn insert_lanelet(&self, node: LaneletNode) -> Result<(), MapError> {
        while self
            .lock
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }

        let count = self.node_count.load(Ordering::Relaxed) as usize;
        if count >= MAX_LOCAL_LANELETS {
            self.lock.store(false, Ordering::Release);
            return Err(MapError::OutOfMemoryBounds);
        }

        // SAFETY: the spin lock acquired above is held (the early return releases it first), so this is the only
        // reference to the node table. `count` was checked against the capacity above.
        let nodes = unsafe { &mut *self.map_nodes.get() };
        nodes[count] = node;
        nodes[count].is_active = true;
        self.node_count.fetch_add(1, Ordering::SeqCst);

        self.lock.store(false, Ordering::Release);
        Ok(())
    }

    /// Finds which lanelet contains the given vehicle ENU coordinate
    pub fn match_current_lanelet(&self, vehicle_pos: &EnuPoint) -> Option<u32> {
        // SAFETY: `TopologyRouter` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the node table is alive.
        // This takes no lock, so it must not run during `insert_lanelet` (the single-context rule).
        let nodes = unsafe { &*self.map_nodes.get() };
        let count = self.node_count.load(Ordering::Acquire) as usize;

        for i in 0..count {
            if nodes[i].is_active && SpatialGeometry::is_point_in_lanelet(vehicle_pos, &nodes[i]) {
                return Some(nodes[i].id);
            }
        }
        None
    }

    /// Zero-allocation bounded pathfinder planning from start lanelet to destination lanelet
    pub fn plan_route(&self, start_id: u32, goal_id: u32) -> Result<RoutePlan, MapError> {
        // SAFETY: `TopologyRouter` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the node table is alive.
        // This takes no lock, so it must not run during `insert_lanelet`.
        let nodes = unsafe { &*self.map_nodes.get() };
        let total = self.node_count.load(Ordering::Acquire) as usize;

        if start_id == goal_id {
            let mut plan = RoutePlan {
                lanelet_ids: [0; 32],
                count: 1,
                total_cost_m: 0.0,
            };
            plan.lanelet_ids[0] = start_id;
            return Ok(plan);
        }

        // Static structures for Dijkstra/A* execution (up to MAX_LOCAL_LANELETS)
        let mut dist = [f32::MAX; MAX_LOCAL_LANELETS];
        let mut parent = [usize::MAX; MAX_LOCAL_LANELETS];
        let mut visited = [false; MAX_LOCAL_LANELETS];

        let mut start_idx = usize::MAX;
        let mut goal_idx = usize::MAX;

        for i in 0..total {
            if nodes[i].is_active {
                if nodes[i].id == start_id {
                    start_idx = i;
                }
                if nodes[i].id == goal_id {
                    goal_idx = i;
                }
            }
        }

        if start_idx == usize::MAX || goal_idx == usize::MAX {
            return Err(MapError::LaneletNotFound);
        }

        dist[start_idx] = 0.0;

        for _ in 0..total {
            // Find unvisited node with smallest distance
            let mut u = usize::MAX;
            let mut min_d = f32::MAX;
            for i in 0..total {
                if !visited[i] && dist[i] < min_d {
                    min_d = dist[i];
                    u = i;
                }
            }

            if u == usize::MAX || u == goal_idx {
                break;
            }

            visited[u] = true;
            let u_node = &nodes[u];

            // Evaluate outgoing successors
            for &succ_id in u_node.successor_ids.iter() {
                if succ_id == 0 {
                    continue;
                }

                for v in 0..total {
                    if nodes[v].is_active && nodes[v].id == succ_id && !visited[v] {
                        let cost = dist[u] + u_node.length_m;
                        if cost < dist[v] {
                            dist[v] = cost;
                            parent[v] = u;
                        }
                    }
                }
            }
        }

        if dist[goal_idx] == f32::MAX {
            return Err(MapError::RouteUnreachable);
        }

        // Reconstruct path backward
        let mut temp_path = [0u32; 32];
        let mut curr = goal_idx;
        let mut hops = 0;

        while curr != usize::MAX && hops < 32 {
            temp_path[hops] = nodes[curr].id;
            hops += 1;
            curr = parent[curr];
        }

        let mut plan = RoutePlan {
            lanelet_ids: [0; 32],
            count: hops,
            total_cost_m: dist[goal_idx],
        };

        // Reverse to forward order
        for i in 0..hops {
            plan.lanelet_ids[i] = temp_path[hops - 1 - i];
        }

        Ok(plan)
    }
}

impl Default for TopologyRouter {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 7. GNSS-RTK / IMU LOCALIZATION FUSION ENGINE
// =================================================================================================

#[repr(C)]
#[derive(Clone, Copy)]
pub struct RtkNavPacket {
    pub position: Wgs84Point,
    pub fix_status: RtkFixStatus,
    pub h_accuracy_mm: u32,
    pub v_accuracy_mm: u32,
    pub heading_deg_e5: u32, // Heading in 10^-5 degrees
    pub speed_mps: f32,
    pub timestamp_us: u64,
}

pub struct LocalizationEstimator {
    origin: UnsafeCell<Wgs84Point>,
    has_origin: AtomicBool,
    last_position_enu: UnsafeCell<EnuPoint>,
    last_heading_rad: UnsafeCell<f32>,
    last_fix_status: AtomicU8,
    last_update_us: AtomicU64,
}

// `LocalizationEstimator` is deliberately NOT `Sync`: it mutates interior state through `&self` with no lock, so
// sharing it between threads or with an interrupt handler would be a data race. A single
// execution context owns it (or the integrator wraps it in `SingleContext`).

impl LocalizationEstimator {
    pub const fn new() -> Self {
        Self {
            origin: UnsafeCell::new(Wgs84Point::new(0, 0, 0)),
            has_origin: AtomicBool::new(false),
            last_position_enu: UnsafeCell::new(EnuPoint::zero()),
            last_heading_rad: UnsafeCell::new(0.0),
            last_fix_status: AtomicU8::new(RtkFixStatus::NoFix as u8),
            last_update_us: AtomicU64::new(0),
        }
    }

    pub fn set_origin(&self, origin: Wgs84Point) {
        // SAFETY: `LocalizationEstimator` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the origin is alive.
        // This writes a `Copy` value and keeps no reference.
        unsafe {
            *self.origin.get() = origin;
        }
        self.has_origin.store(true, Ordering::SeqCst);
    }

    /// Ingests RTK packet, translates geodetics to local ENU, and updates vehicle pose
    pub fn update_rtk_measurement(&self, rtk: &RtkNavPacket) -> Result<EnuPoint, MapError> {
        if !self.has_origin.load(Ordering::Acquire) {
            // Self-anchor on first valid fix
            self.set_origin(rtk.position);
        }

        // SAFETY: `LocalizationEstimator` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the origin is alive.
        // It is only read here.
        let origin = unsafe { &*self.origin.get() };
        let enu = GeodeticTransform::wgs84_to_enu(origin, &rtk.position);

        // SAFETY: `LocalizationEstimator` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the pose cells is alive.
        // These are separate cells, each written once by value.
        unsafe {
            *self.last_position_enu.get() = enu;
            *self.last_heading_rad.get() =
                (rtk.heading_deg_e5 as f32 * 1e-5) * (core::f32::consts::PI / 180.0);
        }

        self.last_fix_status
            .store(rtk.fix_status as u8, Ordering::Release);
        self.last_update_us
            .store(rtk.timestamp_us, Ordering::Release);

        Ok(enu)
    }

    pub fn get_current_pose(&self) -> (EnuPoint, f32, RtkFixStatus) {
        // SAFETY: `LocalizationEstimator` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the position is alive.
        // This reads a `Copy` value.
        let pos = unsafe { *self.last_position_enu.get() };
        // SAFETY: `LocalizationEstimator` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the heading is alive.
        // This reads a `Copy` value.
        let heading = unsafe { *self.last_heading_rad.get() };
        let fix = match self.last_fix_status.load(Ordering::Acquire) {
            1 => RtkFixStatus::AutonomousGnss,
            2 => RtkFixStatus::DifferentialGnss,
            3 => RtkFixStatus::RtkFloat,
            4 => RtkFixStatus::RtkFixed,
            _ => RtkFixStatus::NoFix,
        };
        (pos, heading, fix)
    }
}

impl Default for LocalizationEstimator {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 8. UNIFIED HIGH-DEFINITION MAP & LOCALIZATION CONTROLLER
// =================================================================================================

pub struct HdMapLocalizationSubsystem {
    pub router: TopologyRouter,
    pub locator: LocalizationEstimator,
    current_lanelet_id: AtomicU32,
    active_speed_limit: AtomicU8,
}

impl HdMapLocalizationSubsystem {
    pub const fn new() -> Self {
        Self {
            router: TopologyRouter::new(),
            locator: LocalizationEstimator::new(),
            current_lanelet_id: AtomicU32::new(0),
            active_speed_limit: AtomicU8::new(50),
        }
    }

    /// Primary 50 Hz Navigation Loop: Localizes vehicle, matches lanelet, and extracts speed limits
    pub fn step_navigation_cycle(&self, rtk: &RtkNavPacket) -> Result<(u32, f32, u8), MapError> {
        let vehicle_enu = self.locator.update_rtk_measurement(rtk)?;

        // Map matching
        let matched_id = match self.router.match_current_lanelet(&vehicle_enu) {
            Some(id) => {
                self.current_lanelet_id.store(id, Ordering::SeqCst);
                id
            }
            None => {
                return Err(MapError::VehicleOffRoad);
            }
        };

        // Extract lateral deviation from centerline and speed regulation
        // SAFETY: `TopologyRouter` is not `Sync` (see the note on the type): one execution context owns it, so no other reference to the node table (`self.router` belongs to this struct) is alive.
        // It is only read here.
        let nodes = unsafe { &*self.router.map_nodes.get() };
        let total = self.router.node_count.load(Ordering::Acquire) as usize;

        let mut lateral_offset = 0.0f32;
        let mut speed_limit = 50u8;

        for i in 0..total {
            if nodes[i].is_active && nodes[i].id == matched_id {
                let (dist, _) =
                    SpatialGeometry::distance_to_centerline(&vehicle_enu, &nodes[i].centerline);
                lateral_offset = dist;
                speed_limit = nodes[i].speed_limit_kph;
                self.active_speed_limit
                    .store(speed_limit, Ordering::Relaxed);
                break;
            }
        }

        Ok((matched_id, lateral_offset, speed_limit))
    }
}

impl Default for HdMapLocalizationSubsystem {
    fn default() -> Self {
        Self::new()
    }
}

// =================================================================================================
// 9. UNIT TESTS & VERIFICATION SUITE
// =================================================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_geodetic_enu_conversion() {
        // Stuttgart / Bosch Feuerbach reference point
        let origin = Wgs84Point::new(488152000, 91654000, 300_000);
        // Point shifted ~100m North
        let target = Wgs84Point::new(488161000, 91654000, 300_000);

        let enu = GeodeticTransform::wgs84_to_enu(&origin, &target);
        assert!((enu.north_m - 100.0).abs() < 2.0);
        assert!(enu.east_m.abs() < 1.0);
    }

    #[test]
    fn test_point_in_lanelet_containment() {
        let mut lane = LaneletNode::empty();
        lane.id = 101;
        lane.is_active = true;

        // Construct 4x20m rectangular lane segment
        lane.left_bound.count = 2;
        lane.left_bound.points[0] = EnuPoint::new(-2.0, 0.0, 0.0);
        lane.left_bound.points[1] = EnuPoint::new(-2.0, 20.0, 0.0);

        lane.right_bound.count = 2;
        lane.right_bound.points[0] = EnuPoint::new(2.0, 0.0, 0.0);
        lane.right_bound.points[1] = EnuPoint::new(2.0, 20.0, 0.0);

        lane.compute_centerline();

        // Point inside the lane
        let p_inside = EnuPoint::new(0.0, 10.0, 0.0);
        assert!(SpatialGeometry::is_point_in_lanelet(&p_inside, &lane));

        // Point outside the lane
        let p_outside = EnuPoint::new(5.0, 10.0, 0.0);
        assert!(!SpatialGeometry::is_point_in_lanelet(&p_outside, &lane));
    }

    #[test]
    fn test_zero_alloc_route_planning() {
        let router = TopologyRouter::new();

        // Lane 1 -> Lane 2 -> Lane 3
        let mut l1 = LaneletNode::empty();
        l1.id = 1;
        l1.length_m = 50.0;
        l1.successor_ids[0] = 2;
        router.insert_lanelet(l1).unwrap();

        let mut l2 = LaneletNode::empty();
        l2.id = 2;
        l2.length_m = 60.0;
        l2.successor_ids[0] = 3;
        router.insert_lanelet(l2).unwrap();

        let mut l3 = LaneletNode::empty();
        l3.id = 3;
        l3.length_m = 40.0;
        router.insert_lanelet(l3).unwrap();

        let route = router.plan_route(1, 3).unwrap();
        assert_eq!(route.count, 3);
        assert_eq!(route.lanelet_ids[0], 1);
        assert_eq!(route.lanelet_ids[1], 2);
        assert_eq!(route.lanelet_ids[2], 3);
        assert_eq!(route.total_cost_m, 110.0); // Cost to enter destination
    }

    fn straight_lane() -> LaneletNode {
        let mut lane = LaneletNode::empty();
        lane.id = 7;
        lane.is_active = true;
        lane.left_bound.count = 2;
        lane.left_bound.points[0] = EnuPoint::new(-2.0, 0.0, 0.0);
        lane.left_bound.points[1] = EnuPoint::new(-2.0, 20.0, 0.0);
        lane.right_bound.count = 2;
        lane.right_bound.points[0] = EnuPoint::new(2.0, 0.0, 0.0);
        lane.right_bound.points[1] = EnuPoint::new(2.0, 20.0, 0.0);
        lane
    }

    #[test]
    fn lane_containment_covers_the_whole_rectangle_and_nothing_beyond_it() {
        let lane = straight_lane();
        for (east, north) in [
            (0.0, 10.0),
            (-1.9, 0.1),
            (1.9, 19.9),
            (0.0, 0.5),
            (1.0, 15.0),
        ] {
            assert!(
                SpatialGeometry::is_point_in_lanelet(&EnuPoint::new(east, north, 0.0), &lane),
                "({east}, {north}) is inside"
            );
        }
        for (east, north) in [
            (5.0, 10.0),
            (-2.1, 10.0),
            (2.1, 10.0),
            (0.0, -0.1),
            (0.0, 20.1),
            (0.0, 100.0),
        ] {
            assert!(
                !SpatialGeometry::is_point_in_lanelet(&EnuPoint::new(east, north, 0.0), &lane),
                "({east}, {north}) is outside"
            );
        }
    }

    #[test]
    fn lane_containment_follows_a_bend() {
        // An L-shaped lane: north for 10 m, then east for 10 m, 2 m wide.
        let mut lane = LaneletNode::empty();
        lane.is_active = true;
        lane.left_bound.count = 3;
        lane.left_bound.points[0] = EnuPoint::new(-1.0, 0.0, 0.0);
        lane.left_bound.points[1] = EnuPoint::new(-1.0, 11.0, 0.0);
        lane.left_bound.points[2] = EnuPoint::new(10.0, 11.0, 0.0);
        lane.right_bound.count = 3;
        lane.right_bound.points[0] = EnuPoint::new(1.0, 0.0, 0.0);
        lane.right_bound.points[1] = EnuPoint::new(1.0, 9.0, 0.0);
        lane.right_bound.points[2] = EnuPoint::new(10.0, 9.0, 0.0);
        assert!(
            SpatialGeometry::is_point_in_lanelet(&EnuPoint::new(0.0, 5.0, 0.0), &lane),
            "on the straight leg"
        );
        assert!(
            SpatialGeometry::is_point_in_lanelet(&EnuPoint::new(5.0, 10.0, 0.0), &lane),
            "on the eastbound leg"
        );
        assert!(
            !SpatialGeometry::is_point_in_lanelet(&EnuPoint::new(5.0, 5.0, 0.0), &lane),
            "inside the bend's elbow, off the road"
        );
        assert!(
            !SpatialGeometry::is_point_in_lanelet(&EnuPoint::new(5.0, 12.0, 0.0), &lane),
            "beyond the left boundary"
        );
    }

    #[test]
    fn degenerate_lanes_contain_nothing() {
        let origin = EnuPoint::new(0.0, 0.0, 0.0);
        assert!(!SpatialGeometry::is_point_in_lanelet(
            &origin,
            &LaneletNode::empty()
        ));
        let mut one_sided = straight_lane();
        one_sided.right_bound.count = 0;
        assert!(!SpatialGeometry::is_point_in_lanelet(
            &EnuPoint::new(0.0, 10.0, 0.0),
            &one_sided
        ));
        let mut single_points = straight_lane();
        single_points.left_bound.count = 1;
        single_points.right_bound.count = 1;
        assert!(!SpatialGeometry::is_point_in_lanelet(
            &EnuPoint::new(0.0, 10.0, 0.0),
            &single_points
        ));
    }
}
