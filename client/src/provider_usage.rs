use crate::{executor::CommandExecutor, protocol::*};
use anyhow::{Context, Result};
use base64::{Engine, engine::general_purpose::URL_SAFE_NO_PAD};
use directories::BaseDirs;
use reqwest::{Client, RequestBuilder, StatusCode};
use serde_json::Value;
use std::{env, path::PathBuf, time::Duration};
use time::{OffsetDateTime, format_description::well_known::Rfc3339};

const REPORT_STATE_KEY: &str = "provider_usage:last_automatic_hour";
const CHATGPT_BASE_URL: &str = "https://chatgpt.com/backend-api";
const KIMI_CODE_BASE_URL: &str = "https://api.kimi.com";
const KIMI_WEB_USAGE_URL: &str =
    "https://www.kimi.com/apiv2/kimi.gateway.billing.v1.BillingService/GetUsages";

#[derive(Clone)]
struct OAuthCredentials {
    access_token: String,
    account_id: Option<String>,
    id_token: Option<String>,
    source: &'static str,
}

#[derive(Clone)]
struct KimiCredentials {
    access_token: String,
    account: Option<ProviderUsageAccount>,
    expires_at: Option<f64>,
}

#[derive(Debug, Clone, Copy)]
struct CollectionError {
    code: &'static str,
}

impl CollectionError {
    const fn new(code: &'static str) -> Self {
        Self { code }
    }
}

pub async fn run(executor: CommandExecutor) {
    loop {
        let hour = OffsetDateTime::now_utc().unix_timestamp().div_euclid(3600);
        let already_reported = executor
            .store
            .state_get(REPORT_STATE_KEY)
            .await
            .ok()
            .flatten()
            .is_some_and(|value| value == hour.to_string());
        if !already_reported {
            match collect_and_emit_all(&executor).await {
                Ok(reports) => {
                    if let Err(error) = executor
                        .store
                        .state_set(REPORT_STATE_KEY, &hour.to_string())
                        .await
                    {
                        tracing::warn!(error=?error, "provider usage schedule checkpoint failed");
                    } else {
                        tracing::info!(count = reports.len(), "provider usage snapshots queued");
                    }
                }
                Err(error) => {
                    tracing::warn!(error=?error, "provider usage snapshots could not be queued; retrying");
                    tokio::time::sleep(Duration::from_secs(60)).await;
                    continue;
                }
            }
        }
        let now = OffsetDateTime::now_utc().unix_timestamp();
        let next_hour = now.div_euclid(3600).saturating_add(1).saturating_mul(3600);
        let jitter = stable_jitter_seconds(&executor.device_id);
        let wait = next_hour.saturating_add(jitter).saturating_sub(now).max(1) as u64;
        tokio::time::sleep(Duration::from_secs(wait)).await;
    }
}

pub async fn collect_and_emit_all(executor: &CommandExecutor) -> Result<Vec<ProviderUsageReport>> {
    let client = Client::builder()
        .connect_timeout(Duration::from_secs(5))
        .timeout(Duration::from_secs(15))
        .user_agent("Nuntius Provider Usage")
        .build()
        .context("build provider usage HTTP client")?;
    let (codex, kimi) = tokio::join!(collect_openai(&client), collect_kimi(&client));
    let reports = vec![codex, kimi];
    for report in &reports {
        executor
            .emit(
                "provider.usage.reported",
                None,
                None,
                None,
                serde_json::to_value(report)?,
                true,
            )
            .await?;
    }
    Ok(reports)
}

fn stable_jitter_seconds(device_id: &str) -> i64 {
    use sha2::{Digest, Sha256};
    let digest = Sha256::digest(device_id.as_bytes());
    i64::from(u16::from_be_bytes([digest[0], digest[1]]) % 300)
}

