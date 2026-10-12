#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Operation {
    Add,
    Subtract,
    Multiply,
    Divide,
}

impl Operation {
    pub fn label(self) -> &'static str {
        match self {
            Self::Add => "+",
            Self::Subtract => "−",
            Self::Multiply => "×",
            Self::Divide => "÷",
        }
    }

    fn evaluate(self, left: f64, right: f64) -> Result<f64, &'static str> {
        if self == Self::Divide && right == 0.0 {
            return Err("Cannot divide by zero");
        }
        finite(match self {
            Self::Add => left + right,
            Self::Subtract => left - right,
            Self::Multiply => left * right,
            Self::Divide => left / right,
        })
    }
}

#[derive(Clone, Copy)]
pub enum Command {
    Digit(char),
    Decimal,
    Binary(Operation),
    Equals,
    Clear,
    ClearEntry,
    Backspace,
    Negate,
    Percent,
    Reciprocal,
    Square,
    SquareRoot,
    MemoryClear,
    MemoryRecall,
    MemoryStore,
    MemoryAdd,
    MemorySubtract,
}

pub struct HistoryEntry {
    pub expression: String,
    pub result: String,
}

pub struct Calculator {
    pub input: String,
    pub expression: String,
    pub error: Option<&'static str>,
    pub memory: Option<f64>,
    pub history: Vec<HistoryEntry>,
    accumulator: Option<f64>,
    pending: Option<Operation>,
    repeat: Option<(Operation, f64)>,
    replace_input: bool,
    operand_entered: bool,
}

impl Default for Calculator {
    fn default() -> Self {
        Self {
            input: "0".into(),
            expression: String::new(),
            error: None,
            memory: None,
            history: Vec::new(),
            accumulator: None,
            pending: None,
            repeat: None,
            replace_input: true,
            operand_entered: false,
        }
    }
}

impl Calculator {
    pub fn display(&self) -> &str {
        self.error.unwrap_or(&self.input)
    }

