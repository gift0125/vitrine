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
/// `mpv_node` 数组（`screenshot-raw` 的 argv 与返回值都是 node）
pub const MPV_FORMAT_NODE_ARRAY: c_int = 7;
pub const MPV_FORMAT_NODE_MAP: c_int = 8;
/// `mpv_byte_array`：`screenshot-raw` 的像素就是它
pub const MPV_FORMAT_BYTE_ARRAY: c_int = 9;

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

/// `mpv_byte_array`：一块裸内存（`screenshot-raw` 的像素数据）。
#[repr(C)]
pub struct MpvByteArray {
    pub data: *mut c_void,
    pub size: usize,
}

/// `mpv_node_list`：数组（`keys` 为空）或字典（`values[i]` 对应 `keys[i]`）。
#[repr(C)]
pub struct MpvNodeList {
    pub num: i32,
    pub values: *mut MpvNode,
    pub keys: *mut *mut c_char,
}

/// `mpv_node` 里的那个联合体。
///
/// 用 Rust 的 `union` 而不是「按最大成员占位 + transmute」：`mpv_node` 是
/// `#[repr(C)]` 的 `{ union; mpv_format }`，联合体的对齐是 8（含指针/int64/double），
/// Rust 的 union 也是同一套规则，布局因此逐字节一致。
#[repr(C)]
pub union MpvNodeData {
    pub string: *mut c_char,
    pub flag: c_int,
    pub int64: i64,
    pub double_: f64,
    pub list: *mut MpvNodeList,
    pub byte_array: *mut MpvByteArray,
}

/// `mpv_node`（`client.h`）。
#[repr(C)]
pub struct MpvNode {
    pub data: MpvNodeData,
    pub format: c_int,
}

/// `screenshot-raw` 抓下来的裸帧（每次取色用一次，用完就丢）。
#[derive(Clone, Debug, PartialEq)]
pub struct RawFrame {
    pub width: usize,
    pub height: usize,
    /// 每行字节数（可能大于 `width × bpp`：mpv 对齐到 32/64 字节）
    pub stride: usize,
    /// mpv 报的像素格式名：`bgr0` / `rgb0` / `bgra` / `rgb24` …
    pub format: String,
    pub data: Vec<u8>,
}

impl RawFrame {
    /// 每像素字节数与通道顺序（`(r, g, b)` 在字节里的下标）。
    ///
    /// 不猜格式：mpv 的 `format` 字段首字母就说明了通道顺序（`bgr*` / `rgb*`），
    /// 长度就是每像素字节数（`bgr0` = 4）。认不出来就当 BGRA 处理。
    fn layout(&self) -> (usize, [usize; 3]) {
        let bpp = self.format.len().max(3);
        let order = if self.format.starts_with('r') {
            [0, 1, 2]
        } else {
            [2, 1, 0]
        };
        (bpp, order)
    }

    /// 画面**底部中间 60%×38%** 的平均色（设计稿 `L2_frame_tint.sample_region`：
    /// 那块正好在控制栏背后）。
    ///
    /// 大范围抽样而不是逐像素：一个 1080p 帧的这块有 47 万像素，取均值时每 4 个
    /// 跳一个已经完全不影响结果，但能把一次取色的开销压到 1ms 量级。
    pub fn bottom_band_mean(&self) -> Option<(f32, f32, f32)> {
        if self.width < 4 || self.height < 4 || self.data.is_empty() {
            return None;
        }
        let (bpp, order) = self.layout();
        let x0 = (self.width as f32 * 0.20) as usize;
        let x1 = (self.width as f32 * 0.80) as usize;
        let y0 = (self.height as f32 * 0.62) as usize;
        let y1 = self.height;
        let (region_w, region_h) = (x1.saturating_sub(x0), y1.saturating_sub(y0));
        if region_w == 0 || region_h == 0 {
            return None;
        }
        let step = (((region_w * region_h) as f32 / 20_000.0).sqrt() as usize).max(1);

        let (mut sr, mut sg, mut sb, mut n) = (0u64, 0u64, 0u64, 0u64);
        let mut y = y0;
        while y < y1 {
            let row = y * self.stride;
            let mut x = x0;
            while x < x1 {
                let base = row + x * bpp;
                if base + 2 < self.data.len() {
                    sr += self.data[base + order[0]] as u64;
                    sg += self.data[base + order[1]] as u64;
                    sb += self.data[base + order[2]] as u64;
                    n += 1;
                }
                x += step;
            }
            y += step;
        }
        if n == 0 {
            return None;
        }
        let n = n as f32;
        Some((sr as f32 / n, sg as f32 / n, sb as f32 / n))
    }
}

