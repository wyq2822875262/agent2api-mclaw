use agent2api_wasm_sdk::{self as sdk, http::Response, json, Error, Result, Value};

pub fn body(params: &mut Value) -> Result<(Value, bool)> {
    let mut body = sdk::take_body(params).ok_or_else(|| Error::http(400, "对话请求缺少正文"))?;
    if !body.is_object() {
        return Err(Error::http(400, "对话正文必须为 JSON 对象"));
    }
    let model = params["wire_model"]
        .as_str()
        .filter(|s| !s.is_empty())
        .or_else(|| body["model"].as_str())
        .filter(|s| !s.trim().is_empty())
        .ok_or_else(|| Error::http(400, "请选择有效的模型编码"))?
        .to_owned();
    let stream = params["stream"].as_bool().unwrap_or(false);
    body["model"] = json!(model);
    body["stream"] = json!(stream);
    Ok((body, stream))
}

pub fn relay(mut response: Response, error: impl Fn(u16, &Value) -> Error) -> Result<()> {
    let status = response.status;
    let mime = response
        .headers_raw
        .iter()
        .find(|(key, _)| key.eq_ignore_ascii_case("content-type"))
        .map(|(_, value)| {
            value
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase()
        })
        .unwrap_or_default();
    if !(200..300).contains(&status) {
        let body = response
            .bytes(256 * 1024)
            .ok()
            .and_then(|bytes| serde_json::from_slice(&bytes).ok())
            .unwrap_or(Value::Null);
        return Err(error(status, &body));
    }
    if mime == "text/event-stream" {
        // Wait for the first body bytes so an empty reply does not commit success.
        let first = loop {
            match response.read()? {
                Some(bytes) if !bytes.is_empty() => break bytes,
                Some(_) => continue,
                None => return Err(Error::http(502, "上游未返回对话内容")),
            }
        };
        sdk::stream_head(status)?;
        sdk::stream_bytes(&first)?;
        while let Some(bytes) = response.read()? {
            sdk::stream_bytes(&bytes)?;
        }
    } else {
        let bytes = response.bytes(8 * 1024 * 1024)?;
        let parsed: Value = serde_json::from_slice(&bytes)
            .map_err(|_| Error::http(502, "上游未返回有效的对话 JSON"))?;
        if !parsed["error"].is_null() || !parsed["choices"].is_array() {
            return Err(error(status, &parsed));
        }
        crate::common::head(status, "application/json")?;
        sdk::stream_bytes(&bytes)?;
    }
    Ok(())
}
