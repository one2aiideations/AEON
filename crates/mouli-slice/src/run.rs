//! Glue between the slicing logic and the `mouli-llm` engine: profile a domain with the
//! activation observer, and score multiple-choice items on a model.
//!
//! Scoring measures the probability the model gives the correct answer letter among A-D, one
//! number in `0..=1` per item. That is a small, cheap test of a model; it does not stand in for
//! a real evaluation of the domain.

use crate::closure::LetterScorer;
use crate::profile::Profiler;
use crate::{ActivationSink, SliceError};
use mouli_llm::tokenizer::Tokenizer;
use mouli_llm::{Model, Session};

/// Longest prompt (in tokens) read when profiling or scoring.
pub const MAX_PROMPT_TOKENS: usize = 512;

/// One multiple-choice item: a question, four choices and the index (0-3) of the right one.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Item {
    /// The question text.
    pub question: String,
    /// The four choices, in order A to D.
    pub choices: [String; 4],
    /// Index of the correct choice.
    pub answer: usize,
}

impl Item {
    /// The prompt shown to the model, ending where the answer letter should follow.
    #[must_use]
    pub fn prompt(&self) -> String {
        format!(
            "{}\nA) {}\nB) {}\nC) {}\nD) {}\nAnswer:",
            self.question, self.choices[0], self.choices[1], self.choices[2], self.choices[3]
        )
    }
}

/// Parses items from text. Items are separated by blank lines; each has a question line,
/// four lines starting `A)`, `B)`, `C)`, `D)`, and a line `Answer: X`. Lines starting with `#`
/// are ignored.
///
/// # Errors
/// [`SliceError::BadShape`] naming the problem for a malformed item.
pub fn parse_items(text: &str) -> Result<Vec<Item>, SliceError> {
    let mut items = Vec::new();
    let mut block: Vec<&str> = Vec::new();
    let mut flush = |block: &mut Vec<&str>| -> Result<(), SliceError> {
        if block.is_empty() {
            return Ok(());
        }
        let item = parse_block(block)?;
        items.push(item);
        block.clear();
        Ok(())
    };
    for line in text.lines() {
        let line = line.trim_end();
        if line.starts_with('#') {
            continue;
        }
        if line.trim().is_empty() {
            flush(&mut block)?;
        } else {
            block.push(line);
        }
    }
    flush(&mut block)?;
    Ok(items)
}

fn parse_block(block: &[&str]) -> Result<Item, SliceError> {
    if block.len() != 6 {
        return Err(SliceError::BadShape(
            "an item needs a question, four choices (A-D) and an Answer line",
        ));
    }
    let mut choices: [String; 4] = Default::default();
    for (i, letter) in ['A', 'B', 'C', 'D'].iter().enumerate() {
        let prefix = format!("{letter})");
        let text = block[i + 1]
            .strip_prefix(&prefix)
            .ok_or(SliceError::BadShape("choices must start with A) B) C) D)"))?;
        choices[i] = text.trim().to_string();
    }
    let answer = block[5]
        .strip_prefix("Answer:")
        .map(str::trim)
        .and_then(|a| ["A", "B", "C", "D"].iter().position(|l| *l == a))
        .ok_or(SliceError::BadShape(
            "the last line must be Answer: A, B, C or D",
        ))?;
    Ok(Item {
        question: block[0].trim().to_string(),
        choices,
        answer,
    })
}

/// The four letter logits turned into probabilities that add to one (softmax over just the
/// four).
///
/// # Errors
/// [`SliceError::NonFinite`] for a non-finite logit.
pub fn letter_distribution(logits: [f32; 4]) -> Result<[f64; 4], SliceError> {
    if logits.iter().any(|v| !v.is_finite()) {
        return Err(SliceError::NonFinite("letter logit"));
    }
    let max = logits.iter().copied().fold(f32::MIN, f32::max);
    let mut exps = [0.0f64; 4];
    for (e, v) in exps.iter_mut().zip(logits) {
        *e = f64::from(v - max).exp();
    }
    let sum: f64 = exps.iter().sum();
    for e in &mut exps {
        *e /= sum;
    }
    Ok(exps)
}