    fn value(&self) -> Result<f64, &'static str> {
        self.input.parse().map_err(|_| "Invalid input")
    }

    pub fn press(&mut self, command: Command) {
        if self.error.is_some() {
            match command {
                Command::Digit(_) | Command::Decimal | Command::Clear | Command::ClearEntry => {
                    self.clear();
                }
                Command::MemoryClear => self.memory = None,
                _ => return,
            }
        }
        if let Err(error) = self.apply(command) {
            self.error = Some(error);
            self.replace_input = true;
        }
    }

    fn clear(&mut self) {
        self.input = "0".into();
        self.expression.clear();
        self.error = None;
        self.accumulator = None;
        self.pending = None;
        self.repeat = None;
        self.replace_input = true;
        self.operand_entered = false;
    }

    fn begin_entry(&mut self) {
        if self.replace_input {
            self.input = "0".into();
            self.replace_input = false;
            if self.pending.is_none() {
                self.expression.clear();
                self.accumulator = None;
                self.repeat = None;
            }
        }
        self.operand_entered = true;
    }

    fn set_value(&mut self, value: f64) -> Result<(), &'static str> {
        self.input = format_number(finite(value)?);
        self.replace_input = true;
        Ok(())
    }

    fn apply(&mut self, command: Command) -> Result<(), &'static str> {
        match command {
            Command::Digit(digit) => {
                if !digit.is_ascii_digit() {
                    return Ok(());
                }
                self.begin_entry();
                if self.input.chars().filter(char::is_ascii_digit).count() < 16 {
                    if self.input == "0" {
                        self.input.clear();
                    } else if self.input == "-0" {
                        self.input = "-".into();
                    }
                    self.input.push(digit);
                }
            }
            Command::Decimal => {
                self.begin_entry();
                if !self.input.contains('.') {
                    self.input.push('.');
                }
            }
            Command::Clear => self.clear(),
            Command::ClearEntry => {
                self.input = "0".into();
                self.replace_input = true;
                self.operand_entered = true;
                self.repeat = None;
                if self.pending.is_none() {
                    self.expression.clear();
                    self.accumulator = None;
                }
            }
            Command::Backspace => {
                if !self.replace_input {
                    self.input.pop();
                    if self.input.is_empty() || self.input == "-" {
                        self.input = "0".into();
                    }
                }
            }
            Command::Binary(operation) => {
                let value = self.value()?;
                let result = match (self.accumulator, self.pending) {
                    (Some(left), Some(pending)) if self.operand_entered => {
                        pending.evaluate(left, value)?
                    }
                    (Some(left), Some(_)) => left,
                    _ => value,
                };
                self.set_value(result)?;
                self.accumulator = Some(result);
                self.pending = Some(operation);
                self.repeat = None;
                self.operand_entered = false;
                self.expression = format!("{} {}", self.input, operation.label());
            }
            Command::Equals => {
                let value = self.value()?;
                let calculation = match (self.accumulator, self.pending, self.repeat) {
                    (Some(left), Some(operation), _) => Some((left, operation, value)),
                    (_, None, Some((operation, right))) => Some((value, operation, right)),
                    _ => None,
                };
                if let Some((left, operation, right)) = calculation {
                    self.set_value(operation.evaluate(left, right)?)?;
                    self.expression = format!(
                        "{} {} {} =",
                        format_number(left),
                        operation.label(),
                        format_number(right)
                    );
                    self.history.push(HistoryEntry {
                        expression: self.expression.clone(),
                        result: self.input.clone(),
                    });
                    if self.history.len() > 100 {
                        self.history.remove(0);
                    }
                    self.repeat = Some((operation, right));
                } else {
                    self.expression = format!("{} =", self.input);
                    self.replace_input = true;
                }
                self.pending = None;
                self.accumulator = None;
                self.operand_entered = false;
            }
            Command::Negate => {
                if self.input.starts_with('-') {
                    self.input.remove(0);
                } else if self.value()? != 0.0 {
                    self.input.insert(0, '-');
                }
                self.operand_entered = true;
            }
            Command::Percent | Command::Reciprocal | Command::Square | Command::SquareRoot => {
                let value = self.value()?;
                let result = match command {
                    Command::Percent => match self.pending {
                        Some(Operation::Add | Operation::Subtract) => {
                            self.accumulator.unwrap_or(0.0) * (value / 100.0)
                        }
                        _ => value / 100.0,
                    },
                    Command::Reciprocal => Operation::Divide.evaluate(1.0, value)?,
                    Command::Square => value * value,
                    Command::SquareRoot if value < 0.0 => return Err("Invalid input"),
                    Command::SquareRoot => value.sqrt(),
                    _ => return Err("Invalid operation"),
                };
                self.set_value(result)?;
                self.operand_entered = true;
                self.repeat = None;
            }
            Command::MemoryClear => self.memory = None,
            Command::MemoryRecall => {
                if let Some(value) = self.memory {
                    if self.pending.is_none() {
                        self.expression.clear();
                        self.repeat = None;
                    }
                    self.set_value(value)?;
                    self.operand_entered = true;
                }
            }
            Command::MemoryStore | Command::MemoryAdd | Command::MemorySubtract => {
                let value = self.value()?;
                let result = match command {
                    Command::MemoryAdd => self.memory.unwrap_or(0.0) + value,
                    Command::MemorySubtract => self.memory.unwrap_or(0.0) - value,
                    _ => value,
                };
                self.memory = Some(finite(result)?);
                self.replace_input = true;
            }
        }
        Ok(())
    }

    pub fn recall_history(&mut self, index: usize) {
        if let Some(entry) = self.history.get(index) {
            let input = entry.result.clone();
            self.clear();
            self.input = input;
            self.operand_entered = true;
        }
    }
}

fn finite(value: f64) -> Result<f64, &'static str> {
    value.is_finite().then_some(value).ok_or("Overflow")
}

