//! Python 2.2 protocol 0/1 values. GLOBAL/REDUCE/OBJ are data, never code.
//! Memo entries retain identity while parsing; cyclic inputs are rejected when
//! converting to the acyclic game protocol value tree.
use serde::{Deserialize, Serialize};
use std::{cell::RefCell, collections::HashMap, rc::Rc};

const MAX_DEPTH: usize = 128;
const MAX_NODES: usize = 1_000_000;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type", content = "value", rename_all = "snake_case")]
pub enum Value {
    None,
    /// Decimal text preserves Python's arbitrary precision longs over IPC.
    Int(String),
    /// IEEE bits preserve NaN/infinity and negative zero over JSON IPC.
    Float(u64),
    Bytes(Vec<u8>),
    Unicode(String),
    Tuple(Vec<Value>),
    List(Vec<Value>),
    Dict(Vec<(Value, Value)>),
    Global {
        module: String,
        name: String,
    },
    Instance {
        module: String,
        name: String,
        args: Vec<Value>,
        state: Box<Value>,
    },
    Reduce {
        module: String,
        name: String,
        args: Vec<Value>,
    },
    Persistent(Box<Value>),
}

impl From<i64> for Value {
    fn from(value: i64) -> Self {
        Self::Int(value.to_string())
    }
}
impl From<&str> for Value {
    fn from(value: &str) -> Self {
        if value.chars().all(|c| c as u32 <= 255) {
            Self::Bytes(value.chars().map(|c| c as u8).collect())
        } else {
            Self::Unicode(value.into())
        }
    }
}
impl Value {
    pub fn as_i64(&self) -> Result<i64, String> {
        match self {
            Self::Int(n) => n.parse().map_err(|_| "integer out of range".into()),
            _ => Err("expected integer".into()),
        }
    }
}

fn decimal(raw: &str) -> Result<String, String> {
    let raw = raw.strip_suffix('L').unwrap_or(raw);
    let digits = raw.trim_start_matches(['-', '+']);
    if digits.is_empty()
        || !digits.bytes().all(|b| b.is_ascii_digit())
        || raw.starts_with("--")
        || raw.starts_with("++")
        || raw.starts_with("-+")
        || raw.starts_with("+-")
    {
        return Err("invalid decimal integer".into());
    }
    let digits = digits.trim_start_matches('0');
    Ok(if digits.is_empty() {
        "0".into()
    } else if raw.starts_with('-') {
        format!("-{digits}")
    } else {
        digits.into()
    })
}

pub fn dumps(value: &Value) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut budget = MAX_NODES;
    dump(value, &mut out, 0, &mut budget)?;
    out.push(b'.');
    if out.len() > crate::MAX_FRAME {
        return Err("pickle too large".into());
    }
    Ok(out)
}

fn global(out: &mut Vec<u8>, module: &str, name: &str) -> Result<(), String> {
    out.push(b'c');
    for word in [module, name] {
        if word.contains(['\n', '\r']) || word.chars().any(|c| c as u32 > 255) {
            return Err("invalid global name".into());
        }
        out.extend(word.chars().map(|c| c as u8));
        out.push(b'\n');
    }
    Ok(())
}

