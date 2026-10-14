use std::io;
use std::path::{Path, PathBuf};

fn main() {
    if std::env::var("DOCS_RS").is_ok() {
        // Don't link with libheif in case of building documentation for docs.rs.
        return;
    }

    println!("cargo:rerun-if-changed=build.rs");
    println!("cargo:rerun-if-changed=wrapper.h");

    // https://github.com/Cykooz/libheif-sys/issues/14
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_env = std::env::var("CARGO_CFG_TARGET_ENV").unwrap_or_default();
    let windows_is_build_target = target_os == "windows" && target_env == "msvc";

    #[allow(unused_variables)]
    let include_paths = if windows_is_build_target {
        #[cfg(all(target_os = "windows", target_env = "msvc"))]
        install_libheif_by_vcpkg();
        Vec::new()
    } else {
        find_libheif()
    };

    #[cfg(feature = "use-bindgen")]
    run_bindgen(&include_paths);
}

#[allow(dead_code)]
fn prepare_libheif_src() -> PathBuf {
    let out_path = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let crate_dir = PathBuf::from(std::env::var("CARGO_MANIFEST_DIR").unwrap());
    let libheif_dir = crate_dir.join("vendor/libheif");
    let dst_dir = out_path.join("libheif");
    copy_dir_all(libheif_dir, &dst_dir).unwrap();

    // Patch CMakeLists.txt to disable a building `heifio` library
    // that is used for example applications.
    let cmake_lists_path = dst_dir.join("CMakeLists.txt");
    let mut contents =
        std::fs::read_to_string(&cmake_lists_path).expect("failed to read libheif/CMakeLists.txt");
    contents = contents.replace("add_subdirectory(heifio)", "");
    std::fs::write(&cmake_lists_path, contents).expect("failed to write libheif/CMakeLists.txt");
    dst_dir
}

#[cfg(feature = "embedded-libheif")]
fn compile_libheif() -> String {
    use std::path::PathBuf;

    let out_path = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    let libheif_dir = prepare_libheif_src();

    let mut build_config = cmake::Config::new(libheif_dir);
    build_config.out_dir(out_path.join("libheif_build"));
    build_config.define("CMAKE_INSTALL_LIBDIR", "lib");

    // Disable some options
    for key in [
        "BUILD_SHARED_LIBS",
        "BUILD_TESTING",
        "WITH_GDK_PIXBUF",
        "WITH_EXAMPLES",
        "WITH_EXAMPLE_HEIF_THUMB",
        "WITH_EXAMPLE_HEIF_VIEW",
        "ENABLE_EXPERIMENTAL_FEATURES",
        "ENABLE_PLUGIN_LOADING",
        "BUILD_DOCUMENTATION",
    ] {
        build_config.define(key, "OFF");
    }

    // Enable some options
    for key in [
        "WITH_REDUCED_VISIBILITY",
        "WITH_LIBSHARPYUV",
        // TODO: Try to enable this in the future.
        //       Right now it is the reason of linker's errors like
        //       "undefined reference to `BrotliEncoderDestroyInstance'"
        // "WITH_UNCOMPRESSED_CODEC",
        // "WITH_HEADER_COMPRESSION",
    ] {
        build_config.define(key, "ON");
    }

    // List of encoders and decoders that have corresponding plugins
    let encoders_decoders = [
        "AOM_DECODER",
        "AOM_ENCODER",
        "DAV1D",
        "LIBDE265",
        "RAV1E",
        "SvtEnc",
        "X264",
        "X265",
        "JPEG_DECODER",
        "JPEG_ENCODER",
        "KVAZAAR",
        "OPENJPH_ENCODER",
        "OpenJPEG_DECODER",
        "OpenJPEG_ENCODER",
        "OPEN_JPH_ENCODER",
        "FFMPEG_DECODER",
        "OpenH264_DECODER",
        "UVG266",
        "VVDEC",
        "VVENC",
    ];

    let disabled_enc_dec = [
        // A lot of errors like "undefined reference to `av_packet_free'" during linking
        "FFMPEG_DECODER",
    ];

    // Enable or disable encoders and decoders
    for key in encoders_decoders {
        let v = if disabled_enc_dec.contains(&key) {
            "OFF"
        } else {
            "ON"
        };
        build_config.define(format!("WITH_{}", key), v);

        // Disable external plugin
        build_config.define(format!("WITH_{}_PLUGIN", key), "OFF");
    }

    let libheif_build = build_config.build();

    libheif_build
        .join("lib/pkgconfig")
        .to_string_lossy()
        .to_string()
}