pub fn format_number(value: f64) -> String {
    if value == 0.0 {
        return "0".into();
    }
    let magnitude = value.abs();
    if !(1e-9..1e16).contains(&magnitude) {
        let formatted = format!("{value:.14e}");
        if let Some((coefficient, exponent)) = formatted.split_once('e') {
            return format!(
                "{}e{}",
                coefficient.trim_end_matches('0').trim_end_matches('.'),
                exponent
            );
        }
        return formatted;
    }
    let decimals = (14 - magnitude.log10().floor() as i32).clamp(0, 23) as usize;
    let formatted = format!("{value:.decimals$}");
    if formatted.contains('.') {
        formatted.trim_end_matches('0').trim_end_matches('.').into()
    } else {
        formatted
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn enter(calculator: &mut Calculator, input: &str) {
        for character in input.chars() {
            calculator.press(match character {
                '.' => Command::Decimal,
                _ => Command::Digit(character),
            });
        }
    }

    #[test]
    fn standard_mode_evaluates_left_to_right_and_repeats_equals() {
        let mut calculator = Calculator::default();
        enter(&mut calculator, "2");
        calculator.press(Command::Binary(Operation::Add));
        enter(&mut calculator, "3");
        calculator.press(Command::Binary(Operation::Multiply));
        enter(&mut calculator, "4");
        calculator.press(Command::Equals);
        assert_eq!(calculator.display(), "20");
        calculator.press(Command::Equals);
        assert_eq!(calculator.display(), "80");
        assert_eq!(calculator.history.len(), 2);
        enter(&mut calculator, "7");
        calculator.press(Command::Equals);
        assert_eq!(calculator.display(), "7");
    }

    #[test]
    fn changing_operator_does_not_evaluate() {
        let mut calculator = Calculator::default();
        enter(&mut calculator, "8");
        calculator.press(Command::Binary(Operation::Add));
        calculator.press(Command::Binary(Operation::Subtract));
        enter(&mut calculator, "3");
        calculator.press(Command::Equals);
        assert_eq!(calculator.display(), "5");
    }

    #[test]
    fn percent_uses_base_for_addition_and_fraction_for_multiplication() {
        for (operation, expected) in [(Operation::Add, "220"), (Operation::Multiply, "20")] {
            let mut calculator = Calculator::default();
            enter(&mut calculator, "200");
            calculator.press(Command::Binary(operation));
            enter(&mut calculator, "10");
            calculator.press(Command::Percent);
            calculator.press(Command::Equals);
            assert_eq!(calculator.display(), expected);
        }
    }

    #[test]
    fn decimals_clear_entry_and_backspace() {
        let mut calculator = Calculator::default();
        enter(&mut calculator, "0.12");
        calculator.press(Command::Backspace);
        assert_eq!(calculator.display(), "0.1");
        calculator.press(Command::Binary(Operation::Add));
        enter(&mut calculator, "9");
        calculator.press(Command::ClearEntry);
        enter(&mut calculator, "0.2");
        calculator.press(Command::Equals);
        assert_eq!(calculator.display(), "0.3");
        calculator.press(Command::Backspace);
        assert_eq!(calculator.display(), "0.3");
    }

    #[test]
    fn errors_recover_on_new_input() {
        let mut calculator = Calculator::default();
        enter(&mut calculator, "5");
        calculator.press(Command::Binary(Operation::Divide));
        enter(&mut calculator, "0");
        calculator.press(Command::Equals);
        assert_eq!(calculator.display(), "Cannot divide by zero");
        enter(&mut calculator, "9");
        calculator.press(Command::SquareRoot);
        assert_eq!(calculator.display(), "3");
        calculator.press(Command::Negate);
        calculator.press(Command::SquareRoot);
        assert_eq!(calculator.display(), "Invalid input");
        calculator.press(Command::ClearEntry);
        assert_eq!(calculator.display(), "0");
    }

    #[test]
    fn memory_survives_clear_and_can_be_used_as_an_operand() {
        let mut calculator = Calculator::default();
        enter(&mut calculator, "12");
        calculator.press(Command::MemoryStore);
        calculator.press(Command::Clear);
        enter(&mut calculator, "3");
        calculator.press(Command::MemoryAdd);
        calculator.press(Command::Binary(Operation::Multiply));
        calculator.press(Command::MemoryRecall);
        calculator.press(Command::Equals);
        assert_eq!(calculator.display(), "45");
        calculator.press(Command::MemorySubtract);
        assert_eq!(calculator.memory, Some(-30.0));
        calculator.press(Command::MemoryClear);
        assert_eq!(calculator.memory, None);
    }

    #[test]
    fn reciprocal_square_and_history_recall() {
        let mut calculator = Calculator::default();
        enter(&mut calculator, "4");
        calculator.press(Command::Reciprocal);
        assert_eq!(calculator.display(), "0.25");
        calculator.press(Command::Square);
        assert_eq!(calculator.display(), "0.0625");
        calculator.press(Command::Binary(Operation::Add));
        enter(&mut calculator, "1");
        calculator.press(Command::Equals);
        calculator.press(Command::Clear);
        calculator.recall_history(0);
        assert_eq!(calculator.display(), "1.0625");
        calculator.recall_history(100);
        assert_eq!(calculator.display(), "1.0625");
    }

    #[test]
    fn formatting_preserves_small_numbers_and_removes_rounding_noise() {
        assert_eq!(format_number(0.1 + 0.2), "0.3");
        assert_eq!(format_number(1e-20), "1e-20");
        assert_eq!(format_number(-0.0), "0");
        assert_eq!(finite(f64::INFINITY), Err("Overflow"));
    }
}