fn dump(v: &Value, out: &mut Vec<u8>, depth: usize, budget: &mut usize) -> Result<(), String> {
    if depth > MAX_DEPTH || *budget == 0 {
        return Err("pickle nesting/node limit".into());
    }
    *budget -= 1;
    match v {
        Value::None => out.push(b'N'),
        Value::Int(raw) => {
            let raw = decimal(raw)?;
            match raw.parse::<i64>() {
                Ok(n @ 0..=255) => out.extend([b'K', n as u8]),
                Ok(n @ 256..=65535) => {
                    out.push(b'M');
                    out.extend((n as u16).to_le_bytes());
                }
                Ok(n @ -2147483648..=2147483647) => {
                    out.push(b'J');
                    out.extend((n as i32).to_le_bytes());
                }
                _ => {
                    out.push(b'L');
                    out.extend(raw.bytes());
                    out.extend(b"L\n");
                }
            }
        }
        Value::Float(bits) => {
            out.push(b'G');
            out.extend(bits.to_be_bytes());
        }
        Value::Bytes(raw) => {
            if raw.len() > crate::MAX_FRAME {
                return Err("string too large".into());
            }
            if raw.len() < 256 {
                out.extend([b'U', raw.len() as u8]);
            } else {
                out.push(b'T');
                out.extend((raw.len() as u32).to_le_bytes());
            }
            out.extend(raw);
        }
        Value::Unicode(text) => {
            if text.len() > crate::MAX_FRAME {
                return Err("string too large".into());
            }
            out.push(b'X');
            out.extend((text.len() as u32).to_le_bytes());
            out.extend(text.bytes());
        }
        Value::Tuple(items) if items.is_empty() => out.push(b')'),
        Value::Tuple(items) => {
            out.push(b'(');
            for item in items {
                dump(item, out, depth + 1, budget)?;
            }
            out.push(b't');
        }
        Value::List(items) => {
            out.push(b']');
            if !items.is_empty() {
                out.push(b'(');
                for item in items {
                    dump(item, out, depth + 1, budget)?;
                }
                out.push(b'e');
            }
        }
        Value::Dict(items) => {
            out.push(b'}');
            if !items.is_empty() {
                out.push(b'(');
                for (key, value) in items {
                    dump(key, out, depth + 1, budget)?;
                    dump(value, out, depth + 1, budget)?;
                }
                out.push(b'u');
            }
        }
        Value::Global { module, name } => global(out, module, name)?,
        Value::Instance {
            module,
            name,
            args,
            state,
        } => {
            out.push(b'(');
            global(out, module, name)?;
            for arg in args {
                dump(arg, out, depth + 1, budget)?;
            }
            out.push(b'o');
            dump(state, out, depth + 1, budget)?;
            out.push(b'b');
        }
        Value::Reduce { module, name, args } => {
            global(out, module, name)?;
            dump(&Value::Tuple(args.clone()), out, depth + 1, budget)?;
            out.push(b'R');
        }
        Value::Persistent(pid) => {
            dump(pid, out, depth + 1, budget)?;
            out.push(b'Q');
        }
    }
    if out.len() > crate::MAX_FRAME {
        return Err("pickle too large".into());
    }
    Ok(())
}

type Ref = Rc<RefCell<Node>>;
enum Node {
    Scalar(Value),
    Sequence(bool, Vec<Ref>), // true = tuple
    Dict(Vec<(Ref, Ref)>),
    Object {
        module: String,
        name: String,
        args: Vec<Ref>,
        state: Option<Ref>,
        reduce: bool,
    },
    Persistent(Ref),
}
enum Slot {
    Mark,
    Item(Ref),
}
struct Reader<'a> {
    bytes: &'a [u8],
    at: usize,
    stack: Vec<Slot>,
    memo: HashMap<u32, Ref>,
    nodes: usize,
    arena: Vec<Ref>,
}
impl Drop for Reader<'_> {
    fn drop(&mut self) {
        // Break cycles and long reference chains before releasing the arena.
        // Malformed network inputs must not leak Rc cycles or overflow Drop.
        for node in &self.arena {
            *node.borrow_mut() = Node::Scalar(Value::None);
        }
    }
}

pub fn loads(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() > crate::MAX_FRAME {
        return Err("pickle too large".into());
    }
    Reader {
        bytes,
        at: 0,
        stack: Vec::new(),
        memo: HashMap::new(),
        nodes: 0,
        arena: Vec::new(),
    }
    .parse()
}

