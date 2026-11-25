//! Izhikevich spiking population with dopamine / norepinephrine traces.

use crate::{finite, CogError};

#[derive(Debug, Clone, Copy)]
struct Neuron {
    v: f32,
    u: f32,
    spikes: u32,
}

/// Regular-spiking parameters (a, b, c, d).
const A: f32 = 0.02;
const B: f32 = 0.2;
const C: f32 = -65.0;
const D: f32 = 8.0;
const DT: f32 = 0.01;

/// A population of identical regular-spiking neurons.
///
/// Neuron `i` gets a small bias `(i % fan_in) * 0.01` on top of the shared
/// input, so the population spreads out in time.
#[derive(Debug, Clone)]
pub struct Snn {
    nodes: Vec<Neuron>,
    fan_in: usize,
    /// Fraction of neurons that spiked on the last step.
    pub branching_ratio: f32,
    /// Reward trace.
    pub dopamine: f32,
    /// Arousal trace (input magnitude).
    pub norepinephrine: f32,
}

impl Snn {
    /// Population of `n` neurons with bias period `fan_in`.
    pub fn new(n: usize, fan_in: usize) -> Result<Self, CogError> {
        if n == 0 || fan_in == 0 {
            return Err(CogError::BadValue);
        }
        Ok(Self {
            nodes: vec![
                Neuron {
                    v: C,
                    u: B * C,
                    spikes: 0
                };
                n
            ],
            fan_in,
            branching_ratio: 0.0,
            dopamine: 0.0,
            norepinephrine: 0.0,
        })
    }

    /// Advance one step; returns how many neurons spiked.
    pub fn step(&mut self, input: f32, reward: f32) -> Result<usize, CogError> {
        let input = finite(input)?;
        let reward = finite(reward)?;
        self.dopamine += 0.05 * (reward - self.dopamine);
        self.norepinephrine += 0.05 * (input.abs() - self.norepinephrine);
        let mut fired = 0;
        for (i, n) in self.nodes.iter_mut().enumerate() {
            let current = input + (i % self.fan_in) as f32 * 0.01;
            let dv = 0.04 * n.v * n.v + 5.0 * n.v + 140.0 - n.u + current;
            let du = A * (B * n.v - n.u);
            n.v += dv * DT;
            n.u += du * DT;
            if n.v >= 30.0 {
                n.v = C;
                n.u += D;
                n.spikes += 1;
                fired += 1;
            }
        }
        self.branching_ratio = fired as f32 / self.nodes.len() as f32;
        Ok(fired)
    }

    /// Total spikes per neuron so far.
    pub fn spike_counts(&self) -> impl Iterator<Item = u32> + '_ {
        self.nodes.iter().map(|n| n.spikes)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strong_input_spikes_weak_does_not() {
        let mut quiet = Snn::new(16, 4).unwrap();
        for _ in 0..200 {
            quiet.step(0.0, 0.0).unwrap();
        }
        assert_eq!(quiet.spike_counts().sum::<u32>(), 0);

        let mut loud = Snn::new(16, 4).unwrap();
        let mut total = 0;
        for _ in 0..5000 {
            total += loud.step(20.0, 1.0).unwrap();
        }
        assert!(total > 0);
        assert!(loud.dopamine > 0.9);
        assert!(loud.step(f32::NAN, 0.0).is_err());
    }
}
