mod auth;
mod credentials;
mod login;
mod qr;
mod upstream;

use agent2api_provider_protocol as proto;
use agent2api_provider_sdk::{ChatPlan, PluginCtx, Provider, ProviderMeta, SendFuture, StoreAccount};
use serde_json::{json, Value};
use std::collections::HashMap;
use std::sync::Arc;
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const ID: &str = "mclaw";
type Slot = Arc<Mutex<Option<Cached>>>;

struct Cached {
    fetched: Instant,
    data: upstream::KeyData,
}

#[derive(Default)]
struct MClaw {
    cache: Mutex<HashMap<String, Slot>>,
    login: login::Manager,
}

impl MClaw {
    async fn stored(&self, ctx: &PluginCtx, account: &str) -> Result<StoreAccount, String> {
        if account.is_empty() {
            ctx.store_list(ID).await?.into_iter().next().ok_or_else(|| "请先添加 MClaw 账号".into())
        } else {
            ctx.store_get(ID, account).await.map_err(Into::into)
        }
    }

    async fn key(&self, ctx: &PluginCtx, stored: &StoreAccount, explicit: Option<&proto::WireProxy>, force: bool) -> Result<upstream::KeyData, String> {
        let credentials = credentials::Credentials::from_payload(&stored.secret)?;
        let proxy = upstream::resolve_proxy(ctx, stored, explicit)?;
        let fingerprint = credentials.fingerprint(&proxy);
        let slot = self.cache.lock().await.entry(stored.id.clone()).or_default().clone();
        let mut cached = slot.lock().await;
        if !force {
            if let Some(entry) = cached.as_ref() {
                if entry.data.fingerprint == fingerprint && entry.fetched.elapsed() < Duration::from_secs(300) {
                    return Ok(entry.data.clone());
                }
            }
        }
        let data = upstream::fetch_key(&credentials, &proxy).await?;
        *cached = Some(Cached { fetched: Instant::now(), data: data.clone() });
        Ok(data)
    }

    async fn catalog(&self, ctx: &PluginCtx, account: &str, force: bool) -> Result<proto::ModelList, String> {
        let stored = self.stored(ctx, account).await?;
        let key = self.key(ctx, &stored, None, force).await?;
        let credentials = credentials::Credentials::from_payload(&stored.secret)?;
        let proxy = upstream::resolve_proxy(ctx, &stored, None)?;
        let models = upstream::catalog(&credentials, &proxy, &key).await;
        Ok(proto::ModelList { models, meta: proto::CatalogMeta { remote: true, refreshed_at: upstream::now_ms() } })
    }
}

impl Provider for MClaw {
    fn meta(&self) -> ProviderMeta {
        ProviderMeta::new(ID, "MClaw 移动云盘")
            .with_capabilities(proto::Capabilities { model_refresh: true, usage: true, web_login: true, sse_model_rewrite: true, ..Default::default() })
            .with_login(vec![proto::LoginMethod::Token {
                label: "移动云盘登录 Token / Basic / Cookie".into(),
                placeholder: "粘贴含 | 的登录 token、Basic 凭证或 authorization Cookie".into(),
                extra_fields: vec![
                    proto::LoginField { key: "phone".into(), label: "手机号".into(), placeholder: "纯 token 必填；Basic 中含手机号时可留空".into(), optional: true, multiline: false },
                    proto::LoginField { key: "channelId".into(), label: "渠道 ID".into(), placeholder: "102001（默认）".into(), optional: true, multiline: false },
                ],
            }, proto::LoginMethod::Browser {
                button: "扫码登录".into(),
                hint: Some("请打开侧栏「MClaw 登录」页面生成二维码并扫描。".into()),
            }])
            .with_account_features(proto::AccountFeatures { usage: true, identifier: vec!["phone".into()], ..Default::default() })
    }

