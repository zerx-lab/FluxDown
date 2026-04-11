import 'dart:convert';

/// 图标标识 — 映射到 LucideIcons
enum CategoryIcon {
  folders,
  film,
  music,
  fileText,
  image,
  archive,
  file,
  code,
  database,
  gamepad,
  globe,
  bookmark,
  box,
  cpu,
  disc,
  font,
  hardDrive,
  library,
  package2,
  pen,
  printer,
  smartphone,
  subtitles,
  type,
  zap,
}

/// 匹配模式
enum MatchMode {
  /// 按扩展名匹配（逗号分隔，如 "psd, ai, sketch"）
  extension,

  /// 按正则表达式匹配文件名
  regex,
}

/// 文件分类 — 内置或用户自定义
class CustomCategory {
  final String id;
  final String name;
  final CategoryIcon icon;
  final MatchMode matchMode;

  /// 扩展名列表（MatchMode.extension 时有效），不含点号，已小写化
  final List<String> extensions;

  /// 正则表达式（MatchMode.regex 时有效）
  final String regexPattern;

  /// 排序位置（越小越靠前）
  final int position;

  /// 是否在侧边栏显示
  final bool visible;

  /// 是否为内置分类（不可删除，可编辑/隐藏）
  final bool isBuiltin;

  /// 内置分类类型标识：'all', 'video', 'audio', 'document', 'image', 'archive', 'other'
  /// 自定义分类此值为 null
  final String? builtinType;

  /// 绑定的保存目录，空字符串表示使用全局默认目录
  final String saveDir;

  const CustomCategory({
    required this.id,
    required this.name,
    this.icon = CategoryIcon.file,
    this.matchMode = MatchMode.extension,
    this.extensions = const [],
    this.regexPattern = '',
    this.position = 0,
    this.visible = true,
    this.isBuiltin = false,
    this.builtinType,
    this.saveDir = '',
  });

  /// 检测文件名是否匹配此分类
  bool matches(String fileName) {
    // 'all' 匹配所有文件
    if (builtinType == 'all') return true;
    // 'other' 不在此处处理 — 由调用方计算排除逻辑
    if (builtinType == 'other') return false;

    switch (matchMode) {
      case MatchMode.extension:
        final dot = fileName.lastIndexOf('.');
        if (dot < 0 || dot == fileName.length - 1) return false;
        final ext = fileName.substring(dot + 1).toLowerCase();
        return extensions.contains(ext);
      case MatchMode.regex:
        if (regexPattern.isEmpty) return false;
        try {
          return RegExp(regexPattern, caseSensitive: false).hasMatch(fileName);
        } catch (_) {
          return false;
        }
    }
  }

  CustomCategory copyWith({
    String? id,
    String? name,
    CategoryIcon? icon,
    MatchMode? matchMode,
    List<String>? extensions,
    String? regexPattern,
    int? position,
    bool? visible,
    bool? isBuiltin,
    String? builtinType,
    String? saveDir,
  }) {
    return CustomCategory(
      id: id ?? this.id,
      name: name ?? this.name,
      icon: icon ?? this.icon,
      matchMode: matchMode ?? this.matchMode,
      extensions: extensions ?? this.extensions,
      regexPattern: regexPattern ?? this.regexPattern,
      position: position ?? this.position,
      visible: visible ?? this.visible,
      isBuiltin: isBuiltin ?? this.isBuiltin,
      builtinType: builtinType ?? this.builtinType,
      saveDir: saveDir ?? this.saveDir,
    );
  }

  Map<String, dynamic> toJson() => {
    'id': id,
    'name': name,
    'icon': icon.name,
    'matchMode': matchMode.name,
    'extensions': extensions,
    'regexPattern': regexPattern,
    'position': position,
    'visible': visible,
    'isBuiltin': isBuiltin,
    'builtinType': builtinType,
    'saveDir': saveDir,
  };

