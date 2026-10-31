//! mac 的 GPU 画面通路：mpv（OpenGL）→ IOSurface → GPUI 的 `surface()`。
//!
//! ## 为什么不是一步到位
//!
//! GPUI 在 mac 上能零拷贝合成的只有 `CVPixelBuffer`，而且**只认**
//! `420YpCbCr8BiPlanarFullRange`（NV12、全范围，见 gpui-pre-apple 的 `draw_surfaces`，
//! 别的格式直接 assert 崩掉）。mpv 的 OpenGL 后端只往 RGB 的 FBO 里画，所以中间
//! 多一道我们自己的着色器：
//!
//! ```text
//! mpv ──► RGB 纹理（GL_TEXTURE_2D）──┬─► Y  平面（R8， 全尺寸）  ┐ 同一块 IOSurface
//!                                    └─► CbCr 平面（RG8，半尺寸）┘ = CVPixelBuffer
//! ```
//!
//! 换算用 BT.601 全范围——正好是 GPUI 着色器里 `ycbcrToRGBTransform` 的逆，一来一回
//! 只剩 8 位量化和色度 4:2:0 的损失（视频源本来就是 4:2:0）。
//!
//! 全程不经过 CPU：硬解（VideoToolbox）出来的帧由 mpv 自己通过 IOSurface 绑进 GL，
//! 缩放和色彩管理是 mpv 的着色器，最后这一跳也是 GPU 对 GPU。
//!
//! ## 液态玻璃要的那点像素
//!
//! 玻璃是 CPU 烤的，需要画面像素。不再逐帧整帧拷，而是要的时候（控制层亮着）
//! 对 RGB 纹理生成 mipmap，读回长边不超过 [`snapshot`](GlTarget::snapshot) 上限的那一级
//! ——box 滤波过的小图，几百 KB。
//!
//! ## 线程
//!
//! 所有调用都在 UI 线程上（和 `mpv_render_context` 同一个线程）。GPUI 在 mac 上
//! 用 Metal、不碰 GL，所以这个上下文一直当前也不会和谁打架；每个入口仍然先
//! `make_current` 一次，不依赖这一点。

use std::ffi::{c_char, c_void, CStr, CString};
use std::ptr::{self, NonNull};

use objc2_core_foundation::{CFBoolean, CFDictionary, CFRetained, CFString, CFType};
use objc2_core_video::{
    kCVPixelBufferIOSurfacePropertiesKey, kCVPixelBufferMetalCompatibilityKey,
    kCVPixelBufferOpenGLCompatibilityKey, kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
    kCVReturnSuccess, CVPixelBuffer, CVPixelBufferCreate,
};

use crate::render::FrameBuffer;

/// 交给界面的一帧。
pub type PixelBuffer = CFRetained<CVPixelBuffer>;

/// 一帧的像素尺寸（宽, 高）。
pub fn pixel_buffer_size(buffer: &PixelBuffer) -> (usize, usize) {
    (
        objc2_core_video::CVPixelBufferGetWidth(buffer),
        objc2_core_video::CVPixelBufferGetHeight(buffer),
    )
}

/// 同尺寸的输出缓冲最多留几块。界面手上一块、GPUI 上一帧的场景里一块、
/// Metal 在飞的一两块——再多就说明有人一直不放，直接挑最旧的覆盖。
const MAX_SLOTS: usize = 6;

// ---- CGL / OpenGL（OpenGL.framework 直接导出全部 4.1 core 函数）----

type CGLContextObj = *mut c_void;
type CGLPixelFormatObj = *mut c_void;
type GLenum = u32;
type GLuint = u32;
type GLint = i32;
type GLsizei = i32;

const KCGL_PFA_ACCELERATED: i32 = 73;
const KCGL_PFA_ALLOW_OFFLINE_RENDERERS: i32 = 96;
const KCGL_PFA_OPENGL_PROFILE: i32 = 99;
const KCGL_PFA_SUPPORTS_AUTOMATIC_GRAPHICS_SWITCHING: i32 = 101;
const KCGL_OGLP_VERSION_3_2_CORE: i32 = 0x3200;

