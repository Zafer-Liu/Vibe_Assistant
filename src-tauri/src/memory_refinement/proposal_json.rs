//! Extract one complete proposal envelope without guessing missing facts/fields.
//! Diagnostics contain schema names and positions, never source/response text.
use super::{Proposal, ProposalResponse};
use serde_json::Value;

fn decode(body: &str) -> Result<Vec<Proposal>, String> {
    let value: Value = serde_json::from_str(body).map_err(|error| {
        let category = if error.is_eof() { "JSON 未完整结束" } else { "JSON 语法错误" };
        format!("精炼建议格式错误：{category}（行 {}，列 {}）", error.line(), error.column())
    })?;
    let actions = value.as_object().and_then(|object| object.get("actions")).and_then(Value::as_array)
        .ok_or("精炼建议格式错误：顶层必须为对象，且 actions 必须为数组")?;
    for (index, action) in actions.iter().enumerate() {
        let field_error = |field: &str, expected: &str| format!("精炼建议字段错误：actions[{}].{field} 必须为{expected}", index + 1);
        let object = action.as_object().ok_or_else(|| field_error("记录", "对象"))?;
        for field in ["kind", "reason"] {
            if !object.get(field).is_some_and(Value::is_string) { return Err(field_error(field, "字符串")); }
        }
        if !object.get("memory_ids").and_then(Value::as_array).is_some_and(|ids| ids.iter().all(Value::is_string)) {
            return Err(field_error("memory_ids", "字符串数组"));
        }
        for field in ["content", "memory_type", "durability"] {
            if object.get(field).is_some_and(|value| !value.is_string()) { return Err(field_error(field, "字符串")); }
        }
        if object.get("recommended_source_id").is_some_and(|value| !value.is_null() && !value.is_string()) {
            return Err(field_error("recommended_source_id", "字符串"));
        }
        if object.get("kind").and_then(Value::as_str) == Some("conflict") {
            for field in ["content", "recommended_source_id"] {
                if !object.get(field).and_then(Value::as_str).is_some_and(|value| !value.trim().is_empty()) {
                    return Err(field_error(field, "非空字符串（冲突必须给出处理建议及推荐依据）"));
                }
            }
        }
        if object.get("evidence").is_some_and(|value| !value.as_array().is_some_and(|items| items.iter().all(Value::is_string))) {
            return Err(field_error("evidence", "字符串数组"));
        }
    }
    serde_json::from_value::<ProposalResponse>(value).map(|response| response.actions)
        .map_err(|_| "精炼建议字段错误：建议不符合数据结构".into())
}

pub(super) fn parse(text: &str) -> Result<Vec<Proposal>, String> {
    let text = crate::thinking::strip_thinking_blocks(text);
    let text = text.trim().trim_start_matches('\u{feff}');
    // Prefer the exact document. Arrays/null are rejected by schema validation,
    // never reinterpreted as an empty result or salvaged as nested suggestions.
    if text.starts_with('[') || serde_json::from_str::<Value>(text).is_ok() { return decode(text); }
    let mut start = None;
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    let mut envelope = None;
    for (index, ch) in text.char_indices() {
        if start.is_none() {
            if ch == '{' { start = Some(index); depth = 1; }
            continue;
        }
        if quoted {
            if escaped { escaped = false; }
            else if ch == '\\' { escaped = true; }
            else if ch == '"' { quoted = false; }
            continue;
        }
        match ch {
            '"' => quoted = true,
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    if envelope.is_some() { return Err("精炼建议格式错误：返回多个 JSON 对象，无法确定唯一结果".into()); }
                    envelope = Some(&text[start.take().expect("open envelope")..index + 1]);
                }
            }
            _ => {},
        }
    }
    if start.is_some() { return Err("精炼建议格式错误：JSON 未完整结束".into()); }
    decode(envelope.ok_or("精炼建议格式错误：没有找到 JSON 对象")?)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_actions_and_one_fenced_envelope_are_valid() {
        for text in ["{\"actions\":[]}", "```json\n{\"actions\":[]}\n```", "以下是建议：\n{\"actions\":[]}\n检查结束"] {
            assert!(parse(text).unwrap().is_empty());
        }
    }

    #[test]
    fn escaped_quotes_braces_and_code_fences_in_content_remain_literal() {
        let body = serde_json::json!({"actions":[{"kind":"rewrite", "memory_ids":["a"],
            "content":"保留 {代码}、\\路径和\"引号\"；```json", "reason":"原始证据", "evidence":["source"]}]}).to_string();
        let result = parse(&format!("```json\n{body}\n```" )).unwrap();
        assert_eq!(result[0].content, "保留 {代码}、\\路径和\"引号\"；```json");
    }

    #[test]
    fn conflicts_require_an_actual_draft_and_recommended_source() {
        let mut value = serde_json::json!({"actions":[{"kind":"conflict","memory_ids":["a","b"],
            "content":"Keep the recorded transition from A to B", "reason":"Explicit reversal in the source",
            "evidence":["b"], "recommended_source_id":"b"}]});
        let action = parse(&value.to_string()).unwrap().pop().unwrap();
        assert_eq!(action.recommended_source_id.as_deref(), Some("b"));
        value["actions"][0]["recommended_source_id"] = Value::Null;
        assert!(parse(&value.to_string()).err().unwrap().contains("recommended_source_id"));
        value["actions"][0]["recommended_source_id"] = Value::String("b".into());
        value["actions"][0]["content"] = Value::String(" ".into());
        assert!(parse(&value.to_string()).err().unwrap().contains("content"));
    }

    #[test]
    fn multiple_incomplete_or_wrongly_typed_results_are_never_silently_completed() {
        for text in ["} text {", "{\"actions\":[", "{\"actions\":[]} {\"actions\":[]}", "null", "[]", "[{\"actions\":[]},]", "{\"actions\":null}"] {
            assert!(parse(text).is_err(), "{text}");
        }
        let error = parse(r#"{"actions":[{"kind":"rewrite","memory_ids":["a"],"reason":"secret-value","evidence":[{"text":"private"}]}]}"#).err().unwrap();
        assert!(error.contains("actions[1].evidence"));
        assert!(!error.contains("private") && !error.contains("secret-value"));
        let error = parse("{\"actions\":[],}").err().unwrap();
        assert!(error.contains("行") && error.contains("列"));
    }
}
