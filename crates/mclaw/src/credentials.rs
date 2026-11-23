use base64::{engine::general_purpose::STANDARD, Engine};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};

pub struct Credentials { pub phone: String, pub token: String, pub channel: String }

fn pick<'a>(value: &'a Value, keys: &[&str]) -> &'a str {
    keys.iter().filter_map(|key| value.get(key).and_then(Value::as_str)).map(str::trim).find(|v| !v.is_empty()).unwrap_or("")
}

fn split_phone(value: &str) -> Option<(String, String)> {
    let mut parts = value.splitn(3, ':');
    let scheme = parts.next()?;
    let phone = parts.next()?;
    let token = parts.next()?;
    if ["pc", "mobile"].contains(&scheme) && phone.len() == 11 && phone.bytes().all(|c| c.is_ascii_digit()) {
        Some((phone.into(), token.into()))
    } else { None }
}

fn unescape(value: &str) -> String {
    let bytes = value.as_bytes();
    let mut output = Vec::new();
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' && index + 2 < bytes.len() {
            if let (Some(a), Some(b)) = ((bytes[index + 1] as char).to_digit(16), (bytes[index + 2] as char).to_digit(16)) {
                output.push((a * 16 + b) as u8); index += 3; continue;
            }
        }
        output.push(bytes[index]); index += 1;
    }
    String::from_utf8_lossy(&output).into_owned()
}

impl Credentials {
    pub fn from_payload(payload: &Value) -> Result<Self, String> {
        let mut phone = pick(payload, &["phone", "mobile", "account"]).to_string();
        let mut token = pick(payload, &["token", "accessToken", "authorization", "cookie"]).trim_matches(['\'', '"']).to_string();
        if token.is_empty() { return Err("请粘贴移动云盘登录 token、Basic 或 Cookie".into()); }
        if token.to_ascii_lowercase().starts_with("cookie:") { token = token[7..].trim().into(); }
        if let Some(value) = token.split(';').find_map(|part| {
            let (key, value) = part.trim().split_once('=')?;
            key.eq_ignore_ascii_case("authorization").then_some(value)
        }) { token = unescape(value.trim_matches(['\'', '"'])); }
        token.retain(|c| !c.is_whitespace() && !['\u{200b}', '\u{200c}', '\u{200d}', '\u{feff}'].contains(&c));
        if token.to_ascii_lowercase().starts_with("basic") { token = token[5..].trim_start_matches(':').into(); }
        if !token.contains('|') {
            let padded = format!("{}{}", token, "=".repeat((4 - token.len() % 4) % 4));
            if let Ok(decoded) = STANDARD.decode(padded) {
                if let Ok(decoded) = String::from_utf8(decoded) { token = decoded; }
            }
        }
        if let Some((embedded, value)) = split_phone(&token) { if phone.is_empty() { phone = embedded; } token = value; }
        phone.retain(|c| c.is_ascii_digit());
        if phone.len() == 13 && phone.starts_with("86") { phone = phone[2..].into(); }
        if phone.len() != 11 { return Err("请填写 11 位手机号，或粘贴含手机号的 Basic 凭证".into()); }
        if !token.contains('|') { return Err("登录 token 应包含 |，请检查是否复制完整".into()); }
        let channel = pick(payload, &["channelId", "channel_id"]);
        let channel = if channel.is_empty() { "102001" } else { channel };
        if !channel.bytes().all(|c| c.is_ascii_digit()) { return Err("渠道 ID 必须为数字".into()); }
        Ok(Self { phone, token, channel: channel.into() })
    }

    pub fn basic(&self) -> String { format!("Basic {}", STANDARD.encode(format!("pc:{}:{}", self.phone, self.token))) }
    pub fn identity(&self) -> String { format!("mclaw-{:x}", Sha256::digest(format!("{}:{}", self.phone, self.channel))) }
    pub fn fingerprint(&self, proxy: &str) -> String { format!("{:x}", Sha256::digest(format!("{}:{}:{}:{}", self.phone, self.token, self.channel, proxy))) }
    pub fn to_secret(&self, name: Option<&str>) -> Value { json!({ "phone": self.phone, "token": self.token, "channelId": self.channel, "name": name }) }
    pub fn public(&self, secret: &Value) -> Value {
        let mask = format!("{}****{}", &self.phone[..3], &self.phone[7..]);
        json!({ "name": secret.get("name").and_then(Value::as_str).unwrap_or(&mask), "phone": mask, "channelId": self.channel })
    }
}

pub fn file_payload(content: &str) -> Result<Value, String> {
    let value: Value = serde_json::from_str(content).unwrap_or_else(|_| json!({ "token": content.trim() }));
    if value.is_array() || value.get("accounts").is_some() { return Err("每次请导入一个账号 JSON（phone、token、channelId）".into()); }
    if let Some(text) = value.as_str() { Ok(json!({ "token": text })) } else { Ok(value) }
}

#[cfg(test)]
mod tests {
    use super::*;
    const TOKEN: &str = "example|1|RCS|1795259045072|token-value-";
    #[test]
    fn imports_and_rotation_keep_account_identity() {
        let c = Credentials::from_payload(&json!({"phone": "+86 138-0013-8000", "token": TOKEN})).unwrap();
        for token in [c.basic(), c.basic()[6..].to_string(), format!("pc:13800138000:{TOKEN}"), format!("Cookie: other=x; authorization={}; other=y", c.basic().replace(' ', "%20").replace('=', "%3D"))] {
            let found = Credentials::from_payload(&json!({"token": token})).unwrap();
            assert_eq!(found.phone, c.phone); assert_eq!(found.token, TOKEN); assert_eq!(found.identity(), c.identity());
        }
        let rotated = Credentials::from_payload(&json!({"phone": c.phone, "token": "new|session"})).unwrap();
        assert_eq!(rotated.identity(), c.identity());
        assert_ne!(rotated.fingerprint(""), c.fingerprint(""));
        assert!(!c.public(&c.to_secret(None)).to_string().contains(TOKEN));
        assert!(!c.public(&c.to_secret(None)).to_string().contains("13800138000"));
    }
    #[test]
    fn rejects_missing_phone_and_invalid_token() {
        assert!(Credentials::from_payload(&json!({"token": TOKEN})).is_err());
        assert!(Credentials::from_payload(&json!({"phone":"13800138000", "token":"invalid"})).is_err());
        assert!(file_payload("{\"accounts\":[]}").is_err());
    }
}
