use cargo_metadata::{camino::Utf8PathBuf, Message};
use cfg_if::cfg_if;
use fs::PathExt;
use fs_err as fs;
use std::{
    io::{self, ErrorKind},
    path::{Path, PathBuf},
    process::{exit, Child, Command, Stdio},
};

fn cargo_bin() -> std::ffi::OsString {
    std::env::var_os("CARGO").unwrap_or_else(|| "cargo".to_owned().into())
}

trait CommandExt {
    fn spawn_handling_not_found(&mut self) -> io::Result<Child>;
}

impl CommandExt for Command {
    fn spawn_handling_not_found(&mut self) -> io::Result<Child> {
        let command_name = self.get_program().to_string_lossy().to_string();
        self.spawn().map_err(|err| match err.kind() {
            ErrorKind::NotFound => {
                eprintln!("error: command `{}` not found", command_name);
                eprintln!(
                    "Please refer to the documentation for installing pros-rs on your platform."
                );
                eprintln!("> https://github.com/pros-rs/pros-rs#compiling");
                exit(1);
            }
            _ => err,
        })
    }
}

const TARGET_PATH: &str = "target/armv7a-vexos-eabi.json";

pub fn build(
    path: PathBuf,
    args: Vec<String>,
    for_simulator: bool,
    mut handle_executable: impl FnMut(Utf8PathBuf),
) {
    let target_path = path.join(TARGET_PATH);
    let mut build_cmd = Command::new(cargo_bin());
    build_cmd
        .current_dir(&path)
        .arg("build")
        .arg("--message-format")
        .arg("json-render-diagnostics")
        .arg("--manifest-path")
        .arg(format!("{}/Cargo.toml", path.display()));

    if !is_nightly_toolchain() {
        eprintln!("ERROR: pros-rs requires Nightly Rust features, but you're using stable.");
        eprintln!(" hint: this can be fixed by running `rustup override set nightly`");
        exit(1);
    }

    if for_simulator {
        if !has_wasm_target() {
            eprintln!(
                "ERROR: simulation requires the wasm32-unknown-unknown target to be installed"
            );
            eprintln!(
                " hint: this can be fixed by running `rustup target add wasm32-unknown-unknown`"
            );
            exit(1);
        }

        build_cmd
            .arg("--target")
            .arg("wasm32-unknown-unknown")
            .arg("-Zbuild-std=std,panic_abort")
            .arg("--config=build.rustflags=['-Ctarget-feature=+atomics,+bulk-memory,+mutable-globals','-Clink-arg=--shared-memory','-Clink-arg=--export-table']")
            .stdout(Stdio::piped());
    } else {
        let target = include_str!("armv7a-vexos-eabi.json");
        fs::create_dir_all(target_path.parent().unwrap()).unwrap();
        fs::write(&target_path, target).unwrap();
        build_cmd
            .arg("--target")
            .arg(&target_path);

        build_cmd
            .arg("--config")
            .arg(format!("target.armv7a-vexos-eabi.linker='{}'", linker_path()));
 
        build_cmd
            .arg("-Zbuild-std=core,alloc,compiler_builtins")
            .stdout(Stdio::piped());
    }

    build_cmd.args(args);

    let mut out = build_cmd.spawn_handling_not_found().unwrap();
    let reader = std::io::BufReader::new(out.stdout.take().unwrap());
    for message in Message::parse_stream(reader) {
        if let Message::CompilerArtifact(artifact) = message.unwrap() {
            if let Some(binary_path) = artifact.executable {
                handle_executable(binary_path);
            }
        }
    }
}

#[cfg(target_os = "windows")]
fn find_toolchian_path_windows() -> Option<PathBuf> {
    let config_dir = dirs::config_dir()?;

    let toolchain_install_path = 
        PathBuf::from("C:\\Program Files (x86)\\Arm GNU Toolchain arm-none-eabi");
    let pros_vscode_toolchain_bin = config_dir.join("Code\\User\\globalStorage\\sigbots.pros\\install\\pros-toolchain-windows\\usr\\bin");
    let pros_vscode_insiders_toolchain_bin = config_dir.join("Code - Insiders\\User\\globalStorage\\sigbots.pros\\install\\pros-toolchain-windows\\usr\\bin");

    if toolchain_install_path.exists() {
        let mut versions = fs::read_dir(toolchain_install_path).ok()?;

        Some(versions.next()?.ok()?.path().join("bin"))
    } else if pros_vscode_toolchain_bin.exists() {
        Some(pros_vscode_toolchain_bin)
    } else if pros_vscode_insiders_toolchain_bin.exists() {
        Some(pros_vscode_insiders_toolchain_bin)
    } else {
        None
    }
}

