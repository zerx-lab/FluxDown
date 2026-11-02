//! `fluxdown_engine` 领域模型与 `fluxdown_protocol` wire DTO 的无状态转换。
//!
//! 两端 crate 保持彼此独立；宿主只在边界调用本 crate 的命名函数。

use fluxdown_engine::downloader::CapturedRequestBody;
use fluxdown_engine::model::{
    BtFileEntry, CdnNodeInfo, GroupInfo, HlsQualityOption, QueueInfo, QueueOrderEntry,
    QueuePosition, ResolveVariantOption, SegmentDetail, SourceBytes, TaskInfo,
};
use fluxdown_engine::rss::model::{RssItemInfo, RssSourceInfo};
use fluxdown_engine::selection::PluginCandidate;
use fluxdown_engine::webhook::{PresetInfo, TaskNotice, WebhookDelivery};
use fluxdown_protocol::daemon::{
    BtFileDto, CdnNodeDto, GroupDto, HlsQualityOptionDto, PluginCandidateDto, QueueDto,
    QueueOrderEntryDto, QueuePositionDto, RequestBody, ResolveVariantOptionDto, RssItemDto,
    RssSourceDto, SegmentDetailDto, TaskDto, WebhookDeliveryDto, WebhookPresetDto,
};
#[cfg(feature = "components")]
use fluxdown_protocol::daemon::{ComponentFfmpegStatus, ComponentVersions, ComponentYtdlpStatus};

/// 将捕获请求体转换为引擎请求体。
#[must_use]
pub fn request_body_to_engine(body: RequestBody) -> CapturedRequestBody {
    match body {
        RequestBody::FormData { fields } => CapturedRequestBody::FormData { fields },
        RequestBody::Urlencoded { raw } => CapturedRequestBody::Urlencoded { raw },
        RequestBody::Raw {
            bytes_b64,
            content_type,
        } => CapturedRequestBody::Raw {
            bytes_b64,
            content_type,
        },
    }
}

/// 将分类目录路由转换为引擎路由（`base_dir` 由引擎建任务时填为解析出的保存目录）。
#[must_use]
pub fn category_route_to_engine(
    route: fluxdown_protocol::CategoryRouteDto,
) -> fluxdown_engine::category_route::CategoryRoute {
    fluxdown_engine::category_route::CategoryRoute {
        base_dir: String::new(),
        rules: route
            .rules
            .into_iter()
            .map(|rule| fluxdown_engine::category_route::CategoryRule {
                match_mode: rule.match_mode,
                extensions: rule.extensions,
                regex_pattern: rule.regex_pattern,
                url_pattern: rule.url_pattern,
                save_dir: rule.save_dir,
            })
            .collect(),
        other_dir: route.other_dir,
        archive: None,
    }
}

/// 将加速来源累计字节转换为 wire DTO。
#[must_use]
pub fn source_bytes_to_dto(bytes: SourceBytes) -> fluxdown_protocol::TaskSourceBytesDto {
    fluxdown_protocol::TaskSourceBytesDto {
        cdn_bytes: bytes.cdn,
        proxy_bytes: bytes.proxy,
        nic_bytes: bytes.nic,
    }
}

/// 将宿主自身请求的出口方案转换为 wire DTO（改写地址在前，原地址由调用方兜底）。
#[must_use]
pub fn egress_plan_to_dto(
    plan: fluxdown_engine::egress::EgressPlan,
) -> fluxdown_protocol::EgressPlanDto {
    let proxy = plan.proxy.map(|proxy| fluxdown_protocol::EgressProxyDto {
        url: proxy.url,
        no_proxy: proxy.no_proxy,
    });
    match plan.route.rewrite() {
        Some(rewrite) => fluxdown_protocol::EgressPlanDto {
            url: rewrite.url.clone(),
            headers: rewrite.headers.iter().cloned().collect(),
            plugin: Some(rewrite.plugin.clone()),
            proxy,
        },
        None => fluxdown_protocol::EgressPlanDto {
            url: plan.route.source_url().to_owned(),
            headers: std::collections::BTreeMap::new(),
            plugin: None,
            proxy,
        },
    }
}

