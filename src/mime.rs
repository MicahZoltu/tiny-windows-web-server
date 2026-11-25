use std::ffi::OsStr;
use std::path::Path;

const OCTET_STREAM: &str = "application/octet-stream";

// The one spelling of the plain-text type: the table's `txt` entry and the HTTP error bodies both use it, so they cannot drift apart.
pub(crate) const TEXT_PLAIN: &str = "text/plain; charset=utf-8";

pub(crate) const TEXT_HTML: &str = "text/html; charset=utf-8";

const TEXT_JAVASCRIPT: &str = "text/javascript; charset=utf-8";

const APPLICATION_JSON: &str = "application/json; charset=utf-8";

const IMAGE_JPEG: &str = "image/jpeg";

const TYPES: &[(&str, &str)] = &[
	("html", TEXT_HTML),
	("htm", TEXT_HTML),
	("css", "text/css; charset=utf-8"),
	("js", TEXT_JAVASCRIPT),
	("mjs", TEXT_JAVASCRIPT),
	("json", APPLICATION_JSON),
	("txt", TEXT_PLAIN),
	("svg", "image/svg+xml"),
	("png", "image/png"),
	("jpg", IMAGE_JPEG),
	("jpeg", IMAGE_JPEG),
	("gif", "image/gif"),
	("webp", "image/webp"),
	("ico", "image/vnd.microsoft.icon"),
	("woff", "font/woff"),
	("woff2", "font/woff2"),
	("ttf", "font/ttf"),
	("otf", "font/otf"),
	("wasm", "application/wasm"),
	("xml", "application/xml; charset=utf-8"),
	("pdf", "application/pdf"),
	("mp4", "video/mp4"),
	("webm", "video/webm"),
	("mp3", "audio/mpeg"),
	("map", APPLICATION_JSON),
];

pub(crate) fn mime_for(path: &Path) -> &'static str {
	let Some(extension) = path.extension().and_then(OsStr::to_str) else {
		return OCTET_STREAM;
	};
	for &(name, mime) in TYPES {
		if extension.eq_ignore_ascii_case(name) {
			return mime;
		}
	}
	OCTET_STREAM
}

#[cfg(test)]
mod tests {
	use std::path::Path;

	use super::mime_for;

	const KNOWN: &[(&str, &str)] = &[
		("index.html", "text/html; charset=utf-8"),
		("index.htm", "text/html; charset=utf-8"),
		("style.css", "text/css; charset=utf-8"),
		("app.js", "text/javascript; charset=utf-8"),
		("app.mjs", "text/javascript; charset=utf-8"),
		("data.json", "application/json; charset=utf-8"),
		("notes.txt", "text/plain; charset=utf-8"),
		("icon.svg", "image/svg+xml"),
		("photo.png", "image/png"),
		("photo.jpg", "image/jpeg"),
		("photo.jpeg", "image/jpeg"),
		("anim.gif", "image/gif"),
		("photo.webp", "image/webp"),
		("favicon.ico", "image/vnd.microsoft.icon"),
		("font.woff", "font/woff"),
		("font.woff2", "font/woff2"),
		("font.ttf", "font/ttf"),
		("font.otf", "font/otf"),
		("module.wasm", "application/wasm"),
		("feed.xml", "application/xml; charset=utf-8"),
		("doc.pdf", "application/pdf"),
		("clip.mp4", "video/mp4"),
		("clip.webm", "video/webm"),
		("track.mp3", "audio/mpeg"),
		("app.js.map", "application/json; charset=utf-8"),
		("PHOTO.PNG", "image/png"),
		("INDEX.Html", "text/html; charset=utf-8"),
		("APP.JS", "text/javascript; charset=utf-8"),
	];

	const UNKNOWN: &[&str] = &[
		"README",
		"noext",
		"data.zzz",
		"a.tar.gz",
		".hidden",
	];

	#[test]
	fn mime_for_known_extension() {
		for (path, expected) in KNOWN {
			assert_eq!(mime_for(Path::new(path)), *expected, "case {path:?}");
		}
	}

	#[test]
	fn mime_for_unknown_extension() {
		for path in UNKNOWN {
			let got = mime_for(Path::new(path));
			assert_eq!(got, "application/octet-stream", "case {path:?}");
		}
	}
}
