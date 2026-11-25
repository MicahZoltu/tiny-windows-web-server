use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const INDEX_NAMES: &[&str] = &[
	"index.html",
	"index.htm",
];

// Windows resolves these names to devices even when an extension or a trailing dot or space follows them; the superscript digits and the console and clock devices are devices under the same rule.
const RESERVED_DEVICE_NAMES: &[&str] = &[
	"CON",
	"PRN",
	"AUX",
	"NUL",
	"CLOCK$",
	"CONIN$",
	"CONOUT$",
	"COM1",
	"COM2",
	"COM3",
	"COM4",
	"COM5",
	"COM6",
	"COM7",
	"COM8",
	"COM9",
	"COM¹",
	"COM²",
	"COM³",
	"LPT1",
	"LPT2",
	"LPT3",
	"LPT4",
	"LPT5",
	"LPT6",
	"LPT7",
	"LPT8",
	"LPT9",
	"LPT¹",
	"LPT²",
	"LPT³",
];

#[derive(Debug)]
pub struct BadTarget;

// The only place that decides whether a URL target addresses a file inside the served folder.
pub struct ServedRoot {
	// `canonicalize` answers in absolute extended-length form, so this is the one path candidates are built from and compared against.
	real: PathBuf,
}

impl ServedRoot {
	pub fn new(folder: PathBuf) -> io::Result<ServedRoot> {
		if !fs::metadata(&folder)?.is_dir() {
			return Err(io::Error::new(io::ErrorKind::NotADirectory, "the path is not a folder"));
		}
		let real = fs::canonicalize(&folder)?;
		Ok(ServedRoot { real })
	}

	// `Ok(Some(path))` is a regular file inside the folder; `Ok(None)` is "nothing here" and answers 404; `Err(BadTarget)` is a malformed target and answers 400.
	pub fn resolve(&self, url_target: &str) -> Result<Option<PathBuf>, BadTarget> {
		for candidate in candidates(&self.real, url_target)? {
			// A candidate that does not exist, or that a link carries outside the folder, is treated as absent: a refused link must answer 404 so it cannot be told apart from a missing file.
			let Ok(real) = fs::canonicalize(&candidate) else {
				continue;
			};
			// `starts_with` compares whole components, so a sibling whose name merely begins with the folder's name cannot pass.
			// Containment is name-level: a hard link inside the folder names a file that may live outside it, and it is served like any other name.
			if real.starts_with(&self.real) && real.is_file() {
				// The canonical path is what the caller opens; a link swapped between this call and the open is out of scope for a single-user local tool.
				return Ok(Some(real));
			}
		}
		Ok(None)
	}
}

fn candidates(root: &Path, request_target: &str) -> Result<Vec<PathBuf>, BadTarget> {
	let path = request_target.split_once('?').map_or(request_target, |(path, _)| path);
	let wants_directory = path.ends_with('/');
	let mut directory = root.to_path_buf();
	let mut has_segments = false;
	for raw_segment in path.split('/') {
		if raw_segment.is_empty() {
			continue;
		}
		directory.push(decode_segment(raw_segment)?);
		has_segments = true;
	}
	let mut paths = Vec::with_capacity(INDEX_NAMES.len() + 1);
	if has_segments && !wants_directory {
		paths.push(directory.clone());
	}
	for name in INDEX_NAMES {
		paths.push(directory.join(name));
	}
	Ok(paths)
}

fn decode_segment(raw: &str) -> Result<OsString, BadTarget> {
	let bytes = raw.as_bytes();
	let mut decoded = Vec::with_capacity(bytes.len());
	let mut index = 0;
	while let Some(&byte) = bytes.get(index) {
		if byte == b'%' {
			let high = hex_value(*bytes.get(index + 1).ok_or(BadTarget)?)?;
			let low = hex_value(*bytes.get(index + 2).ok_or(BadTarget)?)?;
			decoded.push(high << 4 | low);
			index += 3;
		} else {
			decoded.push(byte);
			index += 1;
		}
	}
	// A control byte in a name is a malformed target, and Windows refuses the file it would name, so it is rejected here instead of surfacing as a failed lookup.
	for &byte in &decoded {
		if byte.is_ascii_control() || matches!(byte, b'/' | b'\\' | b':') {
			return Err(BadTarget);
		}
	}
	let segment = String::from_utf8(decoded).map_err(|_| BadTarget)?;
	// Windows drops trailing dots and spaces before it resolves a name, so `.`, `..`, `..%20`, and `...` all leave this directory; the check runs on the stripped name.
	let stripped = segment.trim_end_matches(['.', ' ']);
	if stripped.is_empty() || is_reserved_device_name(stripped) {
		return Err(BadTarget);
	}
	Ok(OsString::from(segment))
}

