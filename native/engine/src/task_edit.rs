//! Validated, non-secret task properties and write-only request credentials.

use crate::{
    auth,
    db::{Db, DbError},
    model::TaskInfo,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::HashMap;

pub(crate) mod storage;

/// Per-task overrides. No passwords, cookies or header values belong here.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct TaskProperties {
    pub download_limit_bps: i64,
    pub user_agent: String,
    pub auth_ref: String,
    pub category: String,
    pub tags: Vec<String>,
    pub scheduled_at: i64,
    /// Reference to this task's own protected request material (never projected).
    pub(crate) credential_ref: String,
}

/// A validated partial update; omitted fields retain their existing values.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditTask {
    pub task_id: String,
    pub expected_revision: String,
    pub save_dir: Option<String>,
    pub file_name: Option<String>,
    pub proxy_url: Option<String>,
    pub segments: Option<i32>,
    pub download_limit_bps: Option<i64>,
    pub user_agent: Option<String>,
    pub cookies: Option<String>,
    pub headers: Option<HashMap<String, String>>,
    pub auth_ref: Option<String>,
    pub category: Option<String>,
    pub tags: Option<Vec<String>>,
    pub scheduled_at: Option<i64>,
}

/// Meaningful fields for this task's protocol and lifecycle state.
#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditCapabilities {
    pub request: bool,
    pub proxy: bool,
    pub storage: bool,
    pub rename: bool,
    pub segments: bool,
    pub schedule: bool,
}

/// An editable, redacted snapshot, guarded by a content revision.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditSnapshot {
    pub task_id: String,
    pub url: String,
    pub revision: String,
    pub save_dir: String,
    pub file_name: String,
    pub proxy_url: String,
    pub segments: i32,
    pub properties: TaskProperties,
    pub headers: HashMap<String, String>,
    pub cookies_configured: bool,
    pub capabilities: EditCapabilities,
}

pub(crate) const SECRET_MASK: &str = "********";

pub(crate) fn secret_header(name: &str) -> bool {
    [
        "authorization",
        "cookie",
        "token",
        "secret",
        "password",
        "api-key",
        "api_key",
    ]
    .iter()
    .any(|part| {
        name.as_bytes()
            .windows(part.len())
            .any(|bytes| bytes.eq_ignore_ascii_case(part.as_bytes()))
    })
}

fn inline_ref(task: &TaskInfo) -> Option<String> {
    (!task.properties.credential_ref.is_empty()).then(|| task.properties.credential_ref.clone())
}

/// Load the task's own request settings, without expanding another credential reference.
pub(crate) async fn stored_context(
    db: &Db,
    task: &TaskInfo,
) -> Result<(String, String, HashMap<String, String>), DbError> {
    let (mut cookies, referrer, raw) = db
        .load_task_request_context(&task.task_id)
        .await?
        .unwrap_or_default();
    let mut headers: HashMap<String, String> = if raw.is_empty() {
        HashMap::new()
    } else {
        serde_json::from_str(&raw)
            .map_err(|_| DbError::InvalidConfig("invalid stored task request headers".into()))?
    };
    if let Some(reference) = inline_ref(task) {
        let profile = auth::load(db, &reference)
            .await?
            .ok_or_else(|| DbError::InvalidConfig("task credential record is missing".into()))?;
        cookies = profile.cookies.clone();
        profile.apply_to_headers(&mut headers);
    }
    Ok((cookies, referrer, headers))
}

/// Resolve credentials at the request boundary; fail closed on stale or cross-origin references.
pub(crate) async fn request_context(
    db: &Db,
    task: &TaskInfo,
) -> Result<(String, String, HashMap<String, String>), DbError> {
    let (mut cookies, referrer, mut headers) = stored_context(db, task).await?;
    if !task.properties.auth_ref.is_empty() {
        let profile = auth::load(db, &task.properties.auth_ref)
            .await?
            .ok_or_else(|| DbError::InvalidConfig("task credential reference is missing".into()))?;
        if auth::site_key(&task.url).as_deref() != Some(profile.site.as_str())
            || !profile.is_valid_at(auth::now_unix())
        {
            return Err(DbError::InvalidConfig(
                "task credentials are expired or belong to a different origin".into(),
            ));
        }
        if !profile.cookies.is_empty() {
            cookies.clone_from(&profile.cookies);
        }
        profile.apply_to_headers(&mut headers);
    }
    if !task.properties.user_agent.is_empty() {
        headers.retain(|name, _| !name.eq_ignore_ascii_case("user-agent"));
        headers.insert("User-Agent".into(), task.properties.user_agent.clone());
    }
    Ok((cookies, referrer, headers))
}

