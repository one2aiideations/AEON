//! Spiking network with delayed synapses and a time-ordered event queue.

use crate::neuron::{Neuron, Params};
use std::cmp::Reverse;
use std::collections::BinaryHeap;

/// Neuron index.
pub type NeuronId = usize;

/// A spike delivered to a neuron.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Event {
    /// Delivery time.
    pub time: u64,
    /// Receiving neuron.
    pub target: NeuronId,
    /// Input amount.
    pub input: f32,
}

/// A spike emitted by a neuron.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Spike {
    /// Emitting neuron.
    pub source: NeuronId,
    /// Firing time.
    pub time: u64,
    /// Membrane value at firing.
    pub membrane: f32,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct Synapse {
    target: NeuronId,
    weight: f32,
    delay: u64,
}

/// Energy accounting: one unit per spike plus `synaptic_cost` per delivery.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Energy {
    /// Spikes emitted.
    pub spikes: u64,
    /// Synaptic events delivered.
    pub deliveries: u64,
    /// Weighted total.
    pub total: f64,
}

/// Network errors.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum NetError {
    /// Neuron id out of range.
    BadNeuron,
    /// Parameters invalid or non-finite value.
    BadValue,
}

/// Ordering key for the heap: (time, sequence) so equal times stay FIFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
struct Key(u64, u64);

/// Event-driven spiking network.
#[derive(Debug, Clone)]
pub struct Network {
    neurons: Vec<Neuron>,
    out: Vec<Vec<Synapse>>,
    queue: BinaryHeap<Reverse<(Key, usize)>>,
    pending: Vec<Option<Event>>,
    seq: u64,
    now: u64,
    /// Energy used so far.
    pub energy: Energy,
    /// Cost of one synaptic delivery relative to one spike.
    pub synaptic_cost: f64,
}

impl Network {
    /// Network of `n` identical neurons.
    pub fn new(n: usize, params: Params) -> Result<Self, NetError> {
        if !params.is_valid() {
            return Err(NetError::BadValue);
        }
        Ok(Self {
            neurons: (0..n).map(|_| Neuron::new(params)).collect(),
            out: vec![Vec::new(); n],
            queue: BinaryHeap::new(),
            pending: Vec::new(),
            seq: 0,
            now: 0,
            energy: Energy::default(),
            synaptic_cost: 0.1,
        })
    }

    /// Connect `from -> to` with `weight`, delivered after `delay` steps (>= 1).
    pub fn connect(&mut self, from: NeuronId, to: NeuronId, weight: f32, delay: u64) -> Result<(), NetError> {
        if from >= self.neurons.len() || to >= self.neurons.len() {
            return Err(NetError::BadNeuron);
        }
        if !weight.is_finite() {
            return Err(NetError::BadValue);
        }
        self.out[from].push(Synapse {
            target: to,
            weight,
            delay: delay.max(1),
        });
        Ok(())
    }

    /// Queue external input.
    pub fn inject(&mut self, e: Event) -> Result<(), NetError> {
        if e.target >= self.neurons.len() {
            return Err(NetError::BadNeuron);
        }
        if !e.input.is_finite() {
            return Err(NetError::BadValue);
        }
        let slot = self.pending.len();
        self.pending.push(Some(e));
        self.queue.push(Reverse((Key(e.time, self.seq), slot)));
        self.seq += 1;
        Ok(())
    }

    /// Process events up to and including time `until`, or at most
    /// `max_events`. Returns the spikes emitted, in order.
    pub fn run(&mut self, until: u64, max_events: usize) -> Vec<Spike> {
        let mut spikes = Vec::new();
        let mut handled = 0;
        while handled < max_events {
            let Some(&Reverse((Key(t, _), slot))) = self.queue.peek() else {
                break;
            };
            if t > until {
                break;
            }
            self.queue.pop();
            let Some(e) = self.pending[slot].take() else {
                continue;
            };
            handled += 1;
            self.now = t;
            self.energy.deliveries += 1;
            self.energy.total += self.synaptic_cost;
            if let Some(v) = self.neurons[e.target].receive(t, e.input) {
                self.energy.spikes += 1;
                self.energy.total += 1.0;
                spikes.push(Spike {
                    source: e.target,
                    time: t,
                    membrane: v,
                });
                let syns = self.out[e.target].clone();
                for s in syns {
                    // Targets were validated in `connect`.
                    let _ = self.inject(Event {
                        time: t + s.delay,
                        target: s.target,
                        input: s.weight,
                    });
                }
            }
        }
        if self.queue.is_empty() {
            self.pending.clear();
        }
        spikes
    }

    /// Current simulation time.
    #[must_use]
    pub fn now(&self) -> u64 {
        self.now
    }

    /// Neuron state.
    #[must_use]
    pub fn neuron(&self, id: NeuronId) -> Option<&Neuron> {
        self.neurons.get(id)
    }

    /// Events still queued.
    #[must_use]
    pub fn queued(&self) -> usize {
        self.queue.len()
    }

    /// Fraction of neurons that fired at least once.
    #[must_use]
    pub fn activity(&self) -> f32 {
        if self.neurons.is_empty() {
            return 0.0;
        }
        self.neurons.iter().filter(|n| n.spikes > 0).count() as f32 / self.neurons.len() as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn chain(n: usize) -> Network {
        let mut net = Network::new(n, Params::default()).unwrap();
        for i in 0..n - 1 {
            net.connect(i, i + 1, 1.5, 2).unwrap();
        }
        net
    }

    #[test]
    fn spikes_propagate_with_delay() {
        let mut net = chain(4);
        net.inject(Event {
            time: 0,
            target: 0,
            input: 2.0,
        })
        .unwrap();
        let spikes = net.run(100, 1000);
        let times: Vec<(usize, u64)> = spikes.iter().map(|s| (s.source, s.time)).collect();
        assert_eq!(times, vec![(0, 0), (1, 2), (2, 4), (3, 6)]);
        assert_eq!(net.energy.spikes, 4);
        assert_eq!(net.energy.deliveries, 4);
        assert!((net.activity() - 1.0).abs() < 1e-6);
    }

    #[test]
    fn work_scales_with_activity() {
        let mut quiet = chain(100);
        quiet
            .inject(Event {
                time: 0,
                target: 0,
                input: 0.1,
            })
            .unwrap();
        quiet.run(1000, 10_000);
        assert_eq!(quiet.energy.deliveries, 1);
        assert_eq!(quiet.energy.spikes, 0);
    }

    #[test]
    fn until_and_budget() {
        let mut net = chain(4);
        net.inject(Event {
            time: 0,
            target: 0,
            input: 2.0,
        })
        .unwrap();
        assert_eq!(net.run(3, 1000).len(), 2);
        assert_eq!(net.queued(), 1);
        assert_eq!(net.run(100, 1).len(), 1);
        assert!(net.connect(0, 9, 1.0, 1).is_err());
        assert!(net
            .inject(Event {
                time: 0,
                target: 0,
                input: f32::NAN
            })
            .is_err());
    }
}
