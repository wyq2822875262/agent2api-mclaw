use crate::credentials::Credentials;
use agent2api_provider_protocol as proto;
use agent2api_provider_sdk::{control_error, PluginCtx, StoreAccount, StoreProxy};
use reqwest::{Client, Url};
use serde_json::{json, Value};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const AI: &str = "https://ai.yun.139.com";
const BENEFIT: &str = "https://ypqy.mcloud.139.com";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Clone)]
pub struct KeyData { pub key: String, pub url: String, pub models: Vec<Value>, pub fingerprint: String }

pub fn now_ms() -> i64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as i64 }

pub fn resolve_proxy(ctx: &PluginCtx, stored: &StoreAccount, explicit: Option<&proto::WireProxy>) -> Result<String, String> {
    if agent2api_provider_sdk::extensions::direct_proxy_override() { return Ok("direct".into()); }
    if let Some(proxy) = explicit { return Ok(proxy.to_url()); }
    match stored.proxy.as_ref() {
        Some(StoreProxy::Resolved { proxy }) => Ok(proxy.to_url()),
        Some(StoreProxy::Failed { .. }) => Err("LOCAL_PRECONDITION:MClaw 账号代理不可用，请检查账号代理设置".into()),
        Some(StoreProxy::Static { .. }) => Err("LOCAL_PRECONDITION:MClaw 账号代理尚未解析，请更新网关或保存代理设置".into()),
        _ => Ok(ctx.env.proxy.clone()),
    }
}

fn client(proxy: &str) -> Result<Client, String> {
    let mut builder = Client::builder().no_proxy().user_agent(UA).timeout(Duration::from_secs(30)).connect_timeout(Duration::from_secs(10)).redirect(reqwest::redirect::Policy::none());
    if !proxy.is_empty() && proxy != "direct" {
        builder = builder.proxy(reqwest::Proxy::all(proxy).map_err(|_| "LOCAL_PRECONDITION:MClaw 代理地址无效")?);
    }
    builder.build().map_err(|_| "无法初始化 MClaw HTTP 客户端".into())
}

// Only loopback overrides are accepted, for the offline protocol integration test.
pub(crate) fn endpoint(variable: &str, fallback: &str) -> Result<String, String> {
    match std::env::var(variable) {
        Ok(value) => {
            let parsed = Url::parse(&value).map_err(|_| "MClaw 测试端点地址无效")?;
            if parsed.scheme() != "http" || !matches!(parsed.host_str(), Some("127.0.0.1" | "localhost" | "[::1]")) || parsed.port().is_none() || parsed.path() != "/" || parsed.query().is_some() || parsed.fragment().is_some() || !parsed.username().is_empty() || parsed.password().is_some() {
                return Err("MClaw 测试端点只允许 http://127.0.0.1:端口".into());
            }
            Ok(value.trim_end_matches('/').into())
        }
        Err(_) => Ok(fallback.into()),
    }
}

async fn post(credentials: &Credentials, proxy: &str, url: &str, body: Value, agent: bool) -> Result<(u16, Value), String> {
    let client = client(proxy)?;
    let mut request = client.post(url).header("authorization", credentials.basic()).header("x-yun-api-version", "v1").json(&body);
    if agent { request = request.header("x-yun-app-channel", "101").header("origin", "mcloudlocal://yun.139.com"); }
    let response = request.send().await.map_err(|_| control_error(502, "MClaw 网络请求失败，请检查连接或代理", None, None))?;
    let status = response.status().as_u16();
    if status == 401 || status == 403 { return Err(control_error(401, "移动云盘登录已失效，请重新粘贴 token", None, None)); }
    if status >= 400 { return Err(control_error(status.into(), format!("MClaw 上游返回 HTTP {status}"), None, None)); }
    let value = response.json().await.map_err(|_| control_error(502, "MClaw 上游返回非 JSON 响应", None, None))?;
    Ok((status, value))
}

fn checked(value: &Value, status: u16) -> Result<(), String> {
    if value.get("success").and_then(Value::as_bool) == Some(true) { return Ok(()); }
    let code = value.get("code").and_then(Value::as_str).unwrap_or("");
    if ["01000003", "01000002"].contains(&code) {
        Err(control_error(401, "移动云盘登录已失效，请重新粘贴 token", None, None))
    } else { Err(control_error(if status >= 400 { status.into() } else { 502 }, "MClaw 上游未成功处理请求", None, None)) }
}

fn number(value: &Value) -> Option<f64> { value.as_f64().or_else(|| value.as_str()?.parse().ok()).filter(|n| n.is_finite()) }