fn objcopy_path() -> String {
    #[cfg(target_os = "windows")]
    if let Some(toolchain_path) = find_toolchian_path_windows() {
        toolchain_path.join("arm-none-eabi-objcopy.exe").to_string_lossy().to_string()
    } else {
        "arm-none-eabi-objcopy".to_owned()
    }

    #[cfg(not(target_os = "windows"))]
    "arm-none-eabi-objcopy".to_owned()
}

fn linker_path() -> String {
    #[cfg(target_os = "windows")]
    if let Some(toolchain_path) = find_toolchian_path_windows() {
        toolchain_path.join("arm-none-eabi-gcc.exe").to_string_lossy().to_string()
    } else {
        "arm-none-eabi-gcc".to_owned()
    }

    #[cfg(not(target_os = "windows"))]
    "arm-none-eabi-gcc".to_owned()
}

pub fn strip_binary(bin: Utf8PathBuf) {
    println!("Stripping Binary: {}", bin.clone());
    let objcopy = objcopy_path();
    let strip = std::process::Command::new(&objcopy)
        .args([
            "--strip-symbol=install_hot_table",
            "--strip-symbol=__libc_init_array",
            "--strip-symbol=_PROS_COMPILE_DIRECTORY",
            "--strip-symbol=_PROS_COMPILE_TIMESTAMP",
            "--strip-symbol=_PROS_COMPILE_TIMESTAMP_INT",
            bin.as_str(),
            &format!("{}.stripped", bin),
        ])
        .spawn_handling_not_found()
        .unwrap();
    strip.wait_with_output().unwrap();
    let elf_to_bin = std::process::Command::new(&objcopy)
        .args([
            "-O",
            "binary",
            "-R",
            ".hot_init",
            &format!("{}.stripped", bin),
            &format!("{}.bin", bin),
        ])
        .spawn_handling_not_found()
        .unwrap();
    elf_to_bin.wait_with_output().unwrap();
}

fn is_nightly_toolchain() -> bool {
    let rustc = std::process::Command::new("rustc")
        .arg("--version")
        .output()
        .unwrap();
    let rustc = String::from_utf8(rustc.stdout).unwrap();
    rustc.contains("nightly")
}

fn has_wasm_target() -> bool {
    let Ok(rustup) = std::process::Command::new("rustup")
        .arg("target")
        .arg("list")
        .arg("--installed")
        .output()
    else {
        return true;
    };
    let rustup = String::from_utf8(rustup.stdout).unwrap();
    rustup.contains("wasm32-unknown-unknown")
}

#[cfg(target_os = "windows")]
fn find_simulator_path_windows() -> Option<String> {
    let wix_path = PathBuf::from(r#"C:\Program Files\PROS Simulator\PROS Simulator.exe"#);
    if wix_path.exists() {
        return Some(wix_path.to_string_lossy().to_string());
    }
    // C:\Users\USER\AppData\Local\PROS Simulator
    let nsis_path = PathBuf::from(std::env::var("LOCALAPPDATA").unwrap())
        .join("PROS Simulator")
        .join("PROS Simulator.exe");
    if nsis_path.exists() {
        return Some(nsis_path.to_string_lossy().to_string());
    }
    None
}

fn find_simulator() -> Command {
    cfg_if! {
        if #[cfg(target_os = "macos")] {
            let mut cmd = Command::new("open");
            cmd.args(["-nWb", "rs.pros.simulator", "--args"]);
            cmd
        } else if #[cfg(target_os = "windows")] {
            Command::new(find_simulator_path_windows().expect("Simulator install not found"))
        } else {
            Command::new("pros-simulator")
        }
    }
}

pub fn launch_simulator(ui: Option<String>, workspace_dir: &Path, binary_path: &Path) {
    let mut command = if let Some(ui) = ui {
        Command::new(ui)
    } else {
        find_simulator()
    };
    command
        .arg("--code")
        .arg(binary_path.fs_err_canonicalize().unwrap())
        .arg(workspace_dir.fs_err_canonicalize().unwrap());

    let command_name = command.get_program().to_string_lossy().to_string();
    let args = command
        .get_args()
        .map(|arg| arg.to_string_lossy().to_string())
        .collect::<Vec<_>>();

    eprintln!("$ {} {}", command_name, args.join(" "));

    let res = command
        .spawn()
        .map_err(|err| match err.kind() {
            ErrorKind::NotFound => {
                eprintln!("Failed to start simulator:");
                eprintln!("error: command `{command_name}` not found");
                eprintln!();
                eprintln!("Please install PROS Simulator using the link below.");
                eprintln!("> https://github.com/pros-rs/pros-simulator-gui/releases");
                exit(1);
            }
            _ => err,
        })
        .unwrap()
        .wait();
    if let Err(err) = res {
        eprintln!("Failed to launch simulator: {}", err);
    }
}
