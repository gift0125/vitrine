//! libmpv 的最小 FFI 绑定（运行时加载 `libmpv-2.dll`）。
//!
//! ## 为什么是运行时加载
//!
//! 官方 Windows 构建的 `mpv.exe` 是**静态**链接的，拆不出 DLL；能进程内调用的只有
//! mpv-dev 构建里的 `libmpv-2.dll`。这里用 `libloading` 在运行时加载，而不是构建期
//! 链接导入库：
//!
//! - 构建机上不需要放 DLL / `.dll.a`，`cargo build` 照常能过；
//! - 找不到库时能退成**用户可见的提示**（`VideoError::LibmpvNotFound`），而不是让进程
//!   在启动时被系统拒绝加载；
//! - 库的搜索顺序和应用资源布局由我们控制（exe 同目录优先），发布包里换个 DLL 就能升级。
//!
//! ## 只绑定了用到的部分
//!
//! 手写 ~20 个符号而不是生成全量绑定：`client.h` 里绝大部分是播放器才需要的东西，
//! 绑全了既没人用，也会让「mpv 改了 ABI 哪一处」变得难以审查。
//!
//! ## 线程安全
//!
//! 按 libmpv 的约定：`mpv_wait_event` 只能在**一个**线程里调用（我们的等待线程），
//! 其余命令/属性接口可以从任意线程调用。这里用 `MpvCtx` 把裸指针标成 `Send + Sync`，
//! 并保证 `mpv_terminate_destroy` 在等待线程退出之后才执行。

use std::ffi::{c_char, c_int, c_void, CStr, CString};
use std::path::{Path, PathBuf};

use libloading::Library;

// ---- mpv_format ----
pub const MPV_FORMAT_NONE: c_int = 0;
pub const MPV_FORMAT_STRING: c_int = 1;
pub const MPV_FORMAT_FLAG: c_int = 3;
pub const MPV_FORMAT_INT64: c_int = 4;
pub const MPV_FORMAT_DOUBLE: c_int = 5;

// ---- mpv_event_id ----
pub const MPV_EVENT_NONE: c_int = 0;
pub const MPV_EVENT_START_FILE: c_int = 6;
pub const MPV_EVENT_END_FILE: c_int = 7;
pub const MPV_EVENT_FILE_LOADED: c_int = 8;
pub const MPV_EVENT_LOG_MESSAGE: c_int = 2;
pub const MPV_EVENT_PROPERTY_CHANGE: c_int = 22;

// ---- mpv_end_file_reason ----
pub const MPV_END_FILE_REASON_EOF: c_int = 0;
pub const MPV_END_FILE_REASON_STOP: c_int = 2;
pub const MPV_END_FILE_REASON_QUIT: c_int = 3;
pub const MPV_END_FILE_REASON_ERROR: c_int = 4;
pub const MPV_END_FILE_REASON_REDIRECT: c_int = 5;

/// `mpv_event`（`client.h`）。`data` 的具体类型由 `event_id` 决定。
#[repr(C)]
pub struct MpvEvent {
    pub event_id: c_int,
    pub error: c_int,
    pub reply_userdata: u64,
    pub data: *mut c_void,
}

/// `mpv_event_property`。
#[repr(C)]
pub struct MpvEventProperty {
    pub name: *const c_char,
    /// `MPV_FORMAT_NONE` 表示该属性当前不可用（例如没加载文件时的 `time-pos`）。
    pub format: c_int,
    pub data: *mut c_void,
}

/// `mpv_event_end_file`。
#[repr(C)]
pub struct MpvEventEndFile {
    pub reason: c_int,
    pub error: c_int,
    pub playlist_entry_id: i64,
    pub playlist_insert_id: i64,
    pub playlist_insert_num_entries: c_int,
}

/// `mpv_event_log_message`。
#[repr(C)]
pub struct MpvEventLogMessage {
    pub prefix: *const c_char,
    pub level: *const c_char,
    pub text: *const c_char,
    pub log_level: c_int,
}

