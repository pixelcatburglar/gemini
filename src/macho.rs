//! Build-665 MachoNet framing and envelopes. Constants and tuple layouts are
//! derived from the original Python POC and the client's script/net package.
use crate::pickle::{self, Value};
use crate::{d, l, t};
use serde::{Deserialize, Serialize};
use std::io::Read;

pub const CHALLENGE: i64 = 170472;
pub const VERSION: i64 = 11007;
pub const GUARD_HEAD: u32 = 0x17047200;
pub const GUARD_TAIL: u32 = 0x00151102;

pub fn challenge() -> Value {
    Value::Tuple(vec![CHALLENGE.into(), VERSION.into()])
}

pub fn adler32(bytes: &[u8]) -> u32 {
    let (mut a, mut b) = (1u32, 0u32);
    for chunk in bytes.chunks(5552) {
        for &byte in chunk {
            a += u32::from(byte);
            b += a;
        }
        a %= 65521;
        b %= 65521;
    }
    (b << 16) | a
}

pub fn dumps(value: &Value) -> Result<Vec<u8>, String> {
    let center = pickle::dumps(value)?;
    let mut bytes = Vec::with_capacity(center.len() + 12);
    bytes.extend(GUARD_HEAD.to_be_bytes());
    // Signed Python-2 adler32 has exactly the same bytes as this u32.
    bytes.extend(adler32(&center).to_be_bytes());
    bytes.extend(center);
    bytes.extend(GUARD_TAIL.to_be_bytes());
    if bytes.len() > crate::MAX_FRAME {
        return Err("packet exceeds frame limit".into());
    }
    Ok(bytes)
}

