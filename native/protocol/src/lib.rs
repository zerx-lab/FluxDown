//! FluxDown 本机服务与客户端共享的传输无关协议类型。
//!
//! 本 crate 只定义 wire 契约；不得依赖下载引擎、网络运行时、数据库或 UI。

pub mod agent;
pub mod app_icon;
pub mod capture_link;
pub mod cloud_notify;
pub mod coalesce;
pub mod daemon;
pub mod daemon_config;
mod digest;
pub mod error;
pub mod event;
pub mod handshake;
pub mod local_service;
pub mod method;
pub mod plugin_view;
pub mod proxy_rules;
pub mod rpc;
pub mod settings;
pub mod shell_open;
pub mod shortcuts;
pub mod sound;
pub mod table_sync;
pub mod task_activity;
pub mod update_prompt;
pub use task_activity::{
    TaskActivityDto, TaskActivityPage, TaskActivityQuery, TaskRuntimeDto, TaskSegmentDto,
    TaskSourceBytesDto, TaskSpeedHistoryDto, TaskSpeedHistoryQuery,
};

pub use agent::{
    AgentLoginResult, AgentPreferencesDto, AgentPreferencesPatchResult, AgentSessionDto,
    AuthVerificationDto, CUSTOM_CATEGORIES_PREF_KEY, CaptureCreateGroupParams,
    CapturePreviewParams, CaptureResolveParams, CategoryRouteDto, CategoryRuleDto,
    CloudConnectionDto, CloudConnectionState, CloudDevice, CloudEndpointDto,
    CloudEndpointSetParams, CloudOrder, CloudPlan, CloudPlanCampaign, CloudPlanCampaignStage,
    CloudProfile, CloudReferralCode, CloudReferralCodesResult, CloudReferralRecord,
    CloudReferralRecordsResult, CloudReferralRule, CloudReferralSummary,
    CloudReferralValidateResult, CloudSubscription, CloudUser, CloudUserStatus, CustomCategoryDto,
    DiagnosticCheckDto, DiagnosticLevel, DiagnosticRepairParams, DiagnosticsReportDto,
    Entitlements, FILE_ICON_PER_FILE_EXTENSIONS, GatewayPatchParams, GatewayStatusDto,
    LinkAddressParams, LinkApproveParams, LinkControlCredentialDto, LinkDeviceParams,
    LinkDiscoveryParams, LinkDispatchParams, LinkDispatchResult, LinkPairBeginParams,
    LinkPairFinishParams, LinkPairingCodeDto, LinkPairingRequestDto, LinkRenameParams,
    LinkStopPairingParams, LogExportParams, LogExportResult, LogPathsDto, OriginIdCheckResult,
    PLATFORM_COPY_TASK_FILES_MAX, PathStyle, PendingCaptureDto, PlatformCopyTaskFilesParams,
    PlatformFileIconDto, PlatformFileIconParams, PlatformIntegrationDto, PlatformOpenPathParams,
    PlatformToggleParams, PlatformUrlProtocolParams, PluginFavoriteResultDto,
    PluginFavoriteSetRequest, PluginFavoritesDto, PowerAction, PowerArmParams, PowerStatusDto,
    PrivatePluginInstallRequest, PrivatePluginListDto, ReleaseNoteDto, RemoteCommandAction,
    RemoteCommandParams, RemoteDispatchParams, RemoteDispatchResult, RemoteTaskDto,
    RemoteTaskStatus, ShellStatusDto, SyncLocalOnlyParams, SyncStatusDto, TrayUnavailableReason,
    UpdateCheckParams, UpdateFailure, UpdateInstallKind, UpdateManualReason, UpdatePhase,
    UpdateStatusDto,
};
pub use cloud_notify::{
    CLOUD_NOTIFY_EMAIL_MAX_ADDRESSES, CLOUD_NOTIFY_RECENT_DELIVERIES,
    CloudNotifyChannelCreateParams, CloudNotifyChannelDto, CloudNotifyChannelIdParams,
    CloudNotifyChannelUpdateParams, CloudNotifyDeliveriesPage, CloudNotifyDeliveriesParams,
    CloudNotifyDeliveryDto, CloudNotifyEmailCodeParams, CloudNotifyEmailCodeResult,
    CloudNotifyEmailVerifyParams, CloudNotifyKindDto, CloudNotifyOverviewDto,
    CloudNotifyPrivacyParams, CloudNotifyReportingParams, CloudNotifyStateDto,
    CloudNotifyTelegramBindDto, CloudNotifyTelegramBindStatusDto,
    CloudNotifyTelegramBindStatusParams, CloudNotifyTestResult, CloudNotifyUsageDto, TaskNoticeDto,
    TaskNoticeTaskDto,
};
pub use daemon::{
    ApiInfo, BT_GUARD_ENTRY_ACTIONS, BT_GUARD_LAYERS, BT_GUARD_LIST_DEFAULT_LIMIT,
    BT_GUARD_LIST_MAX_LIMIT, BT_GUARD_PEER_ACTION_ALLOW, BT_GUARD_REMOVE_MAX_IDS, BT_GUARD_RULES,
    BtFileDto, BtGuardClientRuleDto, BtGuardCloudApplyDto, BtGuardCloudApplyParams,
    BtGuardCloudStatusDto, BtGuardCountsDto, BtGuardEntriesAddParams, BtGuardEntriesListDto,
    BtGuardEntriesListParams, BtGuardEntriesRemoveDto, BtGuardEntriesRemoveParams, BtGuardEntryDto,
    BtGuardEventDto, BtGuardEventsListDto, BtGuardEventsListParams, BtGuardEvidenceDrainDto,
    BtGuardEvidenceItemDto, BtGuardExportDto, BtGuardExportParams, BtGuardFilterSourceDto,
    BtGuardFilterSourcesRefreshDto, BtGuardStateDto, BtGuardStatsDto, BtPeerCountsDto, BtPeerDto,
    BtStatusDto, BtTrackerDto, BtTrackerSource, BtTrackerStatus, CREATE_TASKS_BATCH_ID_MAX_LEN,
    CREATE_TASKS_MAX_ITEMS, CdnConfigApplyParams, CdnNodeDto, CdnReportAckParams,
    CdnReportLeaseDto, ChangeTaskUrlParams, ComponentFfmpegStatus, ComponentInstallParams,
    ComponentKind, ComponentParams, ComponentProbeDto, ComponentRepairParams, ComponentStatusDto,
    ComponentVersions, ComponentYtdlpStatus, ConnPolicySummaryDto, CreateGroupRequest,
    CreateGroupResponse, CreateQueueRequest, CreateTaskItem, CreateTaskOutcome, CreateTaskRequest,
    CreatedTask, DaemonBtStatusParams, DaemonConfigPatch, DaemonConfigSnapshot,
    DaemonCreateTaskParams, DaemonCreateTasksParams, DaemonCreateTasksResult,
    DaemonDeleteTasksParams, DaemonEd2kStatusParams, DaemonRuntimeStatsDto, DaemonSeedLocalParams,
    DaemonTaskHashParams, DaemonTaskIdsParams, DaemonTorrentShareParams, DiagnosticsProbeParams,
    DiagnosticsProbeResult, DownloadRequest, Ed2kIdCheckParams, Ed2kIdCheckTarget, Ed2kIdKind,
    Ed2kKadBootstrapDto, Ed2kKadBootstrapParams, Ed2kKadBootstrapSource, Ed2kKadDto,
    Ed2kKadNodeDto, Ed2kKadNodeOrigin, Ed2kKadNodeState, Ed2kKadPhase, Ed2kNetworkDto,
    Ed2kNodesSubRefreshResponse, Ed2kPeerCountsDto, Ed2kPeerDto, Ed2kPeerOrigin, Ed2kPeerState,
    Ed2kServerDto, Ed2kServerSource, Ed2kServerStatus, Ed2kServerSubRefreshResponse, Ed2kStatusDto,
    Ed2kTaskPhase, Ed2kUpnpStatus, EgressPlanDto, EgressPlanParams, EgressProxyDto,
    FileExistsAction, FileMissingUpdateDto, FsEntry, FsListResponse, GatewayMigrationExport,
    GroupDto, GroupItemRequest, HlsQualityOptionDto, InstallFfmpegRequest, InstallPluginDevRequest,
    InstalledPlugin, LATER_QUEUE_ID, LinkAuth, LinkCodeResponse, LinkDeviceInfo,
    LinkDeviceTaskRequest, LinkDevicesResponse, LinkDiscoveredPeer, LinkDiscoveredResponse,
    LinkDiscoveryRequest, LinkMigrationExport, LinkOkResponse, LinkPairApproveRequest,
    LinkPairBeginRequest, LinkPairBeginResponse, LinkPairConfirmOutcome, LinkPairConfirmRequest,
    LinkPairFinishRequest, LinkPairFinishResponse, LinkPairHelloRequest, LinkPairHelloResponse,
    LinkPairRevealRequest, LinkPairRevealResponse, LinkPingInfo, LinkProbeParams, LinkProbeRequest,
    LinkProbeResult, LinkProbeVerdict, LinkTaskRequest, LogFileDto, LogsResponse, MAIN_QUEUE_ID,
    MarketEntryDto, MarketInstallRequest, MigrationAckParams, MoveQueueRequest,
    PluginArchiveRulesDto, PluginArchiveRulesParams, PluginAuditClearParams, PluginAuditEntryDto,
    PluginAuditListDto, PluginAuditListParams, PluginAuditRecordEntry, PluginAuditRecordParams,
    PluginAuthRequest, PluginAuthResponse, PluginCandidateDto, PluginCommandDeclDto,
    PluginConsentParams, PluginContributionsDto, PluginDesktopEntryDto, PluginDto,
    PluginInstallPrivateParams, PluginPageScriptDeclDto, PluginPageScriptDto,
    PluginPageScriptSourceDto, PluginPageScriptSourceParams, PluginPageScriptsDto,
    PluginRevokePrivateParams, PluginSetSiteDefaultParams, PluginSiteDefaultDto,
    PluginSniffRulesDto, PluginViewDeclDto, PreviewItemDto, PreviewVariantDto,
    PrivateRevocationDto, ProxyRouteMatchDto, ProxyRouteRequest, ProxyRouteResponse,
    ProxyRuleIssueDto, ProxyTestRequest, ProxyTestResponse, QueueDto, QueueOrderEntryDto,
    QueuePositionDto, QueueScheduleRequest, RenameTaskRequest, ReorderQueueRequest, RequestBody,
    ResolvePreviewRequest, ResolvePreviewResponse, ResolveVariantOptionDto, ResultMessage,
    RssItemActionRequest, RssItemDto, RssSourceDto, RssValidateRequest, RssValidateResponse,
    SELECTION_NO_DEADLINE, SeedLocalResultDto, SegmentDetailDto, SelectionKind, SelectionOutcome,
    SelectionRequestDto, SelectionResolutionDto, SetPluginEnabledRequest,
    SetTaskPiecePickingParams, SetTaskSegmentsParams, SettingFieldDto, SettingOptionDto,
    SetupRequest, SetupStatusResponse, SiteAuthCredentialDto, SiteAuthDeleteParams,
    SiteAuthEntryDto, SiteAuthGetParams, SiteAuthMatchParams, SiteAuthSaveRequest, StatsResponse,
    StorageProbeDto, StorageProbeFailure, StorageProbeRole, StorageProbeTarget, SystemProxyDto,
    TaskDto, TaskHashResult, TaskVerifyResult, TaskVerifyStatus, TokenResponse, TorrentShareDto,
    TrackerSubRefreshResponse, UpdateQueueRequest, WebhookDeliveriesResponse, WebhookDeliveryDto,
    WebhookPresetDto, WebhookSimulateResponse, WebhookTestRequest, WebhookTestResponse,
    WsClientMsg, WsServerMsg,
};
pub use daemon::{
    BT_FILE_PRIORITY_HIGH, BT_FILE_PRIORITY_MAXIMUM, BT_FILE_PRIORITY_NORMAL,
    BT_FILE_PRIORITY_SKIP, BtFilePriorityDto, BtFilesDto, BtPortMappingDto, BtPortStatusDto,
    BtTaskFileDto, BtWebSeedDto, MoveTaskStorageParams, RenameBtFileParams, SetTaskBtLimitsParams,
    SetTaskFilePrioritiesParams, SetTaskSuperSeedingParams,
};
pub use daemon_config::{
    BT_GUARD_DOWNLOADING_ACTIONS, BT_GUARD_MODES, BT_MSE_MODES, BT_PIECE_ORDERS,
    BT_SEED_LIMIT_OPERATORS, BT_SEED_THEN_ACTIONS, BT_SEED_TIME_UNITS, DAEMON_CONFIG_FIELDS,
    DaemonConfigError, DaemonConfigField, DaemonConfigKind, FILE_EXISTS_BEHAVIORS,
    FILE_MISSING_ACTIONS, HASH_ALGORITHMS, HIGH_SEGMENTS_WARN_ABOVE, MAX_TASK_SEGMENTS,
    PROXY_MODES, PROXY_TYPES, SEVERE_SEGMENTS_WARN_ABOVE, daemon_config_default,
    daemon_config_field, is_public_daemon_config_key, normalize_daemon_config_patch,
    normalize_daemon_config_value,
};
pub use error::{
    APPLICATION_ERROR_CODE, ApplicationErrorCode, ErrorReason, INTERNAL_ERROR_CODE,
    INVALID_PARAMS_CODE, INVALID_REQUEST_CODE, METHOD_NOT_FOUND_CODE, PARSE_ERROR_CODE,
    RpcErrorData, RpcErrorObject,
};
pub use event::{
    AgentEvent, AgentSnapshot, DaemonEvent, DaemonSnapshot, EventFrame, FrameVerdict, ServiceEvent,
    Snapshot, SnapshotBody, TABLE_INLINE_LIMIT, WEBHOOK_DELIVERY_LIMIT, accepted_runtime_status,
    apply_agent_event, apply_daemon_event, apply_queue_order, judge_frame,
    merge_webhook_deliveries, runtime_retained,
};
pub use plugin_view::{
    NAV_BUILTIN_TARGETS, PluginCommandParams, PluginInvokeParams, PluginPageInvokeParams,
    PluginPageInvokeResultDto, PluginViewEventParams, PluginViewFrameDto,
    PluginViewKeepaliveParams, PluginViewOpenParams, PluginViewOpenedDto, PluginViewSessionParams,
    PluginViewSnapshotDto, VIEW_CLOSE_REASON_REQUESTED, VIEW_ERROR_PROP, VIEW_ROOT_ID,
    VIEW_SCHEMA_JSON, VIEW_TONES_JSON, ViewEffect, ViewError, ViewEventDto, ViewEventPayloadType,
    ViewEventSpec, ViewItemSpec, ViewKindSpec, ViewLimits, ViewNode, ViewOp, ViewPropFormat,
    ViewPropSpec, ViewPropType, ViewSchema, apply_view_ops, diff_view, normalize_view,
    plugin_route_id, view_schema,
};
pub use rpc::{
    APP_VERSION, CLOSE_REASON_SERVICE_QUIT, CLOSE_REASON_SERVICE_RESTART, ClientHello,
    JSONRPC_VERSION, MAX_INBOUND_RPC_MESSAGE_BYTES, MIN_PROTOCOL_VERSION, PROTOCOL_VERSION,
    RequestId, RpcFailureResponse, RpcIncoming, RpcNotification, RpcRequest, RpcResponse,
    RpcSuccessResponse, ServiceHello, ServiceRole, is_older_app_version, negotiate_protocol,
    validate_first_request,
};
pub use settings::{
    CUSTOM_THEMES_KEY, FILE_ICON_PACK_KEY, MAX_CUSTOM_THEME_ID_LEN, MAX_SYNC_VALUE_BYTES,
    SYNC_SETTING_SPECS, SettingOwner, SettingSpec, SettingValueKind, custom_theme_fits_sync,
    custom_theme_id, custom_theme_key, daemon_config_to_value, is_custom_theme_id,
    is_icon_pack_ref, setting_spec, setting_value_kind, sync_scope_key, validate_value,
    value_to_daemon_config,
};
pub use table_sync::{
    InvalidPageCursor, LoadedTables, TABLE_SYNC_BACKLOG_LIMIT, TASK_PAGE_DEFAULT_LIMIT,
    TASK_PAGE_MAX_LIMIT, TablePage, TablePageParams, TableSection, TableSync, TableSyncError,
    TaskListParams,
};