fn model(row: &Value, agent: bool) -> Option<Value> {
    let id = row.get("modelCode")?.as_str()?.trim();
    if id.is_empty() { return None; }
    let name = row.get(if agent { "description" } else { "modelName" }).and_then(Value::as_str).filter(|s| !s.is_empty()).unwrap_or(id);
    // Preserve upstream facts; the image does not prove tools/vision support for every model.
    let mut model = json!({"id": id, "name": name, "ability": row.get("ability").cloned().unwrap_or(json!("")), "tokenRatio": row.get("tokenRatio").and_then(number)});
    if let Some(max) = row.get("maxInputLength").and_then(number) { model["contextWindow"] = json!(max); }
    Some(model)
}

pub async fn fetch_key(credentials: &Credentials, proxy: &str) -> Result<KeyData, String> {
    let base = endpoint("MCLAW_TEST_AI_BASE", AI)?;
    let (status, value) = post(credentials, proxy, &format!("{base}/api/llm/apikey/get"), json!({"channelId": credentials.channel}), false).await?;
    checked(&value, status)?;
    let data = &value["data"];
    let key = data.get("apikey").and_then(Value::as_str).filter(|k| !k.is_empty()).ok_or("MClaw 响应缺少 LLM API key")?.to_string();
    let url = data.get("modelUrl").and_then(Value::as_str).filter(|s| !s.is_empty()).map(str::to_string).unwrap_or_else(|| format!("{base}/api/llm/{}/v1/chat/completions", credentials.channel));
    let parsed = Url::parse(&url).map_err(|_| "MClaw 返回的模型地址无效")?;
    let local_test = base != AI && parsed.origin() == Url::parse(&base).map_err(|_| "测试端点无效")?.origin();
    if (!local_test && parsed.scheme() != "https") || !parsed.username().is_empty() || parsed.password().is_some() {
        return Err("MClaw 返回的模型地址必须为 HTTPS".into());
    }
    let models = data.get("modelList").and_then(Value::as_array).map(|rows| rows.iter().filter_map(|r| model(r, false)).collect()).unwrap_or_default();
    Ok(KeyData { key, url, models, fingerprint: credentials.fingerprint(proxy) })
}

pub async fn catalog(credentials: &Credentials, proxy: &str, key: &KeyData) -> Vec<Value> {
    let mut models = key.models.clone();
    if let Ok(base) = endpoint("MCLAW_TEST_AI_BASE", AI) {
        if let Ok((status, value)) = post(credentials, proxy, &format!("{base}/api/adapter/assistant/agent/list"), json!({"sourceChannel":"101", "sceneType":"homePage"}), true).await {
            if checked(&value, status).is_ok() {
                if let Some(agents) = value.pointer("/data/agentList").and_then(Value::as_array) {
                    for agent in agents.iter().filter(|row| row["agentType"] == "mclaw") {
                        if let Some(rows) = agent["modelList"].as_array() {
                            for entry in rows.iter().filter_map(|row| model(row, true)) {
                                if !models.iter().any(|existing| existing["id"] == entry["id"]) { models.push(entry); }
                            }
                        }
                    }
                }
            }
        }
    }
    // These two models were explicitly included by v7; retain them as unverified extras.
    for (id, name, ratio) in [("huoshan_deepseek_v4_1_flash", "DeepSeek-V4.1-Flash", 1.0), ("blian_deepseek_v4_flash", "DeepSeek-V4-Flash", 0.5)] {
        if !models.iter().any(|row| row["id"] == id) { models.push(json!({"id":id, "name":name, "tokenRatio":ratio, "extra":true})); }
    }
    models.sort_by(|a,b| a["id"].as_str().cmp(&b["id"].as_str()));
    models
}

pub async fn quota(credentials: &Credentials, proxy: &str) -> Result<Value, String> {
    let base = endpoint("MCLAW_TEST_BENEFIT_BASE", BENEFIT)?;
    let (_, value) = post(credentials, proxy, &format!("{base}/isbo2/openApi/queryAvailableBenefitV2"), json!({"account":{"userDomainId":""}}), false).await?;
    if value.pointer("/result/resultCode").and_then(Value::as_str) != Some("0") { return Err("MClaw aitokens 权益查询失败".into()); }
    let row = value["userAvailableBenefitList"].as_array().and_then(|rows| rows.iter().find(|row| row["benefitNo"] == "RHR135")).ok_or("没有找到 aitokens 权益（RHR135）")?;
    let total = row.get("benefitValue").and_then(number).ok_or("aitokens 总量字段无效")?;
    let used = row.get("benefitUsed").and_then(number).ok_or("aitokens 已用字段无效")?;
    let remaining = (total - used).max(0.0);
    Ok(json!({"provider":"mclaw", "plan":"移动云盘 MClaw", "unit":"aitokens", "windows":[{"name":"aitokens", "label":"aitokens", "limit":total, "total":total, "used":used, "remaining":remaining, "balance":remaining, "percent":if total > 0.0 { used * 100.0 / total } else { 0.0 }, "unit":"aitokens", "display":remaining.to_string()}]}))
}