impl Reader<'_> {
    fn take(&mut self, n: usize) -> Result<&[u8], String> {
        let end = self.at.checked_add(n).ok_or("length overflow")?;
        if end > self.bytes.len() {
            return Err("truncated pickle".into());
        }
        let part = &self.bytes[self.at..end];
        self.at = end;
        Ok(part)
    }
    fn byte(&mut self) -> Result<u8, String> {
        Ok(self.take(1)?[0])
    }
    fn u32(&mut self) -> Result<u32, String> {
        Ok(u32::from_le_bytes(self.take(4)?.try_into().unwrap()))
    }
    fn line(&mut self) -> Result<String, String> {
        let len = self.bytes[self.at..]
            .iter()
            .position(|b| *b == b'\n')
            .ok_or("unterminated line")?;
        let result = self.take(len)?.iter().map(|&b| char::from(b)).collect();
        self.byte()?;
        Ok(result)
    }
    fn push(&mut self, node: Node) -> Result<(), String> {
        self.nodes += 1;
        if self.nodes > MAX_NODES {
            return Err("too many pickle nodes".into());
        }
        let node = Rc::new(RefCell::new(node));
        self.arena.push(node.clone());
        self.stack.push(Slot::Item(node));
        Ok(())
    }
    fn scalar(&mut self, v: Value) -> Result<(), String> {
        self.push(Node::Scalar(v))
    }
    fn pop(&mut self) -> Result<Ref, String> {
        match self.stack.pop() {
            Some(Slot::Item(v)) => Ok(v),
            _ => Err("pickle stack underflow".into()),
        }
    }
    fn top(&self) -> Result<Ref, String> {
        match self.stack.last() {
            Some(Slot::Item(v)) => Ok(v.clone()),
            _ => Err("pickle stack underflow".into()),
        }
    }
    fn marked(&mut self) -> Result<Vec<Ref>, String> {
        let at = self
            .stack
            .iter()
            .rposition(|s| matches!(s, Slot::Mark))
            .ok_or("missing MARK")?;
        let slots = self.stack.split_off(at); // discard the MARK, retain values
        Ok(slots
            .into_iter()
            .skip(1)
            .map(|s| match s {
                Slot::Item(v) => v,
                Slot::Mark => unreachable!(),
            })
            .collect())
    }
    fn append(&mut self, items: Vec<Ref>) -> Result<(), String> {
        let top = self.top()?;
        let mut node = top.borrow_mut();
        match &mut *node {
            Node::Sequence(false, list) => {
                list.extend(items);
                Ok(())
            }
            _ => Err("APPEND target is not a list".into()),
        }
    }
    fn set_items(&mut self, items: Vec<Ref>) -> Result<(), String> {
        if !items.len().is_multiple_of(2) {
            return Err("odd dictionary item count".into());
        }
        let top = self.top()?;
        let mut node = top.borrow_mut();
        match &mut *node {
            Node::Dict(pairs) => {
                pairs.extend(items.chunks_exact(2).map(|p| (p[0].clone(), p[1].clone())));
                Ok(())
            }
            _ => Err("SETITEM target is not a dictionary".into()),
        }
    }
    fn object(&mut self, global: Ref, args: Vec<Ref>, reduce: bool) -> Result<(), String> {
        let Node::Scalar(Value::Global { module, name }) = &*global.borrow() else {
            return Err("expected GLOBAL callable".into());
        };
        self.push(Node::Object {
            module: module.clone(),
            name: name.clone(),
            args,
            state: None,
            reduce,
        })
    }
    fn parse(mut self) -> Result<Value, String> {
        loop {
            let opcode = self.byte()?;
            match opcode {
                b'.' => {
                    let root = self.pop()?;
                    let mut nodes = MAX_NODES;
                    let mut bytes = crate::MAX_FRAME;
                    return materialize(&root, 0, &mut nodes, &mut bytes);
                }
                b'(' => self.stack.push(Slot::Mark),
                b'N' => self.scalar(Value::None)?,
                b'K' => {
                    let n = self.byte()?;
                    self.scalar(i64::from(n).into())?;
                }
                b'M' => {
                    let n = u16::from_le_bytes(self.take(2)?.try_into().unwrap());
                    self.scalar(i64::from(n).into())?;
                }
                b'J' => {
                    let n = self.u32()? as i32;
                    self.scalar(i64::from(n).into())?;
                }
                b'I' | b'L' => {
                    let raw = self.line()?;
                    self.scalar(Value::Int(decimal(&raw)?))?;
                }
                b'G' => {
                    let bits = u64::from_be_bytes(self.take(8)?.try_into().unwrap());
                    self.scalar(Value::Float(bits))?;
                }
                b'F' => {
                    let f: f64 = self.line()?.parse().map_err(|_| "invalid float")?;
                    self.scalar(Value::Float(f.to_bits()))?;
                }
                b'U' | b'T' | b'X' => {
                    let len = if opcode == b'U' {
                        usize::from(self.byte()?)
                    } else {
                        self.u32()? as usize
                    };
                    let raw = self.take(len)?.to_vec();
                    self.scalar(if opcode == b'X' {
                        Value::Unicode(String::from_utf8(raw).map_err(|_| "invalid UTF-8")?)
                    } else {
                        Value::Bytes(raw)
                    })?;
                }
                b'S' => {
                    let text = self.line()?;
                    let raw: Vec<u8> = text.chars().map(|c| c as u8).collect();
                    if raw.len() < 2
                        || !matches!(raw[0], b'\'' | b'"')
                        || raw.last() != Some(&raw[0])
                    {
                        return Err("invalid quoted string".into());
                    }
                    self.scalar(Value::Bytes(unescape(&raw[1..raw.len() - 1])?))?;
                }
                b'V' => {
                    let text = self.line()?;
                    self.scalar(Value::Unicode(raw_unicode(&text)?))?;
                }
                b')' => self.push(Node::Sequence(true, Vec::new()))?,
                b']' => self.push(Node::Sequence(false, Vec::new()))?,
                b'}' => self.push(Node::Dict(Vec::new()))?,
                b't' | b'l' => {
                    let items = self.marked()?;
                    self.push(Node::Sequence(opcode == b't', items))?;
                }
                b'd' => {
                    let items = self.marked()?;
                    self.push(Node::Dict(Vec::new()))?;
                    self.set_items(items)?;
                }
                b'a' => {
                    let item = self.pop()?;
                    self.append(vec![item])?;
                }
                b'e' => {
                    let items = self.marked()?;
                    self.append(items)?;
                }
                b's' => {
                    let val = self.pop()?;
                    let key = self.pop()?;
                    self.set_items(vec![key, val])?;
                }
                b'u' => {
                    let items = self.marked()?;
                    self.set_items(items)?;
                }
                b'q' | b'r' | b'p' => {
                    let key = match opcode {
                        b'q' => u32::from(self.byte()?),
                        b'r' => self.u32()?,
                        _ => self.line()?.parse().map_err(|_| "invalid memo key")?,
                    };
                    self.memo.insert(key, self.top()?);
                    if self.memo.len() > MAX_NODES {
                        return Err("memo too large".into());
                    }
                }
                b'h' | b'j' | b'g' => {
                    let key = match opcode {
                        b'h' => u32::from(self.byte()?),
                        b'j' => self.u32()?,
                        _ => self.line()?.parse().map_err(|_| "invalid memo key")?,
                    };
                    let item = self.memo.get(&key).ok_or("missing memo key")?.clone();
                    self.stack.push(Slot::Item(item));
                }
                b'c' => {
                    let module = self.line()?;
                    let name = self.line()?;
                    self.scalar(Value::Global { module, name })?;
                }
                b'o' => {
                    let mut args = self.marked()?;
                    if args.is_empty() {
                        return Err("OBJ without class".into());
                    }
                    let class = args.remove(0);
                    self.object(class, args, false)?;
                }
                b'i' => {
                    let module = self.line()?;
                    let name = self.line()?;
                    let args = self.marked()?;
                    self.push(Node::Object {
                        module,
                        name,
                        args,
                        state: None,
                        reduce: false,
                    })?;
                }
                b'R' => {
                    let args = self.pop()?;
                    let class = self.pop()?;
                    let Node::Sequence(true, items) = &*args.borrow() else {
                        return Err("REDUCE args not tuple".into());
                    };
                    self.object(class, items.clone(), true)?;
                }
                b'b' => {
                    let state = self.pop()?;
                    let top = self.top()?;
                    let mut node = top.borrow_mut();
                    let Node::Object {
                        state: slot,
                        reduce,
                        ..
                    } = &mut *node
                    else {
                        return Err("BUILD without object".into());
                    };
                    *slot = Some(state);
                    *reduce = false;
                }
                b'Q' => {
                    let pid = self.pop()?;
                    self.push(Node::Persistent(pid))?;
                }
                b'P' => {
                    let pid = self.line()?;
                    self.scalar(Value::from(pid.as_str()))?;
                    let pid = self.pop()?;
                    self.push(Node::Persistent(pid))?;
                }
                b'0' => {
                    self.stack.pop().ok_or("POP underflow")?;
                }
                b'1' => {
                    self.marked()?;
                }
                b'2' => {
                    self.stack.push(Slot::Item(self.top()?));
                }
                _ => return Err(format!("unsupported Python-2 pickle opcode 0x{opcode:02x}")),
            }
            if self.stack.len() > MAX_NODES {
                return Err("pickle stack limit".into());
            }
        }
    }
}

