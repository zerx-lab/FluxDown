//! Windows 本机 IPC 命名管道的固定安全描述符（浏览器中继端点与桌面网关 IPC 共用）。
//!
//! 管道默认安全描述符取自创建者令牌：提权（UAC 完整令牌）进程建的管道属主是 Administrators、
//! DACL 不含当前用户，还带「高」强制完整性标签，浏览器拉起的中完整性中继只能只读打开（#951 /
//! #787）。降权接手（`elevation.rs`）失败时 agent 会继续以提权身份运行，所以这里不依赖令牌默认值，
//! 统一用固定的安全描述符建管道：
//!
//! - DACL（受保护，不继承）：当前用户 SID 与 SYSTEM 完全控制；不含默认的 Everyone / Anonymous
//!   只读项，其他账户连只读打开都不行。
//! - 强制完整性标签：中（ME），只禁止更低完整性写入（NW）。提权 agent 建的管道同样能被
//!   中完整性的浏览器中继与桌面界面读写。
//!
//! 对外只暴露安全函数：建管道实例与查询当前用户 SID。
#![cfg(windows)]

use std::ffi::c_void;
use std::io;
use std::sync::OnceLock;

use tokio::net::windows::named_pipe::{NamedPipeServer, ServerOptions};
use windows::Win32::Foundation::{HLOCAL, LocalFree};
use windows::Win32::Security::Authorization::{
    ConvertSidToStringSidW, ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
};
use windows::Win32::Security::{
    GetTokenInformation, PSECURITY_DESCRIPTOR, SECURITY_ATTRIBUTES, TOKEN_USER, TokenUser,
};
use windows::core::{HSTRING, PWSTR};

/// 管道的 SDDL：受保护 DACL（当前用户 + SYSTEM 完全控制）+ 中完整性、禁止低完整性写入的标签。
fn pipe_sddl(user_sid: &str) -> String {
    format!("D:P(A;;GA;;;{user_sid})(A;;GA;;;SY)S:(ML;;NW;;;ME)")
}

/// 以固定安全描述符创建一个管道实例。`first_instance` 为真时名字已被占用即失败
/// （`FILE_FLAG_FIRST_PIPE_INSTANCE`），保证首个实例属于本进程。
///
/// # Errors
/// 查询当前用户 SID、构造安全描述符或创建管道失败。
pub fn create_pipe_instance(name: &str, first_instance: bool) -> io::Result<NamedPipeServer> {
    let sid = current_user_sid()?;
    let sddl = HSTRING::from(pipe_sddl(&sid));
    let mut raw = PSECURITY_DESCRIPTOR::default();
    // SAFETY: sddl 是存活到调用返回、以 NUL 结尾的 UTF-16 串；raw 是有效的输出槽，成功后其中的
    // LocalAlloc 内存立即交给 LocalMemory 守卫；不需要描述符长度，最后一个参数传 None。
    unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(&sddl, SDDL_REVISION_1, &mut raw, None)
    }
    .map_err(|error| {
        io::Error::other(format!(
            "could not build the IPC pipe security descriptor: {error}"
        ))
    })?;
    let descriptor = LocalMemory(raw.0);
    let length = u32::try_from(size_of::<SECURITY_ATTRIBUTES>())
        .map_err(|_| io::Error::other("SECURITY_ATTRIBUTES size does not fit u32"))?;
    let mut attributes = SECURITY_ATTRIBUTES {
        nLength: length,
        lpSecurityDescriptor: descriptor.0,
        bInheritHandle: false.into(),
    };
    let mut options = ServerOptions::new();
    options.first_pipe_instance(first_instance);
    let attributes_ptr: *mut c_void = std::ptr::from_mut(&mut attributes).cast();
    // SAFETY: attributes_ptr 指向本栈上完整初始化的 SECURITY_ATTRIBUTES（nLength 为其大小），其
    // lpSecurityDescriptor 指向 descriptor 守卫持有的自相对安全描述符；两者都存活到调用返回，
    // CreateNamedPipeW 只在调用期间读取，不保留指针。
    unsafe { options.create_with_security_attributes_raw(name, attributes_ptr) }
}

/// 当前进程令牌的用户 SID（`S-1-5-21-…`）；进程内只查询一次。
///
/// # Errors
/// 打开令牌、读取 `TokenUser` 或转换 SID 失败。
pub fn current_user_sid() -> io::Result<String> {
    static SID: OnceLock<String> = OnceLock::new();
    if let Some(sid) = SID.get() {
        return Ok(sid.clone());
    }
    let sid = query_current_user_sid()?;
    Ok(SID.get_or_init(|| sid).clone())
}

