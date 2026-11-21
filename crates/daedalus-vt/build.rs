fn main() {
    println!("cargo:rerun-if-env-changed=GHOSTTY_VT_DIR");

    #[cfg(feature = "ghostty-vt")]
    {
        use std::path::PathBuf;

        let include_paths: Vec<PathBuf> = if let Ok(dir) = std::env::var("GHOSTTY_VT_DIR") {
            let root = PathBuf::from(dir);
            println!(
                "cargo:rustc-link-search=native={}",
                root.join("lib").display()
            );
            println!("cargo:rustc-link-lib=ghostty-vt");
            vec![root.join("include")]
        } else {
            let lib = pkg_config::Config::new()
                .probe("libghostty-vt")
                .expect(
                    "libghostty-vt not found via pkg-config. Install libghostty-vt or set GHOSTTY_VT_DIR=/path/to/prefix (with include/ghostty/vt.h and lib/).",
                );
            assert!(
                !lib.include_paths.is_empty(),
                "pkg-config returned no include paths for libghostty-vt"
            );
            lib.include_paths
        };

        let header = include_paths
            .iter()
            .map(|dir| dir.join("ghostty/vt.h"))
            .find(|path| path.exists())
            .unwrap_or_else(|| include_paths[0].join("ghostty/vt.h"));
        assert!(
            header.exists(),
            "Expected libghostty-vt header at {}",
            header.display()
        );
        println!("cargo:rerun-if-changed={}", header.display());

        let mut builder = bindgen::Builder::default()
            .header(header.to_string_lossy())
            .allowlist_function("ghostty_.*")
            .allowlist_type("Ghostty.*");
        for dir in &include_paths {
            builder = builder.clang_arg(format!("-I{}", dir.display()));
        }

        let bindings = builder
            .generate()
            .expect("Unable to generate bindgen bindings for ghostty/vt.h");

        let out_path = PathBuf::from(std::env::var("OUT_DIR").unwrap());
        bindings
            .write_to_file(out_path.join("ghostty_vt_bindings.rs"))
            .expect("Couldn't write ghostty_vt_bindings.rs");
    }
}
