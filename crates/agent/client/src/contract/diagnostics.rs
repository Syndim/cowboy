use serde_json::Value;

pub fn diagnostic_error_text(text: &str) -> String {
    let mut safe = text.to_string();
    for marker in ["; agent reply:", "; last reply:"] {
        if let Some(start) = safe.find(marker) {
            safe.truncate(start + marker.len());
            safe.push_str(" [redacted]");
        }
    }
    for prefix in [
        "unknown variant ",
        "unknown field ",
        "invalid value: string ",
        "invalid type: string ",
    ] {
        if let Some(start) = safe.find(prefix).map(|start| start + prefix.len())
            && let Some(quote) = safe[start..]
                .chars()
                .next()
                .filter(|quote| ['`', '\'', '"'].contains(quote))
            && let Some(end) = safe[start + 1..].find(&format!("{quote}, expected"))
        {
            safe.replace_range(start + 1..start + 1 + end, "[redacted]");
        }
    }
    for key in [
        "authorization",
        "bearer",
        "token",
        "access_token",
        "accesstoken",
        "api_key",
        "api-key",
        "apikey",
        "password",
        "client_secret",
        "secret",
        "credential",
        "private_key",
        "signature",
        "x-amz-signature",
        "prompt",
        "body",
        "data",
        "rawinput",
        "arguments",
        "content",
    ] {
        let mut offset = 0;
        while let Some(found) = safe[offset..].to_ascii_lowercase().find(key) {
            let start = offset + found;
            offset = start + key.len();
            if start > 0 && safe.as_bytes()[start - 1].is_ascii_alphanumeric() {
                continue;
            }
            let tail = safe[offset..].trim_start_matches(['\"', '\'', '`']);
            let tail = tail.trim_start();
            let value = if key == "bearer" && tail.len() < safe[offset..].len() {
                tail
            } else if let Some(value) = tail.strip_prefix([':', '=']) {
                value.trim_start()
            } else {
                continue;
            };
            let value_start = safe.len() - value.len();
            let mut json = serde_json::Deserializer::from_str(value).into_iter::<Value>();
            let length = if key == "authorization"
                && (value.to_ascii_lowercase().starts_with("bearer ")
                    || value.to_ascii_lowercase().starts_with("basic "))
            {
                let after_scheme = &value[value.find(' ').unwrap() + 1..];
                let token = after_scheme.trim_start();
                let token_start = value.len() - token.len();
                token_start
                    + value[token_start..]
                        .find(|ch: char| ch.is_whitespace() || [',', ';', '}'].contains(&ch))
                        .unwrap_or(value.len() - token_start)
            } else if value.starts_with(['{', '[', '\"'])
                && json.next().is_some_and(|result| result.is_ok())
            {
                json.byte_offset()
            } else if let Some(quote) = value
                .chars()
                .next()
                .filter(|quote| ['\"', '\'', '`'].contains(quote))
            {
                value[1..].find(quote).map_or(value.len(), |end| end + 2)
            } else if ["prompt", "body", "data", "rawinput", "arguments", "content"].contains(&key)
            {
                value.find([';', '\n', '\r']).unwrap_or(value.len())
            } else {
                value
                    .find(|ch: char| ch.is_whitespace() || [',', ';', '}'].contains(&ch))
                    .unwrap_or(value.len())
            };
            if length == 0 {
                continue;
            }
            safe.replace_range(value_start..value_start + length, "[redacted]");
            offset = value_start + "[redacted]".len();
        }
    }
    for prefix in ["sk-", "ghp_", "github_pat_", "xoxb-", "xoxp-"] {
        let mut offset = 0;
        while let Some(found) = safe[offset..].find(prefix) {
            let start = offset + found;
            offset = start + prefix.len();
            if start > 0 && safe.as_bytes()[start - 1].is_ascii_alphanumeric() {
                continue;
            }
            let length = safe[start..]
                .find(|ch: char| !(ch.is_ascii_alphanumeric() || ['-', '_'].contains(&ch)))
                .unwrap_or(safe.len() - start);
            safe.replace_range(start..start + length, "[redacted]");
            offset = start + "[redacted]".len();
        }
    }
    safe
}