    fn admin_action<'a>(&'a self, _ctx: &'a PluginCtx, _account: &'a str, action: &'a str, _payload: &'a Value) -> SendFuture<'a, Value> {
        Box::pin(async move {
            if action == "auth.methods" {
                Ok(auth::methods())
            } else {
                Err(agent2api_provider_sdk::unsupported("MClaw 不支持此管理动作"))
            }
        })
    }

    fn normalize_account<'a>(&'a self, _ctx: &'a PluginCtx, input: proto::NormalizeInput) -> SendFuture<'a, proto::NormalizedAccount> {
        Box::pin(async move {
            let payload = match input.source {
                proto::NormalizeSource::Manual { payload } => payload,
                proto::NormalizeSource::FileText { content, .. } => credentials::file_payload(&content)?,
                proto::NormalizeSource::FileBase64 { content, .. } => {
                    use base64::Engine;
                    let data = base64::engine::general_purpose::STANDARD.decode(content.trim()).map_err(|_| "账号文件不是合法 base64")?;
                    credentials::file_payload(std::str::from_utf8(&data).map_err(|_| "账号文件必须是 UTF-8")?)?
                }
                proto::NormalizeSource::Desktop => return Err("UNSUPPORTED:请粘贴登录凭证或导入单账号 JSON".into()),
            };
            let payload = auth::payload(payload)?;
            let credentials = credentials::Credentials::from_payload(&payload)?;
            let name = input.name.filter(|name| !name.trim().is_empty()).or_else(|| payload.get("name").and_then(Value::as_str).map(str::to_string));
            let secret = credentials.to_secret(name.as_deref());
            Ok(proto::NormalizedAccount { public: credentials.public(&secret), identity: credentials.identity(), secret, desktop: false })
        })
    }

    fn account_view(&self, secret: &Value) -> Result<Value, String> {
        Ok(credentials::Credentials::from_payload(secret)?.public(secret))
    }

    fn login_start<'a>(&'a self, ctx: &'a PluginCtx, _method: &'a str, payload: &'a Value) -> SendFuture<'a, proto::LoginStart> {
        Box::pin(async move { self.login.start(ctx, payload).await })
    }

    fn login_poll<'a>(&'a self, _ctx: &'a PluginCtx, flow: &'a str) -> SendFuture<'a, proto::LoginPoll> {
        Box::pin(async move { Ok(self.login.poll(flow).await) })
    }

    fn login_cancel<'a>(&'a self, _ctx: &'a PluginCtx, flow: &'a str) -> SendFuture<'a, ()> {
        Box::pin(async move {
            self.login.cancel(flow).await;
            Ok(())
        })
    }

    fn account_identity(&self, secret: &Value) -> Result<String, String> {
        Ok(credentials::Credentials::from_payload(secret)?.identity())
    }

    fn ensure_token<'a>(&'a self, ctx: &'a PluginCtx, account: &'a str) -> SendFuture<'a, String> {
        Box::pin(async move { let stored = self.stored(ctx, account).await?; Ok(self.key(ctx, &stored, None, false).await?.key) })
    }

    fn refresh_token<'a>(&'a self, ctx: &'a PluginCtx, account: &'a str) -> SendFuture<'a, String> {
        // Reacquire the LLM key; this cannot renew an expired cloud-drive login token.
        Box::pin(async move { let stored = self.stored(ctx, account).await?; Ok(self.key(ctx, &stored, None, true).await?.key) })
    }

    fn build_chat<'a>(&'a self, ctx: &'a PluginCtx, req: proto::ChatRequest) -> SendFuture<'a, ChatPlan> {
        Box::pin(async move {
            let stored = self.stored(ctx, &req.account).await?;
            let key = self.key(ctx, &stored, req.proxy.as_ref(), false).await?;
            let mut body = req.body;
            let object = body.as_object_mut().ok_or("对话请求体必须是 JSON 对象")?;
            if !req.wire_model.is_empty() { object.insert("model".into(), json!(req.wire_model)); }
            if object.get("model").and_then(Value::as_str).unwrap_or("").is_empty() { return Err("请选择 MClaw 模型编码".into()); }
            object.insert("stream".into(), json!(req.stream));
            Ok(ChatPlan::chat(key.url, vec![
                ("Authorization".into(), format!("Bearer {}", key.key)),
                ("Content-Type".into(), "application/json".into()),
                ("Accept".into(), if req.stream { "text/event-stream" } else { "application/json" }.into()),
            ], body))
        })
    }

    fn models<'a>(&'a self, ctx: &'a PluginCtx, account: &'a str) -> SendFuture<'a, proto::ModelList> {
        Box::pin(async move { self.catalog(ctx, account, false).await })
    }

    fn refresh_models<'a>(&'a self, ctx: &'a PluginCtx, account: &'a str, force: bool) -> SendFuture<'a, proto::RefreshOutcome> {
        Box::pin(async move { Ok(proto::RefreshOutcome::refreshed(self.catalog(ctx, account, force).await?.models.len())) })
    }

    fn usage<'a>(&'a self, ctx: &'a PluginCtx, account: &'a str) -> SendFuture<'a, Value> {
        Box::pin(async move {
            let stored = self.stored(ctx, account).await?;
            let credentials = credentials::Credentials::from_payload(&stored.secret)?;
            upstream::quota(&credentials, &upstream::resolve_proxy(ctx, &stored, None)?).await
        })
    }

    fn classify(&self, status: u16, body: &Value) -> proto::ErrorClass {
        let code = body.get("code").or_else(|| body.pointer("/error/code"));
        let code_text = code.and_then(Value::as_str).unwrap_or("");
        let upstream_code = code.and_then(|v| v.as_i64().or_else(|| v.as_str()?.parse().ok()));
        let message = body.pointer("/error/message").or_else(|| body.get("message")).and_then(Value::as_str).unwrap_or("MClaw 上游请求失败").to_string();
        if status == 401 || status == 403 || ["01000003", "01000002"].contains(&code_text) {
            proto::ErrorClass::TokenExpired { message }
        } else if status == 429 || status == 402 {
            proto::ErrorClass::QuotaLimited { reset_at: None, message, upstream_code, status: 429 }
        } else {
            proto::ErrorClass::Fatal { status: if status < 400 { 502 } else { status }, message, upstream_code }
        }
    }

    fn is_error_response(&self, status: u16, _headers: &std::collections::BTreeMap<String, String>) -> bool { status >= 400 }
}

fn main() { agent2api_provider_sdk::serve_blocking(vec![Box::new(MClaw::default())]); }
