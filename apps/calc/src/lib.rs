// SPDX-License-Identifier: MIT
use crowbar_calc::{Error, Mode, Value, evaluate};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Page {
    Standard,
    Scientific,
    Programmer,
}
impl Page {
    pub fn label(self) -> &'static str {
        match self {
            Self::Standard => "Standard",
            Self::Scientific => "Scientific",
            Self::Programmer => "Programmer",
        }
    }
}
#[derive(Clone)]
pub struct Entry {
    pub expression: String,
    pub value: Value,
    pub page: Page,
    pub width: u8,
}
pub struct Calculator {
    pub page: Page,
    pub width: u8,
    pub history: Vec<Entry>,
}
impl Default for Calculator {
    fn default() -> Self {
        Self {
            page: Page::Standard,
            width: 64,
            history: Vec::new(),
        }
    }
}
impl Calculator {
    pub fn evaluate(&self, expression: &str) -> Result<Value, Error> {
        evaluate(
            expression,
            if self.page == Page::Programmer {
                Mode::Word(self.width)
            } else {
                Mode::Real
            },
        )
    }
    pub fn commit(&mut self, expression: &str) -> Result<Value, Error> {
        let value = self.evaluate(expression)?;
        if self
            .history
            .first()
            .is_none_or(|e| e.expression != expression || e.page != self.page || e.width != self.width)
        {
            self.history.insert(
                0,
                Entry {
                    expression: expression.to_string(),
                    value,
                    page: self.page,
                    width: self.width,
                },
            );
            self.history.truncate(100);
        }
        Ok(value)
    }
    pub fn reuse(&mut self, index: usize) -> Option<String> {
        let entry = self.history.get(index)?;
        self.page = entry.page;
        self.width = entry.width;
        Some(entry.expression.clone())
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn history_reuses_exact_mode_and_word_width() {
        let mut c = Calculator {
            page: Page::Programmer,
            width: 8,
            ..Default::default()
        };
        assert_eq!(c.commit("NOT 0"), Ok(Value::Word(255)));
        c.page = Page::Standard;
        c.width = 64;
        let s = c.reuse(0).unwrap();
        assert_eq!(c.evaluate(&s), Ok(Value::Word(255)));
        assert_eq!(c.width, 8);
    }
    #[test]
    fn history_is_bounded_and_errors_are_not_entries() {
        let mut c = Calculator::default();
        assert!(c.commit("1/0").is_err());
        assert!(c.history.is_empty());
        for n in 0..110 {
            c.commit(&n.to_string()).unwrap();
        }
        c.commit("109").unwrap();
        assert_eq!(c.history.len(), 100);
        assert_eq!(c.history[0].expression, "109");
        assert_eq!(c.history[99].expression, "10");
    }
}
