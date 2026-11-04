use crate::pickle::Value as V;
use std::ops::Index;
pub static NONE: V = V::None;
impl Index<usize> for V {
    type Output = V;
    fn index(&self, at: usize) -> &V {
        self.seq().get(at).unwrap_or(&NONE)
    }
}
impl Index<&str> for V {
    type Output = V;
    fn index(&self, key: &str) -> &V {
        self.pairs()
            .iter()
            .find(|(k, _)| k.text() == key)
            .map(|(_, v)| v)
            .unwrap_or(&NONE)
    }
}
impl From<String> for V {
    fn from(v: String) -> Self {
        v.as_str().into()
    }
}
impl From<i32> for V {
    fn from(v: i32) -> Self {
        (v as i64).into()
    }
}
impl From<usize> for V {
    fn from(v: usize) -> Self {
        (v as i64).into()
    }
}
impl From<f64> for V {
    fn from(v: f64) -> Self {
        Self::Float(v.to_bits())
    }
}
impl V {
    pub fn seq(&self) -> &[V] {
        match self {
            Self::Tuple(v) | Self::List(v) => v,
            _ => &[],
        }
    }
    pub fn pairs(&self) -> &[(V, V)] {
        match self {
            Self::Dict(v) => v,
            _ => &[],
        }
    }
    pub fn text(&self) -> String {
        match self {
            Self::Bytes(b) => b.iter().map(|&b| char::from(b)).collect(),
            Self::Unicode(s) | Self::Int(s) => s.clone(),
            Self::None => String::new(),
            Self::Float(bits) => f64::from_bits(*bits).to_string(),
            _ => format!("{self:?}"),
        }
    }
    pub fn raw(&self) -> Result<&[u8], String> {
        match self {
            Self::Bytes(b) => Ok(b),
            _ => Err("expected byte string".into()),
        }
    }
    pub fn int(&self) -> i64 {
        match self {
            Self::Float(b) => f64::from_bits(*b) as i64,
            _ => self.as_i64().unwrap_or(0),
        }
    }
    pub fn float(&self) -> f64 {
        match self {
            Self::Float(b) => f64::from_bits(*b),
            Self::Int(s) => s.parse().unwrap_or(0.),
            _ => 0.,
        }
    }
    pub fn is_none(&self) -> bool {
        matches!(self, Self::None)
    }
    pub fn truth(&self) -> bool {
        match self {
            Self::None => false,
            Self::Int(_) => self.int() != 0,
            Self::Float(_) => self.float() != 0.,
            Self::Bytes(b) => !b.is_empty(),
            Self::Unicode(s) => !s.is_empty(),
            Self::Tuple(v) | Self::List(v) => !v.is_empty(),
            Self::Dict(v) => !v.is_empty(),
            _ => true,
        }
    }
    pub fn or(&self, other: V) -> V {
        if self.is_none() { other } else { self.clone() }
    }
    pub fn key(&self, key: &V) -> V {
        self.pairs()
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .unwrap_or(Self::None)
    }
    pub fn set(&mut self, key: impl Into<V>, value: impl Into<V>) {
        let key = key.into();
        let value = value.into();
        if let Self::Dict(pairs) = self {
            if let Some((_, v)) = pairs.iter_mut().find(|(k, _)| *k == key) {
                *v = value;
            } else {
                pairs.push((key, value));
            }
        }
    }
    pub fn instance(name: &str, state: V) -> V {
        V::Instance {
            module: "util".into(),
            name: name.into(),
            args: vec![],
            state: Box::new(state),
        }
    }
    pub fn state(&self) -> &V {
        match self {
            Self::Instance { state, .. } => state,
            _ => &NONE,
        }
    }
}
#[macro_export]
macro_rules! t { ($($x:expr),* $(,)?) => { $crate::pickle::Value::Tuple(vec![$(($x).into()),*]) }; }
#[macro_export]
macro_rules! l { ($($x:expr),* $(,)?) => { $crate::pickle::Value::List(vec![$(($x).into()),*]) }; }
#[macro_export]
macro_rules! d { ($($k:expr => $v:expr),* $(,)?) => { $crate::pickle::Value::Dict(vec![$((($k).into(),($v).into())),*]) }; }
pub fn row(header: V, line: Vec<V>) -> V {
    V::instance("Row", crate::d!["header"=>header,"line"=>V::List(line)])
}
pub fn rows(header: V, lines: Vec<Vec<V>>) -> V {
    V::List(lines.into_iter().map(|l| row(header.clone(), l)).collect())
}
pub fn rowset(header: V, lines: Vec<Vec<V>>) -> V {
    V::instance(
        "Rowset",
        crate::d!["header"=>header,"lines"=>V::List(lines.into_iter().map(V::List).collect())],
    )
}
