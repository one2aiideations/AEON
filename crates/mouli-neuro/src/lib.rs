//! MOULI event-driven spiking runtime.
//!
//! Dense networks touch every weight on every step and want a GPU. An
//! event-driven network only does work when a neuron fires, so cost scales
//! with activity, which suits laptop CPUs and battery power.
//!
//! * [`neuron`]: leaky membrane with an adaptive (homeostatic) threshold.
//! * [`network`]: neurons, delayed synapses, a time-ordered event queue and
//!   an energy meter.
//! * [`plasticity`]: local learning rules (Hebbian, Oja, pair STDP).
//! * [`competition`]: k-winners-take-all selection.
//! * [`eligibility`]: decaying per-synapse traces and reward-modulated updates for delayed reward.
#![forbid(unsafe_code)]

pub mod competition;
pub mod eligibility;
pub mod network;
pub mod neuron;
pub mod plasticity;
