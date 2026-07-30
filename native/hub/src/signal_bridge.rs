//! `engine::model::*` ↔ `hub::signals::*` 类型转换。
//!
//! orphan rule 决定了不能跨 crate 共享同一 derive 类型(`fluxdown_engine` 不
//! 知道、也不能依赖 `rinf`),因此这里为每个引擎领域类型手写一个到对应
//! Dart 信号 DTO 的 `From` 实现——这是标准 repository-pattern 边界收口做法,
//! 内容是搬移字段而非新写业务逻辑。

use fluxdown_engine::model;
use fluxdown_engine::rss::model as rss_model;
use fluxdown_engine::webhook;

use crate::signals;

impl From<model::TaskInfo> for signals::TaskInfo {
    fn from(t: model::TaskInfo) -> Self {
        Self {
            task_id: t.task_id,
            url: t.url,
            file_name: t.file_name,
            save_dir: t.save_dir,
            status: t.status,
            downloaded_bytes: t.downloaded_bytes,
            total_bytes: t.total_bytes,
            error_message: t.error_message,
            created_at: t.created_at,
            proxy_url: t.proxy_url,
            queue_id: t.queue_id,
            checksum: t.checksum,
            ignore_tls_errors: t.ignore_tls_errors,
            file_missing: t.file_missing,
            completed_at: t.completed_at,
            segments: t.segments,
            queue_order: t.queue_order,
            uploaded_bytes: t.uploaded_bytes,
            uploaded_at_completion: t.uploaded_at_completion,
            seeding_status: t.seeding_status,
            seeding_message: t.seeding_message,
            seed_ratio_limit_milli: t.seed_ratio_limit_milli,
            seed_post_ratio_limit_milli: t.seed_post_ratio_limit_milli,
            seed_time_limit_minutes: t.seed_time_limit_minutes,
            seed_inactive_time_limit_minutes: t.seed_inactive_time_limit_minutes,
            referrer: t.referrer,
            group_id: t.group_id,
            rss_source_id: t.rss_source_id,
            origin_url: t.origin_url,
            auto_route: t.auto_route,
        }
    }
}

impl From<model::QueueInfo> for signals::QueueInfo {
    fn from(q: model::QueueInfo) -> Self {
        Self {
            queue_id: q.queue_id,
            name: q.name,
            speed_limit_kbps: q.speed_limit_kbps,
            max_concurrent: q.max_concurrent,
            default_save_dir: q.default_save_dir,
            position: q.position,
            default_segments: q.default_segments,
            default_user_agent: q.default_user_agent,
            is_running: q.is_running,
            schedule_enabled: q.schedule_enabled,
            schedule_start: q.schedule_start,
            schedule_stop: q.schedule_stop,
            schedule_days: q.schedule_days,
        }
    }
}

impl From<model::QueuePosition> for signals::QueuePosition {
    fn from(p: model::QueuePosition) -> Self {
        Self {
            task_id: p.task_id,
            position: p.position,
        }
    }
}

impl From<model::SegmentDetail> for signals::SegmentDetail {
    fn from(s: model::SegmentDetail) -> Self {
        Self {
            index: s.index,
            start_byte: s.start_byte,
            end_byte: s.end_byte,
            downloaded_bytes: s.downloaded_bytes,
        }
    }
}

impl From<model::CdnNodeInfo> for signals::CdnNodeDetail {
    fn from(n: model::CdnNodeInfo) -> Self {
        Self {
            ip: n.ip,
            origin: n.origin,
            bytes: n.bytes,
            ewma_bps: n.ewma_bps,
            active: n.active,
        }
    }
}

impl From<model::BtFileEntry> for signals::BtFileEntry {
    fn from(f: model::BtFileEntry) -> Self {
        Self {
            index: f.index,
            path: f.path,
            size: f.size,
        }
    }
}

impl From<model::HlsQualityOption> for signals::HlsQualityOption {
    fn from(o: model::HlsQualityOption) -> Self {
        Self {
            index: o.index,
            bandwidth: o.bandwidth,
            width: o.width,
            height: o.height,
        }
    }
}

impl From<model::ResolveVariantOption> for signals::ResolveVariantOption {
    fn from(o: model::ResolveVariantOption) -> Self {
        Self {
            index: o.index,
            label: o.label,
            container: o.container,
            bandwidth: o.bandwidth,
            width: o.width,
            height: o.height,
            total_bytes: o.total_bytes,
        }
    }
}

impl From<model::TorrentMetaResult> for signals::TorrentMetaResult {
    fn from(r: model::TorrentMetaResult) -> Self {
        Self {
            probe_id: r.probe_id,
            name: r.name,
            total_bytes: r.total_bytes,
            files: r.files.into_iter().map(Into::into).collect(),
            error: r.error,
        }
    }
}

