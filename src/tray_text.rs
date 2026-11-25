use std::path::Path;

// A tooltip is not a menu item, so no `&` in it is an accelerator marker and none is doubled.
pub fn tip(folder: &Path, url: &str) -> String {
	format!("{} — {url}", folder.display())
}

// Win32 reads a single `&` in a menu item's text as an accelerator marker.
pub fn menu_text(text: &str) -> String {
	text.replace('&', "&&")
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::{menu_text, tip};

	const MENU_TEXTS: &[(&str, &str)] = &[
		("C:\\sites\\app", "C:\\sites\\app"),
		("C:\\a & b", "C:\\a && b"),
		("C:\\&", "C:\\&&"),
		("http://localhost:5000/", "http://localhost:5000/"),
	];

	const TIPS: &[(&str, &str, &str)] = &[
		("C:\\sites\\app", "http://localhost:5000/", "C:\\sites\\app — http://localhost:5000/"),
		("C:\\a & b", "http://localhost:5001/", "C:\\a & b — http://localhost:5001/"),
		("C:\\&", "http://localhost:5002/", "C:\\& — http://localhost:5002/"),
	];

	#[test]
	fn menu_text_doubles_every_ampersand() {
		for (text, expected) in MENU_TEXTS {
			assert_eq!(menu_text(text), *expected, "case {text:?}");
		}
	}

	#[test]
	fn tip_names_folder_and_url_without_doubling_ampersands() {
		for (folder, url, expected) in TIPS {
			assert_eq!(tip(Path::new(folder), url), *expected, "case {folder:?}");
		}
	}
}
