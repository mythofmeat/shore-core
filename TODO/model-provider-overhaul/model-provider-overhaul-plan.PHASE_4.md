# Phase 4: Multiple API keys per provider with non-sticky fallback

## Goal

Allow providers to define ordered named API keys.

For every request, Shore tries keys in configured order. If a key fails due to quota/budget/credential failure, Shore warns visibly and retries the same request with the next key.

Fallback is not sticky.

## Config

Example:

```toml
[providers.openrouter]
sdk = "openai"
base_url = "https://openrouter.ai/api/v1"

[[providers.openrouter.keys]]
name = "budget"
env = "OPENROUTER_API_KEY_BUDGET"
warn_on_fallback = true

[[providers.openrouter.keys]]
name = "overflow"
env = "OPENROUTER_API_KEY_OVERFLOW"

[[providers.openrouter.keys]]
name = "emergency"
env = "OPENROUTER_API_KEY_EMERGENCY"
enabled = false
```

## Behavior

For each request:

```text
1. Build request for selected provider/model.
2. Resolve enabled provider keys in configured order.
3. Try first key.
4. If it succeeds, use it.
5. If it fails with quota/budget/credential failure, warn and try next key.
6. If next key succeeds, continue normally.
7. If all keys fail, return the last useful error with context.
8. Next user message starts again from key 1.
```

## Warning behavior

When falling back, emit a visible warning to connected clients.

Example:

```text
Budget warning: OpenRouter key "budget" appears exhausted. Continuing with fallback key "overflow".
```

Do not expose:

```text
actual API key value
full env var value
secrets in debug payload logs
```

It is okay to expose:

```text
provider key
friendly key name
status code
sanitized reason
```

## Error classification

Add provider-aware credential failure classification.

Suggested enum:

```rust
enum CredentialFailureKind {
    MissingKey,
    InvalidKey,
    QuotaExhausted,
    BudgetExhausted,
    RateLimitedCredential,
    NotCredentialFailure,
    Unknown,
}
```

Fallback should trigger for:

```text
MissingKey, if another key exists
InvalidKey
QuotaExhausted
BudgetExhausted
RateLimitedCredential, only when clearly credential/account specific
```

Fallback should not trigger for:

```text
malformed request
unsupported model
content filter/refusal
generic 5xx
network outage
generic transient rate limit with no credential/budget indication
stream consumption error after partial output has been emitted
```

## Request-building changes

Current `LlmRequest` contains one concrete API key.

Possible implementation approach:

* Keep `LlmRequest` with a concrete `api_key`.
* Add a helper that builds a base request without permanently choosing the key, or clone/patch the request per attempt.
* Move credential resolution out of `LlmClient::build_request` or add a new API that supports multiple candidate keys.
* Ensure ledger/debug logging never persists secrets.

## Retry interaction

Do not mix key fallback with existing retry semantics too aggressively.

Recommended behavior:

```text
- If the provider returns quota/budget/key failure before streaming starts:
  rotate to next key.

- If the provider returns transient 429/5xx:
  use existing retry behavior with same key.

- If all retries are exhausted due to transient provider errors:
  fail normally, do not rotate keys by default.

- If a stream fails mid-response:
  do not rotate keys by default, because the user may have already seen partial output.
```

## Diagnostics

Record fallback events in diagnostics.

Include:

```text
timestamp
rid
provider
model
character
from_key_name
to_key_name
status_code
reason
```

Do not store actual secrets.

## Validation

Automated tests with mocked provider responses:

* First key succeeds: no fallback warning.
* First key missing, second succeeds: fallback warning.
* First key invalid, second succeeds: fallback warning.
* First key quota exhausted, second succeeds: fallback warning.
* First key generic 500: normal retry path, no key fallback.
* First key malformed request 400: fail, no key fallback.
* Disabled keys are skipped.
* Fallback is not sticky: each new request starts from first enabled key again.
* Warnings contain friendly key names but no secret values.

Manual test:

* Configure one intentionally invalid budget key and one valid overflow key.
* Send a message.
* Confirm Shore warns and still completes the response.
* Send another message.
* Confirm it tries the first key again and warns again.

---
