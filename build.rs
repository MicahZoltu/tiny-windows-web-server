use std::env;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::process::Command;

const SCRIPT: &str = "folderserve.rc";
// `folderserve.rc` names this same file.
const ICON: &str = "folderserve.ico";

fn main() {
	if env::var("CARGO_CFG_TARGET_OS").as_deref() != Ok("windows") {
		return;
	}
	// Cargo cannot see that the script references the icon, so the icon is declared as an input by hand.
	println!("cargo:rerun-if-changed={SCRIPT}");
	println!("cargo:rerun-if-changed={ICON}");
	// `windres` writes a COFF object, while `rc.exe` writes a `.res` file that the linker takes as-is.
	let msvc = env::var("CARGO_CFG_TARGET_ENV").as_deref() == Ok("msvc");
	let name = if msvc {
		"folderserve.res"
	} else {
		"folderserve.res.o"
	};
	let out_dir = env::var_os("OUT_DIR").expect("cargo sets OUT_DIR for a build script");
	let archive = PathBuf::from(out_dir).join(name);
	let mut compiler = resource_compiler(&archive, msvc);
	match compiler.status() {
		Ok(status) if status.success() => {
			// A compiler that exits clean without writing the object would otherwise surface as an unexplained missing-file linker error.
			if !archive.is_file() {
				panic!("{SCRIPT} did not compile: the resource compiler reported success but {} is missing", archive.display());
			}
		}
		// A machine with the linker alone still has to build, and the cost is the file icon, while the tray shows the stock icon instead.
		Err(error) if error.kind() == ErrorKind::NotFound => {
			println!("cargo:warning=no resource compiler was found, so the executable builds without a file icon and the tray shows the stock icon; once one is installed, `cargo clean` followed by a build brings both icons back");
			return;
		}
		Ok(status) => panic!("{SCRIPT} did not compile: the resource compiler failed ({status})"),
		Err(error) => panic!("{SCRIPT} did not compile: {error}"),
	}
	println!("cargo:rustc-link-arg={}", archive.display());
}

fn resource_compiler(archive: &Path, msvc: bool) -> Command {
	if msvc {
		let mut command = Command::new("rc.exe");
		command.args(["/nologo", "/fo"]).arg(archive).arg(SCRIPT);
		command
	} else {
		// A cross build drives the mingw-w64 compiler, whose name carries the target arch; the same name serves a mingw toolchain on a Windows host.
		let arch = env::var("CARGO_CFG_TARGET_ARCH").expect("cargo names the target arch");
		let mut command = Command::new(format!("{}-w64-mingw32-windres", mingw_prefix(&arch)));
		command.args(["-O", "coff", "--input", SCRIPT, "--output"]).arg(archive);
		command
	}
}

// Cargo names the 32-bit x86 architecture `x86`, while mingw-w64 spells it `i686`, so the raw arch cannot become the compiler name.
fn mingw_prefix(arch: &str) -> &'static str {
	match arch {
		"x86" => "i686",
		"x86_64" => "x86_64",
		other => panic!("no mingw-w64 resource compiler is known for the target arch `{other}`"),
	}
}
