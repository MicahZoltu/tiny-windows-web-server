#![cfg_attr(windows, windows_subsystem = "windows")]

#[cfg(any(windows, test))]
mod dispatch;
#[cfg_attr(not(windows), allow(dead_code))]
mod http;
#[cfg_attr(not(windows), allow(dead_code))]
mod mime;
#[cfg_attr(not(windows), allow(dead_code))]
mod served_root;
#[cfg_attr(not(windows), allow(dead_code))]
mod server;
#[cfg(any(windows, test))]
mod tray_text;
#[cfg(windows)]
mod win;

#[cfg(windows)]
use std::env;
#[cfg(windows)]
use std::path::PathBuf;

#[cfg(windows)]
use crate::dispatch::{classify, Dispatch};
#[cfg(windows)]
use crate::served_root::ServedRoot;
#[cfg(windows)]
use crate::server::address;
#[cfg(windows)]
use crate::tray_text::{menu_text, tip};
#[cfg(windows)]
use crate::win::{Failure, PRODUCT_NAME, VERB_LABEL};

#[cfg(windows)]
fn main() {
	std::process::exit(run());
}

#[cfg(not(windows))]
fn main() {
	eprintln!("folderserve is Windows-only");
	std::process::exit(1);
}

// Explorer launches the registered verb once for the folder the user right-clicked, so each served folder is its own process.
#[cfg(windows)]
fn run() -> i32 {
	let arguments: Vec<_> = env::args_os().skip(1).collect();
	match classify(&arguments) {
		Dispatch::Install => install(),
		Dispatch::WrongCount(count) => {
			// A refused write is not reported here: this launch only repairs the verb before exiting, so a later launch's dialog reports the failure.
			let _ = win::register_folder_verb();
			win::warn(&format!("{PRODUCT_NAME} serves exactly one folder and was started with {count} arguments; start it with one folder, or with none to install the Explorer menu entry."));
			1
		}
		Dispatch::Serve(folder) => serve(PathBuf::from(folder), win::register_folder_verb()),
	}
}

#[cfg(windows)]
fn install() -> i32 {
	match win::register_folder_verb() {
		Ok(()) => {
			win::message(&format!("{PRODUCT_NAME} is installed. Right-click a folder and choose \"{VERB_LABEL}\". Each folder you serve gets its own process and tray icon."));
			0
		}
		Err(failure) => {
			if failure.later_launch_writes_it {
				// The verb key and its label are written before the `command` subkey, so a refused write can leave the menu entry present but inert, and only a later launch repairs it.
				win::warn(&format!("{PRODUCT_NAME} could not {failure}. It may be missing or incomplete until a later launch writes it."));
			} else {
				win::warn(&format!("{PRODUCT_NAME} could not {failure}. Move the executable to a path that is valid UTF-8 and start it again to install the Explorer menu entry."));
			}
			1
		}
	}
}

#[cfg(windows)]
fn serve(folder: PathBuf, registration: Result<(), Failure>) -> i32 {
	let root = match ServedRoot::new(folder.clone()) {
		Ok(root) => root,
		Err(failure) => {
			win::warn(&format!("{PRODUCT_NAME} expects an existing folder to serve and could not use {}: {failure}.", folder.display()));
			return 1;
		}
	};
	let port = match server::start(root) {
		Ok(port) => port,
		Err(failure) => {
			win::warn(&format!("{PRODUCT_NAME} could not start serving this folder: {failure}."));
			return 1;
		}
	};
	// A refused write costs only the Explorer entry, and the folder answers now, so the warning that says so is true only once the bind has succeeded.
	if let Err(failure) = registration {
		win::warn(&format!("{PRODUCT_NAME} could not {failure}. Serving this folder is unaffected."));
	}
	let url = address(port);
	let tooltip = tip(&folder, &url);
	let folder_row = menu_text(&folder.display().to_string());
	let url_row = menu_text(&url);
	win::run_tray(&folder_row, &url_row, &tooltip)
}
