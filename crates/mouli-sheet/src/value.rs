//! Cell values.

/// Evaluated value of a cell.
#[derive(Clone, Debug, PartialEq, Default)]
pub enum CellValue {
    /// Nothing.
    #[default]
    Empty,
    /// Number.
    Number(f64),
    /// Text.
    Text(String),
    /// Boolean.
    Bool(bool),
    /// Error code such as `#DIV/0!`.
    Error(String),
}

/// Standard error codes.
pub mod err {
    /// Wrong type of argument.
    pub const VALUE: &str = "#VALUE!";
    /// Division by zero.
    pub const DIV0: &str = "#DIV/0!";
    /// Unknown function or name.
    pub const NAME: &str = "#NAME?";
    /// Reference cycle.
    pub const CYCLE: &str = "#CYCLE!";
    /// Bad formula syntax.
    pub const PARSE: &str = "#PARSE!";
    /// Invalid numeric result.
    pub const NUM: &str = "#NUM!";
}

impl CellValue {
    /// Error value with `code`.
    #[must_use]
    pub fn error(code: &str) -> Self {
        Self::Error(code.to_owned())
    }

    /// Numeric view (empty = 0, booleans = 0/1, numeric text parsed).
    pub fn as_number(&self) -> Result<f64, CellValue> {
        match self {
            Self::Number(n) => Ok(*n),
            Self::Bool(b) => Ok(if *b { 1.0 } else { 0.0 }),
            Self::Text(s) => s.trim().parse::<f64>().map_err(|_| Self::error(err::VALUE)),
            Self::Empty => Ok(0.0),
            e @ Self::Error(_) => Err(e.clone()),
        }
    }

    /// Truth value (numbers: non-zero; text "TRUE"/"FALSE").
    pub fn as_bool(&self) -> Result<bool, CellValue> {
        match self {
            Self::Bool(b) => Ok(*b),
            Self::Text(s) if s.eq_ignore_ascii_case("true") => Ok(true),
            Self::Text(s) if s.eq_ignore_ascii_case("false") => Ok(false),
            other => other.as_number().map(|n| n != 0.0),
        }
    }

    /// Is an error value.
    #[must_use]
    pub fn is_error(&self) -> bool {
        matches!(self, Self::Error(_))
    }

    /// Text shown in the grid.
    #[must_use]
    pub fn display(&self) -> String {
        match self {
            Self::Empty => String::new(),
            Self::Number(n) => {
                if n.fract() == 0.0 && n.abs() < 1e15 {
                    format!("{}", *n as i64)
                } else {
                    format!("{n}")
                }
            }
            Self::Text(s) => s.clone(),
            Self::Bool(b) => if *b { "TRUE" } else { "FALSE" }.to_owned(),
            Self::Error(e) => e.clone(),
        }
    }

    /// Parse what a user typed into a literal value (formulas are handled elsewhere).
    #[must_use]
    pub fn from_input(raw: &str) -> Self {
        let t = raw.trim();
        if t.is_empty() {
            Self::Empty
        } else if let Ok(n) = t.parse::<f64>() {
            Self::Number(n)
        } else if t.eq_ignore_ascii_case("true") {
            Self::Bool(true)
        } else if t.eq_ignore_ascii_case("false") {
            Self::Bool(false)
        } else {
            Self::Text(raw.to_owned())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn conversions_and_display() {
        assert_eq!(CellValue::Text(" 4.5 ".into()).as_number(), Ok(4.5));
        assert!(CellValue::Text("x".into()).as_number().is_err());
        assert_eq!(CellValue::Number(3.0).display(), "3");
        assert_eq!(CellValue::Number(0.25).display(), "0.25");
        assert_eq!(CellValue::from_input("TRUE"), CellValue::Bool(true));
        assert_eq!(CellValue::from_input("  "), CellValue::Empty);
        assert_eq!(CellValue::from_input("hi"), CellValue::Text("hi".into()));
        assert_eq!(CellValue::Text("false".into()).as_bool(), Ok(false));
    }
}
