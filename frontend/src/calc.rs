//! Calculadora da busca do launcher.
//!
//! Parser próprio (nada de `eval`): `+`, `-`, `*`, `/`, `%`, `^`, parênteses,
//! unário e decimais com `.` ou `,`. Entrada vazia, sintaxe inválida,
//! divisão por zero e resultados não finitos devolvem `None`.

/// Limite defensivo: o campo de busca não precisa de mais que isso.
const MAX_INPUT: usize = 160;

/// Avalia a expressão inteira; `None` quando não é uma conta válida.
pub fn evaluate(input: &str) -> Option<f64> {
    let trimmed = input.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_INPUT {
        return None;
    }
    if !trimmed.chars().any(|character| character.is_ascii_digit()) {
        return None;
    }

    let mut parser = Parser {
        chars: trimmed.chars().collect(),
        pos: 0,
    };
    let value = parser.expression()?;
    parser.skip_spaces();
    if parser.pos != parser.chars.len() || !value.is_finite() {
        return None;
    }
    Some(value)
}

/// Formata o resultado sem `-0`, sem zeros à direita e com no máximo 10 casas.
pub fn format_result(value: f64) -> String {
    if value == 0.0 {
        return "0".to_owned();
    }
    let rounded = (value * 1e10).round() / 1e10;
    if rounded.fract() == 0.0 && rounded.abs() < 1e15 {
        format!("{}", rounded as i64)
    } else {
        let text = format!("{rounded:.10}");
        text.trim_end_matches('0').trim_end_matches('.').to_owned()
    }
}

struct Parser {
    chars: Vec<char>,
    pos: usize,
}

impl Parser {
    fn peek(&self) -> Option<char> {
        self.chars.get(self.pos).copied()
    }

    fn bump(&mut self) -> Option<char> {
        let character = self.peek();
        if character.is_some() {
            self.pos += 1;
        }
        character
    }

    fn skip_spaces(&mut self) {
        while matches!(self.peek(), Some(' ') | Some('\t')) {
            self.pos += 1;
        }
    }

    fn expression(&mut self) -> Option<f64> {
        let mut value = self.term()?;
        loop {
            self.skip_spaces();
            match self.peek() {
                Some('+') => {
                    self.bump();
                    value += self.term()?;
                }
                Some('-') => {
                    self.bump();
                    value -= self.term()?;
                }
                _ => break,
            }
        }
        Some(value)
    }

    fn term(&mut self) -> Option<f64> {
        let mut value = self.unary()?;
        loop {
            self.skip_spaces();
            match self.peek() {
                Some('*') => {
                    self.bump();
                    value *= self.unary()?;
                }
                Some('/') => {
                    self.bump();
                    let divisor = self.unary()?;
                    if divisor == 0.0 {
                        return None;
                    }
                    value /= divisor;
                }
                Some('%') => {
                    self.bump();
                    let divisor = self.unary()?;
                    if divisor == 0.0 {
                        return None;
                    }
                    value %= divisor;
                }
                _ => break,
            }
        }
        Some(value)
    }

    /// Unário antes da potência: `-3^2` é `-(3^2)`.
    fn unary(&mut self) -> Option<f64> {
        self.skip_spaces();
        match self.peek() {
            Some('-') => {
                self.bump();
                Some(-self.unary()?)
            }
            Some('+') => {
                self.bump();
                self.unary()
            }
            _ => self.power(),
        }
    }

    /// Potência associativa à direita: `2^3^2` é `2^(3^2)`.
    fn power(&mut self) -> Option<f64> {
        let base = self.primary()?;
        self.skip_spaces();
        if self.peek() == Some('^') {
            self.bump();
            let exponent = self.unary()?;
            let value = base.powf(exponent);
            if value.is_finite() {
                Some(value)
            } else {
                None
            }
        } else {
            Some(base)
        }
    }

    fn primary(&mut self) -> Option<f64> {
        self.skip_spaces();
        match self.peek()? {
            '(' => {
                self.bump();
                let value = self.expression()?;
                self.skip_spaces();
                if self.bump()? != ')' {
                    return None;
                }
                Some(value)
            }
            character if character.is_ascii_digit() || character == '.' || character == ',' => {
                self.number()
            }
            _ => None,
        }
    }

    fn number(&mut self) -> Option<f64> {
        let mut text = String::new();
        let mut has_separator = false;
        let mut has_digit = false;
        while let Some(character) = self.peek() {
            if character.is_ascii_digit() {
                has_digit = true;
                text.push(character);
                self.bump();
            } else if (character == '.' || character == ',') && !has_separator {
                has_separator = true;
                text.push('.');
                self.bump();
            } else {
                break;
            }
        }
        if !has_digit {
            return None;
        }
        text.parse::<f64>().ok()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn evaluates_arithmetic_with_precedence() {
        assert_eq!(evaluate("2+2*3"), Some(8.0));
        assert_eq!(evaluate("(2+3)*4"), Some(20.0));
        assert_eq!(evaluate("10/4"), Some(2.5));
        assert_eq!(evaluate("7%4"), Some(3.0));
        assert_eq!(evaluate("2^10"), Some(1024.0));
        assert_eq!(evaluate("2^3^2"), Some(512.0));
        assert_eq!(evaluate("-3^2"), Some(-9.0));
        assert_eq!(evaluate("2*-3"), Some(-6.0));
        assert_eq!(evaluate("1,5+2"), Some(3.5));
        assert_eq!(evaluate(" 2 + 2 "), Some(4.0));
        assert_eq!(evaluate("-(1+2)*2"), Some(-6.0));
    }

    #[test]
    fn rejects_invalid_input() {
        assert_eq!(evaluate(""), None);
        assert_eq!(evaluate("   "), None);
        assert_eq!(evaluate("abc"), None);
        assert_eq!(evaluate("2+"), None);
        assert_eq!(evaluate("2**3"), None);
        assert_eq!(evaluate("(2+3"), None);
        assert_eq!(evaluate("1/0"), None);
        assert_eq!(evaluate("1%0"), None);
        assert_eq!(evaluate("firefox"), None);
        assert_eq!(evaluate("2 3"), None);
        assert_eq!(evaluate(&"1+".repeat(100)), None);
    }

    #[test]
    fn formats_results() {
        assert_eq!(format_result(8.0), "8");
        assert_eq!(format_result(2.5), "2.5");
        assert_eq!(format_result(-0.0), "0");
        assert_eq!(format_result(1.0 / 3.0), "0.3333333333");
        assert_eq!(format_result(1024.0), "1024");
    }
}