fn query_current_user_sid() -> io::Result<String> {
    let token = super::elevation::open_process_token().map_err(io::Error::other)?;
    let mut needed = 0_u32;
    // SAFETY: 令牌句柄由 token 守卫持有、带 TOKEN_QUERY 权限且存活到调用返回；不传缓冲区
    // （None、长度 0）只查询所需字节数，needed 是有效的输出变量。
    let probe = unsafe { GetTokenInformation(token.handle(), TokenUser, None, 0, &mut needed) };
    if needed == 0 {
        return Err(io::Error::other(match probe {
            Err(error) => format!("GetTokenInformation(TokenUser) size query failed: {error}"),
            Ok(()) => "GetTokenInformation(TokenUser) reported an empty buffer".to_owned(),
        }));
    }
    let bytes = usize::try_from(needed)
        .map_err(|_| io::Error::other("TokenUser size does not fit usize"))?;
    // 按 usize 对齐分配：TOKEN_USER 含指针，Vec<u8> 不保证其对齐。
    let mut buffer = vec![0_usize; bytes.div_ceil(size_of::<usize>())];
    let capacity = buffer
        .len()
        .checked_mul(size_of::<usize>())
        .and_then(|value| u32::try_from(value).ok())
        .ok_or_else(|| io::Error::other("TokenUser buffer size does not fit u32"))?;
    let buffer_ptr: *mut c_void = buffer.as_mut_ptr().cast();
    // SAFETY: buffer_ptr 指向 buffer 的 capacity 字节、按指针对齐，buffer 存活到调用返回；令牌句柄
    // 同上；needed 是有效的输出变量。
    unsafe {
        GetTokenInformation(
            token.handle(),
            TokenUser,
            Some(buffer_ptr),
            capacity,
            &mut needed,
        )
    }
    .map_err(|error| io::Error::other(format!("GetTokenInformation(TokenUser) failed: {error}")))?;
    // SAFETY: 上面的 GetTokenInformation(TokenUser) 成功，buffer 开头是已初始化、对齐的
    // TOKEN_USER；按值读出副本，其中 Sid 指向同一 buffer 内的 SID。
    let user = unsafe { buffer.as_ptr().cast::<TOKEN_USER>().read() };
    let mut text = PWSTR::null();
    // SAFETY: user.User.Sid 指向 buffer 内的有效 SID，buffer 存活到调用返回；text 是有效的输出槽，
    // 成功后其中的 LocalAlloc 内存立即交给 LocalMemory 守卫。
    unsafe { ConvertSidToStringSidW(user.User.Sid, &mut text) }
        .map_err(|error| io::Error::other(format!("ConvertSidToStringSidW failed: {error}")))?;
    let owned = LocalMemory(text.0.cast());
    // SAFETY: text 是 ConvertSidToStringSidW 分配的以 NUL 结尾的 UTF-16 串，owned 守卫存活到
    // 本次读取结束，期间不被释放或修改。
    let sid = unsafe { text.to_string() }
        .map_err(|error| io::Error::other(format!("SID string is not UTF-16: {error}")))?;
    drop(owned);
    Ok(sid)
}

/// 独占持有 Win32 API 以 `LocalAlloc` 分配给调用方的输出内存，drop 时 `LocalFree` 一次。
/// 含裸指针，天然 `!Send` / `!Sync`，只在同步函数内使用。
struct LocalMemory(*mut c_void);

impl Drop for LocalMemory {
    fn drop(&mut self) {
        // SAFETY: self.0 由成功的 Win32 调用以 LocalAlloc 分配、仅此守卫拥有，只在这里释放一次。
        let leaked = unsafe { LocalFree(Some(HLOCAL(self.0))) };
        if !leaked.0.is_null() {
            tracing::debug!("LocalFree did not release Win32 output memory");
        }
    }
}

#[cfg(test)]
mod tests {
    use super::pipe_sddl;

    #[test]
    fn sddl_grants_only_user_and_system_with_medium_label() {
        assert_eq!(
            pipe_sddl("S-1-5-21-1-2-3-1001"),
            "D:P(A;;GA;;;S-1-5-21-1-2-3-1001)(A;;GA;;;SY)S:(ML;;NW;;;ME)"
        );
    }

    #[test]
    fn current_user_sid_is_a_user_sid() {
        let sid = super::current_user_sid().expect("query current user SID");
        assert!(sid.starts_with("S-1-5-"), "{sid}");
    }
}