type FnCreate = unsafe extern "C" fn() -> *mut c_void;
type FnInitialize = unsafe extern "C" fn(*mut c_void) -> c_int;
type FnTerminateDestroy = unsafe extern "C" fn(*mut c_void);
type FnSetOption = unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int;
type FnSetOptionString = unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type FnSetProperty = unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int;
type FnSetPropertyString =
    unsafe extern "C" fn(*mut c_void, *const c_char, *const c_char) -> c_int;
type FnGetProperty = unsafe extern "C" fn(*mut c_void, *const c_char, c_int, *mut c_void) -> c_int;
type FnCommand = unsafe extern "C" fn(*mut c_void, *const *const c_char) -> c_int;
type FnObserveProperty =
    unsafe extern "C" fn(*mut c_void, u64, *const c_char, c_int) -> c_int;
type FnWaitEvent = unsafe extern "C" fn(*mut c_void, f64) -> *mut MpvEvent;
type FnWakeup = unsafe extern "C" fn(*mut c_void);
type FnErrorString = unsafe extern "C" fn(c_int) -> *const c_char;
type FnFree = unsafe extern "C" fn(*mut c_void);
type FnRequestLog = unsafe extern "C" fn(*mut c_void, *const c_char) -> c_int;
type FnClientApiVersion = unsafe extern "C" fn() -> u64;

/// 已加载的 libmpv 函数表。`Library` 必须与函数指针同生命周期。
pub struct LibMpv {
    _library: Library,
    create: FnCreate,
    initialize: FnInitialize,
    terminate_destroy: FnTerminateDestroy,
    set_option: FnSetOption,
    set_option_string: FnSetOptionString,
    set_property: FnSetProperty,
    set_property_string: FnSetPropertyString,
    get_property: FnGetProperty,
    command: FnCommand,
    observe_property: FnObserveProperty,
    wait_event: FnWaitEvent,
    wakeup: FnWakeup,
    error_string: FnErrorString,
    free: FnFree,
    request_log_messages: FnRequestLog,
    client_api_version: FnClientApiVersion,
}

// 函数指针本身与上下文无关；`Library` 的句柄在进程内长期有效。
unsafe impl Send for LibMpv {}
unsafe impl Sync for LibMpv {}

impl LibMpv {
    /// 从指定路径加载。
    pub fn load(path: &Path) -> Result<Self, String> {
        // SAFETY: 加载任意 DLL 都可能执行其初始化代码；这里只加载用户自己安装/我们随包
        // 分发的 libmpv。函数签名按 `client.h` 声明，版本不符时由 `client_api_version` 兜底。
        unsafe {
            let library = Library::new(path)
                .map_err(|e| format!("无法加载 {}: {e}", path.display()))?;

            macro_rules! sym {
                ($name:literal) => {
                    *library
                        .get::<*const c_void>($name)
                        .map_err(|e| {
                            let name = String::from_utf8_lossy($name);
                            format!("缺少符号 {}: {e}", name.trim_end_matches('\0'))
                        })?
                };
            }

            // 逐个 transmute 成具体签名：`get` 只保证地址有效，签名由我们来保证。
            let lib = Self {
                create: std::mem::transmute::<*const c_void, FnCreate>(sym!(b"mpv_create\0")),
                initialize: std::mem::transmute::<*const c_void, FnInitialize>(sym!(
                    b"mpv_initialize\0"
                )),
                terminate_destroy: std::mem::transmute::<*const c_void, FnTerminateDestroy>(sym!(
                    b"mpv_terminate_destroy\0"
                )),
                set_option: std::mem::transmute::<*const c_void, FnSetOption>(sym!(
                    b"mpv_set_option\0"
                )),
                set_option_string: std::mem::transmute::<*const c_void, FnSetOptionString>(sym!(
                    b"mpv_set_option_string\0"
                )),
                set_property: std::mem::transmute::<*const c_void, FnSetProperty>(sym!(
                    b"mpv_set_property\0"
                )),
                set_property_string: std::mem::transmute::<*const c_void, FnSetPropertyString>(
                    sym!(b"mpv_set_property_string\0"),
                ),
                get_property: std::mem::transmute::<*const c_void, FnGetProperty>(sym!(
                    b"mpv_get_property\0"
                )),
                command: std::mem::transmute::<*const c_void, FnCommand>(sym!(b"mpv_command\0")),
                observe_property: std::mem::transmute::<*const c_void, FnObserveProperty>(sym!(
                    b"mpv_observe_property\0"
                )),
                wait_event: std::mem::transmute::<*const c_void, FnWaitEvent>(sym!(
                    b"mpv_wait_event\0"
                )),
                wakeup: std::mem::transmute::<*const c_void, FnWakeup>(sym!(b"mpv_wakeup\0")),
                error_string: std::mem::transmute::<*const c_void, FnErrorString>(sym!(
                    b"mpv_error_string\0"
                )),
                free: std::mem::transmute::<*const c_void, FnFree>(sym!(b"mpv_free\0")),
                request_log_messages: std::mem::transmute::<*const c_void, FnRequestLog>(sym!(
                    b"mpv_request_log_messages\0"
                )),
                client_api_version: std::mem::transmute::<*const c_void, FnClientApiVersion>(sym!(
                    b"mpv_client_api_version\0"
                )),
                _library: library,
            };
            Ok(lib)
        }
    }