fn materialize(
    node: &Ref,
    depth: usize,
    nodes: &mut usize,
    bytes: &mut usize,
) -> Result<Value, String> {
    if depth > MAX_DEPTH || *nodes == 0 {
        return Err("cyclic or excessive pickle graph".into());
    }
    *nodes -= 1;
    let next =
        |n: &Ref, nodes: &mut usize, bytes: &mut usize| materialize(n, depth + 1, nodes, bytes);
    Ok(match &*node.borrow() {
        Node::Scalar(v) => {
            let size = match v {
                Value::Bytes(b) => b.len(),
                Value::Int(s) | Value::Unicode(s) => s.len(),
                Value::Global { module, name } => module.len() + name.len(),
                _ => 8,
            };
            *bytes = bytes
                .checked_sub(size)
                .ok_or("expanded pickle exceeds limit")?;
            v.clone()
        }
        Node::Sequence(tuple, items) => {
            let items = items
                .iter()
                .map(|n| next(n, nodes, bytes))
                .collect::<Result<_, _>>()?;
            if *tuple {
                Value::Tuple(items)
            } else {
                Value::List(items)
            }
        }
        Node::Dict(pairs) => {
            let mut result: Vec<(Value, Value)> = Vec::new();
            for (k, v) in pairs {
                let key = next(k, nodes, bytes)?;
                let value = next(v, nodes, bytes)?;
                if let Some(pair) = result.iter_mut().find(|(k, _)| k == &key) {
                    pair.1 = value;
                } else {
                    result.push((key, value));
                }
            }
            Value::Dict(result)
        }
        Node::Object {
            module,
            name,
            args,
            state,
            reduce,
        } => {
            let args = args
                .iter()
                .map(|n| next(n, nodes, bytes))
                .collect::<Result<_, _>>()?;
            if *reduce {
                Value::Reduce {
                    module: module.clone(),
                    name: name.clone(),
                    args,
                }
            } else {
                Value::Instance {
                    module: module.clone(),
                    name: name.clone(),
                    args,
                    state: Box::new(match state {
                        Some(s) => next(s, nodes, bytes)?,
                        None => Value::None,
                    }),
                }
            }
        }
        Node::Persistent(pid) => Value::Persistent(Box::new(next(pid, nodes, bytes)?)),
    })
}

