// Copyright (c) 2026 Joran
// SPDX-License-Identifier: MIT

//! Windows 资源：把 appicon.ico 嵌入可执行文件（Explorer/任务栏图标）。

use std::env;
use std::fs;
use std::path::PathBuf;

fn main() {
    println!("cargo:rerun-if-changed=assets/appicon.ico");

    #[cfg(target_os = "windows")]
    {
        let manifest_dir = env::var("CARGO_MANIFEST_DIR").expect("CARGO_MANIFEST_DIR 未设置");
        let icon = PathBuf::from(&manifest_dir).join("assets/appicon.ico");
        let icon_escaped = icon.to_string_lossy().replace('\\', "\\\\");
        let rc = format!(
            "// 由 build.rs 自动生成\n\
             #include <winres.h>\n\
             #pragma code_page(65001)\n\
             1 ICON \"{icon_escaped}\"\n"
        );
        let out_dir = PathBuf::from(env::var("OUT_DIR").expect("OUT_DIR 未设置"));
        let rc_path = out_dir.join("app.rc");
        fs::write(&rc_path, rc).expect("写入 app.rc 失败");
        embed_resource::compile(rc_path, embed_resource::NONE)
            .manifest_optional()
            .expect("嵌入应用图标资源失败");
    }
}