impl From<model::GroupInfo> for signals::GroupInfo {
    fn from(g: model::GroupInfo) -> Self {
        Self {
            group_id: g.group_id,
            name: g.name,
            source_url: g.source_url,
            save_dir: g.save_dir,
            created_at: g.created_at,
        }
    }
}

impl From<model::ManifestVariantInfo> for signals::ManifestVariantDto {
    fn from(v: model::ManifestVariantInfo) -> Self {
        Self {
            id: v.id,
            label: v.label,
            size: v.size,
        }
    }
}

impl From<model::ManifestItemInfo> for signals::ManifestItemDto {
    fn from(i: model::ManifestItemInfo) -> Self {
        Self {
            id: i.id,
            name: i.name,
            path: i.path,
            size: i.size,
            variants: i.variants.into_iter().map(Into::into).collect(),
        }
    }
}

impl From<rss_model::RssSourceInfo> for signals::RssSourceEntry {
    fn from(s: rss_model::RssSourceInfo) -> Self {
        Self {
            source_id: s.source_id,
            url: s.url,
            name: s.name,
            enabled: s.enabled,
            auto_download: s.auto_download,
            start_paused: s.start_paused,
            queue_id: s.queue_id,
            save_dir: s.save_dir,
            interval_minutes: s.interval_minutes,
            include_pattern: s.include_pattern,
            exclude_pattern: s.exclude_pattern,
            use_regex: s.use_regex,
            smart_episode: s.smart_episode,
            size_min_bytes: s.size_min_bytes,
            size_max_bytes: s.size_max_bytes,
            send_referer: s.send_referer,
            notify_on_download: s.notify_on_download,
            max_per_fetch: s.max_per_fetch,
            cookies: s.cookies,
            user_agent: s.user_agent,
            proxy_url: s.proxy_url,
            last_fetch_at: s.last_fetch_at,
            last_success_at: s.last_success_at,
            last_error: s.last_error,
            fail_count: s.fail_count,
            seeded: s.seeded,
            position: s.position,
            unread_count: s.unread_count,
        }
    }
}

/// Dart → 引擎方向：只搬**用户可编辑**字段，运行态（`last_*`/`fail_count`/
/// `seeded`/`position`/`unread_count`）一律取默认值——由引擎自己维护，绝不
/// 让一次 UI 保存把退避账本或首轮标记覆盖掉。
impl From<signals::RssSourceEntry> for rss_model::RssSourceInfo {
    fn from(s: signals::RssSourceEntry) -> Self {
        Self {
            source_id: s.source_id,
            url: s.url,
            name: s.name,
            enabled: s.enabled,
            auto_download: s.auto_download,
            start_paused: s.start_paused,
            queue_id: s.queue_id,
            save_dir: s.save_dir,
            interval_minutes: s.interval_minutes,
            include_pattern: s.include_pattern,
            exclude_pattern: s.exclude_pattern,
            use_regex: s.use_regex,
            smart_episode: s.smart_episode,
            size_min_bytes: s.size_min_bytes,
            size_max_bytes: s.size_max_bytes,
            send_referer: s.send_referer,
            notify_on_download: s.notify_on_download,
            max_per_fetch: s.max_per_fetch,
            cookies: s.cookies,
            user_agent: s.user_agent,
            proxy_url: s.proxy_url,
            ..Default::default()
        }
    }
}

impl From<rss_model::RssItemInfo> for signals::RssItemEntry {
    fn from(i: rss_model::RssItemInfo) -> Self {
        Self {
            source_id: i.source_id,
            guid: i.guid,
            title: i.title,
            link: i.link,
            enclosure_url: i.enclosure_url,
            enclosure_length: i.enclosure_length,
            pub_date: i.pub_date,
            fetched_at: i.fetched_at,
            status: i.status.as_i32(),
            task_id: i.task_id,
            episode_key: i.episode_key,
            reason: i.reason,
        }
    }
}

impl From<webhook::WebhookDelivery> for signals::WebhookDeliveryEntry {
    fn from(d: webhook::WebhookDelivery) -> Self {
        Self {
            delivery_id: d.delivery_id,
            timestamp_ms: d.timestamp_ms,
            event: d.event,
            endpoint_id: d.endpoint_id,
            endpoint_name: d.endpoint_name,
            url: d.url,
            request_headers: d.request_headers,
            request_body: d.request_body,
            status_code: d.status_code,
            response_body: d.response_body,
            latency_ms: d.latency_ms,
            attempts: d.attempts,
            success: d.success,
            error: d.error,
        }
    }
}

impl From<webhook::PresetInfo> for signals::WebhookPresetEntry {
    fn from(p: webhook::PresetInfo) -> Self {
        Self {
            id: p.id.to_string(),
            label: p.label.to_string(),
            url_placeholder: p.url_placeholder.to_string(),
            default_template: p.default_template.to_string(),
            content_type: p.content_type.to_string(),
        }
    }
}
