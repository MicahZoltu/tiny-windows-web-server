use std::ptr::null_mut;

use windows_sys::Win32::UI::WindowsAndMessaging::{MessageBoxW, MB_ICONINFORMATION, MB_ICONWARNING, MB_OK, MESSAGEBOX_STYLE};

use super::to_wide_nul_terminated;

pub const PRODUCT_NAME: &str = "Folder Serve";

pub fn message(text: &str) {
	show(text, MB_ICONINFORMATION);
}

pub fn warn(text: &str) {
	show(text, MB_ICONWARNING);
}

fn show(text: &str, icon: MESSAGEBOX_STYLE) {
	let text = to_wide_nul_terminated(text);
	let caption = to_wide_nul_terminated(PRODUCT_NAME);
	// SAFETY: both buffers are NUL-terminated and outlive the call, which copies the text it displays.
	unsafe { MessageBoxW(null_mut(), text.as_ptr(), caption.as_ptr(), MB_OK | icon) };
}
