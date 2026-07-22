use crate::{executor::CommandExecutor, protocol::*, store::InboxRecord};
use futures_util::FutureExt;
use serde_json::Value;
use std::{collections::HashSet, panic::AssertUnwindSafe};
use tokio::task::JoinSet;

const MAX_CONCURRENT_TARGETS: usize = 8;

pub fn target_key(command: &DeviceCommand) -> String {
    if let Some(thread_id) = &command.thread_id {
        return format!("thread:{thread_id}");
    }
    match &command.command {
        DeviceCommandKind::ProjectDelete { project_id }
        | DeviceCommandKind::ThreadCreate { project_id, .. } => format!("project:{project_id}"),
        DeviceCommandKind::ApprovalDecide { approval_id, .. } => {
            format!("approval:{approval_id}")
        }
        _ => format!("device:{}", command.device_id),
    }
}

pub fn priority(command: &DeviceCommand) -> i64 {
    match &command.command {
        DeviceCommandKind::TurnInterrupt { .. } | DeviceCommandKind::ApprovalDecide { .. } => 0,
        DeviceCommandKind::TurnStart { .. } | DeviceCommandKind::TurnSteer { .. } => 1,
        DeviceCommandKind::HistorySync { .. } => 4,
        DeviceCommandKind::ProviderUsageRefresh => 3,
        _ => 2,
    }
}

pub async fn run(executor: CommandExecutor) {
    let mut active_targets = HashSet::<String>::new();
    let mut tasks = JoinSet::<String>::new();
    let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
    tick.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);

    loop {
        while tasks.len() < MAX_CONCURRENT_TARGETS {
            let pending = match executor.store.pending_inbox(256).await {
                Ok(pending) => pending,
                Err(error) => {
                    tracing::error!(error=?error, "cannot read command inbox");
                    break;
                }
            };
            let mut heads = Vec::<InboxRecord>::new();
            let mut seen = HashSet::new();
            for record in pending {
                if seen.insert(record.target_key.clone()) {
                    heads.push(record);
                }
            }
            heads.sort_by_key(|record| (record.priority, record.server_sequence));
            let Some(record) = heads
                .into_iter()
                .find(|record| !active_targets.contains(&record.target_key))
            else {
                break;
            };
            match executor
                .store
                .start_command(&record.command.command_id)
                .await
            {
                Ok(true) => {
                    active_targets.insert(record.target_key.clone());
                    let task_executor = executor.clone();
                    tasks.spawn(async move {
                        let target = record.target_key.clone();
                        let panic_record = record.clone();
                        if AssertUnwindSafe(execute_one(task_executor.clone(), record))
                            .catch_unwind()
                            .await
                            .is_err()
                        {
                            tracing::error!(%target, "command actor panicked");
                            if task_executor
                                .store
                                .finish_command_as(
                                    &panic_record.command.command_id,
                                    &panic_record.queue_epoch,
                                    panic_record.server_sequence,
                                    "unknown",
                                    None,
                                    Some("command_actor_panicked"),
                                    Some("命令执行器异常退出，无法确认执行结果"),
                                )
                                .await
                                .is_ok()
                            {
                                publish_ack(
                                    &task_executor,
                                    &panic_record.command.command_id,
                                    "unknown",
                                    None,
                                    Some("command_actor_panicked".into()),
                                    Some("命令执行器异常退出，无法确认执行结果".into()),
                                );
                            }
                        }
                        target
                    });
                }
                Ok(false) => continue,
                Err(error) => {
                    tracing::error!(error=?error, "cannot claim queued command");
                    break;
                }
            }
        }

        tokio::select! {
            _ = executor.command_notify.notified() => {}
            _ = tick.tick() => {}
            result = tasks.join_next(), if !tasks.is_empty() => {
                if let Some(result) = result {
                    match result {
                        Ok(target) => { active_targets.remove(&target); }
                        Err(error) => tracing::error!(error=?error, "command actor task failed"),
                    }
                }
            }
        }
    }
}