pub(crate) fn revision(
    task: &TaskInfo,
    context: &(String, String, HashMap<String, String>),
) -> Result<String, DbError> {
    // Sort headers before hashing: HashMap iteration must not create false conflicts.
    let ordered: std::collections::BTreeMap<_, _> = context.2.iter().collect();
    let bytes = serde_json::to_vec(&(
        &task.file_name,
        &task.save_dir,
        &task.proxy_url,
        task.segments,
        &task.properties,
        &context.0,
        &context.1,
        ordered,
    ))
    .map_err(|_| DbError::InvalidConfig("could not encode task edit revision".into()))?;
    Ok(format!("{:x}", Sha256::digest(bytes)))
}

/// BT tasks: `magnet:` links or `torrent-file://` sentinels.
fn is_bt(url: &str) -> bool {
    url.starts_with("magnet:") || url.starts_with("torrent-file://")
}

/// Storage-only save for `daemon.task.moveStorage` on a non-BT task, guarded by the
/// current revision. `Ok(None)` for BT tasks, which move through the BT session journal.
pub async fn storage_move(
    db: &Db,
    task_id: &str,
    save_dir: &str,
) -> Result<Option<EditTask>, String> {
    let task = db
        .load_task_by_id(task_id)
        .await
        .map_err(|e| format!("db: {e:#}"))?
        .ok_or("not-found")?;
    if is_bt(&task.url) {
        return Ok(None);
    }
    let context = stored_context(db, &task)
        .await
        .map_err(|e| format!("credentials: {e:#}"))?;
    Ok(Some(EditTask {
        expected_revision: revision(&task, &context).map_err(|e| format!("db: {e:#}"))?,
        task_id: task.task_id,
        save_dir: Some(save_dir.to_owned()),
        ..Default::default()
    }))
}

/// Fetch only non-secret editable values. URL is read-only in this editor.
pub async fn snapshot(db: &Db, task_id: &str) -> Result<EditSnapshot, String> {
    let task = db
        .load_task_by_id(task_id)
        .await
        .map_err(|e| format!("db: {e:#}"))?
        .ok_or("not-found")?;
    let context = stored_context(db, &task)
        .await
        .map_err(|e| format!("credentials: {e:#}"))?;
    let revision = revision(&task, &context).map_err(|e| format!("db: {e:#}"))?;
    let capabilities = capabilities(&task);
    let mut headers = context.2;
    for (name, value) in &mut headers {
        if secret_header(name) {
            *value = SECRET_MASK.into();
        }
    }
    Ok(EditSnapshot {
        task_id: task.task_id,
        url: if task.origin_url.is_empty() {
            task.url
        } else {
            task.origin_url
        },
        revision,
        save_dir: task.save_dir,
        file_name: task.file_name,
        proxy_url: task.proxy_url,
        segments: task.segments,
        properties: task.properties,
        headers,
        cookies_configured: !context.0.is_empty(),
        capabilities,
    })
}

pub(crate) fn protect_request_context(
    task: &TaskInfo,
    cookies: &mut String,
    headers: &mut HashMap<String, String>,
) -> Option<auth::AuthProfile> {
    let site = auth::site_key(&task.url)?;
    let mut secrets = HashMap::new();
    headers.retain(|name, value| {
        if secret_header(name) {
            secrets.insert(name.clone(), std::mem::take(value));
            false
        } else {
            true
        }
    });
    if cookies.is_empty() && secrets.is_empty() {
        return None;
    }
    let plugin_id = format!("task-{}@fluxdown", task.task_id);
    Some(auth::AuthProfile {
        auth_ref: format!("{plugin_id}::{site}"),
        plugin_id,
        site,
        kind: "headers".into(),
        cookies: std::mem::take(cookies),
        headers: secrets,
        ..Default::default()
    })
}