// Windows ignores everything after the first dot and drops trailing spaces before it resolves a device name, so a name like `CON .txt` reaches the device through a stem the whole-name stripping left padded.
fn is_reserved_device_name(name: &str) -> bool {
	let stem = name.split_once('.').map_or(name, |(stem, _)| stem).trim_end_matches(['.', ' ']);
	RESERVED_DEVICE_NAMES.iter().any(|reserved| stem.eq_ignore_ascii_case(reserved))
}

fn hex_value(byte: u8) -> Result<u8, BadTarget> {
	match byte {
		b'0'..=b'9' => Ok(byte - b'0'),
		b'a'..=b'f' => Ok(byte - b'a' + 10),
		b'A'..=b'F' => Ok(byte - b'A' + 10),
		_ => Err(BadTarget),
	}
}

#[cfg(test)]
pub(crate) fn create_unique_temp_folder(label: &str) -> PathBuf {
	let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
	let folder = std::env::temp_dir().join(format!("folderserve-{label}-{}-{nanos}", std::process::id()));
	fs::create_dir(&folder).unwrap();
	folder
}

#[cfg(test)]
mod tests {
	use std::ffi::OsStr;
	use std::fs;
	use std::io;
	use std::path::{Path, PathBuf};

	use super::{candidates, create_unique_temp_folder, BadTarget, ServedRoot};

	const ROOT: &str = "/srv/site";

	const TRAVERSALS: &[&str] = &[
		"/../../etc/passwd",
		"/a/../../b",
		"/..",
		"/a/%2e%2e/b",
		"/a%2f..%2fb",
		"/a%2Fb",
		"/a\\..\\b",
		"/a%5c..%5cb",
		"/c:/windows",
		"/a%00b",
		"/a%2e%2e%2f%2e%2e%2fetc%2fpasswd",
		"/%",
		"/%zz",
		"/%ff",
		"/a%2",
		"/..%20",
		"/..%20%20",
		"/..%2E",
		"/...%20",
		"/a/..%20/b",
		"/%2e",
		"/.%20",
	];

	// A control byte cannot appear in a Windows name, so an encoded one is a malformed target rather than a lookup that fails deeper.
	const CONTROL_BYTES: &[&str] = &[
		"/a%01b",
		"/a%1fb",
		"/a%7fb",
		"/%00",
		"/a%00",
		"/tab%09name.txt",
		"/line%0Abreak",
		"/carriage%0Dreturn",
		"/dir/a%1F",
		"/a/%08b",
		"/a%1fb/",
	];

	const DEVICES: &[&str] = &[
		"/CON",
		"/con",
		"/NUL.txt",
		"/aux.",
		"/COM1",
		"/lpt9.log",
		"/dir/PRN",
		"/con%2Etxt",
		"/CON%20",
		// The superscript digits and the console streams are devices under the same stem rule.
		"/COM¹",
		"/com²",
		"/COM³",
		"/COM¹.txt",
		"/LPT¹",
		"/lpt².log",
		"/dir/LPT³",
		"/CONIN$",
		"/conout$",
		"/CONIN$.txt",
		"/conin$/",
		// The encoded and trailing-dot forms of the new names resolve exactly like the ASCII ones.
		"/COM%C2%B9",
		"/LPT%C2%B3",
		"/CONIN%24",
		"/COM¹.",
		"/CONOUT$%20",
		// The clock device is a DOS device name under the same stem rule.
		"/CLOCK$",
		"/clock$",
		"/Clock$.txt",
		"/dir/CLOCK$",
		"/clock$%20",
		"/CLOCK$.log",
		// Windows drops the trailing spaces before the first dot too, so a padded stem still names the device.
		"/CON .txt",
		"/con .txt",
		"/CON%20.txt",
		"/CON%20%20.txt",
		"/CON  .txt",
		"/NUL .txt",
		"/nul%20.txt",
		"/aux .log",
		"/PRN .txt",
		"/COM1 .txt",
		"/com1%20.txt",
		"/LPT1 .txt",
		"/lpt9%20.log",
		"/CLOCK$ .txt",
		"/conin$ .txt",
		"/COM¹ .txt",
		"/LPT³%20.log",
		"/dir/NUL%20.txt",
		"/CON%20.txt/",
	];

