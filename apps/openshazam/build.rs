use std::{
    env, fs,
    path::{Path, PathBuf},
    process::Command,
};

fn main() {
    println!("cargo:rerun-if-changed=assets/OpenShazam.ico");
    println!("cargo:rerun-if-env-changed=RC");
    println!("cargo:rerun-if-env-changed=WindowsSdkDir");
    println!("cargo:rerun-if-env-changed=WindowsSDKVersion");

    if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows")
        || env::var("CARGO_CFG_TARGET_ENV").as_deref() != Ok("msvc")
    {
        return;
    }

    let host = env::var("HOST").unwrap_or_default();
    if !host.contains("windows") {
        println!("cargo:warning=Skipping the Windows icon resource during cross-compilation; build on Windows to embed it.");
        return;
    }

    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").unwrap());
    let output_dir = PathBuf::from(env::var_os("OUT_DIR").unwrap());
    let resource_path = output_dir.join("openshazam.res");
    let Some(compiler) = find_resource_compiler() else {
        println!("cargo:warning=Windows SDK rc.exe was not found; the app will use the Windows default icon.");
        return;
    };

    let status = Command::new(&compiler)
        .current_dir(&manifest_dir)
        .arg("/nologo")
        .arg("/fo")
        .arg(&resource_path)
        .arg("openshazam.rc")
        .status()
        .unwrap_or_else(|error| panic!("failed to start {}: {error}", compiler.display()));
    if !status.success() {
        panic!("Windows resource compiler failed with status {status}");
    }

    // MSVC link.exe and lld-link accept a .res file as a native link input.
    // This embeds the icon in the executable, so installed builds need no
    // loose icon file at runtime.
    println!("cargo:rustc-link-arg-bins={}", resource_path.display());
}

fn find_resource_compiler() -> Option<PathBuf> {
    if let Some(path) = env::var_os("RC") {
        return Some(PathBuf::from(path));
    }

    let architecture = match env::var("CARGO_CFG_TARGET_ARCH").as_deref() {
        Ok("x86") => "x86",
        Ok("arm64") => "arm64",
        _ => "x64",
    };

    if let Some(sdk_dir) = env::var_os("WindowsSdkDir") {
        let mut path = PathBuf::from(sdk_dir);
        path.push("bin");
        if let Some(version) = env::var_os("WindowsSDKVersion") {
            path.push(version);
            path.push(architecture);
            path.push("rc.exe");
            if path.is_file() {
                return Some(path);
            }
        } else if let Some(compiler) = find_rc_in_sdk_bin(&path, architecture) {
            return Some(compiler);
        }
    }

    let program_files = env::var_os("ProgramFiles(x86)")?;
    let sdk_bin = Path::new(&program_files)
        .join("Windows Kits")
        .join("10")
        .join("bin");
    find_rc_in_sdk_bin(&sdk_bin, architecture)
}

fn find_rc_in_sdk_bin(sdk_bin: &Path, architecture: &str) -> Option<PathBuf> {
    let mut versions: Vec<_> = fs::read_dir(sdk_bin)
        .ok()?
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().is_ok_and(|kind| kind.is_dir()))
        .map(|entry| entry.path())
        .collect();
    versions.sort();
    versions.into_iter().rev().find_map(|versioned_bin| {
        let compiler = versioned_bin.join(architecture).join("rc.exe");
        compiler.is_file().then_some(compiler)
    })
}