/// Probability of choice `answer` among the four letter logits (softmax over just the four).
///
/// # Errors
/// [`SliceError::NonFinite`] for a non-finite logit, [`SliceError::BadParameter`] for `answer`
/// above 3.
pub fn letter_probability(logits: [f32; 4], answer: usize) -> Result<f64, SliceError> {
    if answer > 3 {
        return Err(SliceError::BadParameter("answer"));
    }
    Ok(letter_distribution(logits)?[answer])
}

fn engine_error(e: &dyn std::fmt::Debug) -> SliceError {
    // The engine's errors carry no text we can keep in a `&'static str`; the caller sees the
    // category and the debug text is printed by the tool.
    let _ = e;
    SliceError::BadParameter(
        "the engine refused the input (see the model, prompt length or context)",
    )
}

fn prompt_tokens(tokenizer: &Tokenizer, text: &str) -> Result<Vec<u32>, SliceError> {
    let ids = tokenizer.encode_default(text);
    if ids.is_empty() {
        return Err(SliceError::EmptyInput("prompt tokens"));
    }
    if ids.len() > MAX_PROMPT_TOKENS {
        return Err(SliceError::BadParameter(
            "prompt longer than MAX_PROMPT_TOKENS",
        ));
    }
    Ok(ids)
}

/// Runs each prompt through the model and records every layer's feed-forward activations.
///
/// Returns the profiler and the number of tokens read. Profiling shows what the model used on
/// these prompts only; it does not show what the domain needs.
///
/// # Errors
/// A [`SliceError`] for an empty prompt list, a prompt that is empty or too long, or an engine
/// refusal.
pub fn profile_prompts(
    model: &Model,
    tokenizer: &Tokenizer,
    prompts: &[String],
) -> Result<(Profiler, usize), SliceError> {
    if prompts.is_empty() {
        return Err(SliceError::EmptyInput("profiling prompts"));
    }
    let config = model.config();
    let mut profiler = Profiler::new(config.n_layer, config.n_ff)?;
    let mut tokens_read = 0;
    for prompt in prompts {
        let ids = prompt_tokens(tokenizer, prompt)?;
        let mut session = Session::new(model, ids.len().max(1)).map_err(|e| engine_error(&e))?;
        let sink = &mut profiler;
        session
            .prefill_observed(&ids, &mut |layer, a| sink.observe(layer, a).is_ok())
            .map_err(|e| engine_error(&e))?;
        tokens_read += ids.len();
    }
    Ok((profiler, tokens_read))
}

/// The token ids of " A", " B", " C", " D" (the last token of each), which must be four
/// different tokens.
///
/// # Errors
/// [`SliceError::BadShape`] when the tokenizer cannot give four distinct letter tokens.
pub fn letter_tokens(tokenizer: &Tokenizer) -> Result<[u32; 4], SliceError> {
    let mut ids = [0u32; 4];
    for (i, letter) in [" A", " B", " C", " D"].iter().enumerate() {
        let encoded = tokenizer.encode(letter, false, false);
        ids[i] = *encoded
            .last()
            .ok_or(SliceError::BadShape("a letter encoded to no tokens"))?;
    }
    for i in 0..4 {
        for j in i + 1..4 {
            if ids[i] == ids[j] {
                return Err(SliceError::BadShape("two answer letters share one token"));
            }
        }
    }
    Ok(ids)
}

/// Scores every item: the probability the model gives the correct letter, in `0..=1`.
///
/// # Errors
/// A [`SliceError`] for no items, a prompt that is too long, or an engine refusal.
pub fn score_items(
    model: &Model,
    tokenizer: &Tokenizer,
    items: &[Item],
) -> Result<Vec<f64>, SliceError> {
    if items.is_empty() {
        return Err(SliceError::EmptyInput("scoring items"));
    }
    let letters = letter_tokens(tokenizer)?;
    let mut scores = Vec::with_capacity(items.len());
    for item in items {
        let ids = prompt_tokens(tokenizer, &item.prompt())?;
        let mut session = Session::new(model, ids.len().max(1)).map_err(|e| engine_error(&e))?;
        let logits = session.prefill(&ids).map_err(|e| engine_error(&e))?;
        let pick = |i: usize| logits.get(letters[i] as usize).copied();
        let four = [pick(0), pick(1), pick(2), pick(3)];
        let [Some(a), Some(b), Some(c), Some(d)] = four else {
            return Err(SliceError::BadShape("letter token outside the vocabulary"));
        };
        scores.push(letter_probability([a, b, c, d], item.answer)?);
    }
    Ok(scores)
}