    /// libmpv 的客户端 API 版本（高 16 位主版本、低 16 位次版本）。
    pub fn api_version(&self) -> (u16, u16) {
        let v = unsafe { (self.client_api_version)() };
        ((v >> 16) as u16, (v & 0xFFFF) as u16)
    }

    pub fn create(&self) -> *mut c_void {
        unsafe { (self.create)() }
    }

    pub fn initialize(&self, ctx: *mut c_void) -> c_int {
        unsafe { (self.initialize)(ctx) }
    }

    pub fn terminate_destroy(&self, ctx: *mut c_void) {
        unsafe { (self.terminate_destroy)(ctx) }
    }

    /// 初始化**之前**设置选项（字符串形式）。
    pub fn set_option_string(&self, ctx: *mut c_void, name: &str, value: &str) -> c_int {
        let (Ok(name), Ok(value)) = (CString::new(name), CString::new(value)) else {
            return -1;
        };
        unsafe { (self.set_option_string)(ctx, name.as_ptr(), value.as_ptr()) }
    }

    /// 初始化**之前**设置选项（int64 形式，`wid` 这类句柄必须走它）。
    pub fn set_option_i64(&self, ctx: *mut c_void, name: &str, value: i64) -> c_int {
        let Ok(name) = CString::new(name) else {
            return -1;
        };
        let mut value = value;
        unsafe {
            (self.set_option)(
                ctx,
                name.as_ptr(),
                MPV_FORMAT_INT64,
                &mut value as *mut i64 as *mut c_void,
            )
        }
    }

    pub fn set_property_string(&self, ctx: *mut c_void, name: &str, value: &str) -> c_int {
        let (Ok(name), Ok(value)) = (CString::new(name), CString::new(value)) else {
            return -1;
        };
        unsafe { (self.set_property_string)(ctx, name.as_ptr(), value.as_ptr()) }
    }

    pub fn set_property_flag(&self, ctx: *mut c_void, name: &str, value: bool) -> c_int {
        let Ok(name) = CString::new(name) else {
            return -1;
        };
        let mut value: c_int = value as c_int;
        unsafe {
            (self.set_property)(
                ctx,
                name.as_ptr(),
                MPV_FORMAT_FLAG,
                &mut value as *mut c_int as *mut c_void,
            )
        }
    }

    pub fn set_property_f64(&self, ctx: *mut c_void, name: &str, value: f64) -> c_int {
        let Ok(name) = CString::new(name) else {
            return -1;
        };
        let mut value = value;
        unsafe {
            (self.set_property)(
                ctx,
                name.as_ptr(),
                MPV_FORMAT_DOUBLE,
                &mut value as *mut f64 as *mut c_void,
            )
        }
    }

