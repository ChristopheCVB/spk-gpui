use std::{env, process};

#[path = "build/endpoints.rs"]
mod endpoints;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=build/endpoints.rs");
    println!("cargo::rerun-if-env-changed=SPK_URL");
    println!("cargo::rerun-if-env-changed=LIVEKIT_URL");
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => {
            println!("cargo::rustc-link-arg=-ObjC");
            // The Swift bridge is a library, so its linker arguments do not reach the executable.
            println!("cargo::rustc-link-arg=-Wl,-rpath,/usr/lib/swift");
        }
        Ok("linux") => {
            compile_pipewire_capture();
            for library in ["gio-2.0", "gobject-2.0", "glib-2.0"] {
                println!("cargo::rustc-link-lib=dylib={library}");
            }
        }
        Ok("windows") => {
            println!("cargo::rerun-if-changed=build/windows.rc");
            println!("cargo::rerun-if-changed=assets/logo.ico");
            println!("cargo::rerun-if-changed=build/media-licenses.txt");
            embed_resource::compile_for(
                "build/windows.rc",
                ["spk-gpui"],
                embed_resource::ParamsIncludeDirs(["assets", "build"]),
            )
            .manifest_required()
            .expect("Could not embed the Windows app icon");
        }
        _ => {}
    }

    let allow_loopback = env::var("PROFILE").as_deref() == Ok("debug");
    let (spk, websocket) = endpoints::spk(&required_var("SPK_URL"), allow_loopback)
        .unwrap_or_else(|message| fail("SPK_URL", message));
    let livekit = endpoints::livekit(&required_var("LIVEKIT_URL"), allow_loopback)
        .unwrap_or_else(|message| fail("LIVEKIT_URL", message));

    println!("cargo::rustc-env=SPK_URL={spk}");
    println!("cargo::rustc-env=SPK_WS_URL={websocket}");
    println!("cargo::rustc-env=LIVEKIT_URL={livekit}");
}

fn compile_pipewire_capture() {
    println!("cargo::rerun-if-changed=build/pipewire-capture.cpp");
    println!("cargo::rerun-if-env-changed=LK_CUSTOM_WEBRTC");
    println!("cargo::rerun-if-env-changed=LK_DEBUG_WEBRTC");
    webrtc_sys_build::download_webrtc().expect("Could not obtain the LiveKit WebRTC SDK");
    let include = webrtc_sys_build::webrtc_dir().join("include");
    let mut compiler = cc::Build::new();
    compiler
        .cpp(true)
        .std("c++20")
        .flag("-nostdinc++")
        .flag(format!(
            "-isystem{}",
            include.join("third_party/libc++/src/include").display()
        ))
        .flag(format!(
            "-isystem{}",
            include.join("third_party/libc++abi/src/include").display()
        ))
        .include(include.join("buildtools/third_party/libc++"))
        .include(include.join("third_party/abseil-cpp"))
        .include(include)
        .cpp_link_stdlib(None)
        .warnings(false)
        .file("build/pipewire-capture.cpp");
    if env::var_os("CXX").is_none() {
        compiler.compiler("clang++");
    }
    let gio = pkg_config::Config::new()
        .cargo_metadata(false)
        .probe("gio-2.0")
        .expect("Could not find GIO headers for the WebRTC SDK");
    compiler.includes(gio.include_paths);
    // Use the same C++ ABI and feature defines as webrtc-sys.
    for (name, value) in webrtc_sys_build::webrtc_defines() {
        compiler.define(&name, value.as_deref());
    }
    compiler.compile("spk-pipewire-capture");
}

fn required_var(name: &str) -> String {
    match env::var(name) {
        Ok(value) if !value.trim().is_empty() => value,
        Ok(_) | Err(env::VarError::NotPresent) => fail(name, "set this build variable"),
        Err(env::VarError::NotUnicode(_)) => fail(name, "expected a UTF-8 URL"),
    }
}

fn fail(name: &str, message: &str) -> ! {
    eprintln!("{name}: {message}");
    process::exit(1);
}