async fn collect_openai(client: &Client) -> ProviderUsageReport {
    let sampled_at = now();
    let codex_home = codex_home();
    let Some(credentials) = load_openai_credentials(&codex_home).await else {
        return failed_report(
            AgentProvider::Codex,
            sampled_at,
            "oauth",
            "unavailable",
            None,
            "credentials_missing",
        );
    };
    let account = openai_account(&credentials);
    let base_url = load_chatgpt_base_url(&codex_home).await;
    let usage_path = if base_url.contains("/backend-api") {
        "/wham/usage"
    } else {
        "/api/codex/usage"
    };
    let mut request = client
        .get(format!("{base_url}{usage_path}"))
        .bearer_auth(&credentials.access_token)
        .header("Accept", "application/json")
        .header("User-Agent", "CodexBar");
    if let Some(account_id) = credentials.account_id.as_deref() {
        request = request.header("ChatGPT-Account-Id", account_id);
    }
    let data = match request_json(request).await {
        Ok(value) => value,
        Err(error) => {
            return failed_report(
                AgentProvider::Codex,
                sampled_at,
                credentials.source,
                "error",
                account,
                error.code,
            );
        }
    };
    let rate_limit = data.get("rate_limit").and_then(Value::as_object);
    let credits_value = data.get("credits");
    if rate_limit.is_none() && !credits_value.is_some_and(Value::is_object) {
        return failed_report(
            AgentProvider::Codex,
            sampled_at,
            credentials.source,
            "error",
            account,
            "invalid_response",
        );
    }

    let five_hour = rate_limit
        .and_then(|value| value.get("primary_window"))
        .and_then(openai_window);
    let seven_day = rate_limit
        .and_then(|value| value.get("secondary_window"))
        .and_then(openai_window);
    let balance = credits_value
        .and_then(|value| value.get("balance"))
        .and_then(number);
    let reset_result = fetch_reset_credits(client, &base_url, &credentials).await;
    let (reset_credits_available, next_reset_credit_expires_at, warning_code) = match reset_result {
        Ok((available, expires)) => (Some(available), expires, None),
        Err(_) => (None, None, Some("reset_credits_unavailable".into())),
    };
    let has_credits = balance.is_some() || reset_credits_available.is_some();
    ProviderUsageReport {
        schema_version: 1,
        report_id: new_id("pur"),
        provider: AgentProvider::Codex,
        sampled_at,
        source: credentials.source.into(),
        status: if warning_code.is_some() {
            "partial".into()
        } else {
            "ok".into()
        },
        account,
        entitlement_plan: clean_string(data.get("plan_type")),
        windows: ProviderUsageWindows {
            five_hour,
            seven_day,
        },
        credits: has_credits.then_some(ProviderUsageCredits {
            balance,
            reset_credits_available,
            next_reset_credit_expires_at,
        }),
        warning_code,
        error_code: None,
    }
}

async fn fetch_reset_credits(
    client: &Client,
    base_url: &str,
    credentials: &OAuthCredentials,
) -> std::result::Result<(i64, Option<String>), CollectionError> {
    let mut request = client
        .get(format!("{base_url}/wham/rate-limit-reset-credits"))
        .bearer_auth(&credentials.access_token)
        .header("Accept", "application/json")
        .header("User-Agent", "CodexBar")
        .header("OpenAI-Beta", "codex-1")
        .header("originator", "Codex Desktop");
    if let Some(account_id) = credentials.account_id.as_deref() {
        request = request.header("ChatGPT-Account-Id", account_id);
    }
    let data = request_json(request).await?;
    let available = data
        .get("available_count")
        .and_then(Value::as_i64)
        .filter(|value| *value >= 0)
        .ok_or_else(|| CollectionError::new("invalid_response"))?;
    let now = OffsetDateTime::now_utc().unix_timestamp() as f64;
    let next = data
        .get("credits")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter(|item| item.get("status").and_then(Value::as_str) == Some("available"))
        .filter_map(|item| epoch_seconds(item.get("expires_at")))
        .filter(|value| *value > now)
        .min_by(f64::total_cmp)
        .and_then(epoch_rfc3339);
    Ok((available, next))
}