  factory CustomCategory.fromJson(Map<String, dynamic> json) {
    return CustomCategory(
      id: json['id'] as String,
      name: json['name'] as String,
      icon: CategoryIcon.values.firstWhere(
        (e) => e.name == json['icon'],
        orElse: () => CategoryIcon.file,
      ),
      matchMode: MatchMode.values.firstWhere(
        (e) => e.name == json['matchMode'],
        orElse: () => MatchMode.extension,
      ),
      extensions: (json['extensions'] as List<dynamic>?)
              ?.map((e) => e.toString().toLowerCase())
              .toList() ??
          const [],
      regexPattern: json['regexPattern'] as String? ?? '',
      position: json['position'] as int? ?? 0,
      visible: json['visible'] as bool? ?? true,
      isBuiltin: json['isBuiltin'] as bool? ?? false,
      builtinType: json['builtinType'] as String?,
      saveDir: json['saveDir'] as String? ?? '',
    );
  }

  /// 序列化列表为 JSON 字符串
  static String encodeList(List<CustomCategory> list) {
    return jsonEncode(list.map((c) => c.toJson()).toList());
  }

  /// 从 JSON 字符串反序列化列表
  static List<CustomCategory> decodeList(String json) {
    if (json.isEmpty) return [];
    try {
      final list = jsonDecode(json) as List<dynamic>;
      return list
          .map((e) => CustomCategory.fromJson(e as Map<String, dynamic>))
          .toList();
    } catch (_) {
      return [];
    }
  }

  /// 生成默认内置分类列表（名称使用英文 key，sidebar 渲染时再通过 i18n 映射）
  static List<CustomCategory> defaultCategories() => [
    const CustomCategory(
      id: '_all',
      name: 'all',
      icon: CategoryIcon.folders,
      matchMode: MatchMode.extension,
      extensions: [],
      position: 0,
      isBuiltin: true,
      builtinType: 'all',
    ),
    const CustomCategory(
      id: '_video',
      name: 'video',
      icon: CategoryIcon.film,
      matchMode: MatchMode.extension,
      extensions: [
        'mp4', 'mkv', 'avi', 'mov', 'wmv', 'flv', 'webm', 'ts',
        'm4v', 'rmvb', 'rm', '3gp', 'vob', 'mpg', 'mpeg',
      ],
      position: 1,
      isBuiltin: true,
      builtinType: 'video',
    ),
    const CustomCategory(
      id: '_audio',
      name: 'audio',
      icon: CategoryIcon.music,
      matchMode: MatchMode.extension,
      extensions: [
        'mp3', 'flac', 'wav', 'aac', 'ogg', 'wma', 'm4a', 'opus', 'ape', 'aiff',
      ],
      position: 2,
      isBuiltin: true,
      builtinType: 'audio',
    ),
    const CustomCategory(
      id: '_document',
      name: 'document',
      icon: CategoryIcon.fileText,
      matchMode: MatchMode.extension,
      extensions: [
        'pdf', 'doc', 'docx', 'xls', 'xlsx', 'ppt', 'pptx', 'txt',
        'csv', 'rtf', 'epub', 'mobi', 'md', 'odt', 'ods', 'odp',
      ],
      position: 3,
      isBuiltin: true,
      builtinType: 'document',
    ),
    const CustomCategory(
      id: '_image',
      name: 'image',
      icon: CategoryIcon.image,
      matchMode: MatchMode.extension,
      extensions: [
        'jpg', 'jpeg', 'png', 'gif', 'bmp', 'webp', 'svg', 'ico',
        'tiff', 'tif', 'psd', 'raw', 'heic', 'avif',
      ],
      position: 4,
      isBuiltin: true,
      builtinType: 'image',
    ),
    const CustomCategory(
      id: '_archive',
      name: 'archive',
      icon: CategoryIcon.archive,
      matchMode: MatchMode.extension,
      extensions: [
        'zip', 'rar', '7z', 'tar', 'gz', 'bz2', 'xz', 'zst',
        'iso', 'dmg', 'cab', 'lz', 'lzma',
      ],
      position: 5,
      isBuiltin: true,
      builtinType: 'archive',
    ),
    const CustomCategory(
      id: '_other',
      name: 'other',
      icon: CategoryIcon.file,
      matchMode: MatchMode.extension,
      extensions: [],
      position: 100, // always last among built-in
      isBuiltin: true,
      builtinType: 'other',
    ),
  ];
}