pub(crate) struct PreparedEdit {
    pub original: TaskInfo,
    pub updated: TaskInfo,
    pub cookies: String,
    pub referrer: String,
    pub headers: HashMap<String, String>,
    pub profile: Option<auth::AuthProfile>,
    pub reconnect: bool,
    pub relocate: bool,
}

/// Validate every field before interrupting a transfer or creating any files.
pub(crate) async fn prepare(db: &Db, patch: EditTask) -> Result<PreparedEdit, String> {
    let original = db
        .load_task_by_id(&patch.task_id)
        .await
        .map_err(|e| format!("db: {e:#}"))?
        .ok_or("not-found")?;
    let context = stored_context(db, &original)
        .await
        .map_err(|e| format!("credentials: {e:#}"))?;
    if revision(&original, &context).map_err(|e| format!("db: {e:#}"))? != patch.expected_revision {
        return Err("edit-conflict".into());
    }
    let caps = capabilities(&original);
    if (!caps.proxy && patch.proxy_url.is_some())
        || (!caps.request
            && (patch.user_agent.is_some()
                || patch.headers.is_some()
                || patch.cookies.is_some()
                || patch.auth_ref.is_some()))
        || (!caps.rename && patch.file_name.is_some())
        || (!caps.storage && patch.save_dir.is_some())
        || (!caps.segments && patch.segments.is_some())
        || (!caps.schedule && patch.scheduled_at.is_some())
    {
        return Err("field-unsupported".into());
    }
    let mut updated = original.clone();
    let old_context = context.clone();
    let (mut cookies, referrer, mut headers) = context;
    if let Some(value) = patch.file_name {
        let value = value.trim();
        if value.is_empty()
            || crate::naming::sanitize_filename(value) != value
            || value.contains(['/', '\\'])
        {
            return Err("invalid-name".into());
        }
        updated.file_name = value.into();
    }
    if let Some(value) = patch.save_dir {
        if value.trim().is_empty()
            || !std::path::Path::new(value.trim()).is_absolute()
            || value.contains('\0')
        {
            return Err("invalid-directory".into());
        }
        updated.save_dir = value.trim().into();
    }
    if let Some(value) = patch.proxy_url {
        validate_proxy(&value)?;
        updated.proxy_url = value.trim().into();
    }
    if let Some(value) = patch.segments {
        if !(0..=512).contains(&value) {
            return Err("invalid-segments".into());
        }
        updated.segments = value;
    }
    if let Some(value) = patch.download_limit_bps {
        if !(0..=1_000_000_000_000).contains(&value) {
            return Err("invalid-speed-limit".into());
        }
        updated.properties.download_limit_bps = value;
        if is_bt(&original.url) {
            updated.bt_download_limit_bps = value;
        }
    }
    if let Some(value) = patch.user_agent {
        reqwest::header::HeaderValue::from_str(&value).map_err(|_| "invalid-user-agent")?;
        updated.properties.user_agent = value;
        headers.retain(|name, _| !name.eq_ignore_ascii_case("user-agent"));
    }
    if let Some(values) = patch.headers {
        if values.len() > 128 {
            return Err("too-many-headers".into());
        }
        let mut validated = HashMap::with_capacity(values.len());
        for (name, mut value) in values {
            reqwest::header::HeaderName::from_bytes(name.as_bytes())
                .map_err(|_| "invalid-header-name")?;
            if value == SECRET_MASK && secret_header(&name) {
                value = headers
                    .iter()
                    .find(|(old, _)| old.eq_ignore_ascii_case(&name))
                    .map(|(_, value)| value.clone())
                    .ok_or("invalid-secret-placeholder")?;
            }
            if value.len() > 65536 {
                return Err("header-too-long".into());
            }
            reqwest::header::HeaderValue::from_str(&value).map_err(|_| "invalid-header-value")?;
            if validated.insert(name.to_ascii_lowercase(), value).is_some() {
                return Err("duplicate-header".into());
            }
        }
        headers = validated;
    }
    if let Some(value) = patch.cookies {
        if value.len() > 65536 {
            return Err("cookies-too-long".into());
        }
        reqwest::header::HeaderValue::from_str(&value).map_err(|_| "invalid-cookies")?;
        cookies = value;
        headers.retain(|name, _| !name.eq_ignore_ascii_case("cookie"));
    }
    if let Some(value) = patch.auth_ref {
        let value = value.trim();
        if !value.is_empty() {
            let profile = auth::load(db, value)
                .await
                .map_err(|e| format!("credentials: {e:#}"))?
                .ok_or("credential-not-found")?;
            if auth::site_key(&original.url).as_deref() != Some(profile.site.as_str())
                || !profile.is_valid_at(auth::now_unix())
            {
                return Err("credential-origin-or-expiry".into());
            }
        }
        updated.properties.auth_ref = value.into();
    }
    if let Some(value) = patch.category {
        if value.len() > 256 || value.chars().any(char::is_control) {
            return Err("invalid-category".into());
        }
        updated.properties.category = value;
    }
    if let Some(values) = patch.tags {
        if values.len() > 64 {
            return Err("too-many-tags".into());
        }
        let mut tags = Vec::new();
        for value in values {
            let value = value.trim();
            if value.is_empty() {
                continue;
            }
            if value.len() > 256 || value.chars().any(char::is_control) {
                return Err("invalid-tag".into());
            }
            if !tags.iter().any(|old| old == value) {
                tags.push(value.to_owned());
            }
        }
        updated.properties.tags = tags;
    }
    if let Some(value) = patch.scheduled_at {
        if !(0..=253402300799).contains(&value) {
            return Err("invalid-schedule".into());
        }
        updated.properties.scheduled_at = value;
    }
    let reconnect = original.proxy_url != updated.proxy_url
        || original.segments != updated.segments
        || original.properties.user_agent != updated.properties.user_agent
        || original.properties.auth_ref != updated.properties.auth_ref
        || old_context.0 != cookies
        || old_context.1 != referrer
        || old_context.2 != headers;
    let relocate = !original.file_clear_state.retains_path_only()
        && (original.save_dir != updated.save_dir || original.file_name != updated.file_name);
    let profile = protect_request_context(&updated, &mut cookies, &mut headers);
    updated.properties.credential_ref = profile
        .as_ref()
        .map(|profile| profile.auth_ref.clone())
        .unwrap_or_default();
    Ok(PreparedEdit {
        original,
        updated,
        cookies,
        referrer,
        headers,
        profile,
        reconnect,
        relocate,
    })
}

