//! AEON heterogeneous compute placement.
//!
//! A laptop has a few very different compute units: CPU cores, an
//! integrated GPU sharing RAM, maybe a discrete GPU with its own VRAM, maybe
//! an NPU. Work items (inference layers, agent jobs, media pipelines) prefer
//! some kinds of unit, need memory, and may or may not be movable.
//!
//! [`Topology::place`] picks the best unit for a task; [`Topology::rebalance`]
//! moves migratable tasks off units that run too hot or too full.
#![forbid(unsafe_code)]

use core::fmt;
use std::collections::BTreeMap;

/// Kind of compute unit.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum UnitKind {
    /// CPU cores.
    Cpu,
    /// Integrated GPU (shared memory).
    IGpu,
    /// Discrete GPU (own memory).
    DGpu,
    /// Neural accelerator.
    Npu,
}

/// What a task mostly does.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Work {
    /// Dense tensor math (matmul, attention).
    Tensor,
    /// Branchy scalar code.
    Scalar,
    /// Memory-bound streaming.
    Memory,
}

impl Work {
    /// Relative speed of `kind` for this work (higher is better).
    #[must_use]
    pub fn affinity(self, kind: UnitKind) -> f32 {
        match (self, kind) {
            (Self::Tensor, UnitKind::DGpu) => 10.0,
            (Self::Tensor, UnitKind::Npu) => 6.0,
            (Self::Tensor, UnitKind::IGpu) => 4.0,
            (Self::Tensor, UnitKind::Cpu) => 1.0,
            (Self::Scalar, UnitKind::Cpu) => 10.0,
            (Self::Scalar, UnitKind::IGpu | UnitKind::DGpu) => 0.5,
            (Self::Scalar, UnitKind::Npu) => 0.1,
            (Self::Memory, UnitKind::DGpu) => 6.0,
            (Self::Memory, UnitKind::IGpu | UnitKind::Cpu) => 3.0,
            (Self::Memory, UnitKind::Npu) => 1.0,
        }
    }
}

/// A compute unit and its live state.
#[derive(Debug, Clone, PartialEq)]
pub struct Unit {
    /// Name ("cpu0", "igpu", "dgpu").
    pub name: String,
    /// Kind.
    pub kind: UnitKind,
    /// Memory usable by tasks on this unit, bytes.
    pub mem_bytes: u64,
    /// Temperature in Celsius.
    pub temp_c: f32,
    /// Temperature at which work should move away.
    pub temp_limit_c: f32,
    /// Unit is powered and usable (e.g. dGPU may be off on battery).
    pub online: bool,
}

/// A schedulable task.
#[derive(Debug, Clone, PartialEq)]
pub struct Task {
    /// Id.
    pub id: u64,
    /// Work profile.
    pub work: Work,
    /// Memory needed, bytes.
    pub mem_bytes: u64,
    /// Share of a unit's throughput it uses (0..=1).
    pub load: f32,
    /// May be moved after placement.
    pub migratable: bool,
}

/// Placement errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedError {
    /// No online unit has room.
    NoCapacity,
    /// Unknown unit or task.
    Unknown,
    /// Duplicate id or name.
    Duplicate,
}

impl fmt::Display for SchedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NoCapacity => "no unit has capacity",
            Self::Unknown => "unknown unit or task",
            Self::Duplicate => "duplicate id",
        })
    }
}

impl std::error::Error for SchedError {}

/// A move made by [`Topology::rebalance`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Migration {
    /// Task moved.
    pub task: u64,
    /// From unit.
    pub from: String,
    /// To unit.
    pub to: String,
}

/// Units, tasks and the current placement.
#[derive(Debug, Clone, Default)]
pub struct Topology {
    units: Vec<Unit>,
    tasks: BTreeMap<u64, (Task, usize)>,
}

impl Topology {
    /// Empty topology.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a unit.
    pub fn add_unit(&mut self, u: Unit) -> Result<(), SchedError> {
        if self.units.iter().any(|x| x.name == u.name) {
            return Err(SchedError::Duplicate);
        }
        self.units.push(u);
        Ok(())
    }

    fn unit_index(&self, name: &str) -> Result<usize, SchedError> {
        self.units
            .iter()
            .position(|u| u.name == name)
            .ok_or(SchedError::Unknown)
    }

    /// Memory and load in use on unit `i`.
    fn usage(&self, i: usize) -> (u64, f32) {
        self.tasks
            .values()
            .filter(|(_, u)| *u == i)
            .fold((0, 0.0), |(m, l), (t, _)| (m + t.mem_bytes, l + t.load))
    }

    fn fits(&self, i: usize, t: &Task) -> bool {
        let u = &self.units[i];
        let (mem, load) = self.usage(i);
        u.online
            && u.temp_c < u.temp_limit_c
            && mem.saturating_add(t.mem_bytes) <= u.mem_bytes
            && load + t.load <= 1.0 + 1e-6
    }

    /// Score of running `t` on unit `i`: affinity, discounted by load and heat.
    fn score(&self, i: usize, t: &Task) -> f32 {
        let u = &self.units[i];
        let (_, load) = self.usage(i);
        let heat = (u.temp_c / u.temp_limit_c).clamp(0.0, 1.0);
        t.work.affinity(u.kind) * (1.0 - 0.5 * load) * (1.0 - 0.5 * heat)
    }

