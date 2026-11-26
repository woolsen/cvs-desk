//! Embeds the application icon into the built programs.
//!
//! Explorer, the Start menu and shortcut files read the icon from the executable's
//! resources, so it has to be compiled in at link time. The running window sets its own
//! icon from the same artwork in `main.rs`.

fn main() {
    println!("cargo:rerun-if-changed=assets/cvsdesk.ico");
    println!("cargo:rerun-if-changed=assets/cvsdesk.rc");

    if std::env::var("CARGO_CFG_TARGET_OS").as_deref() == Ok("windows") {
        match embed_resource::compile("assets/cvsdesk.rc", embed_resource::NONE) {
            embed_resource::CompilationResult::Ok => {}
            other => panic!("could not compile assets/cvsdesk.rc: {other:?}"),
        }
    }
}