pub(crate) fn validate_proxy(value: &str) -> Result<(), String> {
    let value = value.trim();
    if value.is_empty() || matches!(value, "direct://" | "system://") {
        return Ok(());
    }
    let url = url::Url::parse(value).map_err(|_| "invalid-proxy")?;
    if crate::proxy_config::ProxyType::parse_str(url.scheme()).is_none()
        || url.host_str().is_none()
        || !value.starts_with(url.scheme())
        || value.ends_with('/')
        || value
            .rsplit(':')
            .next()
            .and_then(|s| s.parse::<u16>().ok())
            .is_none_or(|p| p == 0)
        || !matches!(url.path(), "" | "/")
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err("invalid-proxy".into());
    }
    reqwest::Proxy::all(value).map_err(|_| "invalid-proxy")?;
    Ok(())
}

impl Db {
    pub(crate) async fn commit_task_edit(
        &self,
        edit: &PreparedEdit,
        moved: &[(std::path::PathBuf, std::path::PathBuf)],
    ) -> Result<(), DbError> {
        let props = serde_json::to_string(&edit.updated.properties)
            .map_err(|_| DbError::InvalidConfig("task properties encoding failed".into()))?;
        let headers = serde_json::to_string(&edit.headers)
            .map_err(|_| DbError::InvalidConfig("task headers encoding failed".into()))?;
        let mut tx = self.pool().begin().await?;
        // An unchanged field must not overwrite a name concurrently bound by the start prelude.
        let result = sqlx::query("UPDATE tasks SET name_inferred = CASE WHEN $11 AND file_name <> $1 THEN 0 ELSE name_inferred END,
            category_route = CASE WHEN $12 AND save_dir <> $2 THEN '' ELSE category_route END,
            file_name = CASE WHEN $11 THEN $1 ELSE file_name END,
            save_dir = CASE WHEN $12 THEN $2 ELSE save_dir END,
            proxy_url = $3, segments = $4, edit_properties = $5,
            cookies = $6, referrer = $7, extra_headers = $8, exists_decision = '', bt_download_limit_bps = $9
            WHERE id = $10")
            .bind(&edit.updated.file_name).bind(&edit.updated.save_dir).bind(&edit.updated.proxy_url)
            .bind(edit.updated.segments).bind(props).bind(&edit.cookies).bind(&edit.referrer).bind(headers)
            .bind(edit.updated.bt_download_limit_bps).bind(&edit.updated.task_id)
            .bind(edit.original.file_name != edit.updated.file_name)
            .bind(edit.original.save_dir != edit.updated.save_dir)
            .execute(&mut *tx).await?;
        if result.rows_affected() != 1 {
            return Err(sqlx::Error::RowNotFound.into());
        }
        for (old, new) in moved {
            Self::rename_registered_artifact(&mut tx, &edit.updated.task_id, old, new).await?;
        }
        if let Some(profile) = &edit.profile {
            insert_inline_credential(&mut tx, profile).await?;
        } else if !edit.original.properties.credential_ref.is_empty()
            && let Some((id, site)) = edit.original.properties.credential_ref.split_once("::")
        {
            sqlx::query("DELETE FROM config WHERE key = $1")
                .bind(format!("plugin.{id}.auth.{site}"))
                .execute(&mut *tx)
                .await?;
        }
        tx.commit().await?;
        Ok(())
    }
}

pub(crate) async fn insert_inline_credential(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    profile: &auth::AuthProfile,
) -> Result<(), DbError> {
    let key = format!("plugin.{}.auth.{}", profile.plugin_id, profile.site);
    let value = serde_json::to_string(profile)
        .map_err(|_| DbError::InvalidConfig("task credentials encoding failed".into()))?;
    sqlx::query("INSERT INTO config (key, value) VALUES ($1, $2) ON CONFLICT (key) DO UPDATE SET value = excluded.value")
        .bind(key)
        .bind(value)
        .execute(&mut **tx)
        .await?;
    Ok(())
}

pub(crate) async fn delete_inline_credential(
    tx: &mut sqlx::Transaction<'_, sqlx::Any>,
    properties: &str,
) -> Result<(), DbError> {
    let properties: TaskProperties = serde_json::from_str(properties)
        .map_err(|_| DbError::InvalidConfig("invalid stored task properties".into()))?;
    if let Some((id, site)) = properties.credential_ref.split_once("::") {
        sqlx::query("DELETE FROM config WHERE key = $1")
            .bind(format!("plugin.{id}.auth.{site}"))
            .execute(&mut **tx)
            .await?;
    }
    Ok(())
}

pub(crate) async fn load_properties(db: &Db, id: &str) -> Result<TaskProperties, DbError> {
    let raw: String = sqlx::query_scalar("SELECT edit_properties FROM tasks WHERE id = $1")
        .bind(id)
        .fetch_one(db.pool())
        .await?;
    serde_json::from_str(&raw)
        .map_err(|_| DbError::InvalidConfig("invalid stored task properties".into()))
}

fn capabilities(task: &TaskInfo) -> EditCapabilities {
    let request = task.url.starts_with("http://") || task.url.starts_with("https://");
    let proxy = request || task.url.starts_with("ftp://");
    let bt = is_bt(&task.url);
    let completed = task.status == 3;
    EditCapabilities {
        request: request && !completed,
        proxy: proxy && !completed,
        storage: !bt || cfg!(feature = "bt"),
        rename: !bt,
        segments: !bt && !completed,
        schedule: !completed,
    }
}
