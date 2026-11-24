#[path = "../../crates/mclaw/src/auth.rs"]
mod auth;
mod common;
#[path = "../../crates/mclaw/src/credentials.rs"]
mod credentials;
mod openai;
mod upstream;

use agent2api_wasm_sdk::{self as sdk, catalog, http, json, Error, Result, Value};
use base64::{engine::general_purpose::STANDARD, Engine};
use credentials::Credentials;

sdk::export_provider!(handle);

fn metadata() -> Value {
    json!({"id":"mclaw","label":"MClaw 移动云盘","order":10,
        "capabilities":{"chat":true,"refresh":true,"model_refresh":true,"usage":true,
            "sse_model_rewrite":true,"supports_default_model":false},
        "account_features":{"usage":true,"identifier":["phone"]},"default_models":[]})
}

fn normalize(params: &Value) -> Result<Value> {
    let source = &params["source"];
    let bad = || Error::http(400, "请导入单个账号的有效凭证（最多 256 KiB）");
    let payload = match source["kind"].as_str() {
        Some("manual") => source["payload"].clone(),
        Some("file_text" | "file_base64") => {
            let raw = source["content"].as_str().ok_or_else(bad)?.trim();
            let decoded;
            let content = if source["kind"] == "file_base64" {
                if raw.len() > (256 * 1024usize).div_ceil(3) * 4 {
                    return Err(bad());
                }
                decoded = STANDARD.decode(raw).map_err(|_| bad())?;
                std::str::from_utf8(&decoded).map_err(|_| bad())?
            } else {
                raw
            };
            if content.len() > 256 * 1024 {
                return Err(bad());
            }
            credentials::file_payload(content).map_err(|e| Error::http(400, e))?
        }
        _ => return Err(Error::unsupported()),
    };
    let payload = auth::payload(payload).map_err(|e| Error::http(400, e))?;
    let creds = credentials(&payload)?;
    let name = params["name"]
        .as_str()
        .filter(|s| !s.trim().is_empty())
        .or_else(|| payload["name"].as_str());
    let secret = creds.to_secret(name);
    Ok(
        json!({"secret":secret,"identity":creds.identity(),"public":creds.public(&secret),"desktop":false}),
    )
}

fn credentials(value: &Value) -> Result<Credentials> {
    let creds = Credentials::from_payload(value).map_err(|e| Error::http(400, e))?;
    if creds.token.len() > 32_768
        || creds.token.chars().any(char::is_control)
        || creds.channel.len() > 32
    {
        return Err(Error::http(400, "移动云盘凭证格式无效"));
    }
    Ok(creds)
}

fn key(params: &Value, force: bool) -> Result<(Credentials, upstream::KeyData)> {
    let _guard = sdk::AccountLock::acquire()?;
    let account = common::account()?;
    let creds = credentials(&account["secret"])?;
    let scope = json!([account["id"], params["proxy"], params["proxy_override"]]);
    let cache_id = format!("llm-key:{}", creds.fingerprint(&scope.to_string()));
    if !force {
        if let Ok(cached) = serde_json::from_value(sdk::session_get(&cache_id)?) {
            return Ok((creds, cached));
        }
    }
    let key = upstream::fetch_key(&creds)?;
    // Keep derived credentials only in the host’s expiring memory cache.
    let _ = sdk::session_put(&cache_id, json!(key), 300_000);
    Ok((creds, key))
}

fn conversation(mut params: Value) -> Result<()> {
    let (body, stream) = openai::body(&mut params)?;
    let (_, key) = key(&params, false)?;
    let response = http::open(
        "POST",
        &key.url,
        json!({"authorization":format!("Bearer {}",key.key),
        "content-type":"application/json","accept":if stream { "text/event-stream" } else { "application/json" }}),
        Some(&body),
    )?;
    openai::relay(response, upstream::failure)
}

fn handle(method: &str, params: Value) -> Result<Value> {
    match method {
        "init" => Ok(json!([metadata()])),
        "provider.info" => Ok(metadata()),
        "ping" => Ok(json!(true)),
        "admin.action" if params["action"] == "auth.methods" => {
            let mut methods = auth::methods();
            methods["instructions"] = json!(
                "请导入自己的移动云盘登录凭证。登录过期后请更新凭证。添加账号后可刷新模型目录。"
            );
            Ok(methods)
        }
        "account.normalize" => normalize(&params),
        "account.view" => Ok(credentials(&params["secret"])?.public(&params["secret"])),
        "account.identity" => Ok(json!(credentials(&params["secret"])?.identity())),
        "token.ensure" | "token.refresh" => {
            Ok(json!(key(&params, method == "token.refresh")?.1.key))
        }
        "token.expiring" => Ok(json!(false)),
        "models.list" => catalog::cached(),
        "models.refresh" => {
            let (creds, key) = key(&params, params["force"] == true)?;
            Ok(catalog::refreshed(upstream::catalog(&creds, &key)))
        }
        "usage.query" => upstream::quota(&credentials(&common::account()?["secret"])?),
        "conversation" => {
            conversation(params)?;
            Ok(Value::Null)
        }
        "classify" => {
            let status = params["status"].as_u64().unwrap_or(502) as u16;
            let body = &params["body"];
            let err = upstream::failure(status, body);
            Ok(common::classify(
                err.status.unwrap_or(502),
                None,
                upstream::auth(status, body),
                &err.message,
            ))
        }
        "is_error_response" => Ok(json!(params["status"].as_u64().unwrap_or(502) >= 400)),
        _ => Err(Error::unsupported()),
    }
}
