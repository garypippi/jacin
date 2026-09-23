//! msgpack-rpc codec for the Neovim API (no I/O)
//!
//! Messages are msgpack arrays:
//! - request `[0, msgid, method, params]`
//! - response `[1, msgid, error, result]`
//! - notification `[2, method, params]`

use std::io;

use rmpv::Value;

const REQUEST: u64 = 0;
const RESPONSE: u64 = 1;
const NOTIFICATION: u64 = 2;

/// A decoded message from Neovim
#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Request {
        msgid: u32,
        method: String,
        params: Vec<Value>,
    },
    /// `result` is `Err(error)` when the error field is not nil
    Response {
        msgid: u32,
        result: Result<Value, Value>,
    },
    Notification {
        method: String,
        params: Vec<Value>,
    },
}

/// The stream is not valid msgpack-rpc. It cannot be resynchronized,
/// so the connection is unusable after this.
#[derive(Debug)]
pub struct DecodeError(String);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "invalid msgpack-rpc message: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// Encodes outgoing messages and decodes incoming bytes as they arrive.
#[derive(Default)]
pub struct Rpc {
    next_msgid: u32,
    /// Received bytes; `buf[start..]` is not decoded yet
    buf: Vec<u8>,
    start: usize,
}

impl Rpc {
    pub fn new() -> Self {
        Self::default()
    }

    /// Append a request to `out` and return its msgid
    pub fn request(&mut self, method: &str, params: Vec<Value>, out: &mut Vec<u8>) -> u32 {
        let msgid = self.next_msgid;
        self.next_msgid = self.next_msgid.wrapping_add(1);
        write(
            out,
            Value::Array(vec![
                Value::from(REQUEST),
                Value::from(msgid),
                Value::from(method),
                Value::Array(params),
            ]),
        );
        msgid
    }

    /// Append received bytes (may end in the middle of a message)
    pub fn feed(&mut self, bytes: &[u8]) {
        if self.start == self.buf.len() {
            self.buf.clear();
            self.start = 0;
        } else if self.start > 0 {
            self.buf.drain(..self.start);
            self.start = 0;
        }
        self.buf.extend_from_slice(bytes);
    }

    /// Decode the next complete message, or None until more bytes are fed
    pub fn next_message(&mut self) -> Result<Option<Message>, DecodeError> {
        let mut rest = &self.buf[self.start..];
        if rest.is_empty() {
            return Ok(None);
        }
        let len = rest.len();
        let value = match rmpv::decode::read_value(&mut rest) {
            Ok(value) => value,
            Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => return Ok(None),
            Err(e) => return Err(DecodeError(e.to_string())),
        };
        self.start += len - rest.len();
        parse_message(value).map(Some)
    }
}

fn write(out: &mut Vec<u8>, value: Value) {
    rmpv::encode::write_value(out, &value).expect("writing to a Vec cannot fail");
}

fn parse_message(value: Value) -> Result<Message, DecodeError> {
    let Value::Array(fields) = value else {
        return Err(DecodeError(format!("not an array: {value}")));
    };
    let mut fields = fields.into_iter();
    let kind = fields.next().and_then(|v| v.as_u64());
    let fields: Vec<Value> = fields.collect();
    match (kind, fields.as_slice()) {
        (Some(REQUEST), [msgid, method, params]) => Ok(Message::Request {
            msgid: parse_msgid(msgid)?,
            method: parse_method(method)?,
            params: parse_params(params)?,
        }),
        (Some(RESPONSE), [msgid, error, result]) => Ok(Message::Response {
            msgid: parse_msgid(msgid)?,
            result: if error.is_nil() {
                Ok(result.clone())
            } else {
                Err(error.clone())
            },
        }),
        (Some(NOTIFICATION), [method, params]) => Ok(Message::Notification {
            method: parse_method(method)?,
            params: parse_params(params)?,
        }),
        _ => Err(DecodeError(format!(
            "unknown message type {kind:?} with {} fields",
            fields.len()
        ))),
    }
}

fn parse_msgid(value: &Value) -> Result<u32, DecodeError> {
    value
        .as_u64()
        .and_then(|id| u32::try_from(id).ok())
        .ok_or_else(|| DecodeError(format!("bad msgid: {value}")))
}

fn parse_method(value: &Value) -> Result<String, DecodeError> {
    value
        .as_str()
        .map(str::to_string)
        .ok_or_else(|| DecodeError(format!("bad method: {value}")))
}

fn parse_params(value: &Value) -> Result<Vec<Value>, DecodeError> {
    value
        .as_array()
        .cloned()
        .ok_or_else(|| DecodeError(format!("bad params: {value}")))
}

