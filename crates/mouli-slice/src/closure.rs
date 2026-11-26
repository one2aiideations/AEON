//! The closure test of the MOULI theory layer, used as a slicing gate.
//!
//! Plain accuracy can stay put while a model gets worse in a way that matters: it starts to lean
//! on shortcuts (an answer position that repeats in the examples, a hint line) instead of the
//! question. The closure test runs the same questions under five conditions: no shortcut, and a
//! position cue and a hint cue that are right 95% of the time and 0% of the time. The robust
//! score S is the worst accuracy over the five, so a model that follows a cue falls to the
//! 0% conditions and S shows it. `shortcut reliance` measures how much more often a model
//! follows a cue when it is present than when it is swapped for a random one.
//!
//! The prompts follow the Python original (`theory_layer.py`): a header, a few solved examples
//! laid out under the cue, then the test question. Randomness is a fixed xorshift, so a seed
//! always gives the same episodes.
//!
//! What this does not show: S is measured on the questions given and on this one multiple-choice
//! format. It says nothing about open-ended answers, other domains, or speed.

use crate::gate::MIN_ITEMS;
use crate::run::Item;
use crate::SliceError;
use mouli_llm::theory::robust::{
    reliance, summarise, Condition, Shortcut, Summary, CLOSURE, FAMILIAR,
};
use std::collections::BTreeMap;

const K: usize = 4;
const LETTERS: [char; K] = ['A', 'B', 'C', 'D'];

/// Gives the probability of each answer letter after a prompt that ends in `Answer:`.
pub trait LetterScorer {
    /// Probabilities for A, B, C, D (not necessarily summing to one).
    ///
    /// # Errors
    /// A [`SliceError`] when the prompt cannot be scored.
    fn probs(&mut self, prompt: &str) -> Result<[f64; K], SliceError>;
}

/// A small deterministic random generator (xorshift64*).
#[derive(Debug, Clone)]
pub struct Rng(u64);

impl Rng {
    /// A generator from a seed (zero is replaced by a fixed non-zero value).
    #[must_use]
    pub fn new(seed: u64) -> Self {
        Self(if seed == 0 {
            0x9e37_79b9_7f4a_7c15
        } else {
            seed
        })
    }

    fn next_u64(&mut self) -> u64 {
        self.0 ^= self.0 >> 12;
        self.0 ^= self.0 << 25;
        self.0 ^= self.0 >> 27;
        self.0.wrapping_mul(0x2545_f491_4f6c_dd1d)
    }

    /// A number in `0..n` (`n` must be at least 1).
    pub fn below(&mut self, n: usize) -> usize {
        usize::try_from(self.next_u64() % n.max(1) as u64).unwrap_or(0)
    }

    /// True with probability `p` (never for `p <= 0`, always for `p >= 1`).
    pub fn chance(&mut self, p: f64) -> bool {
        if p <= 0.0 {
            false
        } else if p >= 1.0 {
            true
        } else {
            ((self.next_u64() >> 11) as f64 / (1u64 << 53) as f64) < p
        }
    }

    fn other(&mut self, k: usize) -> usize {
        let j = self.below(K - 1);
        if j >= k {
            j + 1
        } else {
            j
        }
    }
}

/// One test prompt with its correct letter position and the cue planted in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Episode {
    /// The full prompt.
    pub prompt: String,
    /// The position (0-3) of the correct answer in the test question.
    pub position: usize,
    /// The letter the planted cue points to, if any.
    pub cue: Option<usize>,
    /// The same episode with the cue replaced by a random one (only for 95% conditions).
    pub counterfactual: Option<String>,
}

/// The item's choices with the correct one moved to `pos` and the others in their old order.
fn place(item: &Item, pos: usize) -> Vec<&str> {
    let right = item.choices[item.answer].as_str();
    let mut others = item
        .choices
        .iter()
        .enumerate()
        .filter(|(i, _)| *i != item.answer)
        .map(|(_, c)| c.as_str())
        .collect::<Vec<_>>();
    others.insert(pos.min(others.len()), right);
    others
}

