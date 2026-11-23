//! Dynamic account forms consumed by the gateway through admin.action/auth.methods.
use serde_json::{json, Value};

pub fn methods() -> Value {
    let channel = json!({
        "key": "channelId", "type": "text", "label": "渠道 ID（可选）",
        "placeholder": "通常保持默认值 102001", "optional": true,
        "secret": false, "default": "102001"
    });
    json!({
        "instructions": "扫码添加账户请打开侧栏「MClaw 登录」。也可在此手动导入你自己的登录凭证。添加后刷新模型目录；登录过期时可重新扫码或导入。",
        "methods": [
            {
                "methodIndex": 0, "type": "api", "label": "手机号 + Token",
                "instructions": "填写登录移动云盘的手机号，以及完整的登录 Token（包含 | 的字符串）。",
                "prompts": [
                    {
                        "key": "phone", "type": "text", "label": "手机号",
                        "placeholder": "11 位手机号，支持 +86 前缀",
                        "optional": false, "secret": false
                    },
                    {
                        "key": "token", "type": "text", "label": "登录 Token",
                        "placeholder": "粘贴完整的移动云盘登录 Token",
                        "optional": false, "secret": true
                    },
                    channel.clone()
                ]
            },
            {
                "methodIndex": 1, "type": "api", "label": "Basic / Cookie 导入",
                "instructions": "粘贴包含手机号的 Basic 凭证、pc:手机号:TOKEN，或含该凭证的 authorization Cookie。手机号会自动识别；只有纯 Token 时请使用另一种方式。",
                "prompts": [
                    {
                        "key": "token", "type": "text", "label": "Basic 凭证或 Cookie",
                        "placeholder": "Basic … / pc:手机号:TOKEN / authorization=…",
                        "optional": false, "secret": true
                    },
                    channel
                ]
            }
        ]
    })
}

/// The current gateway sends {methodIndex, inputs}; retain legacy flat JSON imports.
pub fn payload(value: Value) -> Result<Value, String> {
    let object = value.as_object().ok_or("账号凭证必须是 JSON 对象")?;
    let Some(index) = object.get("methodIndex") else {
        if object.contains_key("inputs") {
            return Err("缺少登录方式，请重新打开添加账号页面".into());
        }
        return Ok(value);
    };
    let index = index.as_u64().filter(|index| *index == 0 || *index == 1)
        .ok_or("登录方式无效，请重新打开添加账号页面")?;
    let inputs = object.get("inputs").and_then(Value::as_object)
        .ok_or("请填写账号表单后重新提交")?;
    let allowed: &[&str] = if index == 0 { &["phone", "token", "channelId"] } else { &["token", "channelId"] };
    if inputs.iter().any(|(key, value)| !allowed.contains(&key.as_str()) || !value.is_string()) {
        return Err("账号表单字段无效，请重新打开添加账号页面".into());
    }
    let filled = |key| inputs.get(key).and_then(Value::as_str).is_some_and(|text| !text.trim().is_empty());
    if !filled("token") {
        return Err("请粘贴完整的登录 Token 或 Basic / Cookie 凭证".into());
    }
    if index == 0 && !filled("phone") {
        return Err("请填写登录移动云盘的手机号".into());
    }
    Ok(Value::Object(inputs.clone()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::credentials::Credentials;

    #[test]
    fn dynamic_and_legacy_imports_have_the_same_identity() {
        let flat = json!({"phone":"13800138000", "token":"fixture|session", "channelId":"102001"});
        let old = Credentials::from_payload(&payload(flat.clone()).unwrap()).unwrap();
        let form = Credentials::from_payload(&payload(json!({"methodIndex":0,"inputs":flat})).unwrap()).unwrap();
        let imported = Credentials::from_payload(&payload(json!({"methodIndex":1,"inputs":{"token":old.basic()}})).unwrap()).unwrap();
        assert_eq!(old.identity(), form.identity());
        assert_eq!(old.identity(), imported.identity());
        assert_eq!(old.token, form.token);
        assert_eq!(old.token, imported.token);
    }

    #[test]
    fn malformed_dynamic_submissions_fail_without_echoing_credentials() {
        for input in [
            json!({"inputs":{"token":"private|fixture"}}),
            json!({"methodIndex":2,"inputs":{"token":"private|fixture"}}),
            json!({"methodIndex":"0","inputs":{"token":"private|fixture"}}),
            json!({"methodIndex":0,"inputs":{"token":"private|fixture"}}),
            json!({"methodIndex":0,"inputs":{"phone":"13800138000","token":false}}),
            json!({"methodIndex":1,"inputs":{"token":"private|fixture","constructor":"bad"}}),
        ] {
            let error = payload(input).unwrap_err();
            assert!(!error.contains("private|fixture"));
        }
    }
}