fn openai_window(value: &Value) -> Option<ProviderQuotaWindow> {
    let used_percent = number(value.get("used_percent"))?.clamp(0.0, 100.0);
    let window_seconds = number(value.get("limit_window_seconds"))? as i64;
    if window_seconds <= 0 {
        return None;
    }
    Some(ProviderQuotaWindow {
        window_seconds,
        used_percent,
        used: None,
        limit: None,
        remaining: None,
        resets_at: epoch_seconds(value.get("reset_at")).and_then(epoch_rfc3339),
    })
}

async fn load_openai_credentials(codex_home: &PathBuf) -> Option<OAuthCredentials> {
    let data: Value = serde_json::from_str(
        &tokio::fs::read_to_string(codex_home.join("auth.json"))
            .await
            .ok()?,
    )
    .ok()?;
    if let Some(api_key) = clean_string(data.get("OPENAI_API_KEY")) {
        return Some(OAuthCredentials {
            access_token: api_key,
            account_id: None,
            id_token: None,
            source: "api",
        });
    }
    let tokens = data.get("tokens")?;
    Some(OAuthCredentials {
        access_token: string_key(tokens, "access_token", "accessToken")?,
        account_id: string_key(tokens, "account_id", "accountId"),
        id_token: string_key(tokens, "id_token", "idToken"),
        source: "oauth",
    })
}

fn openai_account(credentials: &OAuthCredentials) -> Option<ProviderUsageAccount> {
    let payload = jwt_payload(credentials.id_token.as_deref());
    let auth = payload
        .get("https://api.openai.com/auth")
        .and_then(Value::as_object);
    let profile = payload
        .get("https://api.openai.com/profile")
        .and_then(Value::as_object);
    let account = ProviderUsageAccount {
        external_account_id: credentials
            .account_id
            .clone()
            .or_else(|| auth.and_then(|value| clean_string(value.get("chatgpt_account_id"))))
            .or_else(|| clean_string(payload.get("chatgpt_account_id"))),
        email: clean_string(payload.get("email"))
            .or_else(|| profile.and_then(|value| clean_string(value.get("email")))),
        plan: auth
            .and_then(|value| clean_string(value.get("chatgpt_plan_type")))
            .or_else(|| clean_string(payload.get("chatgpt_plan_type"))),
        scope: None,
        subscription_started_at: auth
            .and_then(|value| normalized_datetime(value.get("chatgpt_subscription_active_start"))),
        subscription_expires_at: auth
            .and_then(|value| normalized_datetime(value.get("chatgpt_subscription_active_until"))),
        subscription_last_checked_at: auth
            .and_then(|value| normalized_datetime(value.get("chatgpt_subscription_last_checked"))),
        credential_expires_at: epoch_seconds(payload.get("exp")).and_then(epoch_rfc3339),
    };
    account_present(account)
}

async fn load_chatgpt_base_url(codex_home: &PathBuf) -> String {
    let configured = tokio::fs::read_to_string(codex_home.join("config.toml"))
        .await
        .ok()
        .and_then(|contents| {
            contents.lines().find_map(|raw| {
                let line = raw.split('#').next()?.trim();
                let (key, value) = line.split_once('=')?;
                (key.trim() == "chatgpt_base_url")
                    .then(|| {
                        value
                            .trim()
                            .trim_matches(|ch| ch == '\"' || ch == '\'')
                            .trim()
                            .to_owned()
                    })
                    .filter(|value| !value.is_empty())
            })
        })
        .unwrap_or_else(|| CHATGPT_BASE_URL.into());
    let mut normalized = configured.trim_end_matches('/').to_owned();
    if (normalized.starts_with("https://chatgpt.com")
        || normalized.starts_with("https://chat.openai.com"))
        && !normalized.contains("/backend-api")
    {
        normalized.push_str("/backend-api");
    }
    normalized
}

