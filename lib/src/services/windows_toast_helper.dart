import 'dart:convert';
import 'dart:io';

import 'package:shared_preferences/shared_preferences.dart';

import 'log_service.dart';

const _tag = 'WinToast';

/// SharedPreferences key：存储已验证的快捷方式 AUMID，避免每次启动重复跑 PowerShell。
/// 值格式：已成功创建并验证的 aumid 字符串；空字符串或不存在表示需要重新检查。
const _kVerifiedAumid = 'win_toast_shortcut_verified_aumid';

/// 便携版标记文件名，与 Rust 侧 PORTABLE_MARKER 保持一致。
const _portableMarker = 'portable';

/// 检测当前是否为便携版（可执行文件同目录存在 `portable` 文件）。
bool _isPortableMode() {
  try {
    final exeDir = File(Platform.resolvedExecutable).parent.path;
    return File(
      '$exeDir${Platform.pathSeparator}$_portableMarker',
    ).existsSync();
  } catch (_) {
    return false;
  }
}

/// Windows 10 Toast notification shortcut helper.
///
/// On Windows 10, unpackaged (non-MSIX) desktop apps require a Start Menu
/// shortcut (.lnk) with `System.AppUserModel.ID` property set to display
/// Toast notifications. Without this shortcut, the Toast API returns success
/// but the notification is silently discarded by the OS.
///
/// ## Implementation
///
/// Uses **IShellLink** COM object to create the shortcut and set AUMID via
/// its **IPropertyStore** interface, then saves via **IPersistFile**.
///
/// **Critical**: `SHGetPropertyStoreFromParsingName` CANNOT write properties
/// to `.lnk` files (returns S_FALSE on SetValue). The IShellLink COM chain
/// is the only correct approach.
///
/// **Critical**: `PROPVARIANT` must be a `sealed class` (reference type,
/// 24 bytes on x64), NOT a `struct` — using a struct causes silent COM
/// failures due to incorrect memory layout.
///
/// Windows 11 does NOT require this shortcut — COM activation alone suffices.
/// However, having the shortcut is harmless on Win11, so we always create it.
///
/// ## 便携版行为
///
/// 便携版（可执行文件同目录存在 `portable` 文件）不需要开始菜单快捷方式，
/// 直接跳过整个流程。
///
/// ## 缓存机制
///
/// 首次成功创建/验证快捷方式后，将 AUMID 写入 SharedPreferences。
/// 后续启动若缓存命中且快捷方式文件仍存在，则跳过 PowerShell 进程，
/// 避免每次启动的额外开销。

