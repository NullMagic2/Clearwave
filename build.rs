//! Embed the Windows common-controls/DPI manifest and existing artwork.
use std::{env, path::PathBuf, process::Command};
fn main() {
    println!("cargo:rerun-if-changed=packaging/windows");
    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
        return;
    }
    let out = PathBuf::from(env::var_os("OUT_DIR").expect("Cargo sets OUT_DIR"));
    let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
    let output = out.join(if msvc {
        "clearwave.res"
    } else {
        "clearwave-res.o"
    });
    let status = if msvc {
        Command::new("rc.exe").arg("/nologo").arg("/Ipackaging/windows")
            .arg(format!("/fo{}", output.display())).arg("packaging/windows/clearwave.rc").status()
    } else {
        let tool = env::var_os("WINDRES").unwrap_or_else(|| "x86_64-w64-mingw32-windres".into());
        Command::new(tool).args(["-I", "packaging/windows", "-i", "packaging/windows/clearwave.rc",
            "-O", "coff", "-o"]).arg(&output).status()
    }.expect("Windows resource compiler unavailable; use a Visual Studio Developer shell or install MinGW windres");
    assert!(status.success(), "Windows resource compilation failed");
    println!("cargo:rustc-link-arg-bin=clearwave={}", output.display());
}