/// RGB → 色相（0~360 度，饱和度/明度丢掉不要）。
///
/// 设计稿只要色相：直接把画面均值混进玻璃等于把它染得更黑（暗场景几乎全黑，
/// 混完看不出色温），固定 S/L 才能让每一部片子的玻璃都落在同一个观感上。
pub fn rgb_hue(r: f32, g: f32, b: f32) -> f32 {
    let max = r.max(g).max(b);
    let min = r.min(g).min(b);
    let delta = max - min;
    if delta <= f32::EPSILON {
        return 0.0; // 灰阶没有色相
    }
    let h = if max == r {
        ((g - b) / delta).rem_euclid(6.0)
    } else if max == g {
        (b - r) / delta + 2.0
    } else {
        (r - g) / delta + 4.0
    };
    h * 60.0
}

/// 把 `mpv_node`（MAP）读成 [`RawFrame`]。
///
/// SAFETY: 调用方必须保证 `node` 来自一次成功的 `mpv_command_node`，且在
/// `mpv_free_node_contents` 之前调用；里面的指针只在这一次返回值里有效。
unsafe fn parse_raw_frame(node: &MpvNode) -> Option<RawFrame> {
    if node.format != MPV_FORMAT_NODE_MAP {
        return None;
    }
    let list = node.data.list.as_ref()?;
    if list.values.is_null() || list.num <= 0 {
        return None;
    }
    let (mut width, mut height, mut stride) = (0usize, 0usize, 0usize);
    let mut format = String::new();
    let mut data: Option<Vec<u8>> = None;
    for i in 0..list.num as usize {
        let key_ptr = if list.keys.is_null() {
            return None;
        } else {
            *list.keys.add(i)
        };
        if key_ptr.is_null() {
            continue;
        }
        let key = CStr::from_ptr(key_ptr).to_str().unwrap_or_default();
        let value = &*list.values.add(i);
        match (key, value.format) {
            ("w", MPV_FORMAT_INT64) => width = value.data.int64.max(0) as usize,
            ("h", MPV_FORMAT_INT64) => height = value.data.int64.max(0) as usize,
            ("stride", MPV_FORMAT_INT64) => stride = value.data.int64.max(0) as usize,
            ("format", MPV_FORMAT_STRING) => {
                let text = value.data.string;
                if !text.is_null() {
                    format = CStr::from_ptr(text).to_string_lossy().into_owned();
                }
            }
            ("data", MPV_FORMAT_BYTE_ARRAY) => {
                let bytes = value.data.byte_array.as_ref()?;
                if bytes.data.is_null() || bytes.size == 0 {
                    return None;
                }
                let slice = std::slice::from_raw_parts(bytes.data as *const u8, bytes.size);
                data = Some(slice.to_vec());
            }
            _ => {}
        }
    }
    Some(RawFrame {
        width,
        height,
        stride,
        format,
        data: data?,
    })
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
type FnCommandNode = unsafe extern "C" fn(*mut c_void, *mut MpvNode, *mut MpvNode) -> c_int;
type FnFreeNodeContents = unsafe extern "C" fn(*mut MpvNode);

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
    command_node: FnCommandNode,
    free_node_contents: FnFreeNodeContents,
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
                command_node: std::mem::transmute::<*const c_void, FnCommandNode>(sym!(
                    b"mpv_command_node\0"
                )),
                free_node_contents: std::mem::transmute::<*const c_void, FnFreeNodeContents>(sym!(
                    b"mpv_free_node_contents\0"
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

    /// `screenshot-raw`：把**当前显示的那一帧**抓成裸像素。
    ///
    /// 只用来做 L2 的画面取色（一次缩略图 + 一次均值），所以：
    /// - 走 `mpv_command_node`：`screenshot-raw` 的结果是一棵只存在于返回值里的
    ///   `mpv_node`（`w` / `h` / `stride` / `format` / `data`），`mpv_command` 那套
    ///   argv 接口拿不到它；
    /// - 结果必须 `mpv_free_node_contents` 归还，否则每取一次色漏一帧的内存；
    /// - 像素格式由 mpv 决定（通常是 `bgr0`），按 `format` 字段的**首字母**判断
    ///   RGB / BGR，不猜。
    pub fn screenshot_raw(&self, ctx: *mut c_void) -> Option<RawFrame> {
        let names = ["screenshot-raw", "video"];
        let owned: Vec<CString> = names
            .iter()
            .map(|n| CString::new(*n).unwrap_or_default())
            .collect();
        let mut args: Vec<MpvNode> = owned
            .iter()
            .map(|c| MpvNode {
                data: MpvNodeData {
                    string: c.as_ptr() as *mut c_char,
                },
                format: MPV_FORMAT_STRING,
            })
            .collect();
        let mut list = MpvNodeList {
            num: args.len() as i32,
            values: args.as_mut_ptr(),
            keys: std::ptr::null_mut(),
        };
        let mut call = MpvNode {
            data: MpvNodeData { list: &mut list },
            format: MPV_FORMAT_NODE_ARRAY,
        };
        // SAFETY: `mpv_node` 是 POJO，零值合法（格式 NONE）；返回前 mpv 会填好它。
        let mut result: MpvNode = unsafe { std::mem::zeroed() };
        let rc = unsafe { (self.command_node)(ctx, &mut call, &mut result) };
        if rc < 0 {
            return None;
        }
        let frame = unsafe { parse_raw_frame(&result) };
        unsafe { (self.free_node_contents)(&mut result) };
        frame
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

    /// 色相：纯红 0°、纯绿 120°、纯蓝 240°，灰阶没有色相。
    #[test]
    fn hue_matches_the_color_wheel() {
        assert!((rgb_hue(255.0, 0.0, 0.0) - 0.0).abs() < 0.5);
        assert!((rgb_hue(0.0, 255.0, 0.0) - 120.0).abs() < 0.5);
        assert!((rgb_hue(0.0, 0.0, 255.0) - 240.0).abs() < 0.5);
        assert_eq!(rgb_hue(90.0, 90.0, 90.0), 0.0, "灰阶没有色相");
        // 设计稿举例的偏蓝底 `rgb(46,62,71)`：色相落在 190~205 之间（他们写 197°）
        let hue = rgb_hue(46.0, 62.0, 71.0);
        assert!(
            (190.0..=205.0).contains(&hue),
            "偏蓝底应落在蓝青区间，实际 {hue}"
        );
    }

    /// 底部中间 60%×38% 的均值：越界、行距、通道顺序都要对。
    #[test]
    fn bottom_band_mean_respects_stride_and_channel_order() {
        // 8x8 的 bgr0 帧：整帧纯蓝（B=255,R=0），行距故意给宽一点（8×4 + 4）
        let (w, h, stride) = (8usize, 8usize, 8 * 4 + 4);
        let mut data = vec![0u8; stride * h];
        for y in 0..h {
            for x in 0..w {
                let base = y * stride + x * 4;
                data[base] = 255; // B
                data[base + 1] = 0; // G
                data[base + 2] = 0; // R
            }
        }
        let frame = RawFrame {
            width: w,
            height: h,
            stride,
            format: "bgr0".to_string(),
            data,
        };
        let (r, g, b) = frame.bottom_band_mean().expect("应该取到均值");
        assert!(
            r < 1.0 && g < 1.0,
            "BGR 顺序读错会在这里露出来：r={r} g={g}"
        );
        assert!((b - 255.0).abs() < 1.0, "蓝色通道应满值，实际 {b}");

        // 同一块数据按 rgb0 解释，通道顺序就该翻过来
        let as_rgb = RawFrame {
            format: "rgb0".to_string(),
            ..frame.clone()
        };
        let (r, _, b) = as_rgb.bottom_band_mean().unwrap();
        assert!((r - 255.0).abs() < 1.0 && b < 1.0, "rgb0 应把首字节当 R");
    }
}