fn find_libheif() -> Vec<String> {
    #[allow(unused_mut)]
    let mut config = system_deps::Config::new();

    #[cfg(feature = "embedded-libheif")]
    {
        std::env::set_var("SYSTEM_DEPS_LIBHEIF_BUILD_INTERNAL", "always");
        config = config.add_build_internal("libheif", |lib, version| {
            let pc_file_path = compile_libheif();
            system_deps::Library::from_internal_pkg_config(pc_file_path, lib, version)
        });
    }

    use system_deps::Error;

    match config.probe() {
        Ok(deps) => deps
            .all_include_paths()
            .iter()
            .filter_map(|p| p.to_str())
            .map(|p| p.to_string())
            .collect(),
        Err(err) => {
            let err_msg = match &err {
                Error::InvalidMetadata(msg) => {
                    if msg.contains("No version") && msg.contains("libheif") {
                        "You MUST enable one of the crate features to specify \
                    minimal supported version of 'libheif' API (e.g. v1_17)."
                            .to_string()
                    } else {
                        err.to_string()
                    }
                }
                _ => err.to_string(),
            };
            println!("cargo:error={err_msg}");
            std::process::exit(1);
        }
    }
}

#[cfg(all(target_os = "windows", target_env = "msvc"))]
fn install_libheif_by_vcpkg() {
    if let Err(err) = setup_vcpkg_env_from_lib_resolver() {
        println!("cargo:warning={err}");
        std::process::exit(1);
    }

    let vcpkg_lib = vcpkg::Config::new()
        .emit_includes(true)
        .find_package("libheif");
    if let Err(err) = vcpkg_lib {
        println!("cargo:warning={}", err);
        std::process::exit(1);
    }
    // Static libraries on Windows do not automatically pull in all of their own
    // system-library dependencies. Since x265 uses registry functions from Advapi32,
    // your Rust crate must explicitly tell Cargo to link that Windows system library.
    println!("cargo:rustc-link-lib=advapi32");
}

#[cfg(all(target_os = "windows", target_env = "msvc"))]
fn setup_vcpkg_env_from_lib_resolver() -> Result<(), String> {
    let workspace = std::env::var_os("CARGO_WORKSPACE_DIR").map(PathBuf::from);
    let vcpkg_root = std::env::var_os("VCPKG_ROOT").map(PathBuf::from);
    if let Some(root) = &vcpkg_root {
        let Some(workspace) = &workspace else {
            return Ok(());
        };
        let root = if root.is_absolute() {
            root.clone()
        } else {
            workspace.join(root)
        };
        if !same_path(&root, &workspace.join(".vcpkg")) {
            return Ok(());
        }
    }

    let resolved = lib_resolver::Resolver::new("libheif")
        .vcpkg("libheif")
        .order(&[lib_resolver::Strategy::Vcpkg])
        .no_metadata()
        .probe();
    let lib = match resolved {
        Ok(lib) => lib,
        Err(err) => {
            println!("cargo:warning=lib-resolver could not locate libheif; trying the configured vcpkg installation: {err}");
            return Ok(());
        }
    };
    let Some(installed) = lib.vcpkg_installed() else {
        println!("cargo:warning=lib-resolver did not use vcpkg for libheif; trying the configured vcpkg installation");
        return Ok(());
    };
    let triplet = installed
        .file_name()
        .ok_or_else(|| format!("invalid vcpkg install path: {}", installed.display()))?;
    let installed_root = installed
        .parent()
        .ok_or_else(|| format!("invalid vcpkg install path: {}", installed.display()))?;
    let target_dir = installed_root
        .parent()
        .ok_or_else(|| format!("invalid vcpkg install path: {}", installed.display()))?;
    let vcpkg_root = match (vcpkg_root, workspace) {
        (Some(root), Some(workspace)) if root.is_relative() => workspace.join(root),
        (Some(root), _) => root,
        (None, _) => target_dir.join(".vcpkg"),
    };
    if !vcpkg_root.join("scripts/buildsystems/vcpkg.cmake").exists() {
        return Err(format!(
            "could not find the vcpkg checkout at {}",
            vcpkg_root.display()
        ));
    }

    link_vcpkg_installed(installed_root, &vcpkg_root.join("installed"))?;
    std::env::set_var("VCPKG_ROOT", vcpkg_root.as_os_str());
    std::env::set_var("VCPKGRS_TRIPLET", triplet);
    Ok(())
}

#[cfg(all(target_os = "windows", target_env = "msvc"))]
fn same_path(left: &Path, right: &Path) -> bool {
    let left = std::fs::canonicalize(left).unwrap_or_else(|_| left.to_path_buf());
    let right = std::fs::canonicalize(right).unwrap_or_else(|_| right.to_path_buf());
    left == right
}

