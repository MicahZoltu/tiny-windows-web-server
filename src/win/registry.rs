use std::env;
use std::fmt;
use std::ptr::{null, null_mut};

use windows_sys::Win32::Foundation::ERROR_SUCCESS;
use windows_sys::Win32::System::Registry::{RegCloseKey, RegCreateKeyExW, RegSetValueExW, HKEY, HKEY_CURRENT_USER, KEY_WRITE, REG_OPTION_NON_VOLATILE, REG_SZ};

use super::to_wide_nul_terminated;

// Per-user scope only: writing HKCU needs no elevation, and every launch re-registers so a moved or rebuilt executable repairs the path itself.
const VERB_KEY: &str = "Software\\Classes\\Directory\\shell\\FolderServe";

const COMMAND_KEY: &str = "command";

pub const VERB_LABEL: &str = "Serve this folder";

#[derive(Debug)]
pub struct Failure {
	action: &'static str,
	code: Option<u32>,
	pub(crate) later_launch_writes_it: bool,
}

impl Failure {
	// A zero code would render as `error 0` and read like success, so only a nonzero answer is kept: every code the crate reports passes through this one check.
	pub(crate) fn win32(action: &'static str, code: u32) -> Failure {
		Failure {
			action,
			code: (code != ERROR_SUCCESS).then_some(code),
			later_launch_writes_it: true,
		}
	}
}

// `action` is a lowercase bare verb phrase that callers complete as `could not {failure}`, so it must not be capitalised or end in a period.
impl fmt::Display for Failure {
	fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
		match self.code {
			Some(code) => write!(formatter, "{} (error {code})", self.action),
			None => formatter.write_str(self.action),
		}
	}
}

// A failed write is reported, not acted on: what it costs is the caller's decision.
pub fn register_folder_verb() -> Result<(), Failure> {
	let exe = env::current_exe().map_err(|error| match error.raw_os_error() {
		Some(code) => Failure::win32("read the path to this program", code as u32),
		None => Failure {
			action: "read the path to this program",
			code: None,
			later_launch_writes_it: true,
		},
	})?;
	let Some(exe) = exe.to_str() else {
		// Reading the same path on a later launch yields the same bytes, so no launch from here can write the entry; only the executable's location can change the answer.
		return Err(Failure {
			action: "read this program's own path as UTF-8",
			code: None,
			later_launch_writes_it: false,
		});
	};
	// The shell substitutes %1 with the folder that was right-clicked, and the quotes keep a path with spaces as one argument.
	let command = format!("\"{exe}\" \"%1\"");
	write_verb(&command)
}

// A registry key that closes itself, so a `?` on any later call still leaves exactly one RegCloseKey on every path.
struct RegKey(HKEY);

impl Drop for RegKey {
	fn drop(&mut self) {
		// SAFETY: the handle came from a successful `create_key` and is closed here exactly once, when this guard is dropped.
		let _ = unsafe { RegCloseKey(self.0) };
	}
}

fn write_verb(command: &str) -> Result<(), Failure> {
	let verb = create_key(HKEY_CURRENT_USER, VERB_KEY, "create the menu entry key")?;
	set_default_value(&verb, VERB_LABEL, "set the menu entry's label")?;
	let command_key = create_key(verb.0, COMMAND_KEY, "create the command subkey")?;
	set_default_value(&command_key, command, "set the command line")
}

fn create_key(parent: HKEY, path: &str, action: &'static str) -> Result<RegKey, Failure> {
	let name = to_wide_nul_terminated(path);
	let mut key: HKEY = null_mut();
	// SAFETY: `name` outlives the call, `key` is a live out-parameter, and null class, security, and disposition ask for the defaults.
	let status = unsafe { RegCreateKeyExW(parent, name.as_ptr(), 0, null(), REG_OPTION_NON_VOLATILE, KEY_WRITE, null(), &mut key, null_mut()) };
	if status != ERROR_SUCCESS {
		return Err(Failure::win32(action, status));
	}
	Ok(RegKey(key))
}

fn set_default_value(key: &RegKey, text: &str, action: &'static str) -> Result<(), Failure> {
	let value = to_wide_nul_terminated(text);
	// A REG_SZ payload is counted in bytes of UTF-16 with its terminating NUL included, which is why the length doubles and nothing is added.
	let bytes = (value.len() * 2) as u32;
	// SAFETY: `value` outlives the call, which copies it, and a null value name addresses the key's default value.
	let status = unsafe { RegSetValueExW(key.0, null(), 0, REG_SZ, value.as_ptr().cast::<u8>(), bytes) };
	if status != ERROR_SUCCESS {
		return Err(Failure::win32(action, status));
	}
	Ok(())
}