	const NOT_DEVICES: &[&str] = &[
		"/console.txt",
		"/com0",
		"/null",
		"/a/COM10",
		"/lpt",
		"/com⁴",
		"/com¹1",
		"/CONIN",
		"/conout",
		"/clock",
		"/clockwork.txt",
		"/clocks$",
		"/clock$1",
		"/a/clock$x",
		// A space inside the stem is part of the name, because Windows strips only the trailing ones.
		"/CON%20notes.txt",
		"/con%20notes.txt",
		"/NUL%20notes.txt",
		"/COM1%20notes.txt",
		"/lpt1%20notes%2Etxt",
		"/dir/CLOCK$%20notes.txt",
		// A stem that merely starts with a reserved name is not that name.
		"/CONSOLE .txt",
		"/CONMAN.txt",
		"/NULLIFY .txt",
		"/COM1a .txt",
		"/LPT10 .txt",
		"/clock$x .txt",
	];

	// A name that is neither a device nor a traversal stays servable: the filters reject the shapes Windows resolves to something else, not ordinary text.
	const ORDINARY: &[&str] = &[
		"/index.html",
		"/a%20b/c.txt",
		"/na%C3%AFve.txt",
		"/a%C2%A0b",
		"/a%2Db",
		"/a%7Eb.txt",
		"/notes%2Etxt",
		"/dir/dots...txt",
		"/com-1.txt",
		"/clock.txt",
	];

	const RESOLVED: &[(&str, &str, &[&str])] = &[
		("/srv/site", "/", &["/srv/site/index.html", "/srv/site/index.htm"]),
		("/srv/site", "", &["/srv/site/index.html", "/srv/site/index.htm"]),
		("/srv/site", "/a/b/", &["/srv/site/a/b/index.html", "/srv/site/a/b/index.htm"]),
		("/srv/site", "/a/b", &["/srv/site/a/b", "/srv/site/a/b/index.html", "/srv/site/a/b/index.htm"]),
		("/srv/site", "/dir/?x=1&y=2", &["/srv/site/dir/index.html", "/srv/site/dir/index.htm"]),
		("/srv/site", "//a///b//c.txt", &["/srv/site/a/b/c.txt", "/srv/site/a/b/c.txt/index.html", "/srv/site/a/b/c.txt/index.htm"]),
		("/srv/site", "/a%20b/n%C3%A4me.txt", &["/srv/site/a b/näme.txt", "/srv/site/a b/näme.txt/index.html", "/srv/site/a b/näme.txt/index.htm"]),
		("/srv/sítе", "/caf%C3%A9/menu.html", &["/srv/sítе/café/menu.html", "/srv/sítе/café/menu.html/index.html", "/srv/sítе/café/menu.html/index.htm"]),
	];

	const SERVED: &[(&str, &str)] = &[
		("/a.txt", "a.txt"),
		("/", "index.html"),
		("/sub", "index.htm"),
		("/sub/", "index.htm"),
	];

	#[test]
	fn candidates_rejects_traversal() {
		for target in TRAVERSALS {
			assert!(candidates(Path::new(ROOT), target).is_err(), "case {target:?}");
		}
	}

	#[test]
	fn candidates_rejects_control_bytes_in_a_segment() {
		for target in CONTROL_BYTES {
			assert!(candidates(Path::new(ROOT), target).is_err(), "case {target:?}");
		}
	}

	#[test]
	fn candidates_rejects_reserved_device_names() {
		for target in DEVICES {
			assert!(candidates(Path::new(ROOT), target).is_err(), "case {target:?}");
		}
	}

	#[test]
	fn candidates_accepts_names_that_only_resemble_devices() {
		for target in NOT_DEVICES {
			assert!(candidates(Path::new(ROOT), target).is_ok(), "case {target:?}");
		}
	}

	#[test]
	fn candidates_accepts_ordinary_names() {
		for target in ORDINARY {
			assert!(candidates(Path::new(ROOT), target).is_ok(), "case {target:?}");
		}
	}

	#[test]
	fn candidates_resolves_target_to_candidate_paths() {
		for (root, target, expected) in RESOLVED {
			let want: Vec<PathBuf> = expected.iter().map(|path| PathBuf::from(*path)).collect();
			let got = candidates(Path::new(root), target).unwrap();
			assert_eq!(got, want, "case {root:?} {target:?}");
		}
	}

