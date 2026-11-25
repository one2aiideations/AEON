//! Boolean rule trees evaluated against a JSON event.
//!
//! A rule is a JSON object. A leaf is `{"field": "payload.amount", "operator": "gt", "value": 100}`;
//! `{"operator": "and"|"or", "conditions": [...]}` and `{"operator": "not", "condition": {...}}`
//! combine rules. Fields are dot paths; numeric segments index arrays.

use serde_json::Value;
use std::fmt;

/// Deepest nesting of and / or / not accepted (guards against hostile rules).
pub const MAX_DEPTH: usize = 32;

/// Why a rule could not be evaluated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuleError {
    /// The rule (or a part of it) is not a JSON object.
    NotAnObject,
    /// The `operator` is missing or unknown.
    UnknownOperator(String),
    /// A leaf has no `field`.
    MissingField,
    /// An `and` / `or` has no `conditions` array, or `not` has no `condition`.
    MissingConditions,
    /// The rule nests deeper than [`MAX_DEPTH`].
    TooDeep,
}

impl fmt::Display for RuleError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            RuleError::NotAnObject => write!(f, "rule must be a JSON object"),
            RuleError::UnknownOperator(o) => write!(f, "unknown operator '{o}'"),
            RuleError::MissingField => write!(f, "leaf rule needs a 'field'"),
            RuleError::MissingConditions => write!(f, "combinator rule needs its conditions"),
            RuleError::TooDeep => write!(f, "rule nests deeper than {MAX_DEPTH} levels"),
        }
    }
}

impl std::error::Error for RuleError {}

/// The result of evaluating a rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Outcome {
    /// Whether the rule matched.
    pub matched: bool,
    /// Human-readable reason, naming the values that decided it.
    pub explanation: String,
    /// Event fields that took part (in first-use order, without repeats).
    pub fields: Vec<String>,
}

/// Looks a dot path up in a JSON value; numeric segments index arrays.
pub fn resolve<'a>(event: &'a Value, path: &str) -> Option<&'a Value> {
    let mut cur = event;
    for part in path.split('.') {
        cur = match cur {
            Value::Object(m) => m.get(part)?,
            Value::Array(a) => a.get(part.parse::<usize>().ok()?)?,
            _ => return None,
        };
    }
    Some(cur)
}

fn push_unique(into: &mut Vec<String>, from: Vec<String>) {
    for f in from {
        if !into.contains(&f) {
            into.push(f);
        }
    }
}

/// Evaluates `rule` against `event`.
pub fn evaluate(rule: &Value, event: &Value) -> Result<Outcome, RuleError> {
    eval(rule, event, 0)
}

fn eval(rule: &Value, event: &Value, depth: usize) -> Result<Outcome, RuleError> {
    if depth > MAX_DEPTH {
        return Err(RuleError::TooDeep);
    }
    let obj = rule.as_object().ok_or(RuleError::NotAnObject)?;
    let op = obj
        .get("operator")
        .and_then(Value::as_str)
        .ok_or_else(|| RuleError::UnknownOperator(String::new()))?;
    match op {
        "and" | "or" => {
            let subs = obj
                .get("conditions")
                .and_then(Value::as_array)
                .ok_or(RuleError::MissingConditions)?;
            let mut fields = Vec::new();
            let mut results = Vec::new();
            for s in subs {
                let o = eval(s, event, depth + 1)?;
                push_unique(&mut fields, o.fields);
                results.push(o.matched);
            }
            let matched = if op == "and" {
                results.iter().all(|r| *r)
            } else {
                results.iter().any(|r| *r)
            };
            let hits = results.iter().filter(|r| **r).count();
            Ok(Outcome {
                matched,
                explanation: format!("{} of {} conditions matched ({op})", hits, results.len()),
                fields,
            })
        }
        "not" => {
            let sub = obj.get("condition").ok_or(RuleError::MissingConditions)?;
            let o = eval(sub, event, depth + 1)?;
            Ok(Outcome {
                matched: !o.matched,
                explanation: format!("not ({})", o.explanation),
                fields: o.fields,
            })
        }
        _ => leaf(op, obj, event),
    }
}

/// The comparison operators a leaf rule may use.
const LEAF_OPS: [&str; 13] = [
    "eq",
    "neq",
    "gt",
    "gte",
    "lt",
    "lte",
    "in",
    "not_in",
    "contains",
    "not_contains",
    "exists",
    "not_exists",
    "between",
];

