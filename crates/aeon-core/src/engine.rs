//! Cognitive engines: the only place reasoning backends plug in.
//!
//! The core never names a model family. An engine receives a task and
//! context and returns an answer with a confidence and a cost, so the
//! router can cascade from cheap to expensive engines.

use core::fmt;

/// What the caller wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Task {
    /// Free-form answer.
    Answer,
    /// Short summary.
    Summarise,
    /// Step-by-step plan.
    Plan,
    /// Pick one of the given options.
    Choose,
    /// Vector embedding.
    Embed,
}

/// A request to an engine.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Task kind.
    pub task: Task,
    /// Main input.
    pub input: String,
    /// Supporting context (retrieved memories, files).
    pub context: Vec<String>,
    /// Upper bound on output tokens.
    pub max_tokens: u32,
}

/// An engine's answer.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// Output text (or a serialised vector for `Embed`).
    pub output: String,
    /// Self-reported probability the output is right (0..=1).
    pub confidence: f32,
    /// Cost incurred (same unit as `Engine::cost`).
    pub cost: f32,
}

/// Engine failure.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EngineError(pub String);

impl fmt::Display for EngineError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for EngineError {}

/// A reasoning backend (local model, rules, remote service).
pub trait CognitiveEngine {
    /// Short name.
    fn name(&self) -> &str;
    /// Whether the engine handles `task`.
    fn supports(&self, task: Task) -> bool;
    /// Run a request.
    fn execute(&mut self, req: &Request) -> Result<Response, EngineError>;
}

/// Cheapest-first cascade: try engines in order and stop at the first
/// answer whose confidence reaches `threshold`. Returns the last answer if
/// none reach it, with the total cost.
pub fn cascade(
    engines: &mut [&mut dyn CognitiveEngine],
    req: &Request,
    threshold: f32,
) -> Result<(String, Response), EngineError> {
    let mut last: Option<(String, Response)> = None;
    let mut spent = 0.0f32;
    for e in engines.iter_mut().filter(|e| e.supports(req.task)) {
        let mut r = e.execute(req)?;
        spent += r.cost;
        r.cost = spent;
        let done = r.confidence >= threshold;
        last = Some((e.name().to_owned(), r));
        if done {
            break;
        }
    }
    last.ok_or_else(|| EngineError("no engine supports this task".into()))
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixed {
        name: &'static str,
        conf: f32,
        cost: f32,
        calls: u32,
    }

    impl CognitiveEngine for Fixed {
        fn name(&self) -> &str {
            self.name
        }
        fn supports(&self, task: Task) -> bool {
            task != Task::Embed
        }
        fn execute(&mut self, req: &Request) -> Result<Response, EngineError> {
            self.calls += 1;
            Ok(Response {
                output: format!("{}:{}", self.name, req.input),
                confidence: self.conf,
                cost: self.cost,
            })
        }
    }

    fn req(task: Task) -> Request {
        Request {
            task,
            input: "q".into(),
            context: vec![],
            max_tokens: 64,
        }
    }

    #[test]
    fn stops_at_confident_engine() {
        let mut small = Fixed {
            name: "small",
            conf: 0.4,
            cost: 1.0,
            calls: 0,
        };
        let mut big = Fixed {
            name: "big",
            conf: 0.9,
            cost: 10.0,
            calls: 0,
        };
        let mut huge = Fixed {
            name: "huge",
            conf: 0.99,
            cost: 100.0,
            calls: 0,
        };
        let (who, r) = cascade(
            &mut [&mut small, &mut big, &mut huge],
            &req(Task::Answer),
            0.8,
        )
        .unwrap();
        assert_eq!(who, "big");
        assert!((r.cost - 11.0).abs() < 1e-6);
        assert_eq!(huge.calls, 0);
        let mut e = Fixed {
            name: "x",
            conf: 1.0,
            cost: 0.0,
            calls: 0,
        };
        assert!(cascade(&mut [&mut e], &req(Task::Embed), 0.5).is_err());
    }
}