/// 将站点规则的校验结果与（可选的）路由解释转换为 wire DTO；`index` 从 0 起。
#[must_use]
pub fn proxy_route_to_dto(
    issues: Vec<fluxdown_engine::proxy_rules::RuleIssue>,
    explanation: Option<fluxdown_engine::proxy_rules::RouteExplanation>,
) -> fluxdown_protocol::ProxyRouteResponse {
    let to_index = |index: usize| u32::try_from(index).unwrap_or(u32::MAX);
    let issues = issues
        .into_iter()
        .map(|issue| fluxdown_protocol::ProxyRuleIssueDto {
            index: to_index(issue.index),
            field: issue.field.to_owned(),
            code: issue.code.to_owned(),
            detail: issue.detail,
        })
        .collect();
    match explanation {
        Some(explanation) => fluxdown_protocol::ProxyRouteResponse {
            issues,
            matched: explanation
                .matched
                .map(|matched| fluxdown_protocol::ProxyRouteMatchDto {
                    index: to_index(matched.index),
                    id: matched.id,
                    name: matched.name,
                }),
            reason: explanation.reason.as_str().to_owned(),
            endpoint: explanation.endpoint,
        },
        None => fluxdown_protocol::ProxyRouteResponse {
            issues,
            matched: None,
            reason: String::new(),
            endpoint: String::new(),
        },
    }
}

/// 将引擎实际传输采样转换成 wire DTO，不推测未知并发。
#[must_use]
pub fn task_runtime_to_dto(
    runtime: fluxdown_engine::transfer_activity::TaskRuntime,
) -> fluxdown_protocol::TaskRuntimeDto {
    fluxdown_protocol::TaskRuntimeDto {
        task_id: runtime.task_id,
        sampled_at_ms: runtime.sampled_at_ms,
        sample_sequence: runtime.sample_sequence,
        active_transfers: runtime.active_transfers,
        connected_peers: runtime.connected_peers,
        parallelism_limit: runtime.parallelism_limit,
        total_bytes: runtime.total_bytes,
        segments: runtime
            .segments
            .into_iter()
            .map(|segment| fluxdown_protocol::TaskSegmentDto {
                index: segment.index,
                start_byte: segment.start_byte,
                end_byte: segment.end_byte,
                downloaded_bytes: segment.downloaded_bytes,
                active: segment.active,
            })
            .collect(),
        source_bytes: runtime.source_bytes.map(source_bytes_to_dto),
    }
}

/// 将已提交的源端日志转换成 wire DTO。
#[must_use]
pub fn task_activity_to_dto(
    activity: fluxdown_engine::task_activity::TaskActivity,
) -> fluxdown_protocol::TaskActivityDto {
    fluxdown_protocol::TaskActivityDto {
        id: activity.id,
        task_id: activity.task_id,
        timestamp_ms: activity.timestamp_ms,
        kind: activity.kind,
        message: activity.message,
        status: activity.status,
    }
}

/// 将任务速度历史转换成 wire DTO（未满桶的当前均值作为末点）。
#[must_use]
pub fn task_speed_history_to_dto(
    task_id: String,
    history: fluxdown_engine::speed_history::SpeedHistory,
) -> fluxdown_protocol::TaskSpeedHistoryDto {
    fluxdown_protocol::TaskSpeedHistoryDto {
        task_id,
        bucket_secs: history.bucket_secs,
        samples: history.points(),
        active_secs: history.active_secs,
        transferred_bytes: history.transferred_bytes,
        peak_bps: history.peak_bps,
    }
}