#[cfg(all(target_os = "windows", target_env = "msvc"))]
fn link_vcpkg_installed(target: &Path, link: &Path) -> Result<(), String> {
    let target = std::fs::canonicalize(target)
        .map_err(|err| format!("failed to resolve {}: {err}", target.display()))?;
    match std::fs::canonicalize(link) {
        Ok(existing) if existing == target => return Ok(()),
        Ok(_) => {
            return Err(format!(
                "{} already exists and does not point to {}",
                link.display(),
                target.display()
            ));
        }
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => return Err(format!("failed to inspect {}: {err}", link.display())),
    }

    let status = std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(&target)
        .status()
        .map_err(|err| format!("failed to create junction {}: {err}", link.display()))?;
    if !status.success() {
        if std::fs::canonicalize(link).ok().as_deref() == Some(target.as_path()) {
            return Ok(());
        }
        return Err(format!(
            "failed to create junction {} -> {}",
            link.display(),
            target.display()
        ));
    }
    Ok(())
}

#[cfg(feature = "use-bindgen")]
fn run_bindgen(include_paths: &[String]) {
    let mut base_builder = bindgen::Builder::default()
        .header("wrapper.h")
        .generate_comments(true)
        .formatter(bindgen::Formatter::Rustfmt)
        .generate_cstr(true)
        .disable_name_namespacing()
        .array_pointers_in_arguments(true)
        .ctypes_prefix("libc")
        .allowlist_function("heif_.*")
        .allowlist_type("heif_.*")
        .size_t_is_usize(true)
        .clang_args([
            "-fparse-all-comments",
            "-fretain-comments-from-system-headers",
        ]);

    for path in include_paths {
        base_builder = base_builder.clang_arg(format!("-I{path}"));
    }

    // Don't derive Copy and Clone for structures with pointers
    // and which represents shared_ptr from C++.
    for struct_name in [
        "heif_plugin_info",
        "heif_decoding_options",
        "heif_encoding_options",
        "heif_property_user_description",
        "heif_reader_range_request_result",
        "heif_entity_group",
        "heif_depth_representation_info",
        "heif_camera_extrinsic_matrix",
        "heif_track",
        "heif_raw_sequence_sample",
        "heif_track_options",
        "heif_sequence_encoding_options",
        "heif_context",
        "heif_image_handle",
        "heif_decoder_plugin",
        "heif_encoder_plugin",
        "heif_image",
        "heif_scaling_options",
        "heif_encoder",
        "heif_reading_options",
        "heif_encoder_descriptor",
        "heif_encoder_parameter",
        "heif_decoder_descriptor",
        "heif_region_item",
        "heif_region",
    ] {
        base_builder = base_builder.no_copy(struct_name);
    }

    // The main module
    // Finish the builder and generate the bindings.
    let bindings = base_builder
        .clone()
        .generate()
        .expect("Unable to generate bindings");

    // Write the bindings to the $OUT_DIR/bindings.rs file.
    let out_path = PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out_path.join("bindings.rs"))
        .expect("Couldn't write bindings.rs!");

    // Create linker_test.rs module for testing cases when not all
    // functions from *.h files are really available in libheif.
    let code = bindings.to_string();
    let mut func_names = Vec::new();
    for line in code.lines() {
        if !line.contains("pub fn ") {
            continue;
        }
        let line = line.trim();
        let res: Vec<&str> = line.split(&[' ', '(']).collect();
        if res.len() > 3 {
            if let &["pub", "fn", name] = &res[..3] {
                func_names.push(name)
            }
        }
    }

    let mut result = vec![
        "use super::*;\n\n",
        "#[test]\n",
        "fn is_all_functions_exists_in_libheif() {\n",
        "    let fn_pointers = [\n",
    ];
    for name in func_names {
        result.push("        ");
        result.push(name);
        result.push(" as *const fn(),\n")
    }
    result.extend(vec![
        "    ];\n",
        "    for pointer in fn_pointers.iter() {\n",
        "        assert!(!pointer.is_null());\n",
        "    }\n",
        "}\n",
    ]);
    let test_module = result.join("");
    let test_path = out_path.join("linker_test.rs");
    std::fs::write(&test_path, test_module).expect("Couldn't write test module!");

    let bindings = base_builder
        .layout_tests(false)
        .generate()
        .expect("Unable to generate bindings without tests");
    bindings
        .write_to_file(out_path.join("bindings_wo_tests.rs"))
        .expect("Couldn't write bindings_wo_tests.rs!");
}

fn copy_dir_all(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> io::Result<()> {
    std::fs::create_dir_all(&dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let ty = entry.file_type()?;
        if ty.is_dir() {
            copy_dir_all(entry.path(), dst.as_ref().join(entry.file_name()))?;
        } else {
            std::fs::copy(entry.path(), dst.as_ref().join(entry.file_name()))?;
        }
    }
    Ok(())
}
