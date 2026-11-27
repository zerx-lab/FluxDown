//! macOS Core Foundation / Launch Services 安全边界。
//!
//! 只向平台业务层提供 Rust 字符串；Create/Copy 所有权、数组元素借用和 FFI
//! 全部封闭在本模块，不能从外部接管裸指针或混用字符串与数组。
#![cfg(target_os = "macos")]

use std::ffi::{CStr, c_char, c_void};
use std::io;

const CF_ENCODING_UTF8: u32 = 0x0800_0100;
const LS_ROLES_ALL: u32 = 0xFFFF_FFFF;
/// `kUTTagClassFilenameExtension` 的稳定字符串值；用字面量免去读取外部常量的 unsafe。
const UT_TAG_CLASS_FILENAME_EXTENSION: &str = "public.filename-extension";
type CFStringRef = *const c_void;
type CFArrayRef = *const c_void;

// SAFETY: Signatures follow Apple's CFString/CFArray/CFBundle headers: CFIndex is
// pointer-sized signed, CFStringEncoding is u32, and Boolean is u8.
#[link(name = "CoreFoundation", kind = "framework")]
unsafe extern "C" {
    fn CFStringCreateWithBytes(
        alloc: *const c_void,
        bytes: *const u8,
        length: isize,
        encoding: u32,
        external: u8,
    ) -> CFStringRef;
    fn CFStringGetCStringPtr(string: CFStringRef, encoding: u32) -> *const c_char;
    fn CFStringGetCString(
        string: CFStringRef,
        buffer: *mut c_char,
        size: isize,
        encoding: u32,
    ) -> u8;
    fn CFStringGetLength(string: CFStringRef) -> isize;
    fn CFStringGetMaximumSizeForEncoding(length: isize, encoding: u32) -> isize;
    fn CFRelease(cf: *const c_void);
    fn CFArrayGetCount(array: CFArrayRef) -> isize;
    fn CFArrayGetValueAtIndex(array: CFArrayRef, index: isize) -> *const c_void;
    fn CFBundleGetMainBundle() -> *const c_void;
    fn CFBundleGetIdentifier(bundle: *const c_void) -> CFStringRef;
}

// SAFETY: Signatures follow LaunchServices LSInfo.h and UTType.h; roles are u32, OSStatus
// is i32, and UTTypeCreateAllIdentifiersForTag follows the Create rule (+1 CFArray).
#[link(name = "CoreServices", kind = "framework")]
unsafe extern "C" {
    fn LSSetDefaultRoleHandlerForContentType(
        content_type: CFStringRef,
        role: u32,
        handler: CFStringRef,
    ) -> i32;
    fn LSCopyAllRoleHandlersForContentType(content_type: CFStringRef, role: u32) -> CFArrayRef;
    fn LSCopyDefaultHandlerForURLScheme(scheme: CFStringRef) -> CFStringRef;
    fn LSSetDefaultHandlerForURLScheme(scheme: CFStringRef, handler: CFStringRef) -> i32;
    fn LSCopyAllHandlersForURLScheme(scheme: CFStringRef) -> CFArrayRef;
    fn UTTypeCreateAllIdentifiersForTag(
        tag_class: CFStringRef,
        tag: CFStringRef,
        conforming_to: CFStringRef,
    ) -> CFArrayRef;
}

/// 私有 Create/Copy 字符串守卫；不提供任何裸指针接管构造器，也不实现 Send/Sync。
struct OwnedString(CFStringRef);

impl OwnedString {
    /// 拷贝 UTF-8 输入。拒绝 NUL，避免 Launch Services 标识符被截断；不分配临时 CString。
    fn from_str(value: &str) -> io::Result<Self> {
        if value.contains('\0') {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "string contains interior NUL",
            ));
        }
        let length = isize::try_from(value.len())
            .map_err(|_| io::Error::other("CFString input too long"))?;
        // SAFETY: UTF-8 输入在调用期间有效，length 精确覆盖其字节；默认分配器复制输入。
        let raw = unsafe {
            CFStringCreateWithBytes(
                std::ptr::null(),
                value.as_ptr(),
                length,
                CF_ENCODING_UTF8,
                0,
            )
        };
        if raw.is_null() {
            return Err(io::Error::other("CFStringCreateWithBytes failed"));
        }
        Ok(Self(raw))
    }

    /// 返回独立的 Rust 字符串；空 Copy 结果表示没有默认处理程序。
    fn text(&self) -> io::Result<Option<String>> {
        // SAFETY: 非空时 self 独占 Create/Copy 引用；借用期间不释放、不修改字符串。
        unsafe { copy_string(self.0) }
    }
}