/// Scores prompts with a real model: the probability of each answer letter after the prompt.
pub struct ModelScorer<'a> {
    model: &'a Model,
    tokenizer: &'a Tokenizer,
    letters: [u32; 4],
}

impl<'a> ModelScorer<'a> {
    /// A scorer for `model`; fails if the tokenizer has no four distinct answer-letter tokens.
    ///
    /// # Errors
    /// [`SliceError::BadShape`] from [`letter_tokens`].
    pub fn new(model: &'a Model, tokenizer: &'a Tokenizer) -> Result<Self, SliceError> {
        Ok(Self {
            model,
            tokenizer,
            letters: letter_tokens(tokenizer)?,
        })
    }
}

impl LetterScorer for ModelScorer<'_> {
    fn probs(&mut self, prompt: &str) -> Result<[f64; 4], SliceError> {
        let ids = prompt_tokens(self.tokenizer, prompt)?;
        let mut session = Session::new(self.model, ids.len()).map_err(|e| engine_error(&e))?;
        let logits = session.prefill(&ids).map_err(|e| engine_error(&e))?;
        let pick = |i: usize| logits.get(self.letters[i] as usize).copied();
        let [Some(a), Some(b), Some(c), Some(d)] = [pick(0), pick(1), pick(2), pick(3)] else {
            return Err(SliceError::BadShape("letter token outside the vocabulary"));
        };
        letter_distribution([a, b, c, d])
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const TWO: &str = "# a comment\nWhat is 2+2?\nA) 3\nB) 4\nC) 5\nD) 6\nAnswer: B\n\nCapital of France?\nA) Rome\nB) Oslo\nC) Paris\nD) Bern\nAnswer: C\n";

    #[test]
    fn items_parse() {
        let items = parse_items(TWO).unwrap();
        assert_eq!(items.len(), 2);
        assert_eq!(items[0].question, "What is 2+2?");
        assert_eq!(items[0].choices[1], "4");
        assert_eq!(items[0].answer, 1);
        assert_eq!(items[1].answer, 2);
        assert!(items[0].prompt().ends_with("D) 6\nAnswer:"));
    }

    #[test]
    fn malformed_items_are_refused() {
        assert!(parse_items("Q?\nA) 1\nB) 2\nC) 3\nAnswer: A\n").is_err());
        assert!(parse_items("Q?\nA) 1\nB) 2\nC) 3\nX) 4\nAnswer: A\n").is_err());
        assert!(parse_items("Q?\nA) 1\nB) 2\nC) 3\nD) 4\nAnswer: E\n").is_err());
        assert!(parse_items("Q?\nA) 1\nB) 2\nC) 3\nD) 4\nanswer A\n").is_err());
        assert!(parse_items("").unwrap().is_empty());
    }

    #[test]
    fn letter_probability_is_a_softmax_over_four() {
        let p = letter_probability([0.0, 0.0, 0.0, 0.0], 2).unwrap();
        assert!((p - 0.25).abs() < 1e-12);
        let sure = letter_probability([20.0, 0.0, 0.0, 0.0], 0).unwrap();
        assert!(sure > 0.999);
        let wrong = letter_probability([20.0, 0.0, 0.0, 0.0], 1).unwrap();
        assert!(wrong < 0.001);
        // The four probabilities add to one.
        let total: f64 = (0..4)
            .map(|i| letter_probability([1.0, 2.0, 3.0, 4.0], i).unwrap())
            .sum();
        assert!((total - 1.0).abs() < 1e-12);
    }

    #[test]
    fn the_distribution_adds_to_one_and_orders_like_the_logits() {
        let d = letter_distribution([1.0, 3.0, 2.0, 0.0]).unwrap();
        assert!((d.iter().sum::<f64>() - 1.0).abs() < 1e-12);
        assert!(d[1] > d[2] && d[2] > d[0] && d[0] > d[3]);
        assert!(letter_distribution([0.0, f32::INFINITY, 0.0, 0.0]).is_err());
    }

    #[test]
    fn letter_probability_rejects_bad_input() {
        assert!(letter_probability([f32::NAN, 0.0, 0.0, 0.0], 0).is_err());
        assert!(letter_probability([0.0; 4], 4).is_err());
        // Large logits do not overflow.
        assert!(letter_probability([1e30, 0.0, 0.0, 0.0], 0).unwrap() > 0.99);
    }
}