/// 将引擎任务投影转换为 wire DTO。
#[must_use]
pub fn task_info_to_dto(task: TaskInfo) -> TaskDto {
    TaskDto {
        task_id: task.task_id,
        url: task.url,
        file_name: task.file_name,
        save_dir: task.save_dir,
        status: task.status,
        downloaded_bytes: task.downloaded_bytes,
        total_bytes: task.total_bytes,
        error_message: task.error_message,
        created_at: task.created_at,
        proxy_url: task.proxy_url,
        queue_id: task.queue_id,
        checksum: task.checksum,
        ignore_tls_errors: task.ignore_tls_errors,
        file_missing: task.file_missing,
        completed_at: task.completed_at,
        referrer: task.referrer,
        group_id: task.group_id,
        rss_source_id: task.rss_source_id,
        origin_url: task.origin_url,
        auto_route: task.auto_route,
        source_bytes: source_bytes_to_dto(task.source_bytes),
        queue_order: task.queue_order,
        uploaded_bytes: task.uploaded_bytes,
        uploaded_at_completion: task.uploaded_at_completion,
        seeding_status: task.seeding_status,
        seeding_message: task.seeding_message,
        seeding_time_secs: task.seeding_time_secs,
        seed_ratio_limit_milli: task.seed_ratio_limit_milli,
        seed_post_ratio_limit_milli: task.seed_post_ratio_limit_milli,
        seed_time_limit_minutes: task.seed_time_limit_minutes,
        seed_inactive_time_limit_minutes: task.seed_inactive_time_limit_minutes,
        seed_upload_limit_bps: task.seed_upload_limit_bps,
    }
}

/// 将引擎队列投影转换为 wire DTO。
#[must_use]
pub fn queue_info_to_dto(queue: QueueInfo) -> QueueDto {
    QueueDto {
        queue_id: queue.queue_id,
        name: queue.name,
        speed_limit_kbps: queue.speed_limit_kbps,
        upload_limit_kbps: queue.upload_limit_kbps,
        max_concurrent: queue.max_concurrent,
        default_save_dir: queue.default_save_dir,
        position: queue.position,
        default_segments: queue.default_segments,
        default_user_agent: queue.default_user_agent,
        is_running: queue.is_running,
        schedule_enabled: queue.schedule_enabled,
        schedule_start: queue.schedule_start,
        schedule_stop: queue.schedule_stop,
        schedule_days: queue.schedule_days,
    }
}

/// 将引擎分组投影转换为 wire DTO。
#[must_use]
pub fn group_info_to_dto(group: GroupInfo) -> GroupDto {
    GroupDto {
        group_id: group.group_id,
        name: group.name,
        source_url: group.source_url,
        save_dir: group.save_dir,
        created_at: group.created_at,
    }
}

/// 将引擎 RSS 源投影转换为 wire DTO。
#[must_use]
pub fn rss_source_info_to_dto(source: RssSourceInfo) -> RssSourceDto {
    RssSourceDto {
        source_id: source.source_id,
        provider_id: source.provider_id,
        provider_config: source.provider_config,
        url: source.url,
        name: source.name,
        enabled: source.enabled,
        auto_download: source.auto_download,
        start_paused: source.start_paused,
        queue_id: source.queue_id,
        save_dir: source.save_dir,
        interval_minutes: source.interval_minutes,
        include_pattern: source.include_pattern,
        exclude_pattern: source.exclude_pattern,
        use_regex: source.use_regex,
        smart_episode: source.smart_episode,
        size_min_bytes: source.size_min_bytes,
        size_max_bytes: source.size_max_bytes,
        send_referer: source.send_referer,
        notify_on_download: source.notify_on_download,
        max_per_fetch: source.max_per_fetch,
        cookies: source.cookies,
        user_agent: source.user_agent,
        proxy_url: source.proxy_url,
        last_fetch_at: source.last_fetch_at,
        last_success_at: source.last_success_at,
        last_error: source.last_error,
        fail_count: source.fail_count,
        seeded: source.seeded,
        position: source.position,
        unread_count: source.unread_count,
    }
}