/// Ensures a Start Menu shortcut with a valid AUMID exists for Toast
/// notifications.
///
/// 幂等：快捷方式存在且 AUMID 正确时跳过创建。
/// 便携版直接跳过，不在开始菜单创建任何文件。
Future<void> ensureWindowsToastShortcut({
  required String appName,
  required String aumid,
  required String clsid,
}) async {
  if (!Platform.isWindows) return;

  // 便携版不需要开始菜单快捷方式，直接跳过
  if (_isPortableMode()) {
    logInfo(_tag, 'portable mode detected, skipping shortcut creation');
    return;
  }

  final appData = Platform.environment['APPDATA'];
  if (appData == null) {
    logInfo(_tag, 'APPDATA not set, skipping');
    return;
  }

  final lnkPath =
      '$appData\\Microsoft\\Windows\\Start Menu\\Programs\\$appName.lnk';

  // 快速路径：检查 SharedPreferences 缓存，若已验证且文件仍存在则跳过 PowerShell
  try {
    final prefs = await SharedPreferences.getInstance();
    final cachedAumid = prefs.getString(_kVerifiedAumid) ?? '';
    if (cachedAumid == aumid && File(lnkPath).existsSync()) {
      logInfo(_tag, 'shortcut cache hit, skipping PowerShell check');
      return;
    }
    // 缓存失效（AUMID 变更或文件被删除），清除缓存重新验证
    if (cachedAumid.isNotEmpty) {
      await prefs.remove(_kVerifiedAumid);
      logInfo(
        _tag,
        'shortcut cache invalidated (cached=$cachedAumid, expected=$aumid)',
      );
    }
  } catch (e) {
    logInfo(_tag, 'prefs read failed, proceeding with full check: $e');
  }

  // 快捷方式存在时，通过 PowerShell 读取 AUMID 进行验证
  if (File(lnkPath).existsSync()) {
    final existingAumid = await _readAumid(lnkPath);
    if (existingAumid == aumid) {
      logInfo(_tag, 'shortcut valid (aumid=$existingAumid): $lnkPath');
      // 写入缓存，下次启动跳过 PowerShell
      await _cacheVerifiedAumid(aumid);
      return;
    }
    logInfo(
      _tag,
      'shortcut exists but aumid mismatch '
      '(expected=$aumid, got=$existingAumid), recreating...',
    );
    // 删除 AUMID 不匹配的旧快捷方式
    try {
      File(lnkPath).deleteSync();
    } catch (e) {
      logError(_tag, 'failed to delete old shortcut', e);
    }
  }

  logInfo(_tag, 'creating Start Menu shortcut for Toast notifications...');

  final exePath = Platform.resolvedExecutable;
  final workDir = File(exePath).parent.path;

  final script = _buildScript(
    lnkPath: lnkPath,
    targetPath: exePath,
    workingDir: workDir,
    appName: appName,
    aumid: aumid,
    clsid: clsid,
  );

  final tempFile = File(
    '${Directory.systemTemp.path}\\fluxdown_toast_setup.ps1',
  );

  try {
    // 以 UTF-8 BOM 写入，PowerShell 默认按 BOM 识别编码，避免中文路径乱码
    await tempFile.writeAsBytes([
      ...utf8.encode('\uFEFF'),
      ...utf8.encode(script),
    ]);
    final result = await Process.run(
      'powershell',
      [
        '-ExecutionPolicy',
        'Bypass',
        '-NoProfile',
        '-NonInteractive',
        '-File',
        tempFile.path,
      ],
      stdoutEncoding: utf8,
      stderrEncoding: utf8,
    );

    final stdout = (result.stdout as String).trim();
    final stderr = (result.stderr as String).trim();

    if (result.exitCode == 0 && stdout.contains('SUCCESS')) {
      logInfo(_tag, 'shortcut created: $lnkPath ($stdout)');
      // 创建成功，写入缓存
      await _cacheVerifiedAumid(aumid);
    } else {
      logError(
        _tag,
        'script failed (exit=${result.exitCode}): '
        'stdout=$stdout, stderr=$stderr',
      );
    }
  } catch (e, stack) {
    logError(_tag, 'shortcut creation error', e, stack);
  } finally {
    try {
      if (tempFile.existsSync()) await tempFile.delete();
    } catch (_) {}
  }
}

/// 将已验证的 AUMID 写入 SharedPreferences 缓存。
Future<void> _cacheVerifiedAumid(String aumid) async {
  try {
    final prefs = await SharedPreferences.getInstance();
    await prefs.setString(_kVerifiedAumid, aumid);
    logInfo(_tag, 'shortcut verified aumid cached');
  } catch (e) {
    logInfo(_tag, 'failed to cache verified aumid: $e');
  }
}

/// Reads the AUMID from an existing .lnk shortcut via IShellLink COM chain.
/// Returns the AUMID string, or empty string if not set / on error.
Future<String> _readAumid(String lnkPath) async {
  final script = _buildReadAumidScript(lnkPath);
  final tempFile = File(
    '${Directory.systemTemp.path}\\fluxdown_read_aumid.ps1',
  );

  try {
    // 以 UTF-8 BOM 写入，PowerShell 默认按 BOM 识别编码，避免中文路径乱码
    await tempFile.writeAsBytes([
      ...utf8.encode('\uFEFF'),
      ...utf8.encode(script),
    ]);
    final result = await Process.run(
      'powershell',
      [
        '-ExecutionPolicy',
        'Bypass',
        '-NoProfile',
        '-NonInteractive',
        '-File',
        tempFile.path,
      ],
      stdoutEncoding: utf8,
      stderrEncoding: utf8,
    );

    if (result.exitCode == 0) {
      final output = (result.stdout as String).trim();
      // Output format: "AUMID=Com.FluxDown.App" or "AUMID="
      const prefix = 'AUMID=';
      if (output.startsWith(prefix)) {
        return output.substring(prefix.length);
      }
    }
    return '';
  } catch (e) {
    logError(_tag, 'readAumid error', e);
    return '';
  } finally {
    try {
      if (tempFile.existsSync()) await tempFile.delete();
    } catch (_) {}
  }
}