const GL_TEXTURE_2D: GLenum = 0x0DE1;
const GL_TEXTURE_RECTANGLE: GLenum = 0x84F5;
const GL_TEXTURE0: GLenum = 0x84C0;
const GL_TEXTURE_MIN_FILTER: GLenum = 0x2801;
const GL_TEXTURE_MAG_FILTER: GLenum = 0x2800;
const GL_TEXTURE_WRAP_S: GLenum = 0x2802;
const GL_TEXTURE_WRAP_T: GLenum = 0x2803;
const GL_LINEAR: GLint = 0x2601;
const GL_CLAMP_TO_EDGE: GLint = 0x812F;
const GL_RGBA: GLenum = 0x1908;
const GL_RGBA8: GLint = 0x8058;
const GL_BGRA: GLenum = 0x80E1;
const GL_RED: GLenum = 0x1903;
const GL_RG: GLenum = 0x8227;
const GL_R8: GLenum = 0x8229;
const GL_RG8: GLenum = 0x822B;
const GL_UNSIGNED_BYTE: GLenum = 0x1401;
const GL_UNSIGNED_INT_8_8_8_8_REV: GLenum = 0x8367;
const GL_FRAMEBUFFER: GLenum = 0x8D40;
const GL_COLOR_ATTACHMENT0: GLenum = 0x8CE0;
const GL_FRAMEBUFFER_COMPLETE: GLenum = 0x8CD5;
const GL_VERTEX_SHADER: GLenum = 0x8B31;
const GL_FRAGMENT_SHADER: GLenum = 0x8B30;
const GL_COMPILE_STATUS: GLenum = 0x8B81;
const GL_LINK_STATUS: GLenum = 0x8B82;
const GL_TRIANGLES: GLenum = 0x0004;
const GL_BLEND: GLenum = 0x0BE2;
const GL_SCISSOR_TEST: GLenum = 0x0C11;
const GL_DEPTH_TEST: GLenum = 0x0B71;
const GL_PACK_ALIGNMENT: GLenum = 0x0D05;

#[link(name = "OpenGL", kind = "framework")]
extern "C" {
    fn CGLChoosePixelFormat(
        attribs: *const i32,
        pix: *mut CGLPixelFormatObj,
        npix: *mut GLint,
    ) -> i32;
    fn CGLCreateContext(pix: CGLPixelFormatObj, share: CGLContextObj, ctx: *mut CGLContextObj)
        -> i32;
    fn CGLReleasePixelFormat(pix: CGLPixelFormatObj);
    fn CGLReleaseContext(ctx: CGLContextObj);
    fn CGLSetCurrentContext(ctx: CGLContextObj) -> i32;
    fn CGLGetCurrentContext() -> CGLContextObj;
    fn CGLTexImageIOSurface2D(
        ctx: CGLContextObj,
        target: GLenum,
        internal_format: GLenum,
        width: GLsizei,
        height: GLsizei,
        format: GLenum,
        type_: GLenum,
        io_surface: *mut c_void,
        plane: GLuint,
    ) -> i32;

    fn glGenTextures(n: GLsizei, textures: *mut GLuint);
    fn glDeleteTextures(n: GLsizei, textures: *const GLuint);
    fn glBindTexture(target: GLenum, texture: GLuint);
    fn glTexParameteri(target: GLenum, pname: GLenum, param: GLint);
    fn glTexImage2D(
        target: GLenum,
        level: GLint,
        internal_format: GLint,
        width: GLsizei,
        height: GLsizei,
        border: GLint,
        format: GLenum,
        type_: GLenum,
        data: *const c_void,
    );
    fn glGenerateMipmap(target: GLenum);
    fn glGetTexImage(target: GLenum, level: GLint, format: GLenum, type_: GLenum, out: *mut c_void);
    fn glActiveTexture(texture: GLenum);
    fn glPixelStorei(pname: GLenum, param: GLint);

    fn glGenFramebuffers(n: GLsizei, ids: *mut GLuint);
    fn glDeleteFramebuffers(n: GLsizei, ids: *const GLuint);
    fn glBindFramebuffer(target: GLenum, fbo: GLuint);
    fn glFramebufferTexture2D(
        target: GLenum,
        attachment: GLenum,
        textarget: GLenum,
        texture: GLuint,
        level: GLint,
    );
    fn glCheckFramebufferStatus(target: GLenum) -> GLenum;

    fn glCreateShader(kind: GLenum) -> GLuint;
    fn glDeleteShader(shader: GLuint);
    fn glShaderSource(
        shader: GLuint,
        count: GLsizei,
        sources: *const *const c_char,
        lengths: *const GLint,
    );
    fn glCompileShader(shader: GLuint);
    fn glGetShaderiv(shader: GLuint, pname: GLenum, out: *mut GLint);
    fn glGetShaderInfoLog(shader: GLuint, cap: GLsizei, len: *mut GLsizei, log: *mut c_char);
    fn glCreateProgram() -> GLuint;
    fn glDeleteProgram(program: GLuint);
    fn glAttachShader(program: GLuint, shader: GLuint);
    fn glLinkProgram(program: GLuint);
    fn glGetProgramiv(program: GLuint, pname: GLenum, out: *mut GLint);
    fn glGetProgramInfoLog(program: GLuint, cap: GLsizei, len: *mut GLsizei, log: *mut c_char);
    fn glUseProgram(program: GLuint);
    fn glGetUniformLocation(program: GLuint, name: *const c_char) -> GLint;
    fn glUniform1i(location: GLint, v: GLint);
    fn glUniform2f(location: GLint, x: f32, y: f32);

    fn glGenVertexArrays(n: GLsizei, ids: *mut GLuint);
    fn glDeleteVertexArrays(n: GLsizei, ids: *const GLuint);
    fn glBindVertexArray(id: GLuint);
    fn glDrawArrays(mode: GLenum, first: GLint, count: GLsizei);
    fn glViewport(x: GLint, y: GLint, w: GLsizei, h: GLsizei);
    fn glDisable(cap: GLenum);
    fn glFlush();
}