async fn execute_one(executor: CommandExecutor, record: InboxRecord) {
    let expired = time::OffsetDateTime::parse(
        &record.command.expires_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map(|expires_at| expires_at <= time::OffsetDateTime::now_utc())
    .unwrap_or(true);
    if expired {
        let message = "命令已过期，请重试";
        if let Err(error) = executor
            .store
            .finish_command_as(
                &record.command.command_id,
                &record.queue_epoch,
                record.server_sequence,
                "expired",
                None,
                Some("expired"),
                Some(message),
            )
            .await
        {
            tracing::error!(error=?error, command_id=%record.command.command_id, "cannot expire queued command");
        } else {
            publish_ack(
                &executor,
                &record.command.command_id,
                "expired",
                None,
                Some("expired".into()),
                Some(message.into()),
            );
        }
        return;
    }
    publish_ack(
        &executor,
        &record.command.command_id,
        "applying",
        None,
        None,
        None,
    );

    match execute_with_retry(&executor, &record).await {
        Ok(result) => {
            if let Err(error) = executor
                .store
                .finish_command(
                    &record.command.command_id,
                    &record.queue_epoch,
                    record.server_sequence,
                    Some(&result),
                    None,
                    None,
                )
                .await
            {
                tracing::error!(error=?error, command_id=%record.command.command_id, "cannot persist command result");
            } else {
                publish_ack(
                    &executor,
                    &record.command.command_id,
                    "completed",
                    Some(result),
                    None,
                    None,
                );
            }
        }
        Err(error) => {
            let (code, message, status) = classify_error(&error);
            tracing::warn!(command_id=%record.command.command_id, %code, error=?error, "queued command failed");
            if let Err(store_error) = executor
                .store
                .finish_command_as(
                    &record.command.command_id,
                    &record.queue_epoch,
                    record.server_sequence,
                    status,
                    None,
                    Some(&code),
                    Some(&message),
                )
                .await
            {
                tracing::error!(error=?store_error, command_id=%record.command.command_id, "cannot persist command failure");
            } else {
                publish_ack(
                    &executor,
                    &record.command.command_id,
                    status,
                    None,
                    Some(code),
                    Some(message),
                );
            }
        }
    }
    executor.command_notify.notify_one();
}

async fn execute_with_retry(
    executor: &CommandExecutor,
    record: &InboxRecord,
) -> anyhow::Result<Value> {
    let retryable_archive = matches!(
        &record.command.command,
        DeviceCommandKind::ThreadArchive { .. }
    );
    let mut delay = std::time::Duration::from_secs(1);
    loop {
        match executor.execute(&record.command).await {
            Ok(result) => return Ok(result),
            Err(error) if retryable_archive && archive_error_is_transient(&error) => {
                let remaining = command_remaining(&record.command);
                if remaining.is_zero() {
                    anyhow::bail!("command expired while waiting for App Server recovery");
                }
                let sleep_for = delay.min(remaining);
                tracing::warn!(
                    command_id=%record.command.command_id,
                    retry_in_ms=sleep_for.as_millis(),
                    error=?error,
                    "archive command waiting for App Server recovery"
                );
                tokio::time::sleep(sleep_for).await;
                delay = (delay * 2).min(std::time::Duration::from_secs(30));
            }
            Err(error) => return Err(error),
        }
    }
}

fn command_remaining(command: &DeviceCommand) -> std::time::Duration {
    time::OffsetDateTime::parse(
        &command.expires_at,
        &time::format_description::well_known::Rfc3339,
    )
    .ok()
    .and_then(|expires_at| {
        let remaining = expires_at - time::OffsetDateTime::now_utc();
        std::time::Duration::try_from(remaining).ok()
    })
    .unwrap_or_default()
}

fn archive_error_is_transient(error: &anyhow::Error) -> bool {
    let lower = format!("{error:#}").to_ascii_lowercase();
    !lower.contains("not found")
        && !lower.contains("outside allowed")
        && !lower.contains("invalid")
        && !lower.contains("already terminal")
        && (lower.contains("app server")
            || lower.contains("codex")
            || lower.contains("timed out")
            || lower.contains("outcome is unknown")
            || lower.contains("writer stopped")
            || lower.contains("exited before")
            || lower.contains("connection")
            || lower.contains("unavailable")
            || lower.contains("temporarily"))
}

fn publish_ack(
    executor: &CommandExecutor,
    command_id: &str,
    stage: &str,
    result: Option<Value>,
    error_code: Option<String>,
    error_message: Option<String>,
) {
    let _ = executor.command_acks.send(TunnelFrame::CommandAck {
        command_id: command_id.into(),
        stage: stage.into(),
        result,
        error_code,
        error_message,
    });
}

fn classify_error(error: &anyhow::Error) -> (String, String, &'static str) {
    let raw = format!("{error:#}");
    let lower = raw.to_ascii_lowercase();
    let (code, status) = if lower.contains("timed out") || lower.contains("outcome is unknown") {
        ("outcome_unknown", "unknown")
    } else if lower.contains("not found") {
        ("not_found", "failed")
    } else if lower.contains("expired") {
        ("expired", "expired")
    } else if lower.contains("already terminal") {
        ("already_terminal", "completed")
    } else if lower.contains("outside allowed") || lower.contains("invalid") {
        ("invalid_request", "failed")
    } else if lower.contains("app server") || lower.contains("codex") {
        ("app_server_unavailable", "failed")
    } else {
        ("execution_failed", "failed")
    };
    let mut message: String = raw.chars().take(500).collect();
    if message.is_empty() {
        message = "命令执行失败".into();
    }
    (code.into(), message, status)
}