/// Builds a PowerShell script to read AUMID from an existing .lnk via
/// IShellLink → IPropertyStore.
String _buildReadAumidScript(String lnkPath) {
  String q(String s) => s.replaceAll("'", "''");

  return '''
\$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;

public class AumidReader {
    [ComImport, Guid("00021401-0000-0000-C000-000000000046")]
    class ShellLink { }

    [ComImport, Guid("000214F9-0000-0000-C000-000000000046")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IShellLinkW {
        void GetPath(IntPtr f, int c, IntPtr d, uint g);
        void GetIDList(out IntPtr p);
        void SetIDList(IntPtr p);
        void GetDescription(IntPtr n, int c);
        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string n);
        void GetWorkingDirectory(IntPtr d, int c);
        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string d);
        void GetArguments(IntPtr a, int c);
        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string a);
        void GetHotkey(out ushort k);
        void SetHotkey(ushort k);
        void GetShowCmd(out int s);
        void SetShowCmd(int s);
        void GetIconLocation(IntPtr p, int c, out int i);
        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string p, int i);
        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string p, uint r);
        void Resolve(IntPtr h, uint f);
        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string p);
    }

    [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IPropertyStore {
        [PreserveSig] int GetCount(out uint c);
        [PreserveSig] int GetAt(uint i, out PropKey k);
        [PreserveSig] int GetValue(ref PropKey k, [In, Out] PropVariant v);
        [PreserveSig] int SetValue(ref PropKey k, PropVariant v);
        [PreserveSig] int Commit();
    }

    [StructLayout(LayoutKind.Sequential, Pack = 4)]
    public struct PropKey {
        public Guid fmtid;
        public uint pid;
    }

    [StructLayout(LayoutKind.Sequential)]
    public sealed class PropVariant {
        public ushort vt;
        public ushort wReserved1;
        public ushort wReserved2;
        public ushort wReserved3;
        public IntPtr ptr;
        public IntPtr ptr2;

        public string AsString() {
            if (vt == 31 && ptr != IntPtr.Zero)
                return Marshal.PtrToStringUni(ptr);
            return "";
        }
    }

    public static string Read(string lnkPath) {
        try {
            var link = (IShellLinkW)new ShellLink();
            var persist = (IPersistFile)link;
            persist.Load(lnkPath, 0);

            var store = (IPropertyStore)link;
            var pk = new PropKey {
                fmtid = new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"),
                pid = 5
            };
            var pv = new PropVariant();
            int hr = store.GetValue(ref pk, pv);
            if (hr != 0) return "";
            return pv.AsString();
        } catch {
            return "";
        }
    }
}
"@

Write-Host ("AUMID=" + [AumidReader]::Read('${q(lnkPath)}'))
''';
}