async fn collect_kimi(client: &Client) -> ProviderUsageReport {
    let sampled_at = now();
    let provider_config = codexbar_provider_config("kimi").await;
    let base_url = kimi_code_base_url(&provider_config);
    let usage_url = kimi_usage_url(&base_url);
    let mut attempted = false;
    let mut last_error = "credentials_missing";
    let cli_credentials = load_kimi_credentials().await;

    if let Some(credentials) = cli_credentials.as_ref() {
        attempted = true;
        if credentials
            .expires_at
            .is_some_and(|value| value <= OffsetDateTime::now_utc().unix_timestamp() as f64)
        {
            last_error = "credentials_expired";
        } else {
            let request = client
                .get(&usage_url)
                .bearer_auth(&credentials.access_token)
                .header("Accept", "application/json")
                .header("User-Agent", "Kimi Code CLI");
            match request_json(request).await {
                Ok(data) => {
                    if let Some(report) = kimi_api_report(
                        &data,
                        sampled_at.clone(),
                        "cli",
                        credentials.account.clone(),
                    ) {
                        return report;
                    }
                    last_error = "invalid_response";
                }
                Err(error) => last_error = error.code,
            }
        }
    }

    if let Some(api_key) =
        env_clean("KIMI_CODE_API_KEY").or_else(|| clean_string(provider_config.get("apiKey")))
    {
        attempted = true;
        let request = client
            .get(&usage_url)
            .bearer_auth(api_key)
            .header("Accept", "application/json");
        match request_json(request).await {
            Ok(data) => {
                if let Some(report) = kimi_api_report(&data, sampled_at.clone(), "api", None) {
                    return report;
                }
                last_error = "invalid_response";
            }
            Err(error) => last_error = error.code,
        }
    }

    if let Some(auth_token) = kimi_auth_token(&provider_config) {
        attempted = true;
        let claims = jwt_payload(Some(&auth_token));
        let mut request = client
            .post(KIMI_WEB_USAGE_URL)
            .bearer_auth(&auth_token)
            .header("Accept", "*/*")
            .header("Content-Type", "application/json")
            .header("Cookie", format!("kimi-auth={auth_token}"))
            .header("Origin", "https://www.kimi.com")
            .header("Referer", "https://www.kimi.com/code/console")
            .header("connect-protocol-version", "1")
            .header("x-language", "zh-CN")
            .header("x-msh-platform", "web")
            .header(
                "r-timezone",
                env::var("TZ").unwrap_or_else(|_| "Asia/Shanghai".into()),
            )
            .json(&serde_json::json!({"scope":["FEATURE_CODING"]}));
        for (header, claim) in [
            ("x-msh-device-id", "device_id"),
            ("x-msh-session-id", "ssid"),
            ("x-traffic-id", "sub"),
        ] {
            if let Some(value) = claims.get(claim).and_then(Value::as_str) {
                request = request.header(header, value);
            }
        }
        match request_json(request).await {
            Ok(data) => {
                if let Some(report) =
                    kimi_web_report(&data, sampled_at.clone(), kimi_account(&claims, None, None))
                {
                    return report;
                }
                last_error = "invalid_response";
            }
            Err(error) => last_error = error.code,
        }
    }

    failed_report(
        AgentProvider::Kimi,
        sampled_at,
        "auto",
        if attempted { "error" } else { "unavailable" },
        cli_credentials.and_then(|value| value.account),
        last_error,
    )
}

fn kimi_api_report(
    data: &Value,
    sampled_at: String,
    source: &str,
    account: Option<ProviderUsageAccount>,
) -> Option<ProviderUsageReport> {
    let weekly = data.get("usage")?;
    let rate_limit = first_kimi_limit(data.get("limits"));
    kimi_report(weekly, rate_limit, sampled_at, source, account)
}

fn kimi_web_report(
    data: &Value,
    sampled_at: String,
    account: Option<ProviderUsageAccount>,
) -> Option<ProviderUsageReport> {
    let coding = data
        .get("usages")?
        .as_array()?
        .iter()
        .find(|value| value.get("scope").and_then(Value::as_str) == Some("FEATURE_CODING"))?;
    kimi_report(
        coding.get("detail")?,
        first_kimi_limit(coding.get("limits")),
        sampled_at,
        "web",
        account,
    )
}

