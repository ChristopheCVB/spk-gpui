use std::{env, process};

#[path = "build/endpoints.rs"]
mod endpoints;

fn main() {
    println!("cargo::rerun-if-changed=build.rs");
    println!("cargo::rerun-if-changed=build/endpoints.rs");
    println!("cargo::rerun-if-env-changed=SPK_URL");
    println!("cargo::rerun-if-env-changed=LIVEKIT_URL");
    match env::var("CARGO_CFG_TARGET_OS").as_deref() {
        Ok("macos") => println!("cargo::rustc-link-arg=-ObjC"),
        Ok("linux") => {
            for library in ["gio-2.0", "gobject-2.0", "glib-2.0"] {
                println!("cargo::rustc-link-lib=dylib={library}");
            }
        }
        Ok("windows") => {
            println!("cargo::rerun-if-changed=build/windows.rc");
            println!("cargo::rerun-if-changed=assets/logo.ico");
            embed_resource::compile_for(
                "build/windows.rc",
                ["spk-gpui"],
                embed_resource::ParamsIncludeDirs(["assets"]),
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