impl Drop for OwnedString {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: 该私有类型仅持有字符串 Create/Copy 的 +1 引用，恰好释放一次。
            unsafe { CFRelease(self.0) };
        }
    }
}

/// 仅接收 Launch Services CopyAll / UTType CreateAll 返回的 CFArray<CFString>；与 OwnedString 不可互换。
struct OwnedStringArray(CFArrayRef);

impl OwnedStringArray {
    /// 在数组仍被借用时复制元素，不向外暴露借用指针。
    fn strings(&self) -> io::Result<Vec<String>> {
        if self.0.is_null() {
            return Ok(Vec::new());
        }
        // SAFETY: self 持有非空、不可变的 Launch Services CFArray<CFString>。
        let count = unsafe { CFArrayGetCount(self.0) };
        let capacity =
            usize::try_from(count).map_err(|_| io::Error::other("invalid CFArray length"))?;
        let mut result = Vec::with_capacity(capacity);
        for index in 0..count {
            // SAFETY: index 在数组范围内，返回的 CFString 由仍存活的 self 保持有效。
            let value = unsafe { CFArrayGetValueAtIndex(self.0, index) };
            // SAFETY: Launch Services 保证元素是 CFString；self 在复制完成前不释放或修改数组。
            if let Some(value) = unsafe { copy_string(value) }? {
                result.push(value);
            }
        }
        Ok(result)
    }
}

impl Drop for OwnedStringArray {
    fn drop(&mut self) {
        if !self.0.is_null() {
            // SAFETY: CopyAll 转移的数组 +1 引用仅由该守卫释放，元素由 CFArray 释放。
            unsafe { CFRelease(self.0) };
        }
    }
}

/// 复制 CF 字符串，不允许任何借用跨出本函数。
///
/// # Safety
/// raw 必须为空或为有效的不可变 CFString；调用方必须保持其所有者存活到返回。
/// 字符串必须不含内嵌 NUL（本模块输入已校验，系统 bundle 标识符满足此约束）。
unsafe fn copy_string(raw: CFStringRef) -> io::Result<Option<String>> {
    if raw.is_null() {
        return Ok(None);
    }
    // SAFETY: 调用方保证 raw 在整个复制期间有效且不可变。
    let direct = unsafe { CFStringGetCStringPtr(raw, CF_ENCODING_UTF8) };
    if !direct.is_null() {
        // SAFETY: CF 返回有效 NUL 结尾 UTF-8 缓冲，所有者在本函数返回前存活；不返回借用。
        let bytes = unsafe { CStr::from_ptr(direct) };
        return bytes
            .to_str()
            .map(|text| Some(text.to_owned()))
            .map_err(io::Error::other);
    }
    let mut local = [0_u8; 512];
    // SAFETY: raw 有效；local 提供完整的 512 字节可写缓冲，CF 成功时写入 NUL。
    let copied =
        unsafe { CFStringGetCString(raw, local.as_mut_ptr().cast(), 512, CF_ENCODING_UTF8) };
    if copied != 0 {
        let bytes = CStr::from_bytes_until_nul(&local).map_err(io::Error::other)?;
        return bytes
            .to_str()
            .map(|text| Some(text.to_owned()))
            .map_err(io::Error::other);
    }
    // SAFETY: raw 是调用方保持有效的不可变 CFString。
    let length = unsafe { CFStringGetLength(raw) };
    // SAFETY: length 来自有效 CFString；该纯查询不接收指针。
    let maximum = unsafe { CFStringGetMaximumSizeForEncoding(length, CF_ENCODING_UTF8) };
    let size = maximum
        .checked_add(1)
        .filter(|size| *size > 0)
        .ok_or_else(|| io::Error::other("CFString UTF-8 size overflow"))?;
    let mut buffer = vec![0_u8; size as usize];
    // SAFETY: raw 有效；buffer 精确分配 size 字节，CF 成功时在范围内写入 NUL。
    let copied =
        unsafe { CFStringGetCString(raw, buffer.as_mut_ptr().cast(), size, CF_ENCODING_UTF8) };
    if copied == 0 {
        return Err(io::Error::other("CFStringGetCString failed"));
    }
    let length = CStr::from_bytes_until_nul(&buffer)
        .map_err(io::Error::other)?
        .to_bytes()
        .len();
    buffer.truncate(length);
    String::from_utf8(buffer)
        .map(Some)
        .map_err(io::Error::other)
}