/// 将 RSS 源写请求转换为引擎模型，只保留可编辑字段。
#[must_use]
pub fn rss_source_dto_to_engine(source: RssSourceDto) -> RssSourceInfo {
    RssSourceInfo {
        source_id: source.source_id,
        provider_id: if source.provider_id.trim().is_empty() {
            fluxdown_engine::rss::model::RSS_PROVIDER_ID.to_string()
        } else {
            source.provider_id
        },
        provider_config: source.provider_config,
        url: source.url,
        name: source.name,
        enabled: source.enabled,
        auto_download: source.auto_download,
        start_paused: source.start_paused,
        queue_id: source.queue_id,
        save_dir: source.save_dir,
        interval_minutes: if source.interval_minutes > 0 {
            source.interval_minutes
        } else {
            fluxdown_engine::rss::model::DEFAULT_INTERVAL_MINUTES
        },
        include_pattern: source.include_pattern,
        exclude_pattern: source.exclude_pattern,
        use_regex: source.use_regex,
        smart_episode: source.smart_episode,
        size_min_bytes: source.size_min_bytes,
        size_max_bytes: source.size_max_bytes,
        send_referer: source.send_referer,
        notify_on_download: source.notify_on_download,
        max_per_fetch: if source.max_per_fetch > 0 {
            source.max_per_fetch
        } else {
            fluxdown_engine::rss::model::DEFAULT_MAX_PER_FETCH
        },
        cookies: source.cookies,
        user_agent: source.user_agent,
        proxy_url: source.proxy_url,
        ..RssSourceInfo::default()
    }
}

/// 将引擎 RSS 条目投影转换为 wire DTO。
#[must_use]
pub fn rss_item_info_to_dto(item: RssItemInfo) -> RssItemDto {
    RssItemDto {
        source_id: item.source_id,
        guid: item.guid,
        title: item.title,
        link: item.link,
        enclosure_url: item.enclosure_url,
        enclosure_length: item.enclosure_length,
        pub_date: item.pub_date,
        fetched_at: item.fetched_at,
        status: item.status.as_i32(),
        task_id: item.task_id,
        episode_key: item.episode_key,
        reason: item.reason,
    }
}

/// 将分段详情转换为 wire DTO。
#[must_use]
pub fn segment_detail_to_dto(segment: SegmentDetail) -> SegmentDetailDto {
    SegmentDetailDto {
        index: segment.index,
        start_byte: segment.start_byte,
        end_byte: segment.end_byte,
        downloaded_bytes: segment.downloaded_bytes,
    }
}

/// 将 CDN 节点投影转换为 wire DTO。
#[must_use]
pub fn cdn_node_info_to_dto(node: CdnNodeInfo) -> CdnNodeDto {
    CdnNodeDto {
        ip: node.ip,
        origin: node.origin,
        bytes: node.bytes,
        ewma_bps: node.ewma_bps,
        active: node.active,
    }
}

/// 将队列位置转换为 wire DTO。
#[must_use]
pub fn queue_position_to_dto(position: QueuePosition) -> QueuePositionDto {
    QueuePositionDto {
        task_id: position.task_id,
        position: position.position,
        order: position.order,
    }
}

/// 将队列顺序增量的一项转换为 wire DTO。
#[must_use]
pub fn queue_order_entry_to_dto(entry: QueueOrderEntry) -> QueueOrderEntryDto {
    QueueOrderEntryDto {
        task_id: entry.task_id,
        order: entry.order,
    }
}

/// 将 HLS 选项转换为 wire DTO。
#[must_use]
pub fn hls_quality_option_to_dto(option: HlsQualityOption) -> HlsQualityOptionDto {
    HlsQualityOptionDto {
        index: option.index,
        bandwidth: option.bandwidth,
        width: option.width,
        height: option.height,
    }
}

/// 将 BT 文件条目转换为 wire DTO。
#[must_use]
pub fn bt_file_entry_to_dto(file: BtFileEntry) -> BtFileDto {
    BtFileDto {
        index: file.index,
        path: file.path,
        size: file.size,
    }
}

