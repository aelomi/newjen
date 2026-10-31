use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

#[derive(Debug, Clone, PartialEq, Eq, Hash, Deserialize, Serialize)]
#[serde(untagged)]
pub(crate) enum RpcId {
    Integer(i64),
    String(String),
}

pub(crate) fn request(id: &RpcId, method: &str, params: Value) -> Value {
    json!({"id":id,"method":method,"params":params})
}

pub(crate) fn initialize(id: &RpcId) -> Value {
    request(
        id,
        "initialize",
        json!({"clientInfo":{"name":"newjen","title":"Newjen","version":env!("CARGO_PKG_VERSION")}}),
    )
}

pub(crate) fn initialized() -> Value {
    json!({"method":"initialized"})
}

pub(crate) fn line(value: &Value) -> serde_json::Result<Vec<u8>> {
    let mut bytes = serde_json::to_vec(value)?;
    bytes.push(b'\n');
    Ok(bytes)
}
