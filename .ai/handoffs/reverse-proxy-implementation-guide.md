# Reverse Proxy Implementation Guide for MoonlightCode

**Purpose:** Intercept all Claude Code ↔ Anthropic API traffic for secret redaction and quota visibility.

## Quick Start

CC will make HTTPS requests to your proxy if you set:
```bash
export ANTHROPIC_BASE_URL=http://127.0.0.1:9999
export HTTPS_PROXY=http://127.0.0.1:9999  # Fallback for other domains
```

Your proxy receives all requests and must:
1. **Redact secrets** from request body before forwarding
2. **Capture response headers** (rate-limit, usage metadata)
3. **Expose quota state** to IDE (file or IPC)

---

## Request/Response Flow

### Incoming Request (from CC)

```
POST https://api.anthropic.com/v1/messages
Authorization: Bearer sk-ant-...
Content-Type: application/json

{
  "model": "claude-opus-4-5-20251001",
  "max_tokens": 4096,
  "messages": [
    {
      "role": "user",
      "content": "Read this file: /home/user/.env"  ← May contain secrets
    },
    {
      "role": "assistant",
      "content": "..."
    },
    {
      "role": "user",
      "content": "DATABASE_PASSWORD=secret123"  ← Redact this
    }
  ],
  "system": "You are an AI assistant..."
}
```

### Redaction Rules (Pattern Bank)

Apply these patterns to the **entire request body** (all `content` fields):

```
# API Keys & Tokens
- AWS: AKIA[0-9A-Z]{16}
- OpenAI: sk-[A-Za-z0-9]{48}
- Anthropic: sk-ant-[A-Za-z0-9]{88}
- Generic Bearer: Bearer [A-Za-z0-9\-._~+/]+=*
- GitHub: ghp_[A-Za-z0-9]{36}

# Credentials
- Password patterns: password['\"]?\s*[:=]\s*['\"]?([^'\";\n]+)['\"]?
- Connection strings: (?i)(mysql|postgres|mongodb)://[^\s]+
- AWS Access Key ID: AKIA[0-9A-Z]{16}
- AWS Secret: [A-Za-z0-9/+=]{40}

# Cloud Credentials
- GCP Service Account: "type": "service_account".*"private_key": "[^"]+
- Azure: DefaultEndpointsProtocol=https;AccountName=[^;]+;AccountKey=[^;]+

# Environment Variables (read from CC's captured output)
- ^[A-Z_]+=(.*?)$  (if marked sensitive in config)
```

**Redaction action:** Replace matched value with `[REDACTED: <type>]`

Example:
```json
{
  "messages": [
    {
      "content": "My AWS key is AKIAIOSFODNN7EXAMPLE and password is hunter2"
    }
  ]
}
```

Becomes:
```json
{
  "messages": [
    {
      "content": "My AWS key is [REDACTED: AWS_ACCESS_KEY] and password is [REDACTED: PASSWORD]"
    }
  ]
}
```

---

## Response Handling

### Capture Headers
```
HTTP/1.1 200 OK
RateLimit-Limit-Requests: 50000
RateLimit-Remaining-Requests: 49999
RateLimit-Reset-Requests: 2026-06-08T15:30:00Z
Content-Type: application/json

{
  "id": "msg_...",
  "type": "message",
  "role": "assistant",
  "content": [...],
  "usage": {
    "input_tokens": 1234,
    "output_tokens": 567,
    "cache_creation_input_tokens": 100,
    "cache_read_input_tokens": 50
  }
}
```

### Expose Quota to IDE

Write a JSON state file that your IDE polls:

```json
// ~/.moonlight/cc-quota.json (or IPC endpoint)
{
  "timestamp_millis": 1717942800000,
  "rate_limit": {
    "limit_requests": 50000,
    "remaining_requests": 49999,
    "reset_requests": "2026-06-08T15:30:00Z"
  },
  "usage": {
    "input_tokens": 1234,
    "output_tokens": 567,
    "cache_creation_input_tokens": 100,
    "cache_read_input_tokens": 50
  },
  "session_id": "abc123def456"
}
```

Your IDE reads this file every 5-10 seconds to decide:
- Whether to launch a new managed session
- Whether to queue/delay pending sessions
- Warn operator if quota is exhausted

---

## Architecture Options

### Option A: Rust (Recommended for MoonlightCode)

```rust
// src/bin/cc_proxy.rs
use hyper::{Server, Request, Response, Body, Client};
use std::sync::Arc;
use tokio::sync::Mutex;

async fn handle_request(req: Request<Body>) -> Result<Response<Body>, Box<dyn std::error::Error>> {
    let (parts, body) = req.into_parts();
    
    // 1. Read body
    let bytes = hyper::body::to_bytes(body).await?;
    let mut payload = String::from_utf8(bytes.to_vec())?;
    
    // 2. Redact secrets
    payload = redact_secrets(&payload);
    
    // 3. Forward to api.anthropic.com
    let client = Client::new();
    let mut new_req = Request::builder()
        .method(parts.method)
        .uri("https://api.anthropic.com/v1/messages")
        .header("Content-Type", "application/json");
    
    // Preserve auth header
    if let Some(auth) = parts.headers.get("Authorization") {
        new_req = new_req.header("Authorization", auth);
    }
    
    let resp = client.request(new_req.body(Body::from(payload))?).await?;
    
    // 4. Capture rate-limit headers
    let rate_limit = extract_rate_limit_headers(&resp.headers());
    write_quota_state(&rate_limit).await?;
    
    Ok(resp)
}

fn redact_secrets(payload: &str) -> String {
    let patterns = vec![
        (r"AKIA[0-9A-Z]{16}", "[REDACTED: AWS_ACCESS_KEY]"),
        (r"sk-ant-[A-Za-z0-9]{88}", "[REDACTED: ANTHROPIC_API_KEY]"),
        // ... more patterns
    ];
    
    let mut result = payload.to_string();
    for (pattern, replacement) in patterns {
        result = regex::Regex::new(pattern)
            .unwrap()
            .replace_all(&result, replacement)
            .to_string();
    }
    result
}
```