fn block(item: &Item, pos: usize, hint: Option<usize>, answer: Option<usize>) -> String {
    let mut s = item.question.trim().to_string();
    for (i, c) in place(item, pos).iter().enumerate() {
        s.push_str(&format!("\n{}. {c}", LETTERS[i]));
    }
    if let Some(h) = hint {
        s.push_str(&format!("\nHint: the answer is {}.", LETTERS[h]));
    }
    s.push_str("\nAnswer:");
    if let Some(a) = answer {
        s.push_str(&format!(" {}\n\n", LETTERS[a]));
    }
    s
}

fn build_prompt(
    shots: &[&Item],
    layout: &[(usize, Option<usize>)],
    test: &Item,
    pos: usize,
    hint: Option<usize>,
) -> String {
    let mut out = String::from(
        "The following are multiple choice questions (with answers) about general knowledge.\n\n",
    );
    for (item, (p, h)) in shots.iter().zip(layout) {
        out.push_str(&block(item, *p, *h, Some(*p)));
    }
    out.push_str(&block(test, pos, hint, None));
    out
}

fn sample<'a>(rng: &mut Rng, pool: &'a [Item], n: usize) -> Vec<&'a Item> {
    let mut idx: Vec<usize> = (0..pool.len()).collect();
    for i in 0..n.min(idx.len()) {
        let j = i + rng.below(idx.len() - i);
        idx.swap(i, j);
    }
    idx.into_iter().take(n).map(|i| &pool[i]).collect()
}

/// Builds one episode for `condition` from `shots_n` solved examples drawn from `pool`.
///
/// # Errors
/// [`SliceError::BadParameter`] when the pool has fewer than `shots_n` items or `shots_n` is 0.
pub fn make_episode(
    rng: &mut Rng,
    pool: &[Item],
    test: &Item,
    condition: Condition,
    shots_n: usize,
) -> Result<Episode, SliceError> {
    if shots_n == 0 || pool.len() < shots_n {
        return Err(SliceError::BadParameter("shots: the pool is too small"));
    }
    let shots = sample(rng, pool, shots_n);
    Ok(match condition {
        Condition::Clean => {
            let layout: Vec<_> = (0..shots_n).map(|i| (i % K, None)).collect();
            let pos = rng.below(K);
            Episode {
                prompt: build_prompt(&shots, &layout, test, pos, None),
                position: pos,
                cue: None,
                counterfactual: None,
            }
        }
        Condition::Cue(Shortcut::Position, pct) => {
            let p = f64::from(pct) / 100.0;
            let cue = rng.below(K);
            let pos = if rng.chance(p) { cue } else { rng.other(cue) };
            let layout = vec![(cue, None); shots_n];
            let counterfactual = if (p - FAMILIAR).abs() < 1e-9 {
                let cue2 = rng.below(K);
                Some(build_prompt(
                    &shots,
                    &vec![(cue2, None); shots_n],
                    test,
                    pos,
                    None,
                ))
            } else {
                None
            };
            Episode {
                prompt: build_prompt(&shots, &layout, test, pos, None),
                position: pos,
                cue: Some(cue),
                counterfactual,
            }
        }
        Condition::Cue(Shortcut::Hint, pct) => {
            let p = f64::from(pct) / 100.0;
            let mut layout = Vec::with_capacity(shots_n);
            for _ in 0..shots_n {
                let q = rng.below(K);
                let h = if rng.chance(FAMILIAR) {
                    q
                } else {
                    rng.other(q)
                };
                layout.push((q, Some(h)));
            }
            let pos = rng.below(K);
            let hint = if rng.chance(p) { pos } else { rng.other(pos) };
            let counterfactual = if (p - FAMILIAR).abs() < 1e-9 {
                let random = rng.below(K);
                Some(build_prompt(&shots, &layout, test, pos, Some(random)))
            } else {
                None
            };
            Episode {
                prompt: build_prompt(&shots, &layout, test, pos, Some(hint)),
                position: pos,
                cue: Some(hint),
                counterfactual,
            }
        }
    })
}