    fn best_unit(&self, t: &Task, exclude: Option<usize>) -> Option<usize> {
        (0..self.units.len())
            .filter(|&i| Some(i) != exclude && self.fits(i, t))
            .max_by(|&a, &b| self.score(a, t).total_cmp(&self.score(b, t)))
    }

    /// Place a new task on the best fitting unit; returns the unit name.
    pub fn place(&mut self, t: Task) -> Result<&str, SchedError> {
        if self.tasks.contains_key(&t.id) {
            return Err(SchedError::Duplicate);
        }
        let i = self.best_unit(&t, None).ok_or(SchedError::NoCapacity)?;
        self.tasks.insert(t.id, (t, i));
        Ok(&self.units[i].name)
    }

    /// Remove a finished task.
    pub fn finish(&mut self, id: u64) -> Result<Task, SchedError> {
        self.tasks
            .remove(&id)
            .map(|(t, _)| t)
            .ok_or(SchedError::Unknown)
    }

    /// Update a unit's sensor readings.
    pub fn update_unit(&mut self, name: &str, temp_c: f32, online: bool) -> Result<(), SchedError> {
        let i = self.unit_index(name)?;
        self.units[i].temp_c = temp_c;
        self.units[i].online = online;
        Ok(())
    }

    /// Move migratable tasks off units that are offline, too hot, or over
    /// capacity, to the best other unit that fits. Pinned tasks stay.
    pub fn rebalance(&mut self) -> Vec<Migration> {
        let mut moves = Vec::new();
        let ids: Vec<u64> = self.tasks.keys().copied().collect();
        for id in ids {
            let (task, from) = match self.tasks.get(&id) {
                Some((t, u)) => (t.clone(), *u),
                None => continue,
            };
            let u = &self.units[from];
            let (mem, load) = self.usage(from);
            let stressed =
                !u.online || u.temp_c >= u.temp_limit_c || mem > u.mem_bytes || load > 1.0 + 1e-6;
            if !stressed || !task.migratable {
                continue;
            }
            // Evaluate fit without this task counted on its current unit.
            self.tasks.remove(&id);
            match self.best_unit(&task, Some(from)) {
                Some(to) => {
                    moves.push(Migration {
                        task: id,
                        from: self.units[from].name.clone(),
                        to: self.units[to].name.clone(),
                    });
                    self.tasks.insert(id, (task, to));
                }
                None => {
                    self.tasks.insert(id, (task, from));
                }
            }
        }
        moves
    }

    /// Unit currently running `id`.
    #[must_use]
    pub fn unit_of(&self, id: u64) -> Option<&str> {
        self.tasks
            .get(&id)
            .map(|(_, u)| self.units[*u].name.as_str())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn unit(name: &str, kind: UnitKind, mem: u64) -> Unit {
        Unit {
            name: name.into(),
            kind,
            mem_bytes: mem,
            temp_c: 50.0,
            temp_limit_c: 90.0,
            online: true,
        }
    }

    fn task(id: u64, work: Work, mem: u64, migratable: bool) -> Task {
        Task {
            id,
            work,
            mem_bytes: mem,
            load: 0.4,
            migratable,
        }
    }

    fn laptop() -> Topology {
        let mut t = Topology::new();
        t.add_unit(unit("cpu", UnitKind::Cpu, 16 << 30)).unwrap();
        t.add_unit(unit("igpu", UnitKind::IGpu, 4 << 30)).unwrap();
        t.add_unit(unit("dgpu", UnitKind::DGpu, 8 << 30)).unwrap();
        t
    }

    #[test]
    fn placement_follows_affinity_and_memory() {
        let mut t = laptop();
        assert_eq!(
            t.place(task(1, Work::Tensor, 6 << 30, true)).unwrap(),
            "dgpu"
        );
        assert_eq!(
            t.place(task(2, Work::Scalar, 1 << 30, true)).unwrap(),
            "cpu"
        );
        // Too big for any GPU now: dGPU has 2 GiB left, iGPU 4 GiB.
        assert_eq!(
            t.place(task(3, Work::Tensor, 5 << 30, true)).unwrap(),
            "cpu"
        );
        assert_eq!(
            t.place(task(3, Work::Tensor, 1, true)),
            Err(SchedError::Duplicate)
        );
        assert_eq!(
            t.place(task(9, Work::Tensor, 64 << 30, true)),
            Err(SchedError::NoCapacity)
        );
    }

    #[test]
    fn hot_or_offline_units_shed_migratable_work() {
        let mut t = laptop();
        t.place(task(1, Work::Tensor, 1 << 30, true)).unwrap();
        t.place(task(2, Work::Tensor, 1 << 30, false)).unwrap();
        assert_eq!(t.unit_of(1), Some("dgpu"));
        t.update_unit("dgpu", 95.0, true).unwrap();
        let moves = t.rebalance();
        assert_eq!(
            moves,
            vec![Migration {
                task: 1,
                from: "dgpu".into(),
                to: "igpu".into()
            }]
        );
        assert_eq!(t.unit_of(2), Some("dgpu"));
        t.update_unit("igpu", 50.0, false).unwrap();
        let moves = t.rebalance();
        assert_eq!(moves[0].to, "cpu");
        assert!(t.finish(1).is_ok());
        assert!(t.finish(1).is_err());
    }
}
