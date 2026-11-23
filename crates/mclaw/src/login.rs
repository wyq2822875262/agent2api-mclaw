//! In-memory QR flows. The gateway owns polling and persists the final account.
use crate::qr::{self, Outcome, QrError};
use agent2api_provider_protocol as proto;
use agent2api_provider_sdk::PluginCtx;
use serde_json::Value;
use std::collections::HashMap;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use std::time::{Duration, Instant};
use tokio::sync::Mutex;

const MAX_FLOWS: usize = 32;

#[derive(Default)]
pub struct Manager {
    flows: Mutex<HashMap<String, Arc<Flow>>>,
}

struct Flow {
    created: Instant,
    cancelled: AtomicBool,
    state: Mutex<State>,
}

struct State {
    session: qr::Session,
    failures: u8,
    last_poll: Option<Instant>,
    hint: String,
    finished: bool,
}

fn failed(message: impl Into<String>) -> proto::LoginPoll {
    proto::LoginPoll::Failed { message: message.into() }
}

impl Manager {
    pub async fn start(&self, ctx: &PluginCtx, payload: &Value) -> Result<proto::LoginStart, String> {
        if let Some(index) = payload.get("methodIndex") {
            if index.as_u64() != Some(2) {
                return Err("此登录方式需要通过账号表单提交".into());
            }
        }
        let proxy = if agent2api_provider_sdk::extensions::direct_proxy_override() {
            "direct".into()
        } else if let Some(value) = payload.get("proxy").filter(|value| !value.is_null()) {
            serde_json::from_value::<proto::WireProxy>(value.clone())
                .map_err(|_| "扫码登录代理参数无效")?.to_url()
        } else {
            ctx.env.proxy.clone()
        };
        {
            let mut flows = self.flows.lock().await;
            flows.retain(|_, flow| {
                if flow.created.elapsed() >= qr::LIFETIME {
                    flow.cancelled.store(true, Ordering::SeqCst);
                    false
                } else {
                    true
                }
            });
            if flows.len() >= MAX_FLOWS {
                return Err("等待中的扫码登录过多，请先取消旧流程".into());
            }
        }
        let session = qr::Session::begin(&proxy).await.map_err(|error| error.to_string())?;
        let started = proto::LoginStart {
            flow: session.flow.clone(),
            // The packaged page renders the QR URL supplied in instructions.
            url: String::new(),
            instructions: format!(
                "请使用移动云盘 App 扫描二维码，并在手机上确认。二维码有效期 5 分钟。\n二维码内容：{}",
                session.qr_content
            ),
            poll_interval_ms: qr::POLL_INTERVAL_MS,
            external_browser: false,
            allowed_hosts: Vec::new(),
        };
        let flow = Arc::new(Flow {
            created: session.created,
            cancelled: AtomicBool::new(false),
            state: Mutex::new(State {
                session,
                failures: 0,
                last_poll: None,
                hint: "等待扫码，请使用移动云盘 App".into(),
                finished: false,
            }),
        });
        let mut flows = self.flows.lock().await;
        if flows.len() >= MAX_FLOWS {
            return Err("等待中的扫码登录过多，请先取消旧流程".into());
        }
        flows.insert(started.flow.clone(), flow);
        Ok(started)
    }

    pub async fn poll(&self, flow_id: &str) -> proto::LoginPoll {
        let flow = self.flows.lock().await.get(flow_id).cloned();
        let Some(flow) = flow else {
            return failed("扫码登录已结束或不存在，请重新生成二维码");
        };
        let mut state = flow.state.lock().await;
        if flow.cancelled.load(Ordering::SeqCst) {
            return failed("已取消扫码登录");
        }
        if state.finished {
            return failed("本次扫码登录已结束，请刷新账号列表");
        }
        if state.session.expired() {
            state.finished = true;
            drop(state);
            self.flows.lock().await.remove(flow_id);
            return failed("二维码已过期，请刷新后重新扫码");
        }
        if state.last_poll.is_some_and(|last| last.elapsed() < Duration::from_millis(qr::POLL_INTERVAL_MS)) {
            return proto::LoginPoll::Pending { hint: Some(state.hint.clone()) };
        }
        state.last_poll = Some(Instant::now());
        let outcome = state.session.poll().await;
        // Cancellation removes the map entry and flips this flag without waiting
        // for the upstream request. An in-flight success must not revive it.
        if flow.cancelled.load(Ordering::SeqCst) {
            state.finished = true;
            return failed("已取消扫码登录");
        }
        let result = match outcome {
            Ok(Outcome::Pending) => {
                state.failures = 0;
                state.hint = "等待扫码，请使用移动云盘 App".into();
                proto::LoginPoll::Pending { hint: Some(state.hint.clone()) }
            }
            Ok(Outcome::Scanned) => {
                state.failures = 0;
                state.hint = "已扫码，请在手机上确认登录".into();
                proto::LoginPoll::Pending { hint: Some(state.hint.clone()) }
            }
            Ok(Outcome::Expired) => failed("二维码已过期，请刷新后重新扫码"),
            Ok(Outcome::Cancelled) => failed("手机端已取消登录，请刷新二维码"),
            Ok(Outcome::Confirmed(credentials)) => {
                let secret = credentials.to_secret(None);
                proto::LoginPoll::Done {
                    account: proto::NormalizedAccount {
                        public: credentials.public(&secret),
                        identity: credentials.identity(),
                        secret,
                        desktop: false,
                    },
                }
            }
            Err(QrError::Network) => {
                state.failures += 1;
                if state.failures >= 3 {
                    failed("扫码登录连续三次网络请求失败，请检查连接或代理后重试")
                } else {
                    state.hint = format!("网络暂时异常，正在重试（{}/3）", state.failures);
                    proto::LoginPoll::Pending { hint: Some(state.hint.clone()) }
                }
            }
            Err(error) => failed(error.to_string()),
        };
        if !matches!(result, proto::LoginPoll::Pending { .. }) {
            state.finished = true;
            drop(state);
            self.flows.lock().await.remove(flow_id);
        }
        result
    }

    pub async fn cancel(&self, flow_id: &str) {
        if let Some(flow) = self.flows.lock().await.remove(flow_id) {
            flow.cancelled.store(true, Ordering::SeqCst);
        }
    }
}