#[link(name = "CoreVideo", kind = "framework")]
extern "C" {
    fn CVPixelBufferGetIOSurface(buffer: *const c_void) -> *mut c_void;
}

#[link(name = "CoreFoundation", kind = "framework")]
extern "C" {
    fn CFGetRetainCount(cf: *const c_void) -> isize;
}

extern "C" {
    fn dlsym(handle: *mut c_void, symbol: *const c_char) -> *mut c_void;
}

/// `dlfcn.h` 的 `RTLD_DEFAULT`（mac 上是 -2）：在已加载的全部镜像里找。
const RTLD_DEFAULT: *mut c_void = -2isize as *mut c_void;

/// 给 mpv 的 `get_proc_address`：GL 函数都是 OpenGL.framework 的导出符号，
/// 我们自己链接了它，所以按名字在进程里找就行（mpv 不自己链接 GL）。
pub unsafe extern "C" fn get_proc_address(_ctx: *mut c_void, name: *const c_char) -> *mut c_void {
    if name.is_null() {
        return ptr::null_mut();
    }
    dlsym(RTLD_DEFAULT, name)
}

// ---- 着色器 ----

/// 一个盖满视口的三角形，不需要顶点数据（core profile 只要绑一个空 VAO）。
const VERTEX: &str = "#version 150
void main() {
    vec2 p = vec2(gl_VertexID == 1 ? 3.0 : -1.0, gl_VertexID == 2 ? 3.0 : -1.0);
    gl_Position = vec4(p, 0.0, 1.0);
}";

/// Y 平面：和 RGB 一样大，逐像素取。
const FRAGMENT_Y: &str = "#version 150
uniform sampler2D src;
out vec4 frag;
void main() {
    vec3 rgb = texelFetch(src, ivec2(gl_FragCoord.xy), 0).rgb;
    frag = vec4(dot(rgb, vec3(0.299, 0.587, 0.114)), 0.0, 0.0, 1.0);
}";

/// CbCr 平面：半尺寸。采样点落在 2×2 块的正中（四个 texel 的公共角），
/// 双线性正好取四者的平均。
const FRAGMENT_CBCR: &str = "#version 150
uniform sampler2D src;
uniform vec2 inv_size;
out vec4 frag;
void main() {
    vec3 rgb = textureLod(src, gl_FragCoord.xy * 2.0 * inv_size, 0.0).rgb;
    float cb = dot(rgb, vec3(-0.168736, -0.331264, 0.5)) + 0.5;
    float cr = dot(rgb, vec3(0.5, -0.418688, -0.081312)) + 0.5;
    frag = vec4(cb, cr, 0.0, 1.0);
}";