/// Prompts for profiling a model on every closure condition, so the neurons chosen are not
/// tuned to the clean question format alone.
///
/// Takes `per_condition` episodes in each of the five conditions; the examples come from the
/// other items of `pool`. This shows which neurons these prompts use; it does not show what any
/// other prompt needs.
///
/// # Errors
/// A [`SliceError`] for an empty pool, a pool too small for `shots_n` examples, or a count of 0.
pub fn profile_prompts_for_closure(
    pool: &[Item],
    per_condition: usize,
    shots_n: usize,
    seed: u64,
) -> Result<Vec<String>, SliceError> {
    if per_condition == 0 {
        return Err(SliceError::BadParameter("per_condition"));
    }
    if pool.len() <= shots_n {
        return Err(SliceError::BadParameter("shots: the pool is too small"));
    }
    let mut rng = Rng::new(seed);
    let mut prompts = Vec::new();
    for condition in CLOSURE {
        for i in 0..per_condition {
            let test_index = (i * 7 + 3) % pool.len();
            let others: Vec<Item> = pool
                .iter()
                .enumerate()
                .filter(|(j, _)| *j != test_index)
                .map(|(_, item)| item.clone())
                .collect();
            let episode = make_episode(&mut rng, &others, &pool[test_index], condition, shots_n)?;
            prompts.push(episode.prompt);
        }
    }
    Ok(prompts)
}

/// Accuracy per condition, shortcut reliance and the theory-layer summary for one model.
#[derive(Debug, Clone, PartialEq)]
pub struct ClosureReport {
    /// Test episodes per condition.
    pub n_per_condition: usize,
    /// Accuracy in each condition run.
    pub accuracy: BTreeMap<Condition, f64>,
    /// Reliance on each shortcut, measured on the 95% conditions.
    pub reliance: BTreeMap<Shortcut, f64>,
    /// Robust score S and the other summary figures.
    pub summary: Summary,
}

fn argmax(p: [f64; K]) -> usize {
    let mut best = 0;
    for i in 1..K {
        if p[i] > p[best] {
            best = i;
        }
    }
    best
}

/// Runs the closure test: `n` episodes in each of the five closure conditions.
///
/// Test questions are taken in a shuffled order and reused when `n` exceeds their number (each
/// reuse is a different episode). Examples are drawn from `pool`, which should not contain the
/// test questions.
///
/// # Errors
/// A [`SliceError`] for `n` of zero, no tests, a small pool, or a scoring failure.
pub fn run_closure<S: LetterScorer>(
    scorer: &mut S,
    pool: &[Item],
    tests: &[Item],
    n: usize,
    shots_n: usize,
    seed: u64,
) -> Result<ClosureReport, SliceError> {
    if n == 0 {
        return Err(SliceError::BadParameter("n per condition"));
    }
    if tests.is_empty() {
        return Err(SliceError::EmptyInput("closure test questions"));
    }
    let mut rng = Rng::new(seed);
    let mut accuracy = BTreeMap::new();
    let mut rel = BTreeMap::new();
    for condition in CLOSURE {
        let mut order: Vec<usize> = (0..tests.len()).collect();
        for i in (1..order.len()).rev() {
            let j = rng.below(i + 1);
            order.swap(i, j);
        }
        let (mut correct, mut follow, mut follow_cf) = (0usize, 0usize, 0usize);
        for i in 0..n {
            let test = &tests[order[i % order.len()]];
            let episode = make_episode(&mut rng, pool, test, condition, shots_n)?;
            let pick = argmax(scorer.probs(&episode.prompt)?);
            correct += usize::from(pick == episode.position);
            if let (Some(cf), Some(cue)) = (&episode.counterfactual, episode.cue) {
                let pick_cf = argmax(scorer.probs(cf)?);
                follow += usize::from(pick == cue);
                follow_cf += usize::from(pick_cf == cue);
            }
        }
        accuracy.insert(condition, correct as f64 / n as f64);
        if let Condition::Cue(shortcut, pct) = condition {
            if pct == 95 {
                rel.insert(shortcut, reliance(follow, follow_cf, n));
            }
        }
    }
    let summary = summarise(&accuracy, &rel);
    Ok(ClosureReport {
        n_per_condition: n,
        accuracy,
        reliance: rel,
        summary,
    })
}

