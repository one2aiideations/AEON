//! Chat-history budgeting for a token-limited context window.
//!
//! There is no real tokenizer available here, so token counts are *estimated*
//! from the number of Unicode scalar values (not bytes) in the text:
//! `ceil(char_count * 2 / 7)`, i.e. roughly one token per 3.5 characters.
//! Every turn additionally costs a fixed framing overhead of 4 tokens, and the
//! system prompt is charged the same way and always counts against the budget.
//!
//! [`History::fit`] keeps the system prompt and as many of the newest turns as
//! fit inside the budget. Oldest turns are dropped first, and they are dropped
//! in pairs so the fitted history never begins with an [`Role::Assistant`]
//! turn. The newest turn is always kept: if the system prompt plus the newest
//! turn alone exceed the budget, the newest turn is truncated at a char
//! boundary (never inside a UTF-8 sequence) and the suffix `" [cut]"` is
//! appended. If even an empty turn cannot fit, the newest turn is returned
//! with empty text. A budget of `0` is accepted and never panics.

use std::collections::VecDeque;

/// Framing overhead added to every turn's cost.
const TURN_OVERHEAD: usize = 4;
/// Framing overhead added to the system prompt's cost.
const SYSTEM_OVERHEAD: usize = 4;
/// Suffix appended to a truncated turn.
const TRUNCATION_SUFFIX: &str = " [cut]";

/// Who authored a turn.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Role {
    /// A turn written by the human user.
    User,
    /// A turn written by the assistant.
    Assistant,
}

/// A single chat turn.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Turn {
    /// The author of the turn.
    pub role: Role,
    /// The turn's text.
    pub text: String,
}

/// Estimates the number of tokens in `text`.
///
/// Counts Unicode scalar values (not bytes) and returns
/// `ceil(char_count * 2 / 7)`, i.e. roughly one token per 3.5 characters.
#[must_use]
pub fn estimate_tokens(text: &str) -> usize {
    let chars = text.chars().count();
    chars.saturating_mul(2).saturating_add(6) / 7
}

/// Estimated cost of a piece of text with one turn of framing overhead.
fn turn_cost(text: &str) -> usize {
    estimate_tokens(text).saturating_add(TURN_OVERHEAD)
}

/// Estimated cost of the system prompt.
fn system_cost(system: &str) -> usize {
    estimate_tokens(system).saturating_add(SYSTEM_OVERHEAD)
}

/// A chat history that can be fitted into a token budget.
#[derive(Clone, Default, PartialEq, Eq, Debug)]
pub struct History {
    turns: Vec<Turn>,
}

impl History {
    /// Creates an empty history.
    #[must_use]
    pub fn new() -> Self {
        Self { turns: Vec::new() }
    }

    /// Appends a turn to the end of the history.
    pub fn push(&mut self, role: Role, text: impl Into<String>) {
        self.turns.push(Turn {
            role,
            text: text.into(),
        });
    }

    /// Returns the number of turns stored.
    #[must_use]
    pub fn len(&self) -> usize {
        self.turns.len()
    }