/// 查询 scheme 的默认处理程序；没有默认值时返回 None，输入 NUL 或转换失败返回错误。
pub(super) fn default_scheme_handler(scheme: &str) -> io::Result<Option<String>> {
    let scheme = OwnedString::from_str(scheme)?;
    // SAFETY: scheme 有效；Copy 返回的 +1 字符串引用立即由专用守卫接管。
    let handler = OwnedString(unsafe { LSCopyDefaultHandlerForURLScheme(scheme.0) });
    handler.text()
}

/// 列出 UTI 的候选 bundle id；系统无候选返回空列表，所有 CF 引用在返回前释放。
pub(super) fn content_handlers(content_type: &str) -> io::Result<Vec<String>> {
    let content_type = OwnedString::from_str(content_type)?;
    // SAFETY: content_type 有效；CopyAll 返回 CFArray<CFString> 的 +1 引用。
    let array = OwnedStringArray(unsafe {
        LSCopyAllRoleHandlersForContentType(content_type.0, LS_ROLES_ALL)
    });
    array.strings()
}

/// 列出 scheme 的候选 bundle id；系统无候选返回空列表，所有 CF 引用在返回前释放。
pub(super) fn scheme_handlers(scheme: &str) -> io::Result<Vec<String>> {
    let scheme = OwnedString::from_str(scheme)?;
    // SAFETY: scheme 有效；CopyAll 返回 CFArray<CFString> 的 +1 引用。
    let array = OwnedStringArray(unsafe { LSCopyAllHandlersForURLScheme(scheme.0) });
    array.strings()
}

/// 列出文件扩展名当前映射到的全部 UTI（含其他程序导出的声明）；无声明时系统返回动态 UTI。
pub(super) fn content_types_for_extension(extension: &str) -> io::Result<Vec<String>> {
    let tag_class = OwnedString::from_str(UT_TAG_CLASS_FILENAME_EXTENSION)?;
    let extension = OwnedString::from_str(extension)?;
    // SAFETY: 两个字符串在调用期间存活，conforming_to 允许为空；Create 返回
    // CFArray<CFString> 的 +1 引用，立即由专用守卫接管。
    let array = OwnedStringArray(unsafe {
        UTTypeCreateAllIdentifiersForTag(tag_class.0, extension.0, std::ptr::null())
    });
    array.strings()
}

/// 设置 UTI 的全部角色默认处理程序；拒绝 NUL，保留失败 OSStatus。
pub(super) fn set_content_handler(content_type: &str, bundle_id: &str) -> io::Result<()> {
    let content_type = OwnedString::from_str(content_type)?;
    let bundle_id = OwnedString::from_str(bundle_id)?;
    // SAFETY: 两个字符串在调用期间存活；Launch Services 不接管所有权。
    let status =
        unsafe { LSSetDefaultRoleHandlerForContentType(content_type.0, LS_ROLES_ALL, bundle_id.0) };
    if status != 0 {
        return Err(io::Error::other(format!(
            "LSSetDefaultRoleHandlerForContentType failed (OSStatus={status})"
        )));
    }
    Ok(())
}