fn compile(kind: GLenum, source: &str) -> Result<GLuint, String> {
    let source = CString::new(source).map_err(|e| e.to_string())?;
    unsafe {
        let shader = glCreateShader(kind);
        let ptr = source.as_ptr();
        glShaderSource(shader, 1, &ptr, ptr::null());
        glCompileShader(shader);
        let mut ok = 0;
        glGetShaderiv(shader, GL_COMPILE_STATUS, &mut ok);
        if ok == 0 {
            let mut log = [0 as c_char; 1024];
            let mut len = 0;
            glGetShaderInfoLog(shader, log.len() as GLsizei, &mut len, log.as_mut_ptr());
            glDeleteShader(shader);
            return Err(format!(
                "着色器编译失败：{}",
                CStr::from_ptr(log.as_ptr()).to_string_lossy()
            ));
        }
        Ok(shader)
    }
}

/// 链接好的程序 + 它的 `src` 采样器固定在 0 号纹理单元。
struct Program {
    id: GLuint,
    inv_size: GLint,
}

impl Program {
    fn new(fragment: &str) -> Result<Self, String> {
        let vs = compile(GL_VERTEX_SHADER, VERTEX)?;
        let fs = compile(GL_FRAGMENT_SHADER, fragment).inspect_err(|_| unsafe { glDeleteShader(vs) })?;
        unsafe {
            let id = glCreateProgram();
            glAttachShader(id, vs);
            glAttachShader(id, fs);
            glLinkProgram(id);
            glDeleteShader(vs);
            glDeleteShader(fs);
            let mut ok = 0;
            glGetProgramiv(id, GL_LINK_STATUS, &mut ok);
            if ok == 0 {
                let mut log = [0 as c_char; 1024];
                let mut len = 0;
                glGetProgramInfoLog(id, log.len() as GLsizei, &mut len, log.as_mut_ptr());
                glDeleteProgram(id);
                return Err(format!(
                    "着色器链接失败：{}",
                    CStr::from_ptr(log.as_ptr()).to_string_lossy()
                ));
            }
            glUseProgram(id);
            glUniform1i(glGetUniformLocation(id, c"src".as_ptr()), 0);
            let inv_size = glGetUniformLocation(id, c"inv_size".as_ptr());
            glUseProgram(0);
            Ok(Self { id, inv_size })
        }
    }
}

/// mpv 画进去的那张 RGB 纹理。
struct RgbTarget {
    tex: GLuint,
    fbo: GLuint,
    width: usize,
    height: usize,
}

impl Drop for RgbTarget {
    fn drop(&mut self) {
        unsafe {
            glDeleteFramebuffers(1, &self.fbo);
            glDeleteTextures(1, &self.tex);
        }
    }
}

/// 一块输出缓冲：NV12 的 CVPixelBuffer + 绑在它两个平面上的纹理和 FBO。
struct Slot {
    buffer: PixelBuffer,
    textures: [GLuint; 2],
    fbos: [GLuint; 2],
    width: usize,
    height: usize,
    /// 最近一次被写的序号（挑空闲块时先用最旧的）
    used: u64,
}

impl Drop for Slot {
    fn drop(&mut self) {
        unsafe {
            glDeleteFramebuffers(2, self.fbos.as_ptr());
            glDeleteTextures(2, self.textures.as_ptr());
        }
    }
}

