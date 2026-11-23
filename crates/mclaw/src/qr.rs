//! Mobile Cloud QR protocol, ported from the user-supplied 139cloudDiskAutoSave source.
//! Credentials and key material deliberately do not implement Debug.
use aes::cipher::{block_padding::Pkcs7, BlockDecryptMut, BlockEncryptMut, KeyInit, KeyIvInit};
use base64::{engine::general_purpose::STANDARD as BASE64, Engine};
use chrono::{FixedOffset, Utc};
use md5::{Digest, Md5};
use rand::{distributions::Alphanumeric, rngs::OsRng, Rng, RngCore};
use reqwest::{Client, Url};
use rsa::{pkcs8::DecodePublicKey, Pkcs1v15Encrypt, RsaPublicKey};
use serde_json::{json, Value};
use sha1::Sha1;
use std::time::{Duration, Instant};

const WIRE_KEY: &[u8; 32] = b"UqEZkrjCKfa02pP6jntzFmkzOz86zHUC";
const DATA_KEY: &[u8; 16] = b"qPqDw263XgFgL3u8";
const PUBLIC_KEY_URL: &str =
    "https://yun.139.com/orchestration/auth-rebuild/key/v1.0/getRsaPublicKey";
const POLL_URL: &str = "https://user-njs.yun.139.com/user/thirdlogin";
pub const LIFETIME: Duration = Duration::from_secs(300);
pub const POLL_INTERVAL_MS: u64 = 2500;
const MAX_RESPONSE: usize = 1024 * 1024;

pub enum QrError {
    Network,
    Protocol(&'static str),
}

impl std::fmt::Display for QrError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Network => f.write_str("扫码登录网络异常，请检查连接或代理"),
            Self::Protocol(message) => f.write_str(message),
        }
    }
}

pub enum Outcome {
    Pending,
    Scanned,
    Expired,
    Cancelled,
    Confirmed(crate::credentials::Credentials),
}

pub struct Session {
    pub flow: String,
    pub qr_content: String,
    pub created: Instant,
    session_id: String,
    visitor_id: String,
    client: Client,
    poll_url: String,
    // The reference generates this for subsequent cloud APIs, not CBC transport.
    // MClaw only needs the recovered phone/token; do not expose this in public output.
    _mcloud_skey: String,
}