fn leaf(
    op: &str,
    obj: &serde_json::Map<String, Value>,
    event: &Value,
) -> Result<Outcome, RuleError> {
    if !LEAF_OPS.contains(&op) {
        return Err(RuleError::UnknownOperator(op.to_string()));
    }
    let field = obj
        .get("field")
        .and_then(Value::as_str)
        .ok_or(RuleError::MissingField)?;
    let target = obj.get("value").unwrap_or(&Value::Null);
    let actual = resolve(event, field);
    let matched = compare(op, actual, target)?;
    let shown = actual.map_or("<missing>".to_string(), |v| v.to_string());
    Ok(Outcome {
        matched,
        explanation: format!("{field} {op} {target}: actual {shown}"),
        fields: vec![field.to_string()],
    })
}

fn num(v: &Value) -> Option<f64> {
    v.as_f64().filter(|n| n.is_finite())
}

fn ordering(a: &Value, b: &Value) -> Option<std::cmp::Ordering> {
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x.partial_cmp(&y),
        _ => match (a.as_str(), b.as_str()) {
            (Some(x), Some(y)) => Some(x.cmp(y)),
            _ => None,
        },
    }
}

fn same(a: &Value, b: &Value) -> bool {
    match (num(a), num(b)) {
        (Some(x), Some(y)) => x == y,
        _ => a == b,
    }
}

fn contains(haystack: &Value, needle: &Value) -> bool {
    match haystack {
        Value::String(s) => needle.as_str().is_some_and(|n| s.contains(n)),
        Value::Array(a) => a.iter().any(|x| same(x, needle)),
        Value::Object(m) => needle.as_str().is_some_and(|k| m.contains_key(k)),
        _ => false,
    }
}