impl Slot {
    fn new(ctx: CGLContextObj, width: usize, height: usize) -> Result<Self, String> {
        let buffer = create_nv12_buffer(width, height)?;
        let surface = unsafe {
            CVPixelBufferGetIOSurface(CFRetained::as_ptr(&buffer).as_ptr() as *const c_void)
        };
        if surface.is_null() {
            return Err("CVPixelBuffer 没有 IOSurface 支撑".into());
        }
        let mut textures = [0; 2];
        let mut fbos = [0; 2];
        unsafe {
            glGenTextures(2, textures.as_mut_ptr());
            glGenFramebuffers(2, fbos.as_mut_ptr());
        }
        let slot = Self {
            buffer,
            textures,
            fbos,
            width,
            height,
            used: 0,
        };
        let planes = [
            (GL_R8, GL_RED, width, height),
            (GL_RG8, GL_RG, width / 2, height / 2),
        ];
        for (plane, (internal, format, w, h)) in planes.into_iter().enumerate() {
            unsafe {
                glBindTexture(GL_TEXTURE_RECTANGLE, slot.textures[plane]);
                let rc = CGLTexImageIOSurface2D(
                    ctx,
                    GL_TEXTURE_RECTANGLE,
                    internal,
                    w as GLsizei,
                    h as GLsizei,
                    format,
                    GL_UNSIGNED_BYTE,
                    surface,
                    plane as GLuint,
                );
                if rc != 0 {
                    return Err(format!("CGLTexImageIOSurface2D 失败（平面 {plane}，{rc}）"));
                }
                glBindFramebuffer(GL_FRAMEBUFFER, slot.fbos[plane]);
                glFramebufferTexture2D(
                    GL_FRAMEBUFFER,
                    GL_COLOR_ATTACHMENT0,
                    GL_TEXTURE_RECTANGLE,
                    slot.textures[plane],
                    0,
                );
                if glCheckFramebufferStatus(GL_FRAMEBUFFER) != GL_FRAMEBUFFER_COMPLETE {
                    return Err(format!("NV12 平面 {plane} 的 FBO 不完整"));
                }
            }
        }
        unsafe {
            glBindTexture(GL_TEXTURE_RECTANGLE, 0);
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
        }
        Ok(slot)
    }

    /// 除了池子自己，还有没有人（界面 / GPUI 的场景）攥着它。
    fn in_use(&self) -> bool {
        unsafe { CFGetRetainCount(CFRetained::as_ptr(&self.buffer).as_ptr() as *const c_void) > 1 }
    }
}

/// IOSurface 支撑、GL 和 Metal 都能直接用的 NV12 缓冲。
fn create_nv12_buffer(width: usize, height: usize) -> Result<PixelBuffer, String> {
    let empty = CFDictionary::<CFString, CFType>::from_slices(&[], &[]);
    let yes: &CFType = CFBoolean::new(true);
    let (keys, values): ([&CFString; 3], [&CFType; 3]) = unsafe {
        (
            [
                kCVPixelBufferIOSurfacePropertiesKey,
                kCVPixelBufferOpenGLCompatibilityKey,
                kCVPixelBufferMetalCompatibilityKey,
            ],
            [&empty, yes, yes],
        )
    };
    let attributes = CFDictionary::<CFString, CFType>::from_slices(&keys, &values);
    let mut out: *mut CVPixelBuffer = ptr::null_mut();
    let rc = unsafe {
        CVPixelBufferCreate(
            None,
            width,
            height,
            kCVPixelFormatType_420YpCbCr8BiPlanarFullRange,
            Some(attributes.as_opaque()),
            NonNull::from(&mut out),
        )
    };
    match NonNull::new(out) {
        Some(buffer) if rc == kCVReturnSuccess => Ok(unsafe { CFRetained::from_raw(buffer) }),
        _ => Err(format!("CVPixelBufferCreate 失败（{rc}）")),
    }
}

/// 离屏 GL 上下文 + 换算着色器 + 输出缓冲池。
pub struct GlTarget {
    ctx: CGLContextObj,
    luma: Program,
    chroma: Program,
    vao: GLuint,
    rgb: Option<RgbTarget>,
    slots: Vec<Slot>,
    serial: u64,
}

impl GlTarget {
    pub fn new() -> Result<Self, String> {
        let ctx = create_context()?;
        unsafe { CGLSetCurrentContext(ctx) };
        let programs = Program::new(FRAGMENT_Y).and_then(|luma| Ok((luma, Program::new(FRAGMENT_CBCR)?)));
        let (luma, chroma) = match programs {
            Ok(programs) => programs,
            Err(err) => {
                unsafe {
                    CGLSetCurrentContext(ptr::null_mut());
                    CGLReleaseContext(ctx);
                }
                return Err(err);
            }
        };
        let mut vao = 0;
        unsafe { glGenVertexArrays(1, &mut vao) };
        Ok(Self {
            ctx,
            luma,
            chroma,
            vao,
            rgb: None,
            slots: Vec::new(),
            serial: 0,
        })
    }

    pub fn make_current(&self) {
        unsafe {
            if CGLGetCurrentContext() != self.ctx {
                CGLSetCurrentContext(self.ctx);
            }
        }
    }