### Option B: Go (Simpler for quick iteration)

```go
package main

import (
	"io"
	"net/http"
	"net/http/httputil"
	"regexp"
)

func redactSecrets(body string) string {
	patterns := map[string]string{
		`AKIA[0-9A-Z]{16}`:            "[REDACTED: AWS_ACCESS_KEY]",
		`sk-ant-[A-Za-z0-9]{88}`:      "[REDACTED: ANTHROPIC_API_KEY]",
	}
	
	result := body
	for pattern, replacement := range patterns {
		re := regexp.MustCompile(pattern)
		result = re.ReplaceAllString(result, replacement)
	}
	return result
}

func main() {
	target := "https://api.anthropic.com"
	
	http.HandleFunc("/v1/messages", func(w http.ResponseWriter, r *http.Request) {
		// Read request body
		body, _ := io.ReadAll(r.Body)
		payload := string(body)
		
		// Redact
		payload = redactSecrets(payload)
		
		// Forward with redacted payload
		proxy := httputil.NewSingleHostReverseProxy(/* ... */)
		// ... handle response headers
	})
	
	http.ListenAndServe(":9999", nil)
}
```

---

## Integration with MoonlightCode

### At Session Launch

In `workspace.rs` or `session_monitor.rs`, inject proxy env vars:

```rust
let env_vars = vec![
    ("ANTHROPIC_BASE_URL", "http://127.0.0.1:9999"),
    ("HTTPS_PROXY", "http://127.0.0.1:9999"),
];

// When spawning CC session, pass these env vars to the PTY
```

### IDE Quota Polling

In `panels/services.rs` (or new status panel):

```rust
fn poll_quota_state(cx: &mut ViewContext<Self>) {
    std::thread::spawn(|| {
        loop {
            if let Ok(quota_json) = std::fs::read_to_string(
                dirs::home_dir().unwrap().join(".moonlight/cc-quota.json")
            ) {
                if let Ok(quota) = serde_json::from_str::<QuotaState>(&quota_json) {
                    cx.emit(QuotaUpdated(quota));
                }
            }
            std::thread::sleep(std::time::Duration::from_secs(5));
        }
    });
}
```

### Audit Logging

Record redaction events in `moonlight.db`:

```sql
INSERT INTO audit_log (session_id, action, details, timestamp_millis)
VALUES (
  'abc123',
  'CC_SECRET_REDACTED',
  '{"count": 2, "patterns": ["AWS_ACCESS_KEY", "PASSWORD"]}',
  1717942800000
);
```

---

## Testing

### Unit Test: Redaction

```rust
#[test]
fn test_redact_aws_keys() {
    let payload = r#"{"content": "My key is AKIAIOSFODNN7EXAMPLE"}"#;
    let redacted = redact_secrets(payload);
    assert!(!redacted.contains("AKIAIOSFODNN7EXAMPLE"));
    assert!(redacted.contains("[REDACTED: AWS_ACCESS_KEY]"));
}
```

### Integration Test: Full Flow

```bash
# 1. Start proxy
cargo run --bin cc_proxy &
PROXY_PID=$!

# 2. Point CC at proxy
export ANTHROPIC_BASE_URL=http://127.0.0.1:9999
export ANTHROPIC_API_KEY=sk-ant-...

# 3. Launch CC session with a secret in the prompt
claude -p "My password is hunter2" 2>&1 | tee /tmp/cc-output.log

# 4. Check redaction happened (proxy logs should show [REDACTED])
grep "REDACTED" /tmp/cc-output.log

# 5. Check quota file was written
jq . ~/.moonlight/cc-quota.json | head

kill $PROXY_PID
```

---

## Known Challenges

1. **Streaming responses:** Proxy must handle chunked `Transfer-Encoding: chunked` and preserve SSE format
2. **Connection pooling:** Keep-alive connections; proxy should reuse them
3. **Auth headers:** Preserve API key; don't redact it (it's already in your control)
4. **mTLS (if operator uses):** Proxy must forward client certs to api.anthropic.com
5. **Retry handling:** Proxy should not retry on 429s—let CC handle it (proxy is just a pipe)

---

## Rollout Checklist

- [ ] Proxy compiles and starts on `127.0.0.1:9999`
- [ ] Proxy forwards requests to api.anthropic.com unmodified (first pass)
- [ ] Proxy redacts secrets (second pass)
- [ ] Proxy captures rate-limit headers and writes quota JSON
- [ ] IDE reads quota file and displays headroom
- [ ] Operator launches managed session with proxy enabled
- [ ] Secrets in CC's file reads do NOT appear in API requests (verify in proxy logs)
- [ ] Rate limits are visible in IDE status
- [ ] Audit log records redaction events

---

**Status:** Ready to implement · **Owner:** [MoonlightCode backend lane]  
**Next:** Coordinate with operator on Rust vs. Go choice + secret detection rules
