fn main() {
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_DIR");

    #[cfg(feature = "ghostty-vt")]
    {
        use std::path::PathBuf;

        let include_dir = if let Ok(dir) = std::env::var("GHOSTTY_VT_DIR") {
            let root = PathBuf::from(dir);
            println!("cargo:rustc-link-search=native={}", root.join("lib").display());
            println!("cargo:rustc-link-lib=ghostty-vt");
            root.join("include")
        } else {
            let lib = pkg_config::Config::new()
                .probe("libghostty-vt")
                .expect(
                    "libghostty-vt not found via pkg-config. Install libghostty-vt or set GHOSTTY_VT_DIR=/path/to/prefix (with include/ghostty/vt.h and lib/).",
                );
            lib.include_paths
                .into_iter()
                .next()
                .expect("pkg-config returned no include paths for libghostty-vt")
        };

        let header = include_dir.join("ghostty/vt.h");
        assert!(
            header.exists(),
            "Expected libghostty-vt header at {}",
            header.display()
        );

        let bindings = bindgen::Builder::default()
            .header(header.to_string_lossy())
            .allowlist_function("ghostty_.*")
            .allowlist_type("Ghostty.*")
            .generate()
            .expect("Unable to generate bindgen bindings for ghostty/vt.h");

        let out_path = PathBuf::from(std::env::var("OUT_DIR").unwrap());
        bindings
            .write_to_file(out_path.join("ghostty_vt_bindings.rs"))
            .expect("Couldn't write ghostty_vt_bindings.rs");
    }
}