/// Builds the PowerShell script that creates a .lnk shortcut with AUMID
/// via the IShellLink → IPropertyStore → IPersistFile COM chain.
///
/// This is the ONLY correct approach for setting AUMID on .lnk files.
/// `SHGetPropertyStoreFromParsingName` cannot write to .lnk files.
String _buildScript({
  required String lnkPath,
  required String targetPath,
  required String workingDir,
  required String appName,
  required String aumid,
  required String clsid,
}) {
  String q(String s) => s.replaceAll("'", "''");

  return '''
\$ErrorActionPreference = 'Stop'

Add-Type -TypeDefinition @"
using System;
using System.Runtime.InteropServices;
using System.Runtime.InteropServices.ComTypes;

public class ShellLinkCreator {
    [ComImport, Guid("00021401-0000-0000-C000-000000000046")]
    class ShellLink { }

    [ComImport, Guid("000214F9-0000-0000-C000-000000000046")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IShellLinkW {
        void GetPath(IntPtr pszFile, int cch, IntPtr pfd, uint fFlags);
        void GetIDList(out IntPtr ppidl);
        void SetIDList(IntPtr pidl);
        void GetDescription(IntPtr pszName, int cch);
        void SetDescription([MarshalAs(UnmanagedType.LPWStr)] string pszName);
        void GetWorkingDirectory(IntPtr pszDir, int cch);
        void SetWorkingDirectory([MarshalAs(UnmanagedType.LPWStr)] string pszDir);
        void GetArguments(IntPtr pszArgs, int cch);
        void SetArguments([MarshalAs(UnmanagedType.LPWStr)] string pszArgs);
        void GetHotkey(out ushort pwHotkey);
        void SetHotkey(ushort wHotkey);
        void GetShowCmd(out int piShowCmd);
        void SetShowCmd(int iShowCmd);
        void GetIconLocation(IntPtr pszIconPath, int cch, out int piIcon);
        void SetIconLocation([MarshalAs(UnmanagedType.LPWStr)] string pszIconPath, int iIcon);
        void SetRelativePath([MarshalAs(UnmanagedType.LPWStr)] string pszPathRel, uint dwReserved);
        void Resolve(IntPtr hwnd, uint fFlags);
        void SetPath([MarshalAs(UnmanagedType.LPWStr)] string pszFile);
    }

    [ComImport, Guid("886D8EEB-8CF2-4446-8D02-CDBA1DBDCF99")]
    [InterfaceType(ComInterfaceType.InterfaceIsIUnknown)]
    interface IPropertyStore {
        [PreserveSig] int GetCount(out uint c);
        [PreserveSig] int GetAt(uint i, out PropKey k);
        [PreserveSig] int GetValue(ref PropKey k, [In, Out] PropVariant v);
        [PreserveSig] int SetValue(ref PropKey k, PropVariant v);
        [PreserveSig] int Commit();
    }

    [StructLayout(LayoutKind.Sequential, Pack = 4)]
    public struct PropKey {
        public Guid fmtid;
        public uint pid;
    }

    // PROPVARIANT: sealed class (reference type) for correct 24-byte x64
    // layout. Using a struct (value type) causes 16-byte layout with
    // LayoutKind.Explicit, leading to silent SetValue failures.
    [StructLayout(LayoutKind.Sequential)]
    public sealed class PropVariant : IDisposable {
        public ushort vt;
        public ushort wReserved1;
        public ushort wReserved2;
        public ushort wReserved3;
        public IntPtr ptr;
        public IntPtr ptr2;

        public void Dispose() {
            if (ptr != IntPtr.Zero) {
                Marshal.FreeCoTaskMem(ptr);
                ptr = IntPtr.Zero;
            }
        }

        public static PropVariant FromString(string s) {
            var pv = new PropVariant();
            pv.vt = 31; // VT_LPWSTR
            pv.ptr = Marshal.StringToCoTaskMemUni(s);
            return pv;
        }

        public static PropVariant FromClsid(string guid) {
            var pv = new PropVariant();
            pv.vt = 72; // VT_CLSID
            pv.ptr = Marshal.AllocCoTaskMem(16);
            Marshal.Copy(new Guid(guid).ToByteArray(), 0, pv.ptr, 16);
            return pv;
        }

        public string AsString() {
            if (vt == 31 && ptr != IntPtr.Zero)
                return Marshal.PtrToStringUni(ptr);
            return "(vt=" + vt + ")";
        }
    }

    public static string CreateAndVerify(
        string lnkPath, string target, string workDir, string desc,
        string aumid, string clsid
    ) {
        try {
            // 1. Create IShellLink and set basic properties
            var link = (IShellLinkW)new ShellLink();
            link.SetPath(target);
            link.SetWorkingDirectory(workDir);
            link.SetDescription(desc);

            // 2. Set AUMID via IPropertyStore on the ShellLink object
            var store = (IPropertyStore)link;

            var pkAumid = new PropKey {
                fmtid = new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"),
                pid = 5  // System.AppUserModel.ID
            };
            using (var pv = PropVariant.FromString(aumid)) {
                int hr = store.SetValue(ref pkAumid, pv);
                if (hr != 0)
                    return "FAIL SetValue(AUMID) hr=0x" + hr.ToString("X8");
            }

            // 3. Set Toast Activator CLSID
            if (!string.IsNullOrEmpty(clsid)) {
                var pkClsid = new PropKey {
                    fmtid = new Guid("9F4C2855-9F79-4B39-A8D0-E1D42DE1D5F3"),
                    pid = 26  // System.AppUserModel.ToastActivatorCLSID
                };
                using (var pv2 = PropVariant.FromClsid(clsid)) {
                    int hr2 = store.SetValue(ref pkClsid, pv2);
                    if (hr2 != 0)
                        return "FAIL SetValue(CLSID) hr=0x" + hr2.ToString("X8");
                }
            }

            // 4. Commit property changes
            int hr3 = store.Commit();
            if (hr3 != 0) return "FAIL Commit hr=0x" + hr3.ToString("X8");

            // 5. Save the .lnk file via IPersistFile
            var persist = (IPersistFile)link;
            persist.Save(lnkPath, true);

            // 6. Verify: reopen and read AUMID back
            var link2 = (IShellLinkW)new ShellLink();
            var persist2 = (IPersistFile)link2;
            persist2.Load(lnkPath, 0);

            var store2 = (IPropertyStore)link2;
            var readPv = new PropVariant();
            int hr4 = store2.GetValue(ref pkAumid, readPv);
            if (hr4 != 0)
                return "FAIL Verify GetValue hr=0x" + hr4.ToString("X8");

            string readBack = readPv.AsString();
            if (readBack == aumid)
                return "SUCCESS aumid=" + readBack;
            else
                return "FAIL verify mismatch expected=" + aumid
                    + " got=" + readBack;
        } catch (Exception ex) {
            return "EXCEPTION: " + ex.GetType().Name + ": " + ex.Message;
        }
    }
}
"@

\$result = [ShellLinkCreator]::CreateAndVerify(
    '${q(lnkPath)}',
    '${q(targetPath)}',
    '${q(workingDir)}',
    '${q(appName)}',
    '${q(aumid)}',
    '${q(clsid)}'
)
Write-Host \$result
''';
}