fn compare(op: &str, actual: Option<&Value>, target: &Value) -> Result<bool, RuleError> {
    use std::cmp::Ordering::{Equal, Greater, Less};
    let exists = actual.is_some_and(|v| !v.is_null());
    Ok(match op {
        "exists" => exists,
        "not_exists" => !exists,
        // Every other operator needs a present value; a missing one never matches, except the
        // negative forms, which hold because the value is not there.
        "neq" => actual.is_none_or(|a| !same(a, target)),
        "not_in" => actual.is_none_or(|a| {
            !target
                .as_array()
                .is_some_and(|t| t.iter().any(|x| same(x, a)))
        }),
        "not_contains" => actual.is_none_or(|a| !contains(a, target)),
        "eq" | "gt" | "gte" | "lt" | "lte" | "in" | "contains" | "between" => {
            let Some(a) = actual else { return Ok(false) };
            match op {
                "eq" => same(a, target),
                "gt" => ordering(a, target) == Some(Greater),
                "gte" => matches!(ordering(a, target), Some(Greater | Equal)),
                "lt" => ordering(a, target) == Some(Less),
                "lte" => matches!(ordering(a, target), Some(Less | Equal)),
                "in" => target
                    .as_array()
                    .is_some_and(|t| t.iter().any(|x| same(x, a))),
                "contains" => contains(a, target),
                _ => match target.as_array().map(Vec::as_slice) {
                    Some([lo, hi]) => {
                        matches!(ordering(a, lo), Some(Greater | Equal))
                            && matches!(ordering(a, hi), Some(Less | Equal))
                    }
                    _ => false,
                },
            }
        }
        other => return Err(RuleError::UnknownOperator(other.to_string())),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ev() -> Value {
        json!({"type": "PAYMENT", "payload": {"amount": 250, "currency": "USD", "tags": ["wire", "new"]},
               "lines": [{"sku": "A"}, {"sku": "B"}], "note": null})
    }

    fn leaf(op: &str, field: &str, value: Value) -> Value {
        json!({"field": field, "operator": op, "value": value})
    }

    fn hit(rule: Value) -> bool {
        evaluate(&rule, &ev()).unwrap().matched
    }

    #[test]
    fn numeric_comparisons_use_values_not_text() {
        assert!(hit(leaf("gt", "payload.amount", json!(100))));
        assert!(!hit(leaf("gt", "payload.amount", json!(250))));
        assert!(hit(leaf("gte", "payload.amount", json!(250))));
        assert!(hit(leaf("lt", "payload.amount", json!(251))));
        assert!(hit(leaf("lte", "payload.amount", json!(250))));
        assert!(hit(leaf("eq", "payload.amount", json!(250.0))));
        // 9 < 10 numerically, although "9" > "10" as text.
        assert!(hit(leaf("lt", "payload.amount", json!(1000))));
    }

    #[test]
    fn mismatched_types_never_order() {
        assert!(!hit(leaf("gt", "payload.amount", json!("100"))));
        assert!(!hit(leaf("lt", "payload.currency", json!(5))));
    }

    #[test]
    fn membership_and_containment() {
        assert!(hit(leaf("in", "payload.currency", json!(["USD", "EUR"]))));
        assert!(hit(leaf("not_in", "payload.currency", json!(["GBP"]))));
        assert!(!hit(leaf("in", "payload.currency", json!("USD"))));
        assert!(hit(leaf("contains", "payload.tags", json!("wire"))));
        assert!(hit(leaf("contains", "type", json!("PAY"))));
        assert!(hit(leaf("not_contains", "payload.tags", json!("cash"))));
        assert!(hit(leaf("contains", "payload", json!("amount"))));
    }

    #[test]
    fn between_is_inclusive_and_needs_two_bounds() {
        assert!(hit(leaf("between", "payload.amount", json!([250, 300]))));
        assert!(hit(leaf("between", "payload.amount", json!([200, 250]))));
        assert!(!hit(leaf("between", "payload.amount", json!([251, 300]))));
        assert!(!hit(leaf("between", "payload.amount", json!([1, 2, 3]))));
        assert!(!hit(leaf("between", "payload.amount", json!(5))));
    }

    #[test]
    fn missing_and_null_fields() {
        assert!(!hit(leaf("eq", "nope.here", json!(1))));
        assert!(hit(leaf("neq", "nope.here", json!(1))));
        assert!(hit(leaf("not_exists", "nope", json!(null))));
        assert!(hit(leaf("not_exists", "note", json!(null))));
        assert!(hit(leaf("exists", "payload.amount", json!(null))));
        assert!(hit(leaf("not_in", "nope", json!(["a"]))));
    }

    #[test]
    fn array_indexing_in_paths() {
        assert!(hit(leaf("eq", "lines.1.sku", json!("B"))));
        assert!(!hit(leaf("eq", "lines.2.sku", json!("B"))));
        assert!(!hit(leaf("eq", "lines.x.sku", json!("B"))));
    }

    #[test]
    fn combinators_and_explanations() {
        let rule = json!({"operator": "and", "conditions": [
            leaf("gt", "payload.amount", json!(100)),
            {"operator": "or", "conditions": [leaf("eq", "type", json!("REFUND")), leaf("eq", "payload.currency", json!("USD"))]},
            {"operator": "not", "condition": leaf("exists", "payload.blocked", json!(null))}]});
        let o = evaluate(&rule, &ev()).unwrap();
        assert!(o.matched);
        assert_eq!(
            o.fields,
            vec![
                "payload.amount",
                "type",
                "payload.currency",
                "payload.blocked"
            ]
        );
        assert!(o.explanation.contains("3 of 3"));
        let failing =
            json!({"operator": "and", "conditions": [leaf("gt", "payload.amount", json!(999))]});
        let o = evaluate(&failing, &ev()).unwrap();
        assert!(!o.matched && o.explanation.contains("0 of 1"));
        assert!(evaluate(&leaf("gt", "payload.amount", json!(5)), &ev())
            .unwrap()
            .explanation
            .contains("actual 250"));
    }

    #[test]
    fn empty_combinators_follow_logic() {
        assert!(hit(json!({"operator": "and", "conditions": []})));
        assert!(!hit(json!({"operator": "or", "conditions": []})));
    }

    #[test]
    fn malformed_rules_are_errors_not_matches() {
        assert_eq!(evaluate(&json!(5), &ev()), Err(RuleError::NotAnObject));
        assert_eq!(
            evaluate(&json!({"field": "a"}), &ev()),
            Err(RuleError::UnknownOperator(String::new()))
        );
        assert_eq!(
            evaluate(&json!({"operator": "xor"}), &ev()),
            Err(RuleError::UnknownOperator("xor".into()))
        );
        assert_eq!(
            evaluate(&json!({"operator": "eq"}), &ev()),
            Err(RuleError::MissingField)
        );
        assert_eq!(
            evaluate(&json!({"operator": "and"}), &ev()),
            Err(RuleError::MissingConditions)
        );
        assert_eq!(
            evaluate(&json!({"operator": "not"}), &ev()),
            Err(RuleError::MissingConditions)
        );
        // An error inside a combinator is not swallowed.
        let bad = json!({"operator": "or", "conditions": [leaf("eq", "type", json!("PAYMENT")), {"operator": "zz"}]});
        assert!(evaluate(&bad, &ev()).is_err());
    }

    #[test]
    fn hostile_depth_is_refused() {
        let mut rule = leaf("exists", "type", json!(null));
        for _ in 0..(MAX_DEPTH + 5) {
            rule = json!({"operator": "not", "condition": rule});
        }
        assert_eq!(evaluate(&rule, &ev()), Err(RuleError::TooDeep));
        let mut ok = leaf("exists", "type", json!(null));
        for _ in 0..MAX_DEPTH {
            ok = json!({"operator": "not", "condition": ok});
        }
        assert!(evaluate(&ok, &ev()).is_ok());
    }
}