/// 设置 scheme 默认处理程序；拒绝 NUL，保留失败 OSStatus。
pub(super) fn set_scheme_handler(scheme: &str, bundle_id: &str) -> io::Result<()> {
    let scheme = OwnedString::from_str(scheme)?;
    let bundle_id = OwnedString::from_str(bundle_id)?;
    // SAFETY: 两个字符串在调用期间存活；Launch Services 不接管所有权。
    let status = unsafe { LSSetDefaultHandlerForURLScheme(scheme.0, bundle_id.0) };
    if status != 0 {
        return Err(io::Error::other(format!(
            "LSSetDefaultHandlerForURLScheme failed (OSStatus={status})"
        )));
    }
    Ok(())
}

/// 当前进程所在 .app 的 bundle id；未打包时返回 None。
/// 辅助 bundle 的 id 由 platform::host_bundle_id 转换为外层宿主 id。
pub fn main_bundle_id() -> Option<String> {
    // SAFETY: 无参数系统查询；主 bundle 是进程持有的借用引用，无需释放。
    let bundle = unsafe { CFBundleGetMainBundle() };
    if bundle.is_null() {
        return None;
    }
    // SAFETY: bundle 是进程持有的非空主 bundle；返回标识符同样由 bundle 持有。
    let id = unsafe { CFBundleGetIdentifier(bundle) };
    // SAFETY: 主 bundle 在进程期间保持有效，标识符不可变且没有内嵌 NUL；只返回拷贝。
    match unsafe { copy_string(id) } {
        Ok(id) => id,
        Err(error) => {
            tracing::warn!(%error, "failed to read main bundle identifier");
            None
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cf_string_round_trips_empty_ascii_unicode_and_long_values() {
        for text in [
            String::new(),
            "com.fluxdown.app".to_owned(),
            "下载🙂e\u{301}日本語".to_owned(),
            "下载🙂e\u{301}".repeat(4096),
            "界".repeat(170),
            "界".repeat(171),
            "a".repeat(511),
            "a".repeat(512),
            "a".repeat(513),
            "a".repeat(8192),
        ] {
            let copy = {
                let value = OwnedString::from_str(&text).expect("create real CFString");
                value.text().expect("read real CFString")
            };
            assert_eq!(copy, Some(text));
        }
    }

    #[test]
    fn cf_string_rejects_nul_without_truncation() {
        for text in ["\0", "prefix\0suffix", "尾\0"] {
            let error = match OwnedString::from_str(text) {
                Ok(_) => panic!("NUL input unexpectedly accepted"),
                Err(error) => error,
            };
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }
    #[test]
    fn business_apis_reject_nul_before_launch_services_calls() {
        let invalid = "invalid\0identifier";
        assert_eq!(
            default_scheme_handler(invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            content_handlers(invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            scheme_handlers(invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            content_types_for_extension(invalid).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        // 两种参数位置均在 FFI 写入前被拒绝，不更改任何系统关联。
        for (name, id) in [
            (invalid, "com.fluxdown.test"),
            ("com.fluxdown.test", invalid),
        ] {
            assert_eq!(
                set_content_handler(name, id).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
            assert_eq!(
                set_scheme_handler(name, id).unwrap_err().kind(),
                io::ErrorKind::InvalidInput
            );
        }
    }

    #[test]
    fn launch_services_queries_are_read_only_and_accept_missing_handlers() {
        let missing = "com.fluxdown.test.nonexistent-handler-9a2bc815";
        assert!(
            content_handlers(missing)
                .expect("query content candidates")
                .is_empty()
        );
        assert!(
            default_scheme_handler(missing)
                .expect("query scheme default")
                .is_none()
        );
        assert!(
            scheme_handlers(missing)
                .expect("query scheme candidates")
                .is_empty()
        );
        let candidates = scheme_handlers("https").expect("query real string array");
        assert!(candidates.iter().all(|value| !value.is_empty()));
        let pdf = content_types_for_extension("pdf").expect("query extension UTIs");
        assert!(pdf.iter().any(|value| value == "com.adobe.pdf"));
    }
}
