use std::ffi::OsString;

// Whether the folder exists and whether the port binds are deliberately the caller's to check.
pub enum Dispatch {
	Install,
	Serve(OsString),
	WrongCount(usize),
}

pub fn classify(args: &[OsString]) -> Dispatch {
	match args {
		[] => Dispatch::Install,
		[folder] => Dispatch::Serve(folder.clone()),
		_ => Dispatch::WrongCount(args.len()),
	}
}

#[cfg(test)]
mod tests {
	use std::ffi::OsString;

	use super::{classify, Dispatch};

	const LAUNCHES: &[(&[&str], &str)] = &[
		(&[], "install"),
		(&["C:\\sites\\app"], "serve C:\\sites\\app"),
		(&["C:\\one", "C:\\two"], "reports 2 arguments"),
		(&["one", "two", "three"], "reports 3 arguments"),
	];

	#[test]
	fn classify_reports_what_each_argument_list_asks_for() {
		for (arguments, expected) in LAUNCHES {
			let args: Vec<OsString> = arguments.iter().map(OsString::from).collect();
			let got = match classify(&args) {
				Dispatch::Install => "install".to_string(),
				Dispatch::Serve(folder) => format!("serve {}", folder.to_string_lossy()),
				Dispatch::WrongCount(count) => format!("reports {count} arguments"),
			};
			assert_eq!(got, *expected, "case {args:?}");
		}
	}
}