/// 将解析变体转换为 wire DTO。
#[must_use]
pub fn resolve_variant_option_to_dto(option: ResolveVariantOption) -> ResolveVariantOptionDto {
    ResolveVariantOptionDto {
        index: option.index,
        label: option.label,
        container: option.container,
        bandwidth: option.bandwidth,
        width: option.width,
        height: option.height,
        total_bytes: option.total_bytes,
    }
}

/// 将多插件选择的候选插件转换为 wire DTO。
#[must_use]
pub fn plugin_candidate_to_dto(candidate: PluginCandidate) -> PluginCandidateDto {
    PluginCandidateDto {
        identity: candidate.identity,
        name: candidate.name,
        version: candidate.version,
        description: candidate.description,
    }
}

/// 将 Webhook 投递记录转换为 wire DTO。
#[must_use]
pub fn webhook_delivery_to_dto(delivery: WebhookDelivery) -> WebhookDeliveryDto {
    WebhookDeliveryDto {
        delivery_id: delivery.delivery_id,
        timestamp_ms: delivery.timestamp_ms,
        event: delivery.event,
        endpoint_id: delivery.endpoint_id,
        endpoint_name: delivery.endpoint_name,
        url: delivery.url,
        request_headers: delivery.request_headers,
        request_body: delivery.request_body,
        status_code: delivery.status_code,
        response_body: delivery.response_body,
        latency_ms: delivery.latency_ms,
        attempts: delivery.attempts,
        success: delivery.success,
        error: delivery.error,
    }
}

/// 将云端推送语义事件转换为 wire DTO（`DaemonEvent::TaskNotice`）。
#[must_use]
pub fn task_notice_to_dto(notice: TaskNotice) -> fluxdown_protocol::TaskNoticeDto {
    fluxdown_protocol::TaskNoticeDto {
        delivery_id: notice.delivery_id,
        event: notice.event,
        timestamp_ms: notice.timestamp_ms,
        queue_id: notice.queue_id,
        queue_name: notice.queue_name,
        task: notice
            .task
            .map(|task| fluxdown_protocol::TaskNoticeTaskDto {
                id: task.id,
                file_name: task.file_name,
                url: task.url,
                save_dir: task.save_dir,
                total_bytes: task.total_bytes,
                status: task.status,
                error_message: task.error_message,
            }),
    }
}

/// 将 Webhook 预设转换为 wire DTO。
#[must_use]
pub fn webhook_preset_to_dto(preset: PresetInfo) -> WebhookPresetDto {
    WebhookPresetDto {
        id: preset.id.to_owned(),
        label: preset.label.to_owned(),
        url_placeholder: preset.url_placeholder.to_owned(),
        default_template: preset.default_template.to_owned(),
        content_type: preset.content_type.to_owned(),
    }
}

/// 将插件设置字段转换为 wire DTO。
#[cfg(feature = "plugins")]
#[must_use]
pub fn setting_field_to_dto(
    field: fluxdown_engine::plugin::SettingField,
) -> fluxdown_protocol::daemon::SettingFieldDto {
    use fluxdown_engine::plugin::{SettingType, SettingWidget};
    use fluxdown_protocol::daemon::{SettingFieldDto, SettingOptionDto};

    let setting_type = match field.ty {
        SettingType::String => "string",
        SettingType::Number => "number",
        SettingType::Boolean => "boolean",
    }
    .to_owned();
    let widget = match field.effective_widget() {
        SettingWidget::Text => "text",
        SettingWidget::Password => "password",
        SettingWidget::Textarea => "textarea",
        SettingWidget::Select => "select",
        SettingWidget::Toggle => "toggle",
        SettingWidget::Number => "number",
        SettingWidget::Folder => "folder",
    }
    .to_owned();
    SettingFieldDto {
        key: field.key,
        title: field.title,
        description: field.description,
        setting_type,
        widget,
        options: field
            .options
            .into_iter()
            .map(|option| SettingOptionDto {
                value: option.value,
                label: option.label,
            })
            .collect(),
        default: field.default,
        required: field.required,
        min: field.min,
        max: field.max,
        helper_script: field.helper_script,
        helper_label: field.helper_label,
        pattern: field.pattern,
    }
}