pub fn loads(bytes: &[u8]) -> Result<Value, String> {
    if bytes.len() < 13 || bytes.len() > crate::MAX_FRAME {
        return Err("invalid packet length".into());
    }
    if bytes[..4] != GUARD_HEAD.to_be_bytes() {
        return Err("bad packet header".into());
    }
    if bytes[bytes.len() - 4..] != GUARD_TAIL.to_be_bytes() {
        return Err("bad packet footer".into());
    }
    let center = &bytes[8..bytes.len() - 4];
    if bytes[4..8] != adler32(center).to_be_bytes() {
        return Err("adler32 mismatch".into());
    }
    pickle::loads(center)
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Packet {
    pub command: i64,
    pub source: Value,
    pub destination: Value,
    pub user_id: Value,
    pub channel: Value,
    pub body: Vec<Value>,
}

impl Packet {
    pub fn decode(bytes: &[u8]) -> Result<Self, String> {
        let Value::Tuple(mut parts) = loads(bytes)? else {
            return Err("expected (header, body) tuple".into());
        };
        if parts.len() != 2 {
            return Err("expected two packet fields".into());
        }
        let body = match parts.pop().unwrap() {
            Value::Tuple(v) | Value::List(v) => v,
            _ => return Err("expected packet body sequence".into()),
        };
        let Value::Tuple(mut header) = parts.pop().unwrap() else {
            return Err("expected header tuple".into());
        };
        if header.is_empty() || header.len() > 5 {
            return Err("invalid header length".into());
        }
        header.resize(5, Value::None);
        let command = header[0].as_i64()?;
        Ok(Self {
            command,
            source: header[1].clone(),
            destination: header[2].clone(),
            user_id: header[3].clone(),
            channel: header[4].clone(),
            body,
        })
    }

    pub fn encode(&self) -> Result<Vec<u8>, String> {
        let mut header = vec![
            self.command.into(),
            self.source.clone(),
            self.destination.clone(),
            self.user_id.clone(),
            self.channel.clone(),
        ];
        while header.last() == Some(&Value::None) {
            header.pop();
        }
        dumps(&Value::Tuple(vec![
            Value::Tuple(header),
            Value::Tuple(self.body.clone()),
        ]))
    }
}

pub fn address_service(address: &Value) -> Value {
    match address[0].text().as_str() {
        "C" => address[3].clone(),
        "N" => address[2].clone(),
        "A" => address[1].clone(),
        _ => Value::None,
    }
}
pub fn node() -> Value {
    t!["N", 1]
}
pub fn client(id: i64) -> Value {
    t!["C", id]
}
pub fn object_payload(value: &Value) -> Result<Value, String> {
    Ok(Value::Bytes(pickle::dumps(value)?))
}
fn unwrap_compression(value: &Value) -> Result<Value, String> {
    if value.seq().len() != 2 || !matches!(value[0].as_i64(), Ok(0 | 1)) {
        return Ok(value.clone());
    }
    if value[0].int() == 1 && matches!(value[1], Value::Bytes(_)) {
        let mut raw = Vec::new();
        flate2::read::ZlibDecoder::new(value[1].raw()?)
            .take(crate::MAX_FRAME as u64 + 1)
            .read_to_end(&mut raw)
            .map_err(|e| e.to_string())?;
        if raw.len() > crate::MAX_FRAME {
            return Err("compressed payload exceeds limit".into());
        }
        return pickle::loads(&raw);
    }
    Ok(value[1].clone())
}
pub struct Call {
    pub object: Value,
    pub method: String,
    pub args: Value,
    pub keywords: Value,
}
pub fn decode_call(value: &Value) -> Result<Call, String> {
    let value = unwrap_compression(value)?;
    let mut object = false;
    let mut blob = value.clone();
    if value.seq().len() == 2 && matches!(value[0].as_i64(), Ok(0 | 1)) {
        object = value[0].int() == 1;
        blob = match &value[1] {
            Value::Bytes(b) => pickle::loads(b)?,
            v => v.clone(),
        };
    }
    if blob.seq().len() < 3 {
        return Err("unrecognised call payload".into());
    }
    Ok(Call {
        object: if object { blob[0].clone() } else { Value::None },
        method: blob[1].text(),
        args: blob[2].clone(),
        keywords: blob[3].or(d![]),
    })
}
pub fn notification_call(value: &Value) -> Result<Option<Call>, String> {
    let mut value = unwrap_compression(value)?;
    if value.seq().len() == 4 && value[0].int() == 1 {
        return Ok(None);
    }
    if value.seq().len() == 2 && value[0].int() == 0 {
        value = value[1].clone();
    }
    if value.seq().len() != 2 {
        return Ok(None);
    }
    let object = value[0].int() == 1;
    let blob = match &value[1] {
        Value::Bytes(b) => pickle::loads(b)?,
        v => v.clone(),
    };
    if blob.seq().len() < 2 {
        return Ok(None);
    }
    Ok(Some(Call {
        object: if object { blob[0].clone() } else { Value::None },
        method: blob[1].text(),
        args: blob[2].or(t![]),
        keywords: d![],
    }))
}
pub fn broadcast_payload(args: Value) -> Result<Value, String> {
    Ok(t![0, t![0, t![0, object_payload(&t![0, t![1, args]])?]]])
}
pub fn callback_payload(object: Value, method: &str, args: Value) -> Result<Value, String> {
    Ok(t![
        0,
        t![0, t![1, object_payload(&t![object, method, args])?]]
    ])
}
pub fn error_payload(error: &crate::state::Error, id: i64) -> Value {
    let (module, name, state, severity) = match error {
        crate::state::Error::User(msg, params) => (
            "ccp_exceptions",
            "UserError",
            d!["msg"=>msg.clone(),"dict"=>params.clone(),"args"=>t![msg.clone(),params.clone()]],
            0,
        ),
        crate::state::Error::Internal(msg) => {
            ("exceptions", "RuntimeError", d!["args"=>t![msg.clone()]], 1)
        }
    };
    let exc = Value::Instance {
        module: module.into(),
        name: name.into(),
        args: vec![],
        state: Box::new(state),
    };
    t![
        exc,
        t![
            severity,
            l![format!("poc_server Rust GPCS: {error:?}")],
            format!("{{server-1-{id}}}")
        ]
    ]
}