fn unescape(raw: &[u8]) -> Result<Vec<u8>, String> {
    let mut out = Vec::new();
    let mut i = 0;
    while i < raw.len() {
        let c = raw[i];
        i += 1;
        if c != b'\\' {
            out.push(c);
            continue;
        }
        let c = *raw.get(i).ok_or("truncated escape")?;
        i += 1;
        match c {
            b'\\' | b'\'' | b'"' => out.push(c),
            b'n' => out.push(b'\n'),
            b'r' => out.push(b'\r'),
            b't' => out.push(b'\t'),
            b'a' => out.push(7),
            b'b' => out.push(8),
            b'f' => out.push(12),
            b'v' => out.push(11),
            b'\n' => (),
            b'x' => {
                let hex = raw.get(i..i + 2).ok_or("truncated hex escape")?;
                let hex = std::str::from_utf8(hex).map_err(|_| "invalid hex escape")?;
                out.push(u8::from_str_radix(hex, 16).map_err(|_| "invalid hex escape")?);
                i += 2;
            }
            b'0'..=b'7' => {
                let mut val = u16::from(c - b'0');
                let mut n = 1;
                while n < 3 && raw.get(i).is_some_and(|c| matches!(c, b'0'..=b'7')) {
                    val = val * 8 + u16::from(raw[i] - b'0');
                    i += 1;
                    n += 1;
                }
                out.push(val as u8);
            }
            _ => out.extend([b'\\', c]),
        }
    }
    Ok(out)
}

fn raw_unicode(text: &str) -> Result<String, String> {
    let mut chars = text.chars().peekable();
    let mut out = String::new();
    while let Some(c) = chars.next() {
        if c != '\\' {
            out.push(c);
            continue;
        }
        // raw-unicode-escape only interprets an odd run of slashes before u/U.
        let mut count = 1;
        while chars.peek() == Some(&'\\') {
            chars.next();
            count += 1;
        }
        if count % 2 == 1 && matches!(chars.peek(), Some('u' | 'U')) {
            for _ in 0..count - 1 {
                out.push('\\');
            }
            let width = if chars.next() == Some('u') { 4 } else { 8 };
            let digits: String = chars.by_ref().take(width).collect();
            if digits.len() != width {
                return Err("truncated Unicode escape".into());
            }
            let code = u32::from_str_radix(&digits, 16).map_err(|_| "invalid Unicode escape")?;
            out.push(char::from_u32(code).ok_or("invalid Unicode codepoint")?);
        } else {
            for _ in 0..count {
                out.push('\\');
            }
        }
    }
    Ok(out)
}