    pub fn get_property_f64(&self, ctx: *mut c_void, name: &str) -> Option<f64> {
        let name = CString::new(name).ok()?;
        let mut value: f64 = 0.0;
        let rc = unsafe {
            (self.get_property)(
                ctx,
                name.as_ptr(),
                MPV_FORMAT_DOUBLE,
                &mut value as *mut f64 as *mut c_void,
            )
        };
        (rc >= 0).then_some(value)
    }

    pub fn get_property_string(&self, ctx: *mut c_void, name: &str) -> Option<String> {
        let name = CString::new(name).ok()?;
        let mut out = std::ptr::null_mut::<c_char>();
        let rc = unsafe {
            (self.get_property)(
                ctx,
                name.as_ptr(),
                MPV_FORMAT_STRING,
                &mut out as *mut *mut c_char as *mut c_void,
            )
        };
        if rc < 0 || out.is_null() {
            return None;
        }
        // mpv 分配、调用方释放；字符串很短，但漏掉会随磁盘/流媒体的元数据无界增长。
        let text = unsafe { CStr::from_ptr(out) }.to_string_lossy().into_owned();
        unsafe { (self.free)(out as *mut c_void) };
        Some(text)
    }

    /// 执行命令（argv 风格，`args[0]` 是命令名，自动补 NULL 结尾）。
    pub fn command(&self, ctx: *mut c_void, args: &[&str]) -> c_int {
        let owned: Vec<CString> = args
            .iter()
            .filter_map(|a| CString::new(*a).ok())
            .collect();
        if owned.len() != args.len() {
            return -1; // 参数里有内嵌 NUL，属于调用方错误
        }
        let mut ptrs: Vec<*const c_char> = owned.iter().map(|c| c.as_ptr()).collect();
        ptrs.push(std::ptr::null());
        unsafe { (self.command)(ctx, ptrs.as_ptr()) }
    }

    pub fn observe_property(
        &self,
        ctx: *mut c_void,
        reply_userdata: u64,
        name: &str,
        format: c_int,
    ) -> c_int {
        let Ok(name) = CString::new(name) else {
            return -1;
        };
        unsafe { (self.observe_property)(ctx, reply_userdata, name.as_ptr(), format) }
    }

    /// 等一个事件。`timeout` 秒内没有事件时返回 `MPV_EVENT_NONE` 事件。
    pub fn wait_event(&self, ctx: *mut c_void, timeout: f64) -> *mut MpvEvent {
        unsafe { (self.wait_event)(ctx, timeout) }
    }

    /// 打断正在阻塞的 `mpv_wait_event`（退出时用）。
    pub fn wakeup(&self, ctx: *mut c_void) {
        unsafe { (self.wakeup)(ctx) }
    }

    pub fn request_log_messages(&self, ctx: *mut c_void, level: &str) -> c_int {
        let Ok(level) = CString::new(level) else {
            return -1;
        };
        unsafe { (self.request_log_messages)(ctx, level.as_ptr()) }
    }

    /// mpv 错误码 → 可读文本。
    pub fn error_string(&self, code: c_int) -> String {
        if code >= 0 {
            return String::new();
        }
        let ptr = unsafe { (self.error_string)(code) };
        if ptr.is_null() {
            return format!("mpv error {code}");
        }
        unsafe { CStr::from_ptr(ptr) }.to_string_lossy().into_owned()
    }
}

/// 裸 mpv 句柄，标记成可跨线程传递（libmpv 客户端 API 本身是线程安全的，
/// 见 `client.h` 里对 `mpv_wait_event` 的单线程约束说明）。
#[derive(Clone, Copy)]
pub struct MpvCtx(pub *mut c_void);

unsafe impl Send for MpvCtx {}
unsafe impl Sync for MpvCtx {}

impl MpvCtx {
    pub fn raw(self) -> *mut c_void {
        self.0
    }

    pub fn is_null(self) -> bool {
        self.0.is_null()
    }
}

