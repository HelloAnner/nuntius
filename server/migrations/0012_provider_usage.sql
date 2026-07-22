CREATE TABLE provider_usage_reports (
    report_id TEXT PRIMARY KEY,
    event_id TEXT NOT NULL UNIQUE,
    user_id TEXT NOT NULL REFERENCES users(id) ON DELETE CASCADE,
    device_id TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    provider TEXT NOT NULL CHECK (provider IN ('codex', 'kimi')),
    source TEXT NOT NULL,
    status TEXT NOT NULL CHECK (status IN ('ok', 'partial', 'error', 'unavailable')),
    schema_version INTEGER NOT NULL,
    sampled_at TEXT NOT NULL,
    received_at TEXT NOT NULL,
    created_at TEXT NOT NULL,
    external_account_id TEXT,
    account_email TEXT,
    account_plan TEXT,
    account_scope TEXT,
    subscription_started_at TEXT,
    subscription_expires_at TEXT,
    subscription_last_checked_at TEXT,
    credential_expires_at TEXT,
    entitlement_plan TEXT,
    five_hour_window_seconds INTEGER,
    five_hour_used_percent REAL,
    five_hour_used REAL,
    five_hour_limit REAL,
    five_hour_remaining REAL,
    five_hour_resets_at TEXT,
    seven_day_window_seconds INTEGER,
    seven_day_used_percent REAL,
    seven_day_used REAL,
    seven_day_limit REAL,
    seven_day_remaining REAL,
    seven_day_resets_at TEXT,
    credit_balance REAL,
    reset_credits_available INTEGER,
    next_reset_credit_expires_at TEXT,
    warning_code TEXT,
    error_code TEXT,
    payload_json TEXT NOT NULL,
    CHECK (five_hour_used_percent IS NULL OR (five_hour_used_percent >= 0 AND five_hour_used_percent <= 100)),
    CHECK (seven_day_used_percent IS NULL OR (seven_day_used_percent >= 0 AND seven_day_used_percent <= 100)),
    CHECK (reset_credits_available IS NULL OR reset_credits_available >= 0)
);

CREATE INDEX idx_provider_usage_user_latest
    ON provider_usage_reports(user_id, received_at DESC, report_id DESC);

CREATE INDEX idx_provider_usage_device_latest
    ON provider_usage_reports(user_id, device_id, provider, received_at DESC, report_id DESC);

CREATE INDEX idx_provider_usage_account_latest
    ON provider_usage_reports(user_id, provider, external_account_id, received_at DESC, report_id DESC);