fn random_alnum(length: usize) -> String {
    OsRng.sample_iter(&Alphanumeric).take(length).map(char::from).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn md5(bytes: impl AsRef<[u8]>) -> String {
    hex(&Md5::digest(bytes.as_ref()))
}

// Match encodeURIComponent plus the reference's lower-case escaping of !'()*.
fn encode_sign_text(text: &str) -> String {
    let mut encoded = String::new();
    for byte in text.bytes() {
        if byte.is_ascii_alphanumeric() || b"-_.~".contains(&byte) {
            encoded.push(char::from(byte));
        } else if b"!'()*".contains(&byte) {
            encoded.push_str(&format!("%{byte:02x}"));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

fn sign(body: &Value, timestamp: &str, nonce: &str) -> Result<String, QrError> {
    let text = serde_json::to_string(body)
        .map_err(|_| QrError::Protocol("扫码请求无法编码"))?;
    let mut characters = encode_sign_text(&text).into_bytes();
    characters.sort_unstable();
    let payload_digest = md5(BASE64.encode(characters));
    let nonce_digest = md5(format!("{timestamp}:{nonce}"));
    let digest = md5(format!("{payload_digest}{nonce_digest}")).to_uppercase();
    Ok(format!("{timestamp},{nonce},{digest}"))
}

fn encrypt_with_iv(body: &Value, iv: &[u8; 16]) -> Result<String, QrError> {
    let text = serde_json::to_vec(body)
        .map_err(|_| QrError::Protocol("扫码请求无法编码"))?;
    let encrypted = cbc::Encryptor::<aes::Aes256>::new(WIRE_KEY.into(), iv.into())
        .encrypt_padded_vec_mut::<Pkcs7>(&text);
    let mut envelope = iv.to_vec();
    envelope.extend(encrypted);
    Ok(BASE64.encode(envelope))
}

fn decrypt_wire(text: &str) -> Result<Value, QrError> {
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    let bytes = BASE64.decode(compact)
        .map_err(|_| QrError::Protocol("扫码响应编码无效"))?;
    if bytes.len() <= 16 || (bytes.len() - 16) % 16 != 0 {
        return Err(QrError::Protocol("扫码响应密文长度无效"));
    }
    let plain = cbc::Decryptor::<aes::Aes256>::new(WIRE_KEY.into(), bytes[..16].into())
        .decrypt_padded_vec_mut::<Pkcs7>(&bytes[16..])
        .map_err(|_| QrError::Protocol("扫码响应解密失败"))?;
    serde_json::from_slice(&plain)
        .map_err(|_| QrError::Protocol("扫码响应内容无效"))
}

fn parse_response(text: &str) -> Result<Value, QrError> {
    let trimmed = text.trim();
    let mut value = match serde_json::from_str::<Value>(trimmed) {
        Ok(Value::String(cipher)) => decrypt_wire(&cipher)?,
        Ok(value) => value,
        Err(_) => decrypt_wire(trimmed)?,
    };
    // Some gateways wrap the entire encrypted response in a JSON field.
    for key in ["data", "result", "body", "payload", "content", "response", "params"] {
        if let Some(cipher) = value.get(key).and_then(Value::as_str) {
            if let Ok(decrypted) = decrypt_wire(cipher) {
                if decrypted.is_object()
                    && ["success", "code", "message"].iter().any(|key| decrypted.get(key).is_some())
                {
                    return Ok(decrypted);
                }
                value[key] = decrypted;
                break;
            }
        }
    }
    if !value.is_object() {
        return Err(QrError::Protocol("扫码响应不是有效对象"));
    }
    Ok(value)
}

fn decrypt_login_data(cipher: &str) -> Result<Value, QrError> {
    let compact: String = cipher.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.is_empty() || compact.len() % 32 != 0 || !compact.bytes().all(|c| c.is_ascii_hexdigit()) {
        return Err(QrError::Protocol("扫码登录凭证密文无效"));
    }
    let bytes: Result<Vec<_>, _> = compact.as_bytes().chunks_exact(2)
        .map(|pair| {
            std::str::from_utf8(pair).ok().and_then(|s| u8::from_str_radix(s, 16).ok())
                .ok_or(QrError::Protocol("扫码登录凭证密文无效"))
        }).collect();
    let plain = ecb::Decryptor::<aes::Aes128>::new(DATA_KEY.into())
        .decrypt_padded_vec_mut::<Pkcs7>(&bytes?)
        .map_err(|_| QrError::Protocol("扫码登录凭证解密失败"))?;
    serde_json::from_slice(&plain)
        .map_err(|_| QrError::Protocol("扫码登录凭证内容无效"))
}

fn pick(value: &Value, keys: &[&str]) -> String {
    keys.iter().filter_map(|key| value.get(key).and_then(Value::as_str))
        .map(str::trim).find(|text| !text.is_empty()).unwrap_or_default().to_string()
}

fn outcome(value: &Value) -> Result<Outcome, QrError> {
    if value.get("success").and_then(Value::as_bool) == Some(true) {
        let data = match value.get("data") {
            Some(Value::String(cipher)) => decrypt_login_data(cipher)?,
            Some(data) if data.is_object() => data.clone(),
            _ => value.clone(),
        };
        let token = pick(&data, &["token", "authToken"]);
        let mut phone = pick(&data, &["account", "simplifyAccount", "phoneNumber", "msisdn"]);
        if phone.is_empty() {
            phone = BASE64.decode(pick(&data, &["encryptAccount"])).ok()
                .and_then(|bytes| String::from_utf8(bytes).ok()).unwrap_or_default();
        }
        let credentials = crate::credentials::Credentials::from_payload(&json!({
            "phone": phone, "token": token
        })).map_err(|_| QrError::Protocol("扫码已确认，但没有获取到完整登录凭证，请重新扫码"))?;
        return Ok(Outcome::Confirmed(credentials));
    }
    let code = value.get("code").map(|code| match code {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }).unwrap_or_default();
    match code.as_str() {
        "200059541" => Ok(Outcome::Pending),
        "200059548" => Ok(Outcome::Scanned),
        "200059542" => Ok(Outcome::Expired),
        "200059549" => Ok(Outcome::Cancelled),
        "200059543" | "200059545" | "200059546" | "200059547" =>
            Err(QrError::Protocol("移动云盘拒绝了本次扫码登录，请重新扫码")),
        _ => Err(QrError::Protocol("无法识别扫码登录状态，请刷新二维码重试")),
    }
}

fn poll_body(session_id: &str) -> Value {
    let secinfo = hex(&Sha1::digest(format!("fetion.com.cn:{session_id}"))).to_uppercase();
    json!({
        "msisdn": "", "random": "", "dycpwd": session_id, "cpid": 292,
        "clienttype": 670, "version": "mCloud_4.3.0_536", "pintype": 21,
        "secinfo": secinfo, "loginMode": "0", "extInfo": {}
    })
}

async fn request(client: &Client, url: &str, visitor: &str, body: &Value, encrypted: bool) -> Result<Value, QrError> {
    let timezone = FixedOffset::east_opt(8 * 3600)
        .ok_or(QrError::Protocol("无法生成扫码请求时间"))?;
    let timestamp = Utc::now().with_timezone(&timezone).format("%Y-%m-%d %H:%M:%S").to_string();
    let mut request = client.post(url)
        .header("mcloud-sign", sign(body, &timestamp, &random_alnum(16))?);
    for (name, value) in [
        ("accept", "application/json, text/plain, */*"),
        ("accept-language", "zh,zh-CN;q=0.9,en;q=0.8"),
        ("caller", "web"), ("cms-device", "default"),
        ("content-type", "application/json;charset=UTF-8"),
        ("inner-hcy-router-https", "1"), ("mcloud-channel", "1000101"),
        ("mcloud-client", "10701"), ("mcloud-route", "001"),
        ("mcloud-version", "7.18.1"), ("origin", "https://yun.139.com"),
        ("referer", "https://yun.139.com/"), ("x-huawei-channelsrc", "10000034"),
        ("x-inner-ntwk", "2"), ("x-m4c-caller", "PC"), ("x-m4c-src", "10002"),
        ("x-svctype", "1"), ("x-yun-api-version", "v1"),
        ("x-yun-app-channel", "10000034"), ("x-yun-channel-source", "10000034"),
        ("x-yun-module-type", "100"), ("x-yun-svc-type", "1"),
    ] {
        request = request.header(name, value);
    }
    let device = format!("||9|7.18.1|chrome|155.0.0.0|{visitor}||macos 10.15.7||zh-CN|||");
    request = request.header("x-deviceinfo", &device)
        .header("x-yun-client-info", format!("{device}dW5kZWZpbmVk||"));
    // The RSA endpoint requires plain JSON with hcy-cool-flag omitted.
    if encrypted {
        let mut iv = [0u8; 16];
        OsRng.fill_bytes(&mut iv);
        request = request.header("hcy-cool-flag", "1").body(encrypt_with_iv(body, &iv)?);
    } else {
        request = request.json(body);
    }
    let mut response = request.send().await.map_err(|_| QrError::Network)?;
    if !response.status().is_success() {
        return Err(QrError::Network);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| QrError::Network)? {
        if bytes.len().saturating_add(chunk.len()) > MAX_RESPONSE {
            return Err(QrError::Protocol("扫码响应超过长度限制"));
        }
        bytes.extend_from_slice(&chunk);
    }
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| QrError::Protocol("扫码响应文本编码无效"))?;
    parse_response(text)
}

impl Session {
    pub async fn begin(proxy: &str) -> Result<Self, QrError> {
        let test_base = crate::upstream::endpoint("MCLAW_TEST_QR_BASE", "")
            .map_err(|_| QrError::Protocol("扫码测试端点只允许本机回环地址"))?;
        let public_key_url = if test_base.is_empty() { PUBLIC_KEY_URL.into() }
            else { format!("{test_base}/orchestration/auth-rebuild/key/v1.0/getRsaPublicKey") };
        let poll_url = if test_base.is_empty() { POLL_URL.into() }
            else { format!("{test_base}/user/thirdlogin") };
        let mut builder = Client::builder().no_proxy()
            .user_agent("Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/155.0.0.0 Safari/537.36")
            .timeout(Duration::from_secs(8)).connect_timeout(Duration::from_secs(8))
            .redirect(reqwest::redirect::Policy::none());
        if !proxy.is_empty() && proxy != "direct" {
            Url::parse(proxy).map_err(|_| QrError::Protocol("扫码登录代理地址无效"))?;
            builder = builder.proxy(reqwest::Proxy::all(proxy)
                .map_err(|_| QrError::Protocol("扫码登录代理地址无效"))?);
        }
        let client = builder.build().map_err(|_| QrError::Protocol("无法初始化扫码登录连接"))?;
        let session_id = random_alnum(16);
        let mut visitor_bytes = [0u8; 16];
        OsRng.fill_bytes(&mut visitor_bytes);
        let visitor_id = hex(&visitor_bytes);
        let response = request(&client, &public_key_url, &visitor_id,
            &json!({"clientCode": "10701", "type": 1}), false).await?;
        if response.get("success").and_then(Value::as_bool) != Some(true) {
            return Err(QrError::Protocol("无法初始化二维码，请重试或使用手动添加账号"));
        }
        let key_text = response.pointer("/data/publicKey").or_else(|| response.get("data"))
            .and_then(Value::as_str).ok_or(QrError::Protocol("扫码公钥响应无效"))?;
        let key_bytes = BASE64.decode(key_text.split_whitespace().collect::<String>())
            .map_err(|_| QrError::Protocol("扫码公钥编码无效"))?;
        let public_key = RsaPublicKey::from_public_key_der(&key_bytes)
            .map_err(|_| QrError::Protocol("扫码公钥格式无效"))?;
        let mcloud_skey = BASE64.encode(public_key.encrypt(&mut OsRng, Pkcs1v15Encrypt,
            random_alnum(16).as_bytes()).map_err(|_| QrError::Protocol("无法初始化扫码会话密钥"))?);
        Ok(Self {
            flow: random_alnum(32),
            qr_content: format!("https://yun.139.com/w/#/qrcLogin?sID={session_id}&dID={visitor_id}&cType=9"),
            created: Instant::now(),
            session_id, visitor_id, client, poll_url, _mcloud_skey: mcloud_skey,
        })
    }

    pub fn expired(&self) -> bool {
        self.created.elapsed() >= LIFETIME
    }

    pub async fn poll(&self) -> Result<Outcome, QrError> {
        if self.expired() { return Ok(Outcome::Expired); }
        let response = request(&self.client, &self.poll_url, &self.visitor_id,
            &poll_body(&self.session_id), true).await?;
        if self.expired() { return Ok(Outcome::Expired); }
        outcome(&response)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn fixture() -> Value {
        serde_json::from_str(include_str!("../tests/qr-fixtures.json")).unwrap()
    }

    #[test]
    fn cbc_and_sign_match_independent_reference_vectors() {
        let data = fixture();
        let body = &data["body"];
        let iv = [0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15];
        assert_eq!(encrypt_with_iv(body, &iv).ok().as_deref(), data["wire"].as_str());
        assert_eq!(sign(body, "2026-11-17 12:34:56", "Ab0123456789xyZQ").ok().as_deref(), data["sign"].as_str());
        assert_eq!(decrypt_wire(data["wire"].as_str().unwrap()).ok().as_ref(), Some(body));
    }

    #[test]
    fn encrypted_success_recovers_credentials_and_masks_public_phone() {
        let data = fixture();
        let response = parse_response(data["successWire"].as_str().unwrap()).ok().unwrap();
        let credentials = match outcome(&response) {
            Ok(Outcome::Confirmed(credentials)) => credentials,
            _ => panic!("reference response must yield credentials"),
        };
        assert_eq!(credentials.phone, "13800138000");
        assert_eq!(credentials.token, "fixture|qr-login");
        let secret = credentials.to_secret(None);
        let public = credentials.public(&secret).to_string();
        assert!(!public.contains("fixture|qr-login"));
        assert!(!public.contains("13800138000"));
    }

    #[test]
    fn qr_statuses_and_malformed_success_do_not_create_accounts() {
        assert!(matches!(outcome(&json!({"code":"200059541"})), Ok(Outcome::Pending)));
        assert!(matches!(outcome(&json!({"code":200059548u64})), Ok(Outcome::Scanned)));
        assert!(matches!(outcome(&json!({"code":"200059542"})), Ok(Outcome::Expired)));
        assert!(matches!(outcome(&json!({"code":"200059549"})), Ok(Outcome::Cancelled)));
        for value in [
            json!({"success": true, "data": {}}),
            json!({"success": true, "data": "not-hex"}),
            json!({"code": "200059543", "message": "secret-token"}),
            json!({"code": "unexpected", "message": "secret-token"}),
        ] {
            match outcome(&value) {
                Err(error) => assert!(!error.to_string().contains("secret-token")),
                _ => panic!("invalid success or unknown status must fail"),
            }
        }
    }

    #[test]
    fn malformed_ciphertext_is_rejected_without_panicking() {
        for text in ["", "not-base64!", "AA==", "0000000000000000", "éé", "0"] {
            assert!(decrypt_wire(text).is_err());
            assert!(decrypt_login_data(text).is_err());
        }
        let data = fixture();
        let wrapped = json!({"data": data["successWire"]}).to_string();
        assert!(parse_response(&wrapped).ok().unwrap()["success"].as_bool().unwrap());
    }
}