fn kimi_report(
    weekly: &Value,
    rate_limit: Option<&Value>,
    sampled_at: String,
    source: &str,
    account: Option<ProviderUsageAccount>,
) -> Option<ProviderUsageReport> {
    let seven_day = kimi_window(weekly, 7 * 24 * 60 * 60)?;
    Some(ProviderUsageReport {
        schema_version: 1,
        report_id: new_id("pur"),
        provider: AgentProvider::Kimi,
        sampled_at,
        source: source.into(),
        status: "ok".into(),
        account,
        entitlement_plan: Some("kimi-code".into()),
        windows: ProviderUsageWindows {
            five_hour: rate_limit.and_then(|value| kimi_window(value, 5 * 60 * 60)),
            seven_day: Some(seven_day),
        },
        credits: None,
        warning_code: None,
        error_code: None,
    })
}

fn first_kimi_limit(value: Option<&Value>) -> Option<&Value> {
    let limits = value?.as_array()?;
    limits
        .iter()
        .find(|item| {
            item.pointer("/window/duration").and_then(number) == Some(5.0)
                && item
                    .pointer("/window/timeUnit")
                    .and_then(Value::as_str)
                    .is_some_and(|unit| unit.eq_ignore_ascii_case("hour"))
        })
        .or_else(|| limits.first())?
        .get("detail")
}

fn kimi_window(value: &Value, window_seconds: i64) -> Option<ProviderQuotaWindow> {
    let limit = number(value.get("limit"))?;
    if limit <= 0.0 {
        return None;
    }
    let remaining = number(value.get("remaining"));
    let used =
        number(value.get("used")).or_else(|| remaining.map(|value| (limit - value).max(0.0)))?;
    Some(ProviderQuotaWindow {
        window_seconds,
        used_percent: (used / limit * 100.0).clamp(0.0, 100.0),
        used: Some(used),
        limit: Some(limit),
        remaining,
        resets_at: ["resetTime", "resetAt", "reset_time", "reset_at"]
            .into_iter()
            .find_map(|key| normalized_datetime(value.get(key))),
    })
}

async fn load_kimi_credentials() -> Option<KimiCredentials> {
    for home in kimi_homes() {
        let path = home.join("credentials/kimi-code.json");
        let Ok(contents) = tokio::fs::read_to_string(path).await else {
            continue;
        };
        let Ok(data) = serde_json::from_str::<Value>(&contents) else {
            continue;
        };
        let Some(access_token) = clean_string(data.get("access_token")) else {
            continue;
        };
        let claims = jwt_payload(Some(&access_token));
        let expires_at = number(data.get("expires_at")).or_else(|| number(claims.get("exp")));
        return Some(KimiCredentials {
            access_token,
            account: kimi_account(&claims, clean_string(data.get("scope")), expires_at),
            expires_at,
        });
    }
    None
}

fn kimi_account(
    claims: &Value,
    scope: Option<String>,
    expires_at: Option<f64>,
) -> Option<ProviderUsageAccount> {
    account_present(ProviderUsageAccount {
        external_account_id: clean_string(claims.get("sub"))
            .or_else(|| clean_string(claims.get("user_id"))),
        email: clean_string(claims.get("email")),
        plan: None,
        scope: scope.or_else(|| clean_string(claims.get("scope"))),
        subscription_started_at: None,
        subscription_expires_at: None,
        subscription_last_checked_at: None,
        credential_expires_at: expires_at
            .or_else(|| number(claims.get("exp")))
            .and_then(epoch_rfc3339),
    })
}

fn kimi_homes() -> Vec<PathBuf> {
    if let Some(value) = env_clean("KIMI_CODE_HOME") {
        return vec![PathBuf::from(value)];
    }
    let home = BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    vec![home.join(".kimi-code"), home.join(".kimi")]
}

fn kimi_code_base_url(config: &Value) -> String {
    env_clean("KIMI_CODE_BASE_URL")
        .or_else(|| clean_string(config.get("enterpriseHost")))
        .unwrap_or_else(|| KIMI_CODE_BASE_URL.into())
        .trim_end_matches('/')
        .to_owned()
}