/// 将已安装插件投影转换为 wire DTO。
#[cfg(feature = "plugins")]
#[must_use]
pub fn plugin_info_to_dto(
    plugin: fluxdown_engine::plugin::PluginInfo,
) -> fluxdown_protocol::daemon::PluginDto {
    fluxdown_protocol::daemon::PluginDto {
        identity: plugin.identity,
        name: plugin.name,
        version: plugin.version,
        description: plugin.description,
        homepage: plugin.homepage,
        enabled: plugin.enabled,
        dev_mode: plugin.dev_mode,
        disabled_reason: plugin.disabled_reason,
        settings: plugin
            .settings
            .into_iter()
            .map(setting_field_to_dto)
            .collect(),
        settings_values: plugin.settings_values.into_iter().collect(),
        permissions: plugin.permissions,
        auth_supported: plugin.auth_supported,
        subscription_provider_ids: plugin.subscription_provider_ids,
        resolver_urls: plugin.resolver_urls,
        load_status: plugin.load_status,
        load_error: plugin.load_error,
        contributions: plugin_contributions_to_dto(&plugin.contributions, plugin.view_icons),
        risk: plugin.risk.as_str().to_owned(),
        consent_pending: plugin.consent_pending,
        consent_pending_hosts: plugin.consent_pending_hosts,
        network_hosts: plugin.network_hosts,
        package_dir: plugin.package_dir,
        package_revision: plugin.package_revision,
    }
}

#[cfg(feature = "plugins")]
fn plugin_contributions_to_dto(
    contributes: &fluxdown_engine::plugin::manifest::ContributesDecl,
    mut view_icons: Vec<(String, String)>,
) -> fluxdown_protocol::daemon::PluginContributionsDto {
    use fluxdown_protocol::daemon::{
        PluginCommandDeclDto, PluginContributionsDto, PluginDesktopEntryDto,
        PluginPageScriptDeclDto, PluginViewDeclDto,
    };
    PluginContributionsDto {
        views: contributes
            .views
            .iter()
            .map(|view| PluginViewDeclDto {
                id: view.id.clone(),
                title: view.title.clone(),
                location: view.location.as_str().to_owned(),
                portable: view.is_portable(),
                desktop: view.desktop.as_ref().map(|d| PluginDesktopEntryDto {
                    entry: d.entry.clone(),
                    width: d.width,
                    height: d.height,
                    network_hosts: d.network_hosts.clone(),
                }),
                icon_svg: view_icons
                    .iter()
                    .position(|(id, _)| *id == view.id)
                    .map(|index| view_icons.swap_remove(index).1),
            })
            .collect(),
        commands: contributes
            .commands
            .iter()
            .map(|cmd| PluginCommandDeclDto {
                id: cmd.id.clone(),
                title: cmd.title.clone(),
                palette: cmd.palette,
                task_menu: cmd.task_menu,
            })
            .collect(),
        sniff_rules: contributes.sniff_rules.is_some(),
        page_scripts: contributes
            .page_scripts
            .iter()
            .map(|s| PluginPageScriptDeclDto {
                id: s.id.clone(),
                title: s.title.clone(),
                matches: s.matches.clone(),
                world: s.world.as_str().to_owned(),
            })
            .collect(),
    }
}

/// 将插件市场条目转换为 wire DTO。
#[cfg(feature = "plugins")]
#[must_use]
pub fn market_entry_to_dto(
    entry: fluxdown_engine::plugin::MarketEntry,
) -> fluxdown_protocol::daemon::MarketEntryDto {
    fluxdown_protocol::daemon::MarketEntryDto {
        plugin_id: entry.plugin_id,
        version: entry.version,
        sequence: entry.sequence,
        content_hash: entry.content_hash,
        min_app_version: entry.min_app_version,
        name: entry.name,
        description: entry.description,
        author: entry.author,
        homepage: entry.homepage,
        mirrors: entry.mirrors,
        publish_time: entry.publish_time,
        yanked: entry.yanked,
        tags: entry.tags,
        permissions: entry.permissions,
        network_hosts: entry.network_hosts,
    }
}