    /// Returns `true` if no turns are stored.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.turns.is_empty()
    }

    /// Removes all turns.
    pub fn clear(&mut self) {
        self.turns.clear();
    }

    /// Borrows the stored turns in order, oldest first.
    #[must_use]
    pub fn turns(&self) -> &[Turn] {
        &self.turns
    }

    /// Fits the history into `budget_tokens`, keeping the system prompt.
    ///
    /// The system prompt (charged as `estimate_tokens(system)` plus the same
    /// framing overhead as a turn) always counts against the budget and is
    /// always kept. Turns are dropped oldest-first; whenever dropping leaves
    /// an assistant turn at the front, that turn is dropped too, so the
    /// fitted history never starts with [`Role::Assistant`] (unless it is the
    /// single newest turn, which is always kept).
    ///
    /// If the system prompt plus the newest turn alone exceed the budget, the
    /// newest turn's text is truncated at a char boundary and `" [cut]"` is
    /// appended so the result fits; `truncated` is set to `true`. If even the
    /// `" [cut]"` suffix cannot fit, the newest turn is returned with empty
    /// text and `truncated` set to `true`. `dropped` counts every removed
    /// turn, including pair-dropped assistant turns. A budget of `0` never
    /// panics.
    #[must_use]
    pub fn fit(&self, budget_tokens: usize, system: &str) -> Fitted {
        let system_tokens = system_cost(system);
        let mut kept: VecDeque<Turn> = self.turns.iter().cloned().collect();
        let mut dropped = 0usize;

        loop {
            let total = kept.iter().fold(system_tokens, |acc, turn| {
                acc.saturating_add(turn_cost(&turn.text))
            });
            if total <= budget_tokens || kept.len() <= 1 {
                break;
            }
            kept.pop_front();
            dropped = dropped.saturating_add(1);
            while kept.len() > 1 && kept.front().is_some_and(|t| t.role == Role::Assistant) {
                kept.pop_front();
                dropped = dropped.saturating_add(1);
            }
        }

        let mut truncated = false;
        if let Some(newest) = kept.back_mut() {
            let available = budget_tokens.saturating_sub(system_tokens);
            if turn_cost(&newest.text) > available {
                truncated = true;
                let suffix_chars = TRUNCATION_SUFFIX.chars().count();
                // Need ceil(c * 2 / 7) <= available - TURN_OVERHEAD, i.e.
                // c <= floor(7 * (available - TURN_OVERHEAD) / 2).
                let slack = available.saturating_sub(TURN_OVERHEAD);
                let max_total_chars = slack.saturating_mul(7) / 2;
                if max_total_chars < suffix_chars {
                    newest.text.clear();
                } else {
                    let keep = max_total_chars - suffix_chars;
                    let mut cut: String = newest.text.chars().take(keep).collect();
                    cut.push_str(TRUNCATION_SUFFIX);
                    newest.text = cut;
                }
            }
        }

        Fitted {
            system: system.to_string(),
            turns: kept.into_iter().collect(),
            dropped,
            truncated,
        }
    }
}

/// The result of fitting a [`History`] into a token budget.
#[derive(Clone, PartialEq, Eq, Debug)]
pub struct Fitted {
    /// The system prompt; always kept, always counts against the budget.
    pub system: String,
    /// The turns that were kept, oldest first.
    pub turns: Vec<Turn>,
    /// How many turns were dropped from the front (including pair-dropped
    /// assistant turns).
    pub dropped: usize,
    /// `true` when the newest turn had to be truncated (or emptied) to fit.
    pub truncated: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn turn_cost_of(text: &str) -> usize {
        estimate_tokens(text) + 4
    }

    #[test]
    fn estimate_tokens_basic_boundaries() {
        assert_eq!(estimate_tokens(""), 0);
        assert_eq!(estimate_tokens("a"), 1);
        assert_eq!(estimate_tokens("abc"), 1);
        assert_eq!(estimate_tokens("abcd"), 2);
        assert_eq!(estimate_tokens("1234567"), 2);
        assert_eq!(estimate_tokens("12345678"), 3);
    }

    #[test]
    fn estimate_tokens_counts_scalars_not_bytes() {
        // 'é' is 2 bytes but 1 scalar value.
        assert_eq!(estimate_tokens("é"), 1);
        assert_eq!(estimate_tokens("éé"), 1);
        assert_eq!("éé".len(), 4);
    }

    #[test]
    fn nothing_dropped_when_it_fits() {
        let mut h = History::new();
        h.push(Role::User, "hello there");
        h.push(Role::Assistant, "hi, how can I help?");
        let system = "you are terse";
        let budget = estimate_tokens(system)
            + 4
            + turn_cost_of("hello there")
            + turn_cost_of("hi, how can I help?");
        let fitted = h.fit(budget, system);
        assert_eq!(fitted.dropped, 0);
        assert!(!fitted.truncated);
        assert_eq!(&fitted.turns, h.turns());
        assert_eq!(fitted.system, system);
    }

    #[test]
    fn exact_boundary_budget_keeps_everything() {
        let mut h = History::new();
        h.push(Role::User, "aaaaaaa");
        h.push(Role::Assistant, "bbbbbbb");
        h.push(Role::User, "ccccccc");
        let system = "sssssss";
        let exact = estimate_tokens(system)
            + 4
            + turn_cost_of("aaaaaaa")
            + turn_cost_of("bbbbbbb")
            + turn_cost_of("ccccccc");
        let fitted = h.fit(exact, system);
        assert_eq!(fitted.dropped, 0);
        assert_eq!(fitted.turns.len(), 3);
        assert!(!fitted.truncated);
    }

