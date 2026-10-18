//! 构建期把应用图标（+ 版本信息）写进 PE 资源。
//!
//! 为什么必须有这一步：GPUI 的 Windows 后端在注册窗口类时调用
//! `LoadImageW(module, MAKEINTRESOURCE(1), IMAGE_ICON, …)`（见 `gpui-pre-windows`
//! 的 `load_icon`），**只认 ID = 1 的图标组**。exe 里没有这条资源时它拿到空句柄，
//! 窗口类就没有图标，任务栏和 Alt+Tab 里显示的是 Windows 的通用占位图。
//!
//! 资源脚本在构建目录里现场生成，而不是仓库里放一个 `.rc`：图标路径必须是绝对的，
//! 否则 rc.exe 的解析基准会随工作目录漂移；版本号也想跟着 `Cargo.toml` 走。
//!
//! 找不到 rc.exe（没装 Windows SDK / 非 Windows 目标）时只告警不失败——
//! 别的平台本来就不需要这段资源，不该因为它挂掉构建。

use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=build.rs");

    // 非 Windows 目标直接跳过（Linux/macOS 的图标由打包脚本处理）
    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }

    let manifest = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR"));
    let icon = manifest.join("../../assets/chroma.ico");
    println!("cargo:rerun-if-changed={}", icon.display());

    let icon = match std::fs::canonicalize(&icon) {
        Ok(path) => path,
        Err(err) => {
            // 图标缺了不该阻止编译：界面照常能用，只是任务栏没 logo
            println!(
                "cargo:warning=跳过图标资源，找不到 {}: {err}",
                icon.display()
            );
            return;
        }
    };

    // 去掉 `\\?\` 前缀并把分隔符换成 `/`。
    //
    // `canonicalize` 在 Windows 上返回的是 `\\?\C:\...` 这种「扩展长度路径」，
    // 直接写进 `.rc` 会被 rc.exe 的字符串转义吃掉（`\\` 是转义序列），
    // 最后 rc 报的是「找不到文件」——踩过一次。
    let icon_posix = icon
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .replace('\\', "/");

    let out_dir = PathBuf::from(std::env::var("OUT_DIR").expect("OUT_DIR"));
    let rc_path = out_dir.join("chroma.rc");
    let version = std::env::var("CARGO_PKG_VERSION").unwrap_or_else(|_| "0.0.0".into());
    let dotted = version.replace('.', ",");
    let version_tuple = format!("{dotted},0");

    // 图标路径用正斜杠：rc.exe 走的是标准 Win32 文件 API，认 `/`。
    // 图标 ID 必须是 1，理由见文件头。
    let rc = format!(
        r#"1 ICON "{icon}"

1 VERSIONINFO
FILEVERSION {version_tuple}
PRODUCTVERSION {version_tuple}
FILEOS 0x40004L
FILETYPE 0x1L
BEGIN
  BLOCK "StringFileInfo"
  BEGIN
    BLOCK "040904b0"
    BEGIN
      VALUE "FileDescription", "Chroma Media Player"
      VALUE "FileVersion", "{version}"
      VALUE "InternalName", "simple-media-player"
      VALUE "OriginalFilename", "simple-media-player.exe"
      VALUE "ProductName", "Chroma"
      VALUE "ProductVersion", "{version}"
    END
  END
  BLOCK "VarFileInfo"
  BEGIN
    VALUE "Translation", 0x409, 1200
  END
END
"#,
        icon = icon_posix
    );

    if let Err(err) = std::fs::write(&rc_path, rc) {
        println!("cargo:warning=写资源脚本失败: {err}");
        return;
    }

    match embed_resource::compile(&rc_path, embed_resource::NONE) {
        embed_resource::CompilationResult::Ok => {}
        _ => println!("cargo:warning=图标资源未嵌入（rc.exe 不可用？），任务栏将显示系统占位图"),
    }
}