    /// 准备好 `width × height` 的 RGB 目标，返回给 mpv 画的 FBO。
    ///
    /// NV12 的色度是半尺寸，宽高必须是偶数——调用方给的奇数会被向下取偶。
    pub fn begin(&mut self, width: usize, height: usize) -> Result<(GLuint, usize, usize), String> {
        self.make_current();
        let width = (width & !1).max(2);
        let height = (height & !1).max(2);
        let fits = self
            .rgb
            .as_ref()
            .is_some_and(|rgb| rgb.width == width && rgb.height == height);
        if !fits {
            self.rgb = None;
            self.rgb = Some(create_rgb_target(width, height)?);
        }
        let fbo = self.rgb.as_ref().map(|rgb| rgb.fbo).unwrap_or(0);
        Ok((fbo, width, height))
    }

    /// mpv 画完之后：换算进一块空闲的 NV12 缓冲，交出去。
    pub fn finish(&mut self) -> Result<PixelBuffer, String> {
        self.make_current();
        let Some(rgb) = self.rgb.as_ref() else {
            return Err("还没有渲染目标".into());
        };
        let (width, height, src) = (rgb.width, rgb.height, rgb.tex);
        let index = self.pick_slot(width, height)?;
        self.serial += 1;
        let slot = &mut self.slots[index];
        slot.used = self.serial;
        unsafe {
            // mpv 会随手改状态（混合、裁剪、绑定），我们要的每一项都重新设一遍
            glDisable(GL_BLEND);
            glDisable(GL_SCISSOR_TEST);
            glDisable(GL_DEPTH_TEST);
            glBindVertexArray(self.vao);
            glActiveTexture(GL_TEXTURE0);
            glBindTexture(GL_TEXTURE_2D, src);

            glBindFramebuffer(GL_FRAMEBUFFER, slot.fbos[0]);
            glViewport(0, 0, width as GLsizei, height as GLsizei);
            glUseProgram(self.luma.id);
            glDrawArrays(GL_TRIANGLES, 0, 3);

            glBindFramebuffer(GL_FRAMEBUFFER, slot.fbos[1]);
            glViewport(0, 0, (width / 2) as GLsizei, (height / 2) as GLsizei);
            glUseProgram(self.chroma.id);
            glUniform2f(self.chroma.inv_size, 1.0 / width as f32, 1.0 / height as f32);
            glDrawArrays(GL_TRIANGLES, 0, 3);

            glUseProgram(0);
            glBindVertexArray(0);
            glBindTexture(GL_TEXTURE_2D, 0);
            glBindFramebuffer(GL_FRAMEBUFFER, 0);
            // IOSurface 自带跨 API 的 GPU 同步：提交出去即可，不必 glFinish 卡住 UI 线程
            glFlush();
        }
        Ok(slot.buffer.clone())
    }

    /// 读回一张长边不超过 `max_long` 的缩小图（BGRA、从上到下）给液态玻璃。
    pub fn snapshot(&mut self, max_long: usize, out: &mut FrameBuffer) -> bool {
        self.make_current();
        let Some(rgb) = self.rgb.as_ref() else {
            return false;
        };
        let max_long = max_long.max(1);
        let (mut level, mut w, mut h) = (0, rgb.width, rgb.height);
        while w.max(h) > max_long && w.max(h) > 1 {
            level += 1;
            w = (w / 2).max(1);
            h = (h / 2).max(1);
        }
        out.ensure(w, h);
        unsafe {
            glBindTexture(GL_TEXTURE_2D, rgb.tex);
            if level > 0 {
                glGenerateMipmap(GL_TEXTURE_2D);
            }
            glPixelStorei(GL_PACK_ALIGNMENT, 4);
            glGetTexImage(
                GL_TEXTURE_2D,
                level,
                GL_BGRA,
                GL_UNSIGNED_INT_8_8_8_8_REV,
                out.pixels_ptr() as *mut c_void,
            );
            glBindTexture(GL_TEXTURE_2D, 0);
        }
        out.set_opaque();
        true
    }