/// 按「环境变量 → exe 同目录 → PATH → 常见安装路径」找 `libmpv-2.dll`。
///
/// 与旧的 `find_mpv` 一一对应：`SMP_MPV_PATH` 仍然有效（老的批处理/脚本不用改），
/// 另外加了 `SMP_LIBMPV_PATH`（语义更准，优先用它）。
pub fn find_libmpv(exe_dir: Option<&Path>) -> Option<PathBuf> {
    find_libmpv_in(
        exe_dir,
        [
            std::env::var_os("SMP_LIBMPV_PATH"),
            std::env::var_os("SMP_MPV_PATH"),
        ],
        std::env::current_exe().ok(),
        std::env::var_os("PATH"),
    )
}

/// `find_libmpv` 的纯函数内核：把「进程环境」全部作为参数传进来。
///
/// 单测要用一个假 DLL 冒充真库。以前直接在测试里 `set_var`，和并行跑的
/// 真 DLL 集成用例抢同一个进程环境（各测试同线程池随机交错，表现为偶发失败：
/// 真用例读到假路径、假用例读到被清空的变量）。改成传参之后不存在共享状态。
fn find_libmpv_in(
    exe_dir: Option<&Path>,
    overrides: [Option<std::ffi::OsString>; 2],
    current_exe: Option<PathBuf>,
    path_var: Option<std::ffi::OsString>,
) -> Option<PathBuf> {
    const DLL: &str = "libmpv-2.dll";

    for path in overrides.into_iter().flatten() {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Some(path);
        }
    }

    // exe 同目录优先：发布包里 DLL 就放在这儿
    if let Some(dir) = exe_dir {
        let candidate = dir.join(DLL);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Some(dir) = current_exe.as_deref().and_then(Path::parent) {
        let candidate = dir.join(DLL);
        if candidate.is_file() {
            return Some(candidate);
        }
    }

    if let Some(paths) = path_var {
        for dir in std::env::split_paths(&paths) {
            let candidate = dir.join(DLL);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }

    const CANDIDATES: &[&str] = &[
        r"C:\ProgramData\chocolatey\lib\mpvio.install\tools\libmpv-2.dll",
        r"C:\Program Files\mpv\libmpv-2.dll",
        r"C:\Program Files (x86)\mpv\libmpv-2.dll",
    ];
    CANDIDATES
        .iter()
        .map(PathBuf::from)
        .find(|path| path.is_file())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_library_reports_the_path() {
        let err = match LibMpv::load(Path::new(r"C:\definitely\not\here\libmpv-2.dll")) {
            Ok(_) => panic!("不存在的路径必须报错"),
            Err(err) => err,
        };
        assert!(err.contains("libmpv-2.dll"), "错误里要带路径：{err}");
    }

    /// 环境变量优先于 exe 同目录，且 `SMP_LIBMPV_PATH` 排在 `SMP_MPV_PATH` 前面。
    ///
    /// 走 `find_libmpv_in` 传参而**不是** `set_var`：并行跑的真 DLL 用例会读到
    /// 同一个进程环境，两边互相污染（表现为随机失败）。
    #[test]
    fn env_override_wins() {
        // 用一个真实存在的文件冒充 DLL：只验查找顺序，不验加载
        let dir = std::env::temp_dir().join("smp-libmpv-test-env");
        std::fs::create_dir_all(&dir).unwrap();
        let fake = dir.join("fake-libmpv-2.dll");
        std::fs::write(&fake, b"not a dll").unwrap();
        // 同目录也放一个"真名"的候选，用来证明 override 确实压过了它
        let decoy = dir.join("libmpv-2.dll");
        std::fs::write(&decoy, b"not a dll either").unwrap();

        let found = find_libmpv_in(Some(&dir), [Some(fake.clone().into()), None], None, None);
        assert_eq!(found.as_deref(), Some(fake.as_path()), "环境变量应优先");

        // 没有 override 时才轮到 exe 同目录
        let found = find_libmpv_in(Some(&dir), [None, None], None, None);
        assert_eq!(found.as_deref(), Some(decoy.as_path()));

        std::fs::remove_dir_all(&dir).ok();
    }
}