    #[test]
    fn one_token_below_exact_boundary_drops_oldest_pair() {
        let mut h = History::new();
        h.push(Role::User, "aaaaaaa");
        h.push(Role::Assistant, "bbbbbbb");
        h.push(Role::User, "ccccccc");
        let system = "sssssss";
        let exact = estimate_tokens(system)
            + 4
            + turn_cost_of("aaaaaaa")
            + turn_cost_of("bbbbbbb")
            + turn_cost_of("ccccccc");
        let fitted = h.fit(exact - 1, system);
        assert_eq!(fitted.dropped, 2);
        assert_eq!(fitted.turns.len(), 1);
        assert_eq!(
            fitted.turns.first().map(|t| t.text.as_str()),
            Some("ccccccc")
        );
    }

    #[test]
    fn oldest_pairs_dropped_first() {
        let mut h = History::new();
        for i in 0..4 {
            h.push(Role::User, format!("question number {i} with padding"));
            h.push(Role::Assistant, format!("answer number {i} with padding"));
        }
        let system = "sys";
        let keep = turn_cost_of("question number 3 with padding")
            + turn_cost_of("answer number 3 with padding");
        let budget = estimate_tokens(system) + 4 + keep;
        let fitted = h.fit(budget, system);
        assert_eq!(fitted.dropped, 6);
        assert_eq!(fitted.turns.len(), 2);
        assert_eq!(fitted.turns.first().map(|t| t.role), Some(Role::User));
        assert_eq!(
            fitted.turns.first().map(|t| t.text.as_str()),
            Some("question number 3 with padding")
        );
    }

    #[test]
    fn fitted_history_never_starts_with_assistant() {
        let mut h = History::new();
        h.push(Role::User, "aaaaaaa");
        h.push(Role::Assistant, "bbbbbbb");
        h.push(Role::User, "ccccccc");
        let system = "";
        // Room for system + two turns, but the two oldest must drop as a pair.
        let budget =
            estimate_tokens(system) + 4 + turn_cost_of("bbbbbbb") + turn_cost_of("ccccccc");
        let fitted = h.fit(budget, system);
        assert_eq!(fitted.dropped, 2);
        assert_eq!(fitted.turns.len(), 1);
        assert_eq!(fitted.turns.first().map(|t| t.role), Some(Role::User));
    }

    #[test]
    fn newest_assistant_turn_is_still_always_kept() {
        let mut h = History::new();
        h.push(Role::User, "aaaaaaa");
        h.push(Role::Assistant, "bbbbbbb");
        let fitted = h.fit(estimate_tokens("") + 4 + turn_cost_of("bbbbbbb") - 1, "");
        // Oldest user turn drops; the assistant turn is the newest, so it stays.
        assert_eq!(fitted.turns.len(), 1);
        assert_eq!(fitted.turns.first().map(|t| t.role), Some(Role::Assistant));
        assert_eq!(fitted.dropped, 1);
    }

    #[test]
    fn truncation_respects_multibyte_boundaries() {
        let mut h = History::new();
        let text = "é".repeat(100);
        h.push(Role::User, text.clone());
        let system = "";
        // System costs 4; 16 tokens left for the turn.
        let budget = estimate_tokens(system) + 4 + 16;
        let fitted = h.fit(budget, system);
        assert!(fitted.truncated);
        assert_eq!(fitted.turns.len(), 1);
        let cut = &fitted.turns[0].text;
        assert!(cut.ends_with(" [cut]"));
        let prefix = cut.strip_suffix(" [cut]").unwrap_or("");
        assert!(prefix.chars().all(|c| c == 'é'));
        // floor(7 * (16 - 4) / 2) = 42 total chars, minus 6 for " [cut]".
        assert_eq!(prefix.chars().count(), 36);
        assert!(text.starts_with(prefix));
        // The truncated turn plus system must fit the budget.
        assert!(estimate_tokens(cut) + 4 + estimate_tokens(system) + 4 <= budget);
    }

    #[test]
    fn truncation_suffix_fits_exactly_at_boundary() {
        let mut h = History::new();
        h.push(Role::User, "é".repeat(50));
        // System costs 4; available = 6, so floor(7*2/2) = 7 chars total:
        // one 'é' plus " [cut]".
        let fitted = h.fit(10, "");
        assert_eq!(fitted.turns[0].text, "é [cut]");
        assert!(fitted.truncated);
        assert_eq!(estimate_tokens("é [cut]") + 4, 6);
    }