fn kimi_usage_url(base_url: &str) -> String {
    let path = reqwest::Url::parse(base_url)
        .ok()
        .map(|url| url.path().trim_matches('/').to_owned())
        .unwrap_or_default();
    if path == "coding/v1" || path.ends_with("/coding/v1") {
        format!("{base_url}/usages")
    } else if path == "coding" || path.ends_with("/coding") {
        format!("{base_url}/v1/usages")
    } else {
        format!("{base_url}/coding/v1/usages")
    }
}

fn kimi_auth_token(config: &Value) -> Option<String> {
    let raw = env_clean("KIMI_AUTH_TOKEN")
        .or_else(|| env_clean("kimi_auth_token"))
        .or_else(|| clean_string(config.get("cookieHeader")))?;
    for marker in ["kimi-auth=", "kimi-auth:"] {
        if let Some(index) = raw.to_ascii_lowercase().find(marker) {
            return clean_owned(&raw[index + marker.len()..].split(';').next().unwrap_or(""));
        }
    }
    Some(raw)
}

async fn codexbar_provider_config(provider: &str) -> Value {
    let path = codexbar_config_path();
    let Ok(contents) = tokio::fs::read_to_string(path).await else {
        return Value::Null;
    };
    let Ok(data) = serde_json::from_str::<Value>(&contents) else {
        return Value::Null;
    };
    data.get("providers")
        .and_then(Value::as_array)
        .and_then(|providers| {
            providers
                .iter()
                .find(|value| value.get("id").and_then(Value::as_str) == Some(provider))
        })
        .cloned()
        .unwrap_or(Value::Null)
}