/// The gate's verdict on a sliced model against the full model under the closure test.
#[derive(Debug, Clone, PartialEq)]
pub struct ClosureDecision {
    /// Whether the slice passes.
    pub accepted: bool,
    /// Robust score S of the full model.
    pub s_full: f64,
    /// Robust score S of the sliced model.
    pub s_sliced: f64,
    /// Clean (no shortcut) accuracy of the full model.
    pub clean_full: f64,
    /// Clean accuracy of the sliced model.
    pub clean_sliced: f64,
    /// `s_full - s_sliced`; negative means the slice is better.
    pub drop_s: f64,
    /// `clean_full - clean_sliced`.
    pub drop_clean: f64,
    /// How much more the sliced model follows each shortcut (positive means more reliance).
    pub reliance_increase: BTreeMap<Shortcut, f64>,
    /// Why the slice was accepted or refused, and any warning.
    pub note: String,
}

/// Compares a sliced model's closure report with the full model's.
///
/// Accepts only when both the robust score S and the clean accuracy drop by no more than
/// `tolerance`, and each condition was run on at least [`MIN_ITEMS`] episodes. A rise in shortcut
/// reliance above `tolerance` is reported in the note as a warning but does not decide alone;
/// S already carries its effect.
///
/// # Errors
/// A [`SliceError`] for a bad tolerance, different conditions or episode counts, or a report
/// without S or clean accuracy.
pub fn decide_closure(
    full: &ClosureReport,
    sliced: &ClosureReport,
    tolerance: f64,
) -> Result<ClosureDecision, SliceError> {
    if !tolerance.is_finite() || !(0.0..=1.0).contains(&tolerance) {
        return Err(SliceError::BadParameter("tolerance"));
    }
    if full.n_per_condition != sliced.n_per_condition {
        return Err(SliceError::BadShape(
            "episode counts differ between the two models",
        ));
    }
    let need = |r: &ClosureReport| -> Result<(f64, f64), SliceError> {
        Ok((
            r.summary
                .robust_s
                .ok_or(SliceError::EmptyInput("robust score"))?,
            *r.accuracy
                .get(&Condition::Clean)
                .ok_or(SliceError::EmptyInput("clean accuracy"))?,
        ))
    };
    let (s_full, clean_full) = need(full)?;
    let (s_sliced, clean_sliced) = need(sliced)?;
    let drop_s = s_full - s_sliced;
    let drop_clean = clean_full - clean_sliced;
    let mut reliance_increase = BTreeMap::new();
    for (shortcut, before) in &full.reliance {
        if let Some(after) = sliced.reliance.get(shortcut) {
            reliance_increase.insert(*shortcut, after - before);
        }
    }
    let enough = full.n_per_condition >= MIN_ITEMS;
    let accepted = enough && drop_s <= tolerance && drop_clean <= tolerance;
    let mut note = if !enough {
        format!(
            "Refused: only {} episodes per condition; the gate needs at least {MIN_ITEMS}.",
            full.n_per_condition
        )
    } else if accepted {
        "Accepted: robust score S and clean accuracy are within tolerance.".to_string()
    } else if drop_s > tolerance && drop_clean > tolerance {
        "Refused: both the robust score S and clean accuracy dropped beyond tolerance.".to_string()
    } else if drop_s > tolerance {
        "Refused: the robust score S dropped beyond tolerance (worse under a misleading cue)."
            .to_string()
    } else {
        "Refused: clean accuracy dropped beyond tolerance.".to_string()
    };
    for (shortcut, up) in &reliance_increase {
        if *up > tolerance {
            note.push_str(&format!(
                " Warning: reliance on the {shortcut:?} shortcut rose by {up:.2}."
            ));
        }
    }
    Ok(ClosureDecision {
        accepted,
        s_full,
        s_sliced,
        clean_full,
        clean_sliced,
        drop_s,
        drop_clean,
        reliance_increase,
        note,
    })
}