/// 将 ffmpeg 状态转换为 wire DTO。
#[cfg(feature = "components")]
#[must_use]
pub fn ffmpeg_status_to_dto(
    status: fluxdown_engine::components::FfmpegStatus,
) -> ComponentFfmpegStatus {
    ComponentFfmpegStatus {
        source: status.source.as_str().to_owned(),
        path: status.path,
        version: status.version,
        managed_version: status.managed_version,
        system_path: status.system_path,
        managed_supported: status.managed_supported,
    }
}

/// 将 ffmpeg 版本目录转换为 wire DTO。
#[cfg(feature = "components")]
#[must_use]
pub fn ffmpeg_versions_to_dto(
    versions: fluxdown_engine::components::FfmpegVersions,
) -> ComponentVersions {
    ComponentVersions {
        versions: versions.versions,
        latest_stable: versions.latest_stable,
    }
}

/// 将 yt-dlp 状态转换为 wire DTO。
#[cfg(feature = "components")]
#[must_use]
pub fn ytdlp_status_to_dto(
    status: fluxdown_engine::components::YtdlpStatus,
) -> ComponentYtdlpStatus {
    ComponentYtdlpStatus {
        source: status.source.as_str().to_owned(),
        path: status.path,
        version: status.version,
        managed_version: status.managed_version,
        system_path: status.system_path,
        managed_supported: status.managed_supported,
    }
}

/// 将 yt-dlp 版本目录转换为 wire DTO。
#[cfg(feature = "components")]
#[must_use]
pub fn ytdlp_versions_to_dto(
    versions: fluxdown_engine::components::YtdlpVersions,
) -> ComponentVersions {
    ComponentVersions {
        versions: versions.versions,
        latest_stable: versions.latest_stable,
    }
}

#[cfg(test)]
mod tests {
    use super::{queue_order_entry_to_dto, queue_position_to_dto};
    use fluxdown_engine::queue_order::{QueueOrderDelta, QueueOrderPublisher};
    use fluxdown_protocol::{QueuePositionDto, apply_queue_order};

    /// 引擎发布的「全量 + 增量」经协议 reducer 应用后，必须与队列顺序逐项一致，
    /// 且 `position` 为名次。
    #[test]
    fn engine_deltas_applied_by_the_protocol_reducer_match_the_queue() {
        let mut publisher = QueueOrderPublisher::default();
        let mut client: Vec<QueuePositionDto> = Vec::new();
        let steps: [&[&str]; 7] = [
            &["a", "b", "c", "d"],
            &["b", "c", "d"],
            &["b", "c", "d", "e", "f"],
            &["x", "b", "c", "d", "e", "f"],
            &["x", "b", "m", "c", "d", "e", "f"],
            &["f", "x", "b", "m", "c", "d", "e"],
            &[],
        ];
        for queue in steps {
            match publisher.diff(queue.iter().copied()) {
                QueueOrderDelta::Unchanged => {}
                QueueOrderDelta::Full(positions) => {
                    client = positions.into_iter().map(queue_position_to_dto).collect();
                }
                QueueOrderDelta::Incremental { removed, added } => {
                    let added: Vec<_> = added.into_iter().map(queue_order_entry_to_dto).collect();
                    apply_queue_order(&mut client, &removed, &added);
                }
            }
            assert_eq!(
                client
                    .iter()
                    .map(|p| p.task_id.as_str())
                    .collect::<Vec<_>>(),
                queue,
                "queue {queue:?}"
            );
            for (index, entry) in client.iter().enumerate() {
                assert_eq!(entry.position, i32::try_from(index + 1).unwrap_or(0));
            }
            assert!(client.windows(2).all(|w| w[0].order < w[1].order));
        }
    }
}