fn codexbar_config_path() -> PathBuf {
    if let Some(value) = env_clean("CODEXBAR_CONFIG") {
        return PathBuf::from(value);
    }
    if let Some(value) = env_clean("XDG_CONFIG_HOME") {
        return PathBuf::from(value).join("codexbar/config.json");
    }
    let home = BaseDirs::new()
        .map(|dirs| dirs.home_dir().to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    let xdg = home.join(".config/codexbar/config.json");
    if xdg.is_file() {
        xdg
    } else {
        home.join(".codexbar/config.json")
    }
}

fn codex_home() -> PathBuf {
    env_clean("CODEX_HOME")
        .map(PathBuf::from)
        .or_else(|| BaseDirs::new().map(|dirs| dirs.home_dir().join(".codex")))
        .unwrap_or_else(|| PathBuf::from(".codex"))
}

async fn request_json(request: RequestBuilder) -> std::result::Result<Value, CollectionError> {
    let response = request.send().await.map_err(|error| {
        if error.is_timeout() {
            CollectionError::new("upstream_timeout")
        } else {
            CollectionError::new("upstream_network")
        }
    })?;
    match response.status() {
        status if status.is_success() => response
            .json::<Value>()
            .await
            .map_err(|_| CollectionError::new("invalid_response")),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => {
            Err(CollectionError::new("upstream_unauthorized"))
        }
        StatusCode::TOO_MANY_REQUESTS => Err(CollectionError::new("upstream_rate_limited")),
        _ => Err(CollectionError::new("upstream_error")),
    }
}

fn failed_report(
    provider: AgentProvider,
    sampled_at: String,
    source: &str,
    status: &str,
    account: Option<ProviderUsageAccount>,
    error_code: &str,
) -> ProviderUsageReport {
    ProviderUsageReport {
        schema_version: 1,
        report_id: new_id("pur"),
        provider,
        sampled_at,
        source: source.into(),
        status: status.into(),
        account,
        entitlement_plan: None,
        windows: ProviderUsageWindows::default(),
        credits: None,
        warning_code: None,
        error_code: Some(error_code.into()),
    }
}

fn jwt_payload(token: Option<&str>) -> Value {
    let Some(payload) = token.and_then(|value| value.split('.').nth(1)) else {
        return Value::Null;
    };
    URL_SAFE_NO_PAD
        .decode(payload)
        .ok()
        .and_then(|bytes| serde_json::from_slice(&bytes).ok())
        .unwrap_or(Value::Null)
}

fn account_present(account: ProviderUsageAccount) -> Option<ProviderUsageAccount> {
    [
        account.external_account_id.as_ref(),
        account.email.as_ref(),
        account.plan.as_ref(),
        account.scope.as_ref(),
        account.subscription_started_at.as_ref(),
        account.subscription_expires_at.as_ref(),
        account.subscription_last_checked_at.as_ref(),
        account.credential_expires_at.as_ref(),
    ]
    .into_iter()
    .any(|value| value.is_some())
    .then_some(account)
}

fn normalized_datetime(value: Option<&Value>) -> Option<String> {
    let value = value?;
    if let Some(epoch) = epoch_seconds(Some(value)) {
        return epoch_rfc3339(epoch);
    }
    let text = value.as_str()?.trim();
    OffsetDateTime::parse(text, &Rfc3339)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

fn epoch_seconds(value: Option<&Value>) -> Option<f64> {
    let value = value?;
    number(Some(value)).or_else(|| {
        let parsed = OffsetDateTime::parse(value.as_str()?.trim(), &Rfc3339).ok()?;
        Some(parsed.unix_timestamp() as f64)
    })
}

fn epoch_rfc3339(value: f64) -> Option<String> {
    OffsetDateTime::from_unix_timestamp(value as i64)
        .ok()?
        .format(&Rfc3339)
        .ok()
}

fn number(value: Option<&Value>) -> Option<f64> {
    match value? {
        Value::Number(value) => value.as_f64(),
        Value::String(value) => value.trim().parse().ok(),
        _ => None,
    }
}

fn string_key(value: &Value, snake_case: &str, camel_case: &str) -> Option<String> {
    clean_string(value.get(snake_case)).or_else(|| clean_string(value.get(camel_case)))
}

fn clean_string(value: Option<&Value>) -> Option<String> {
    clean_owned(value?.as_str()?)
}

fn clean_owned(value: &str) -> Option<String> {
    let value = value
        .trim()
        .trim_matches(|ch| ch == '\"' || ch == '\'')
        .trim();
    (!value.is_empty()).then(|| value.to_owned())
}

fn env_clean(key: &str) -> Option<String> {
    env::var(key).ok().and_then(|value| clean_owned(&value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn kimi_windows_keep_absolute_and_percentage_values() {
        let value = serde_json::json!({
            "limit":"1000",
            "used":"250",
            "remaining":"750",
            "resetTime":"2026-06-23T08:13:36Z"
        });
        let window = kimi_window(&value, 604800).unwrap();
        assert_eq!(window.used_percent, 25.0);
        assert_eq!(window.used, Some(250.0));
        assert_eq!(window.limit, Some(1000.0));
        assert_eq!(window.remaining, Some(750.0));
        assert_eq!(window.resets_at.as_deref(), Some("2026-06-23T08:13:36Z"));
    }

    #[test]
    fn openai_window_uses_upstream_duration() {
        let value = serde_json::json!({
            "used_percent":12,
            "reset_at":1780114189,
            "limit_window_seconds":18000
        });
        let window = openai_window(&value).unwrap();
        assert_eq!(window.window_seconds, 18000);
        assert_eq!(window.used_percent, 12.0);
        assert!(window.resets_at.is_some());
    }

    #[test]
    fn api_and_web_credentials_are_never_used_as_account_plan() {
        let claims = serde_json::json!({"sub":"kimi-user","scope":"kimi-code","exp":1781668970});
        let account = kimi_account(&claims, None, None).unwrap();
        assert_eq!(account.external_account_id.as_deref(), Some("kimi-user"));
        assert_eq!(account.scope.as_deref(), Some("kimi-code"));
        assert_eq!(account.plan, None);
        assert!(account.credential_expires_at.is_some());
        assert_eq!(account.subscription_expires_at, None);
    }
}
