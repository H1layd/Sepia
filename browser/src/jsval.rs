//! The values that cross between the page's JS and the browser.
//!
//! Host functions (js.rs `natives()`) take and return these, never an
//! engine's own types, so the same functions serve Boa today and V8 next:
//! each engine only converts its values to `Val` and back.

#[derive(Clone, Debug, PartialEq)]
pub enum Val {
    Undef,
    Null,
    Bool(bool),
    Num(f64),
    Str(String),
    /// a JS array (nested arrays become nested lists)
    List(Vec<Val>),
}

impl Val {
    /// The number, if this is one.
    pub fn num(&self) -> Option<f64> {
        match self {
            Val::Num(n) => Some(*n),
            _ => None,
        }
    }

    /// JS ToBoolean.
    pub fn truthy(&self) -> bool {
        match self {
            Val::Undef | Val::Null => false,
            Val::Bool(b) => *b,
            Val::Num(n) => *n != 0.0 && !n.is_nan(),
            Val::Str(s) => !s.is_empty(),
            Val::List(_) => true,
        }
    }

    /// JS ToNumber.
    pub fn to_number(&self) -> Option<f64> {
        Some(match self {
            Val::Undef => f64::NAN,
            Val::Null => 0.0,
            Val::Bool(b) => *b as u8 as f64,
            Val::Num(n) => *n,
            Val::Str(s) => {
                let t = s.trim();
                if t.is_empty() {
                    0.0
                } else if let Some(hex) = t.strip_prefix("0x").or_else(|| t.strip_prefix("0X")) {
                    u64::from_str_radix(hex, 16).map(|n| n as f64).unwrap_or(f64::NAN)
                } else {
                    match t {
                        "Infinity" | "+Infinity" => f64::INFINITY,
                        "-Infinity" => f64::NEG_INFINITY,
                        _ if t.contains(|c: char| c.is_ascii_alphabetic() && c != 'e' && c != 'E') => f64::NAN,
                        _ => t.parse().unwrap_or(f64::NAN),
                    }
                }
            }
            Val::List(items) => match items.as_slice() {
                [] => 0.0,
                [one] => one.to_number()?,
                _ => f64::NAN,
            },
        })
    }

    /// JS ToString (arrays join with commas, as in JS).
    pub fn text(&self) -> String {
        match self {
            Val::Undef => "undefined".into(),
            Val::Null => "null".into(),
            Val::Bool(b) => b.to_string(),
            Val::Num(n) => number_text(*n),
            Val::Str(s) => s.clone(),
            Val::List(items) => items
                .iter()
                .map(|v| match v {
                    Val::Undef | Val::Null => String::new(),
                    v => v.text(),
                })
                .collect::<Vec<_>>()
                .join(","),
        }
    }
}

/// Number::toString for the common cases: integers without ".0",
/// NaN and Infinity by name, exponents as JS writes them.
pub fn number_text(n: f64) -> String {
    if n.is_nan() {
        return "NaN".into();
    }
    if n.is_infinite() {
        return if n > 0.0 { "Infinity" } else { "-Infinity" }.into();
    }
    if n == 0.0 {
        return "0".into();
    }
    if n.fract() == 0.0 && n.abs() < 1e21 {
        return format!("{}", n as i128);
    }
    let a = n.abs();
    if !(1e-6..1e21).contains(&a) {
        // 1e-7, 1.5e+21
        let s = format!("{n:e}");
        return match s.split_once('e') {
            Some((m, e)) if !e.starts_with('-') => format!("{m}e+{e}"),
            _ => s,
        };
    }
    format!("{n}")
}

macro_rules! from_number {
    ($($t:ty),*) => {$(
        impl From<$t> for Val {
            fn from(n: $t) -> Val {
                Val::Num(n as f64)
            }
        }
    )*};
}
from_number!(f64, f32, u8, u16, u32, i32, i64, u64, usize);

impl From<bool> for Val {
    fn from(b: bool) -> Val {
        Val::Bool(b)
    }
}

impl From<String> for Val {
    fn from(s: String) -> Val {
        Val::Str(s)
    }
}

impl From<&str> for Val {
    fn from(s: &str) -> Val {
        Val::Str(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn converts_like_js() {
        assert_eq!(Val::Num(3.0).text(), "3");
        assert_eq!(Val::Num(0.5).text(), "0.5");
        assert_eq!(Val::Num(1e21).text(), "1e+21");
        assert_eq!(Val::Num(1e-7).text(), "1e-7");
        assert_eq!(Val::List(vec![Val::Num(1.0), Val::Null, Val::Str("a".into())]).text(), "1,,a");
        assert_eq!(Val::Str(" 12 ".into()).to_number(), Some(12.0));
        assert!(Val::Str("12px".into()).to_number().unwrap().is_nan());
        assert_eq!(Val::Str("".into()).to_number(), Some(0.0));
        assert!(!Val::Num(f64::NAN).truthy());
        assert!(Val::List(vec![]).truthy());
    }
}
