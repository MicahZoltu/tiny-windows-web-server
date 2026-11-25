mod dialog;
mod registry;
mod tray;

pub use dialog::{message, warn, PRODUCT_NAME};
pub use registry::{register_folder_verb, Failure, VERB_LABEL};
pub use tray::run as run_tray;

pub fn to_wide_nul_terminated(text: &str) -> Vec<u16> {
	let mut wide: Vec<u16> = text.encode_utf16().collect();
	wide.push(0);
	wide
}
