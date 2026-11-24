use crate::credentials::Credentials;
use agent2api_wasm_sdk::{http, json, Error, Result, Value};
use serde::{Deserialize, Serialize};

const AI: &str = "https://ai.yun.139.com";
const BENEFIT: &str = "https://ypqy.mcloud.139.com";
const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36";

#[derive(Serialize, Deserialize)]
pub struct KeyData {
    pub key: String,
    pub url: String,
    pub models: Vec<Value>,
}

fn post(credentials: &Credentials, url: &str, body: Value, agent: bool) -> Result<Value> {
    let mut headers = json!({"authorization":credentials.basic(),"x-yun-api-version":"v1",
        "content-type":"application/json", "user-agent":UA});
    if agent {
        headers["x-yun-app-channel"] = json!("101");
        headers["origin"] = json!("mcloudlocal://yun.139.com");
    }
    let response = http::open_timeout("POST", url, headers, Some(&body), 30_000)?;
    if !(200..300).contains(&response.status) {
        return Err(failure(response.status, &Value::Null));
    }
    response.json()
}

pub fn auth(status: u16, body: &Value) -> bool {
    matches!(status, 401 | 403)
        || matches!(
            body.get("code")
                .or_else(|| body.pointer("/error/code"))
                .and_then(Value::as_str),
            Some("01000003" | "01000002")
        )
}

pub fn failure(status: u16, body: &Value) -> Error {
    if auth(status, body) {
        Error::http(401, "移动云盘登录已失效，请重新导入凭证")
    } else {
        Error::http(
            if status < 400 { 502 } else { status },
            "MClaw 上游请求失败",
        )
    }
}

fn checked(value: &Value) -> Result<()> {
    if value["success"] == true {
        Ok(())
    } else {
        Err(failure(502, value))
    }
}

fn model_url(raw: &str) -> Result<String> {
    if !raw.starts_with("https://ai.yun.139.com/")
        || raw.contains(['\\', '#'])
        || raw
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return Err(Error::http(
            502,
            "MClaw 返回的模型地址不在已授权的 AI 域名内",
        ));
    }
    Ok(raw.to_owned())
}

fn number(value: &Value) -> Option<f64> {
    value
        .as_f64()
        .or_else(|| value.as_str()?.parse().ok())
        .filter(|n| n.is_finite())
}

fn model(row: &Value, agent: bool) -> Option<Value> {
    let id = row.get("modelCode")?.as_str()?.trim();
    if id.is_empty() {
        return None;
    }
    let name = row
        .get(if agent { "description" } else { "modelName" })
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
        .unwrap_or(id);
    // Preserve upstream facts; the image does not prove tools/vision support for every model.
    let mut model = json!({"id": id, "name": name, "ability": row.get("ability").cloned().unwrap_or(json!("")), "tokenRatio": row.get("tokenRatio").and_then(number)});
    if let Some(max) = row.get("maxInputLength").and_then(number) {
        model["contextWindow"] = json!(max);
    }
    Some(model)
}

pub fn fetch_key(credentials: &Credentials) -> Result<KeyData> {
    let value = post(
        credentials,
        &format!("{AI}/api/llm/apikey/get"),
        json!({"channelId":credentials.channel}),
        false,
    )?;
    checked(&value)?;
    let data = &value["data"];
    let key = data["apikey"]
        .as_str()
        .filter(|key| {
            !key.is_empty() && key.len() <= 32_768 && key.bytes().all(|b| b.is_ascii_graphic())
        })
        .ok_or_else(|| Error::http(502, "MClaw 返回的模型密钥无效"))?
        .to_owned();
    let fallback = format!("{AI}/api/llm/{}/v1/chat/completions", credentials.channel);
    let url = model_url(
        data["modelUrl"]
            .as_str()
            .filter(|s| !s.is_empty())
            .unwrap_or(&fallback),
    )?;
    let models = data["modelList"]
        .as_array()
        .map(|rows| rows.iter().filter_map(|r| model(r, false)).collect())
        .unwrap_or_default();
    Ok(KeyData { key, url, models })
}

pub fn catalog(credentials: &Credentials, key: &KeyData) -> Vec<Value> {
    let mut models = key.models.clone();
    {
        if let Ok(value) = post(
            credentials,
            &format!("{AI}/api/adapter/assistant/agent/list"),
            json!({"sourceChannel":"101", "sceneType":"homePage"}),
            true,
        ) {
            if checked(&value).is_ok() {
                if let Some(agents) = value.pointer("/data/agentList").and_then(Value::as_array) {
                    for agent in agents.iter().filter(|row| row["agentType"] == "mclaw") {
                        if let Some(rows) = agent["modelList"].as_array() {
                            for entry in rows.iter().filter_map(|row| model(row, true)) {
                                if !models.iter().any(|existing| existing["id"] == entry["id"]) {
                                    models.push(entry);
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    // These two models were explicitly included by v7; retain them as unverified extras.
    for (id, name, ratio) in [
        ("huoshan_deepseek_v4_1_flash", "DeepSeek-V4.1-Flash", 1.0),
        ("blian_deepseek_v4_flash", "DeepSeek-V4-Flash", 0.5),
    ] {
        if !models.iter().any(|row| row["id"] == id) {
            models.push(json!({"id":id, "name":name, "tokenRatio":ratio, "extra":true}));
        }
    }
    models.sort_by(|a, b| a["id"].as_str().cmp(&b["id"].as_str()));
    models
}

pub fn quota(credentials: &Credentials) -> Result<Value> {
    let value = post(
        credentials,
        &format!("{BENEFIT}/isbo2/openApi/queryAvailableBenefitV2"),
        json!({"account":{"userDomainId":""}}),
        false,
    )?;
    if value.pointer("/result/resultCode").and_then(Value::as_str) != Some("0") {
        return Err(Error::http(502, "MClaw aitokens 权益查询失败"));
    }
    let row = value["userAvailableBenefitList"]
        .as_array()
        .and_then(|rows| rows.iter().find(|row| row["benefitNo"] == "RHR135"))
        .ok_or_else(|| Error::http(502, "没有找到 aitokens 权益（RHR135）"))?;
    let total = row
        .get("benefitValue")
        .and_then(number)
        .ok_or_else(|| Error::http(502, "aitokens 总量字段无效"))?;
    let used = row
        .get("benefitUsed")
        .and_then(number)
        .ok_or_else(|| Error::http(502, "aitokens 已用字段无效"))?;
    let remaining = (total - used).max(0.0);
    Ok(
        json!({"provider":"mclaw", "plan":"移动云盘 MClaw", "unit":"aitokens", "windows":[{"name":"aitokens", "label":"aitokens", "limit":total, "total":total, "used":used, "remaining":remaining, "balance":remaining, "percent":if total > 0.0 { used * 100.0 / total } else { 0.0 }, "unit":"aitokens", "display":remaining.to_string()}]}),
    )
}
