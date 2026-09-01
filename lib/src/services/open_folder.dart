import 'dart:io';

import 'package:flutter/services.dart';

import '../bindings/bindings.dart';

/// 移动端"打开文件"失败原因，供调用端映射为 i18n 提示。
enum OpenFileError {
  /// 文件不存在（已被外部删除/移动）
  notFound,

  /// 没有应用能处理该文件类型（或系统拒绝）
  noHandler,

  /// 其他失败（FileProvider 根未覆盖该路径等）
  failed,
}

/// 移动端打开文件失败异常，携带结构化原因。
class OpenFileException implements Exception {
  final OpenFileError error;
  final String message;

  const OpenFileException(this.error, this.message);

  @override
  String toString() => 'OpenFileException($error): $message';
}

/// 与 MainActivity.kt / AppDelegate.swift 的 `com.fluxdown/storage` 通道对应。
const _storageChannel = MethodChannel('com.fluxdown/storage');

/// 在文件管理器中打开文件所在目录（尽可能选中文件）或目录本身。
///
/// **桌面平台（Windows/macOS/Linux）实现**：
/// 实际实现完全在 Rust 端：见 native/hub/src/reveal_file.rs。
/// Rust 端会按以下顺序决定：
///   1. 用户在设置中配置了自定义命令模板（reveal_file_cmd / open_dir_cmd）
///      → 走模板（cmd /c 或 sh -c），支持任意第三方文件管理器
///   2. 否则走平台默认：
///      Windows: SHOpenFolderAndSelectItems（标准 Shell API）打开父目录并
///      选中文件，失败回退 ShellExecuteW("open") 打开父目录；目录→
///      ShellExecuteW("open")
///      macOS:   open -R 或 open
///      Linux:   D-Bus FileManager1.ShowItems 或 xdg-open
///
/// **移动平台（Android/iOS）**：不支持——移动端没有可靠的"打开文件管理器到
/// 指定目录"标准 Intent/URL（各厂商文件管理器行为参差，iOS 沙箱直接禁止），
/// 移动端 UI 不应展示该入口。此处静默返回，仅作防御。
///
/// [filePath] 可以是文件路径或目录路径——Rust 端会用 fs::metadata 自动判定。
Future<void> openFolder(String filePath) async {
  if (Platform.isAndroid || Platform.isIOS) {
    // 移动端不支持打开文件管理器（入口已在移动端 UI 隐藏）
    return;
  }
  // 桌面端：发送 Rust 信号
  RevealFile(path: filePath).sendSignalToRust();
}

/// 用系统默认程序打开文件。
///
/// **桌面平台（Windows/macOS/Linux）实现**：
/// 交给 Rust 端以**裸路径**经 shell 打开（Windows `explorer.exe` / macOS `open`
/// / Linux `xdg-open`），正确解析扩展名关联，包括 .mp4 等由 UWP/Store 应用处理
/// 的类型。此前用 `launchUrl(Uri.file())` 传 `file://` URL，ShellExecute 无法据此
/// 激活 UWP 关联应用，导致这类文件"点开没反应"。实现见 native/hub/src/reveal_file.rs。
///
/// **移动平台（Android/iOS）实现**：
/// 经 `com.fluxdown/storage` MethodChannel 走原生实现：
/// - **Android**（MainActivity.kt `openFile`）：FileProvider 生成 content:// URI +
///   ACTION_VIEW（targetSdk ≥ 24 禁止 file:// 出应用）；按扩展名解析 MIME 交给
///   默认关联应用，无关联时回退系统选择器（chooser）让用户自选。
/// - **iOS**（AppDelegate.swift `openFile`）：UIDocumentInteractionController
///   弹出系统"打开方式"菜单，由用户选择应用。
///
/// 失败抛 [OpenFileException]（notFound / noHandler / failed），由调用端映射
/// 为 i18n 提示。
Future<void> openFile(String filePath) async {
  if (Platform.isAndroid || Platform.isIOS) {
    try {
      await _storageChannel.invokeMethod<bool>('openFile', {'path': filePath});
    } on PlatformException catch (e) {
      final error = switch (e.code) {
        'not_found' => OpenFileError.notFound,
        'no_handler' => OpenFileError.noHandler,
        _ => OpenFileError.failed,
      };
      throw OpenFileException(error, e.message ?? e.code);
    }
  } else {
    // 桌面端：发送 Rust 信号
    OpenFile(path: filePath).sendSignalToRust();
  }
}
