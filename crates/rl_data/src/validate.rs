//! Collecting validation problems with the field they concern.

/// One problem found in a data file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    /// Dotted path of the offending field, e.g. `ground.run_speed`.
    pub field: String,
    /// What is wrong with it.
    pub message: String,
}

impl std::fmt::Display for Problem {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}: {}", self.field, self.message)
    }
}

/// Accumulates [`Problem`]s, so a file reports every mistake at once rather
/// than one per edit-and-reload cycle.
#[derive(Debug, Default)]
pub(crate) struct Validator {
    problems: Vec<Problem>,
}

impl Validator {
    /// Records a problem with `field`.
    pub(crate) fn fail(&mut self, field: &str, message: &str) {
        self.problems.push(Problem {
            field: field.to_owned(),
            message: message.to_owned(),
        });
    }

    /// Requires a finite value above zero.
    pub(crate) fn positive(&mut self, field: &str, value: f32) {
        if !(value.is_finite() && value > 0.0) {
            self.fail(field, &format!("must be positive, got {value}"));
        }
    }

    /// Requires a finite value of at least zero.
    pub(crate) fn non_negative(&mut self, field: &str, value: f32) {
        if !(value.is_finite() && value >= 0.0) {
            self.fail(field, &format!("must not be negative, got {value}"));
        }
    }

    /// Requires `min <= value <= max`.
    pub(crate) fn range(&mut self, field: &str, value: f32, min: f32, max: f32) {
        if !(min..=max).contains(&value) {
            self.fail(field, &format!("must be within {min}..={max}, got {value}"));
        }
    }

    /// The problems found so far.
    pub(crate) fn finish(self) -> Vec<Problem> {
        self.problems
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_every_problem_with_its_field() {
        let mut check = Validator::default();
        check.positive("a", 1.0);
        check.positive("b", 0.0);
        check.non_negative("c", f32::NAN);
        check.range("d", 2.0, 0.0, 1.0);
        let problems = check.finish();
        let fields: Vec<&str> = problems.iter().map(|p| p.field.as_str()).collect();
        assert_eq!(fields, ["b", "c", "d"]);
        assert_eq!(problems[2].to_string(), "d: must be within 0..=1, got 2");
    }
}