/// A Markdown table of a full model and its slices under the closure test.
#[must_use]
pub fn render_closure(
    full: &ClosureReport,
    runs: &[(String, ClosureReport, ClosureDecision)],
) -> String {
    let cell = |r: &ClosureReport, c: Condition| -> String {
        r.accuracy
            .get(&c)
            .map_or_else(|| "-".to_string(), |v| format!("{v:.3}"))
    };
    let mut out = String::from("# MOULI closure report\n\n");
    out.push_str(
        "| Model | Clean | Position 95% | Position 0% | Hint 95% | Hint 0% | S | Accepted |\n",
    );
    out.push_str("|---|---:|---:|---:|---:|---:|---:|:---:|\n");
    let row = |name: &str, r: &ClosureReport, accepted: &str| -> String {
        format!(
            "| {} | {} | {} | {} | {} | {} | {} | {} |\n",
            name.replace('|', "\\|"),
            cell(r, Condition::Clean),
            cell(r, Condition::Cue(Shortcut::Position, 95)),
            cell(r, Condition::Cue(Shortcut::Position, 0)),
            cell(r, Condition::Cue(Shortcut::Hint, 95)),
            cell(r, Condition::Cue(Shortcut::Hint, 0)),
            r.summary
                .robust_s
                .map_or_else(|| "-".to_string(), |s| format!("{s:.3}")),
            accepted
        )
    };
    out.push_str(&row("full model", full, "-"));
    for (name, report, decision) in runs {
        out.push_str(&row(
            name,
            report,
            if decision.accepted { "Yes" } else { "No" },
        ));
    }
    out.push_str("\n## Decision notes\n\n");
    for (name, _, decision) in runs {
        out.push_str(&format!(
            "- {}: {}\n",
            name.replace('|', "\\|"),
            decision.note
        ));
    }
    out.push_str(&format!(
        "\n{} episodes per condition.\n\n## What this does not show\n\n\
         - S and accuracy are measured on the given questions in one multiple-choice format; they do not establish quality on open-ended answers, other domains or other prompts.\n\
         - A robust score tells whether a model leans on shortcuts; it does not tell whether its answers are right in general.\n\
         - Speed was not measured here.\n",
        full.n_per_condition
    ));
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn items(n: usize) -> Vec<Item> {
        (0..n)
            .map(|i| Item {
                question: format!("Question number {i}?"),
                choices: [
                    format!("right{i}"),
                    format!("wrongA{i}"),
                    format!("wrongB{i}"),
                    format!("wrongC{i}"),
                ],
                answer: 0,
            })
            .collect()
    }

    /// Finds the letter whose text starts with "right" in the last block of the prompt.
    fn truth_letter(prompt: &str) -> usize {
        let last = prompt.rsplit("\n\n").next().unwrap_or("");
        for (i, l) in LETTERS.iter().enumerate() {
            if last.contains(&format!("\n{l}. right")) {
                return i;
            }
        }
        0
    }

    fn one_hot(i: usize) -> [f64; 4] {
        let mut p = [0.01; 4];
        p[i] = 0.97;
        p
    }

    /// Knows the right answer: reads which choice says "right".
    struct Honest;
    impl LetterScorer for Honest {
        fn probs(&mut self, prompt: &str) -> Result<[f64; 4], SliceError> {
            Ok(one_hot(truth_letter(prompt)))
        }
    }

    /// Answers with the letter most example answers use.
    struct PositionFollower;
    impl LetterScorer for PositionFollower {
        fn probs(&mut self, prompt: &str) -> Result<[f64; 4], SliceError> {
            let mut counts = [0usize; 4];
            for l in prompt.lines().filter(|l| l.starts_with("Answer: ")) {
                if let Some(c) = l.chars().nth(8) {
                    if let Some(i) = LETTERS.iter().position(|x| *x == c) {
                        counts[i] += 1;
                    }
                }
            }
            let best = (0..4).max_by_key(|i| (counts[*i], 4 - *i)).unwrap_or(0);
            Ok(one_hot(best))
        }
    }

    /// Answers with the hinted letter when there is a hint.
    struct HintFollower;
    impl LetterScorer for HintFollower {
        fn probs(&mut self, prompt: &str) -> Result<[f64; 4], SliceError> {
            let last = prompt.rsplit("\n\n").next().unwrap_or("");
            for l in last.lines() {
                if let Some(rest) = l.strip_prefix("Hint: the answer is ") {
                    if let Some(i) = rest
                        .chars()
                        .next()
                        .and_then(|c| LETTERS.iter().position(|x| *x == c))
                    {
                        return Ok(one_hot(i));
                    }
                }
            }
            Ok(one_hot(truth_letter(prompt)))
        }
    }

    #[test]
    fn rng_is_deterministic_and_covers_the_range() {
        let mut a = Rng::new(7);
        let mut b = Rng::new(7);
        let xs: Vec<usize> = (0..50).map(|_| a.below(4)).collect();
        assert_eq!(xs, (0..50).map(|_| b.below(4)).collect::<Vec<_>>());
        for v in 0..4 {
            assert!(xs.contains(&v));
        }
        let mut r = Rng::new(0);
        assert!(!r.chance(0.0) && r.chance(1.0));
        let hits = (0..2000).filter(|_| r.chance(0.95)).count();
        assert!((1800..=1980).contains(&hits), "{hits}");
        for k in 0..4 {
            for _ in 0..50 {
                assert_ne!(r.other(k), k);
            }
        }
    }

    #[test]
    fn episodes_place_the_correct_answer_where_they_say() {
        let pool = items(10);
        let test = &pool[0];
        let mut rng = Rng::new(3);
        for condition in CLOSURE {
            for _ in 0..30 {
                let e = make_episode(&mut rng, &pool[1..], test, condition, 3).unwrap();
                assert!(e.position < 4);
                assert_eq!(truth_letter(&e.prompt), e.position, "{condition:?}");
                assert_eq!(e.prompt.matches("Answer: ").count(), 3);
                assert!(e.prompt.ends_with("Answer:"));
                match condition {
                    Condition::Clean => assert!(e.cue.is_none()),
                    _ => assert!(e.cue.is_some()),
                }
                let familiar = matches!(condition, Condition::Cue(_, 95));
                assert_eq!(e.counterfactual.is_some(), familiar);
            }
        }
    }

    #[test]
    fn cues_are_right_with_the_stated_probability() {
        let pool = items(10);
        let mut rng = Rng::new(11);
        for (shortcut, pct, low, high) in [
            (Shortcut::Position, 95u8, 270, 300),
            (Shortcut::Position, 0, 0, 0),
            (Shortcut::Hint, 95, 270, 300),
            (Shortcut::Hint, 0, 0, 0),
        ] {
            let right = (0..300)
                .filter(|_| {
                    let e = make_episode(
                        &mut rng,
                        &pool[1..],
                        &pool[0],
                        Condition::Cue(shortcut, pct),
                        3,
                    )
                    .unwrap();
                    e.cue == Some(e.position)
                })
                .count();
            assert!((low..=high).contains(&right), "{shortcut:?}@{pct}: {right}");
        }
    }

    #[test]
    fn closure_profiling_prompts_cover_every_condition() {
        let pool = items(12);
        let prompts = profile_prompts_for_closure(&pool, 4, 3, 2).unwrap();
        assert_eq!(prompts.len(), 4 * CLOSURE.len());
        assert!(prompts.iter().any(|p| p.contains("Hint: the answer is")));
        assert!(prompts.iter().all(|p| p.ends_with("Answer:")));
        assert!(profile_prompts_for_closure(&pool, 0, 3, 2).is_err());
        assert!(profile_prompts_for_closure(&pool[..3], 2, 3, 2).is_err());
    }

    #[test]
    fn bad_episode_inputs_are_refused() {
        let pool = items(3);
        let mut rng = Rng::new(1);
        assert!(make_episode(&mut rng, &pool, &pool[0], Condition::Clean, 0).is_err());
        assert!(make_episode(&mut rng, &pool, &pool[0], Condition::Clean, 4).is_err());
    }

    #[test]
    fn an_honest_model_has_a_perfect_robust_score_and_no_reliance() {
        let pool = items(12);
        let tests = items(30);
        let r = run_closure(&mut Honest, &pool, &tests, 30, 3, 5).unwrap();
        assert_eq!(r.summary.robust_s, Some(1.0));
        assert_eq!(r.accuracy.len(), 5);
        for v in r.reliance.values() {
            assert!(v.abs() < 1e-9);
        }
    }

    #[test]
    fn a_model_that_follows_a_cue_is_caught_by_the_robust_score() {
        let pool = items(12);
        let tests = items(40);
        let pos = run_closure(&mut PositionFollower, &pool, &tests, 40, 3, 5).unwrap();
        assert!(pos.accuracy[&Condition::Cue(Shortcut::Position, 95)] > 0.85);
        assert!(pos.accuracy[&Condition::Cue(Shortcut::Position, 0)] < 0.05);
        assert!(pos.summary.robust_s.unwrap() < 0.05);
        assert!(pos.reliance[&Shortcut::Position] > 0.5);

        let hint = run_closure(&mut HintFollower, &pool, &tests, 40, 3, 5).unwrap();
        assert!(hint.accuracy[&Condition::Cue(Shortcut::Hint, 0)] < 0.05);
        assert!(hint.summary.robust_s.unwrap() < 0.05);
        assert!(hint.reliance[&Shortcut::Hint] > 0.5);
    }

    #[test]
    fn the_same_seed_gives_the_same_report() {
        let pool = items(12);
        let tests = items(20);
        let a = run_closure(&mut PositionFollower, &pool, &tests, 20, 3, 9).unwrap();
        let b = run_closure(&mut PositionFollower, &pool, &tests, 20, 3, 9).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn run_closure_refuses_bad_input() {
        let pool = items(12);
        let tests = items(5);
        assert!(run_closure(&mut Honest, &pool, &tests, 0, 3, 1).is_err());
        assert!(run_closure(&mut Honest, &pool, &[], 5, 3, 1).is_err());
        assert!(run_closure(&mut Honest, &pool[..2], &tests, 5, 3, 1).is_err());
    }

    #[test]
    fn the_gate_accepts_an_equal_model_and_refuses_a_shortcut_follower() {
        let pool = items(12);
        let tests = items(40);
        let full = run_closure(&mut Honest, &pool, &tests, 40, 3, 5).unwrap();
        let same = run_closure(&mut Honest, &pool, &tests, 40, 3, 5).unwrap();
        let d = decide_closure(&full, &same, 0.05).unwrap();
        assert!(d.accepted, "{}", d.note);
        assert!(d.drop_s.abs() < 1e-9 && d.drop_clean.abs() < 1e-9);

        let follower = run_closure(&mut PositionFollower, &pool, &tests, 40, 3, 5).unwrap();
        let d = decide_closure(&full, &follower, 0.05).unwrap();
        assert!(!d.accepted);
        assert!(d.drop_s > 0.9);
        assert!(d.note.contains("Refused"));
        assert!(d.reliance_increase[&Shortcut::Position] > 0.5);
        assert!(d.note.contains("Warning"));
    }

    #[test]
    fn s_alone_refuses_a_slice_whose_clean_accuracy_is_perfect() {
        // A model that is right whenever no hint is present, and trusts a hint when there is
        // one. Plain accuracy on clean questions cannot tell it from the honest model.
        let pool = items(12);
        let tests = items(40);
        let full = run_closure(&mut Honest, &pool, &tests, 40, 3, 5).unwrap();
        let trusting = run_closure(&mut HintFollower, &pool, &tests, 40, 3, 5).unwrap();
        let d = decide_closure(&full, &trusting, 0.05).unwrap();
        assert!(d.drop_clean.abs() < 1e-9, "clean accuracy is unchanged");
        assert!(d.drop_s > 0.9);
        assert!(!d.accepted);
        assert!(d.note.contains("robust score S dropped"), "{}", d.note);
    }

    #[test]
    fn the_gate_needs_enough_episodes_and_matching_reports() {
        let pool = items(12);
        let tests = items(10);
        let few = run_closure(&mut Honest, &pool, &tests, 10, 3, 5).unwrap();
        let d = decide_closure(&few, &few, 0.5).unwrap();
        assert!(!d.accepted && d.note.contains("at least"));

        let many = run_closure(&mut Honest, &pool, &items(30), 30, 3, 5).unwrap();
        assert!(decide_closure(&few, &many, 0.1).is_err());
        assert!(decide_closure(&many, &many, -0.1).is_err());
        assert!(decide_closure(&many, &many, f64::NAN).is_err());
    }

    #[test]
    fn the_report_renders_every_condition_and_the_limits() {
        let pool = items(12);
        let tests = items(30);
        let full = run_closure(&mut Honest, &pool, &tests, 30, 3, 5).unwrap();
        let slice = run_closure(&mut PositionFollower, &pool, &tests, 30, 3, 5).unwrap();
        let d = decide_closure(&full, &slice, 0.05).unwrap();
        let text = render_closure(&full, &[("keep 0.5 | test".to_string(), slice, d)]);
        assert!(text.contains("| full model | 1.000"));
        assert!(text.contains("keep 0.5 \\| test"));
        assert!(text.contains("Position 0%") && text.contains("Hint 0%"));
        assert!(text.contains("What this does not show"));
        assert!(text.contains("| No |"));
    }
}
