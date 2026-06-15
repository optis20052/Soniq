fn main() {
    // Emit the linker search path + `-lmpv` for the system libmpv. On macOS
    // (Homebrew) and most Linux distros the .pc file is found via pkg-config;
    // this is what makes the build portable across platforms.
    pkg_config::Config::new()
        .atleast_version("2.0")
        .probe("mpv")
        .expect("libmpv not found via pkg-config (install mpv / libmpv-dev)");

    // Compile the UI with the dark Fluent widget style so the std-widgets we use
    // (LineEdit, ListView) match the app's dark theme instead of the light
    // platform default (a white box with a red Material underline).
    let cfg = slint_build::CompilerConfiguration::new().with_style("fluent-dark".into());
    slint_build::compile_with_config("ui/app.slint", cfg).expect("slint compile failed");
}