    /// 挑一块能写的：同尺寸、没人攥着、最久没用的。都被攥着就新开，到上限了
    /// 只能覆盖最旧的那块（最坏也只是那一帧撕一下）。
    fn pick_slot(&mut self, width: usize, height: usize) -> Result<usize, String> {
        self.slots.retain(|slot| slot.width == width && slot.height == height);
        let free = self
            .slots
            .iter()
            .enumerate()
            .filter(|(_, slot)| !slot.in_use())
            .min_by_key(|(_, slot)| slot.used)
            .map(|(index, _)| index);
        if let Some(index) = free {
            return Ok(index);
        }
        if self.slots.len() < MAX_SLOTS {
            self.slots.push(Slot::new(self.ctx, width, height)?);
            return Ok(self.slots.len() - 1);
        }
        Ok(self
            .slots
            .iter()
            .enumerate()
            .min_by_key(|(_, slot)| slot.used)
            .map(|(index, _)| index)
            .unwrap_or(0))
    }
}

impl Drop for GlTarget {
    fn drop(&mut self) {
        self.make_current();
        self.slots.clear();
        self.rgb = None;
        unsafe {
            glDeleteVertexArrays(1, &self.vao);
            glDeleteProgram(self.luma.id);
            glDeleteProgram(self.chroma.id);
            CGLSetCurrentContext(ptr::null_mut());
            CGLReleaseContext(self.ctx);
        }
    }
}

/// 3.2 core 的离屏上下文。允许集显：双显卡机器上不因为开了个 GL 上下文就切独显。
fn create_context() -> Result<CGLContextObj, String> {
    let attributes = [
        KCGL_PFA_OPENGL_PROFILE,
        KCGL_OGLP_VERSION_3_2_CORE,
        KCGL_PFA_ACCELERATED,
        KCGL_PFA_ALLOW_OFFLINE_RENDERERS,
        KCGL_PFA_SUPPORTS_AUTOMATIC_GRAPHICS_SWITCHING,
        0,
    ];
    let mut pix: CGLPixelFormatObj = ptr::null_mut();
    let mut count = 0;
    let rc = unsafe { CGLChoosePixelFormat(attributes.as_ptr(), &mut pix, &mut count) };
    if rc != 0 || pix.is_null() {
        return Err(format!("CGLChoosePixelFormat 失败（{rc}）"));
    }
    let mut ctx: CGLContextObj = ptr::null_mut();
    let rc = unsafe { CGLCreateContext(pix, ptr::null_mut(), &mut ctx) };
    unsafe { CGLReleasePixelFormat(pix) };
    if rc != 0 || ctx.is_null() {
        return Err(format!("CGLCreateContext 失败（{rc}）"));
    }
    Ok(ctx)
}

fn create_rgb_target(width: usize, height: usize) -> Result<RgbTarget, String> {
    let mut tex = 0;
    let mut fbo = 0;
    unsafe {
        glGenTextures(1, &mut tex);
        glGenFramebuffers(1, &mut fbo);
    }
    let target = RgbTarget {
        tex,
        fbo,
        width,
        height,
    };
    unsafe {
        glBindTexture(GL_TEXTURE_2D, tex);
        glTexImage2D(
            GL_TEXTURE_2D,
            0,
            GL_RGBA8,
            width as GLsizei,
            height as GLsizei,
            0,
            GL_RGBA,
            GL_UNSIGNED_BYTE,
            ptr::null(),
        );
        // 换算只读第 0 级；mipmap 是给玻璃回读用的，不参与采样
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MIN_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_MAG_FILTER, GL_LINEAR);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_S, GL_CLAMP_TO_EDGE);
        glTexParameteri(GL_TEXTURE_2D, GL_TEXTURE_WRAP_T, GL_CLAMP_TO_EDGE);
        glBindTexture(GL_TEXTURE_2D, 0);
        glBindFramebuffer(GL_FRAMEBUFFER, fbo);
        glFramebufferTexture2D(GL_FRAMEBUFFER, GL_COLOR_ATTACHMENT0, GL_TEXTURE_2D, tex, 0);
        let status = glCheckFramebufferStatus(GL_FRAMEBUFFER);
        glBindFramebuffer(GL_FRAMEBUFFER, 0);
        if status != GL_FRAMEBUFFER_COMPLETE {
            return Err(format!("RGB 渲染目标不完整（0x{status:X}）"));
        }
    }
    Ok(target)
}