	#[test]
	fn new_rejects_a_path_that_is_not_a_folder() {
		let folder = create_unique_temp_folder("not-a-folder");
		let file = folder.join("plain.txt");
		fs::write(&file, "not a folder\n").unwrap();
		let kind = ServedRoot::new(file).err().map(|error| error.kind());
		assert_eq!(kind, Some(io::ErrorKind::NotADirectory));
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn resolve_finds_a_file_and_the_index_of_a_directory() {
		let folder = create_unique_temp_folder("resolve");
		fs::write(folder.join("index.html"), "<title>index</title>\n").unwrap();
		fs::write(folder.join("a.txt"), "a\n").unwrap();
		fs::create_dir(folder.join("sub")).unwrap();
		fs::write(folder.join("sub").join("index.htm"), "<title>sub</title>\n").unwrap();
		let root = ServedRoot::new(folder.clone()).unwrap();
		for (target, expected) in SERVED {
			let resolved = root.resolve(target).unwrap();
			let name = resolved.as_deref().and_then(Path::file_name);
			assert_eq!(name, Some(OsStr::new(expected)), "case {target:?}");
		}
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn resolve_answers_none_for_a_missing_file() {
		let folder = create_unique_temp_folder("missing");
		let root = ServedRoot::new(folder.clone()).unwrap();
		assert_eq!(root.resolve("/missing.txt").unwrap(), None);
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn resolve_rejects_a_malformed_target() {
		let folder = create_unique_temp_folder("traversal");
		let root = ServedRoot::new(folder.clone()).unwrap();
		assert!(matches!(root.resolve("/../secret.txt"), Err(BadTarget)));
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn resolve_answers_paths_that_carry_the_canonical_root() {
		let folder = create_unique_temp_folder("canonical");
		fs::write(folder.join("index.html"), "<title>index</title>\n").unwrap();
		let root = ServedRoot::new(folder.clone()).unwrap();
		let canonical = fs::canonicalize(&folder).unwrap();
		let resolved = root.resolve("/").unwrap().unwrap();
		assert!(resolved.starts_with(&canonical), "resolved {resolved:?} is not under {canonical:?}");
		fs::remove_dir_all(&folder).unwrap();
	}

	// A link is the one way a name inside the folder can name something outside it, so it is the case the containment rule exists for.
	#[cfg(unix)]
	#[test]
	fn resolve_answers_none_for_a_link_that_leaves_the_folder() {
		use std::os::unix::fs::symlink;
		let outside = create_unique_temp_folder("outside");
		fs::write(outside.join("secret.txt"), "secret\n").unwrap();
		let folder = create_unique_temp_folder("escape");
		symlink(&outside, folder.join("escape")).unwrap();
		let root = ServedRoot::new(folder.clone()).unwrap();
		assert_eq!(root.resolve("/escape/secret.txt").unwrap(), None);
		fs::remove_dir_all(&folder).unwrap();
		fs::remove_dir_all(&outside).unwrap();
	}

	#[cfg(unix)]
	#[test]
	fn resolve_serves_a_link_that_lands_inside_the_folder() {
		use std::os::unix::fs::symlink;
		let folder = create_unique_temp_folder("link-inside");
		fs::write(folder.join("real.txt"), "real\n").unwrap();
		symlink(folder.join("real.txt"), folder.join("link.txt")).unwrap();
		let root = ServedRoot::new(folder.clone()).unwrap();
		let resolved = root.resolve("/link.txt").unwrap();
		let name = resolved.as_deref().and_then(Path::file_name);
		assert_eq!(name, Some(OsStr::new("real.txt")));
		fs::remove_dir_all(&folder).unwrap();
	}

	// Candidates are built from the canonical folder, so a root whose own path later disappears still serves the files it was created on.
	#[cfg(unix)]
	#[test]
	fn resolve_serves_from_the_canonical_folder_after_the_given_path_is_gone() {
		use std::os::unix::fs::symlink;
		let folder = create_unique_temp_folder("canonical-root");
		fs::write(folder.join("a.txt"), "a\n").unwrap();
		let holder = create_unique_temp_folder("alias");
		let alias = holder.join("link");
		symlink(&folder, &alias).unwrap();
		let root = ServedRoot::new(alias.clone()).unwrap();
		fs::remove_file(&alias).unwrap();
		let resolved = root.resolve("/a.txt").unwrap();
		let name = resolved.as_deref().and_then(Path::file_name);
		assert_eq!(name, Some(OsStr::new("a.txt")));
		fs::remove_dir_all(&folder).unwrap();
		fs::remove_dir_all(&holder).unwrap();
	}
}