    #[test]
    fn empty_newest_turn_when_suffix_cannot_fit() {
        let mut h = History::new();
        h.push(Role::User, "x".repeat(64));
        // System 4 + available 4: an empty turn (cost 4) fits exactly, but
        // there is no room for the " [cut]" suffix, so text is emptied.
        let fitted = h.fit(8, "");
        assert_eq!(fitted.turns[0].text, "");
        assert!(fitted.truncated);
    }

    #[test]
    fn budget_zero_does_not_panic() {
        let mut h = History::new();
        h.push(Role::User, "hello");
        h.push(Role::Assistant, "world");
        let fitted = h.fit(0, "system prompt");
        assert_eq!(fitted.turns.len(), 1);
        assert_eq!(fitted.turns[0].text, "");
        assert_eq!(fitted.turns[0].role, Role::Assistant);
        assert!(fitted.truncated);
        assert_eq!(fitted.dropped, 1);
        assert_eq!(fitted.system, "system prompt");
    }

    #[test]
    fn budget_zero_with_empty_history() {
        let h = History::new();
        let fitted = h.fit(0, "");
        assert!(fitted.turns.is_empty());
        assert_eq!(fitted.dropped, 0);
        assert!(!fitted.truncated);
    }

    #[test]
    fn empty_history_keeps_system_and_costs_nothing() {
        let fitted = History::new().fit(1000, "you are helpful");
        assert_eq!(fitted.system, "you are helpful");
        assert!(fitted.turns.is_empty());
        assert_eq!(fitted.dropped, 0);
        assert!(!fitted.truncated);
    }

    #[test]
    fn system_prompt_counts_against_budget() {
        let mut h = History::new();
        h.push(Role::User, "aaaaaaa");
        h.push(Role::User, "bbbbbbb");
        let system = "you are a very long-winded system prompt indeed";
        let sys_cost = estimate_tokens(system) + 4;
        let budget = sys_cost + turn_cost_of("bbbbbbb");
        let fitted = h.fit(budget, system);
        assert_eq!(fitted.dropped, 1);
        assert_eq!(fitted.turns.len(), 1);
        assert_eq!(fitted.turns[0].text, "bbbbbbb");
    }

    #[test]
    fn system_alone_exceeding_budget_still_kept() {
        let mut h = History::new();
        h.push(Role::User, "hi");
        let fitted = h.fit(1, "a much longer system prompt than one token");
        assert_eq!(fitted.system, "a much longer system prompt than one token");
        // Newest turn is kept (emptied) even though nothing fits.
        assert_eq!(fitted.turns.len(), 1);
        assert_eq!(fitted.turns[0].text, "");
        assert!(fitted.truncated);
    }

    #[test]
    fn dropped_counts_every_removed_turn() {
        let mut h = History::new();
        for i in 0..6 {
            h.push(Role::User, format!("user turn {i} padding padding"));
        }
        let keep = turn_cost_of("user turn 5 padding padding")
            .saturating_add(turn_cost_of("user turn 4 padding padding"));
        let budget = estimate_tokens("") + 4 + keep;
        let fitted = h.fit(budget, "");
        assert_eq!(fitted.turns.len(), 2);
        assert_eq!(fitted.dropped, 4);
    }

    #[test]
    fn clear_removes_all_turns() {
        let mut h = History::new();
        h.push(Role::User, "a");
        h.push(Role::Assistant, "b");
        assert_eq!(h.len(), 2);
        assert!(!h.is_empty());
        h.clear();
        assert_eq!(h.len(), 0);
        assert!(h.is_empty());
        assert!(h.turns().is_empty());
    }

    #[test]
    fn push_accepts_owned_string() {
        let mut h = History::new();
        h.push(Role::User, String::from("owned text"));
        assert_eq!(h.turns()[0].text, "owned text");
    }

    #[test]
    fn empty_newest_turn_that_fits_is_not_marked_truncated() {
        let mut h = History::new();
        h.push(Role::Assistant, "");
        let budget = estimate_tokens("") + 4 + turn_cost_of("");
        let fitted = h.fit(budget, "");
        assert_eq!(fitted.turns[0].text, "");
        assert!(!fitted.truncated);
    }
}