/// Human-readable text of an error response.
/// Neovim sends `[type, message]`.
pub fn error_message(error: &Value) -> String {
    match error.as_array().map(Vec::as_slice) {
        Some([_, message]) if message.is_str() => message.as_str().unwrap_or_default().to_string(),
        _ => error.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn encode(value: Value) -> Vec<u8> {
        let mut out = Vec::new();
        write(&mut out, value);
        out
    }

    fn response(msgid: u32, error: Value, result: Value) -> Vec<u8> {
        encode(Value::Array(vec![
            Value::from(RESPONSE),
            Value::from(msgid),
            error,
            result,
        ]))
    }

    fn notification(method: &str, params: Vec<Value>) -> Vec<u8> {
        encode(Value::Array(vec![
            Value::from(NOTIFICATION),
            Value::from(method),
            Value::Array(params),
        ]))
    }

    fn decode_all(rpc: &mut Rpc) -> Vec<Message> {
        let mut messages = Vec::new();
        while let Some(message) = rpc.next_message().unwrap() {
            messages.push(message);
        }
        messages
    }

    #[test]
    fn request_encodes_and_numbers_msgids() {
        let mut rpc = Rpc::new();
        let mut out = Vec::new();
        let first = rpc.request("nvim_input", vec![Value::from("a")], &mut out);
        let second = rpc.request("nvim_get_mode", vec![], &mut out);
        assert_eq!((first, second), (0, 1));

        let mut decoder = Rpc::new();
        decoder.feed(&out);
        assert_eq!(
            decode_all(&mut decoder),
            vec![
                Message::Request {
                    msgid: 0,
                    method: "nvim_input".into(),
                    params: vec![Value::from("a")],
                },
                Message::Request {
                    msgid: 1,
                    method: "nvim_get_mode".into(),
                    params: vec![],
                },
            ]
        );
    }

    #[test]
    fn msgid_wraps_around() {
        let mut rpc = Rpc {
            next_msgid: u32::MAX,
            ..Rpc::default()
        };
        let mut out = Vec::new();
        assert_eq!(rpc.request("a", vec![], &mut out), u32::MAX);
        assert_eq!(rpc.request("b", vec![], &mut out), 0);
    }

    #[test]
    fn decodes_response_ok_and_error() {
        let mut rpc = Rpc::new();
        rpc.feed(&response(3, Value::Nil, Value::from(true)));
        let error = Value::Array(vec![
            Value::from(1),
            Value::from("E492: Not an editor command"),
        ]);
        rpc.feed(&response(4, error.clone(), Value::Nil));
        assert_eq!(
            decode_all(&mut rpc),
            vec![
                Message::Response {
                    msgid: 3,
                    result: Ok(Value::from(true)),
                },
                Message::Response {
                    msgid: 4,
                    result: Err(error.clone()),
                },
            ]
        );
        assert_eq!(error_message(&error), "E492: Not an editor command");
    }

    #[test]
    fn decodes_byte_by_byte() {
        let mut bytes = notification("redraw", vec![Value::Array(vec![Value::from("flush")])]);
        bytes.extend(response(7, Value::Nil, Value::from("ok")));

        let mut rpc = Rpc::new();
        let mut messages = Vec::new();
        for byte in &bytes {
            rpc.feed(std::slice::from_ref(byte));
            messages.extend(decode_all(&mut rpc));
        }
        assert_eq!(
            messages,
            vec![
                Message::Notification {
                    method: "redraw".into(),
                    params: vec![Value::Array(vec![Value::from("flush")])],
                },
                Message::Response {
                    msgid: 7,
                    result: Ok(Value::from("ok")),
                },
            ]
        );
        assert!(rpc.buf.is_empty() || rpc.start == rpc.buf.len());
    }

    #[test]
    fn keeps_partial_message_across_feeds() {
        let first = response(1, Value::Nil, Value::from(1));
        let second = response(2, Value::Nil, Value::from("x".repeat(100)));
        let (head, tail) = second.split_at(10);

        let mut rpc = Rpc::new();
        rpc.feed(&[first.as_slice(), head].concat());
        assert_eq!(decode_all(&mut rpc).len(), 1);
        rpc.feed(tail);
        assert_eq!(
            decode_all(&mut rpc),
            vec![Message::Response {
                msgid: 2,
                result: Ok(Value::from("x".repeat(100))),
            }]
        );
    }

    #[test]
    fn rejects_unknown_message_type() {
        let mut rpc = Rpc::new();
        rpc.feed(&encode(Value::Array(vec![
            Value::from(5),
            Value::from("x"),
        ])));
        assert!(rpc.next_message().is_err());
    }

    #[test]
    fn rejects_non_array_and_bad_fields() {
        let mut rpc = Rpc::new();
        rpc.feed(&encode(Value::from("hello")));
        assert!(rpc.next_message().is_err());

        let mut rpc = Rpc::new();
        rpc.feed(&notification("redraw", vec![]));
        rpc.feed(&encode(Value::Array(vec![
            Value::from(NOTIFICATION),
            Value::from(1),
            Value::Array(vec![]),
        ])));
        assert!(rpc.next_message().unwrap().is_some());
        assert!(rpc.next_message().is_err());
    }

    #[test]
    fn error_message_falls_back_to_value() {
        assert_eq!(error_message(&Value::from("oops")), "\"oops\"");
    }
}
