use std::io;
use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use crate::http::handle_connection;
use crate::served_root::ServedRoot;

// A transient accept error leaves the listener usable, so the loop pauses this long and keeps accepting instead of ending the folder's service.
const ACCEPT_BACKOFF: Duration = Duration::from_millis(50);

// Each retry asks the kernel for a fresh port, but the kernel may hand back the same number because the IPv4 listener is dropped first, so this attempt cap is a probabilistic guard against a launch that keeps losing the race, not a promise that the next port differs.
const BIND_ATTEMPTS: u32 = 4;

/// Port 0 lets the kernel choose a free port atomically, so two launches cannot race for the same number; `Err` may leave one accept thread answering on a port the caller never got, because a spawned thread cannot be recalled.
pub(crate) fn start(root: ServedRoot) -> io::Result<u16> {
	let (ipv4, ipv6) = bind_loopback()?;
	let port = ipv4.local_addr()?.port();
	let root = Arc::new(root);
	let ipv4_root = Arc::clone(&root);
	// One accept thread per listener, so a browser that dials whichever family `localhost` resolves first is answered at once rather than waiting behind the other family's connections.
	thread::Builder::new().spawn(move || accept_connections(ipv4, ipv4_root))?;
	thread::Builder::new().spawn(move || accept_connections(ipv6, root))?;
	Ok(port)
}

// Both loopback families share the one port the kernel picks, because `address` advertises `localhost`, so the IPv6 side must answer the port the advertised URL names.
fn bind_loopback() -> io::Result<(TcpListener, TcpListener)> {
	bind_loopback_within(TcpListener::bind)
}

// The bind is a parameter so a test can send back a taken `::1` port, which no real socket can arrange because the port in play is the one the kernel just chose for the IPv4 listener; `FnMut` because those test closures record each request into a captured `Vec`.
fn bind_loopback_within(mut bind: impl FnMut(SocketAddr) -> io::Result<TcpListener>) -> io::Result<(TcpListener, TcpListener)> {
	let mut attempts = 0;
	loop {
		attempts += 1;
		// `Ipv4Addr::LOCALHOST` rather than a wildcard address, since a listener on `0.0.0.0` would serve the folder to the whole network.
		let ipv4 = bind(SocketAddr::from((Ipv4Addr::LOCALHOST, 0)))?;
		let port = ipv4.local_addr()?.port();
		match bind(SocketAddr::from((Ipv6Addr::LOCALHOST, port))) {
			Ok(ipv6) => return Ok((ipv4, ipv6)),
			Err(error) => {
				// Every `::1` bind failure earns a retry, because a port another process took is the common case and the next attempt usually wins it; the `>=` keeps the exit reachable for any value of `BIND_ATTEMPTS`.
				if attempts >= BIND_ATTEMPTS {
					return Err(error);
				}
			}
		}
	}
}

const ADVERTISED_HOST: &str = "localhost";

// The address a browser opens: the name `ADVERTISED_HOST` holds resolves to both loopback families this program binds, so the user never has to read an IP literal.
pub(crate) fn address(port: u16) -> String {
	format!("http://{ADVERTISED_HOST}:{port}/")
}

// The loop ends only when the process does: stopping a folder is exiting, and the OS closes the listening sockets then.
fn accept_connections(listener: TcpListener, root: Arc<ServedRoot>) {
	for stream in listener.incoming() {
		let stream = match stream {
			Ok(stream) => stream,
			// A peer that dropped before the handshake is not a reason to stop accepting.
			Err(error) if error.kind() == io::ErrorKind::ConnectionAborted => continue,
			// Descriptor exhaustion and the like pass on their own, so the folder keeps answering instead of dying while its tray icon still looks healthy.
			Err(_) => {
				thread::sleep(ACCEPT_BACKOFF);
				continue;
			}
		};
		let root = Arc::clone(&root);
		// A refused spawn is not worth a panic: the dropped stream is the only way left to tell that peer the request failed.
		let _ = thread::Builder::new().spawn(move || handle_connection(stream, &root));
	}
}

#[cfg(test)]
mod tests {
	use std::fs;
	use std::io;
	use std::net::{Ipv4Addr, Ipv6Addr, SocketAddr, TcpListener};

	use crate::http::tests::{exchange, exchange_within};
	use crate::served_root::{create_unique_temp_folder, ServedRoot};

	use super::{address, bind_loopback_within, start, BIND_ATTEMPTS};

	// A failure text of the test's own, so an assertion can tell the reported message carried this text rather than a paraphrase of it.
	const TAKEN_PORT: &str = "the stand-in port is taken";

	#[test]
	fn start_serves_the_folder_on_loopback() {
		let folder = create_unique_temp_folder("get");
		fs::write(folder.join("index.html"), "<!doctype html><title>served</title>\n").unwrap();
		let port = start(ServedRoot::new(folder.clone()).unwrap()).unwrap();
		let response = exchange(port, b"GET / HTTP/1.1\r\n\r\n".to_vec());
		assert!(response.starts_with("HTTP/1.1 200"), "response {response:?}");
		assert!(response.contains("<title>served</title>"), "{response:?}");
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn address_names_localhost_on_the_given_port() {
		let url = address(5000);
		assert_eq!(url, "http://localhost:5000/");
	}

	#[test]
	fn start_serves_the_folder_over_ipv6() {
		let folder = create_unique_temp_folder("ipv6");
		fs::write(folder.join("index.html"), "<!doctype html><title>served</title>\n").unwrap();
		let port = start(ServedRoot::new(folder.clone()).unwrap()).unwrap();
		let response = exchange(port, b"GET / HTTP/1.1\r\n\r\n".to_vec());
		assert!(response.starts_with("HTTP/1.1 200"), "response {response:?}");
		assert!(response.contains("<title>served</title>"), "{response:?}");
		let request = format!("GET / HTTP/1.1\r\nHost: [::1]:{port}\r\n\r\n").into_bytes();
		let response = exchange_within(SocketAddr::from((Ipv6Addr::LOCALHOST, port)), request);
		assert!(response.starts_with("HTTP/1.1 200"), "response {response:?}");
		assert!(response.contains("<title>served</title>"), "{response:?}");
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn bind_loopback_within_retries_on_a_fresh_port_when_the_ipv6_one_is_taken() {
		let mut requests = Vec::new();
		let mut bound = Vec::new();
		let mut ipv6_attempts = 0;
		let bind = |requested: SocketAddr| -> io::Result<TcpListener> {
			requests.push(requested);
			match requested {
				SocketAddr::V4(_) => {
					let listener = TcpListener::bind(requested)?;
					bound.push(listener.local_addr()?.port());
					Ok(listener)
				}
				// A port another program holds is what the first `::1` request stands for; the second gets the listener it asked for, so the returned pair carries a real `::1` listener.
				SocketAddr::V6(_) => {
					ipv6_attempts += 1;
					if ipv6_attempts == 1 {
						Err(io::Error::from(io::ErrorKind::AddrInUse))
					} else {
						TcpListener::bind(requested)
					}
				}
			}
		};
		let (ipv4, ipv6) = bind_loopback_within(bind).unwrap();
		assert_eq!(requests.len(), 4, "one forced ::1 failure must leave two attempts of two requests, requests {requests:?}");
		// Each request is asserted as a whole socket address, so a bind site that asked for a wildcard address fails here even though its family and port are still right.
		assert_eq!(requests[0], SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), "the first bind must ask for the IPv4 loopback on a kernel-chosen port, requests {requests:?}");
		assert_eq!(requests[1], SocketAddr::from((Ipv6Addr::LOCALHOST, bound[0])), "the IPv6 bind must ask for the loopback on the port the IPv4 listener got, requests {requests:?}");
		assert_eq!(requests[2], SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), "a retry must ask the kernel for a fresh port on the IPv4 loopback, requests {requests:?}");
		assert_eq!(requests[3], SocketAddr::from((Ipv6Addr::LOCALHOST, bound[1])), "the retry must ask for the IPv6 loopback on the fresh port, requests {requests:?}");
		let port = ipv4.local_addr().unwrap().port();
		assert_eq!(port, bound[1], "port {port} bound {bound:?}");
		assert_eq!(ipv6.local_addr().unwrap(), SocketAddr::from((Ipv6Addr::LOCALHOST, bound[1])), "the IPv6 listener must answer the loopback on the port the IPv4 listener got");
	}

	#[test]
	fn bind_loopback_within_releases_the_ipv4_port_before_it_retries() {
		let mut earlier_port = None;
		let mut rebound = None;
		let mut ipv6_attempts = 0;
		let bind = |requested: SocketAddr| -> io::Result<TcpListener> {
			match requested {
				SocketAddr::V4(_) => {
					// An IPv4 listener an abandoned attempt returned holds its port until it drops, so binding that same port here fails while it is still alive.
					if let Some(port) = earlier_port {
						let earlier = SocketAddr::from((Ipv4Addr::LOCALHOST, port));
						rebound = Some(TcpListener::bind(earlier).is_ok());
					}
					let listener = TcpListener::bind(requested)?;
					earlier_port = Some(listener.local_addr()?.port());
					Ok(listener)
				}
				// One forced `::1` failure is what reaches the retry this test measures, and the second attempt gets the listener it asked for.
				SocketAddr::V6(_) => {
					ipv6_attempts += 1;
					if ipv6_attempts == 1 {
						Err(io::Error::from(io::ErrorKind::AddrInUse))
					} else {
						TcpListener::bind(requested)
					}
				}
			}
		};
		let (_, ipv6) = bind_loopback_within(bind).unwrap();
		assert_eq!(rebound, Some(true), "the abandoned attempt's IPv4 port must be free by the time the retry binds");
		assert_eq!(ipv6.local_addr().unwrap(), SocketAddr::from((Ipv6Addr::LOCALHOST, earlier_port.unwrap())), "the IPv6 listener must answer the loopback on the port the IPv4 listener got");
	}

	#[test]
	fn bind_loopback_within_reports_the_ipv6_bind_once_the_attempts_are_spent() {
		let mut requests = Vec::new();
		let mut bound = Vec::new();
		let mut ipv6_attempts = 0;
		let bind = |requested: SocketAddr| -> io::Result<TcpListener> {
			requests.push(requested);
			match requested {
				SocketAddr::V4(_) => {
					let listener = TcpListener::bind(requested)?;
					bound.push(listener.local_addr()?.port());
					Ok(listener)
				}
				SocketAddr::V6(_) => {
					ipv6_attempts += 1;
					Err(io::Error::new(io::ErrorKind::AddrInUse, TAKEN_PORT))
				}
			}
		};
		let error = bind_loopback_within(bind).unwrap_err();
		assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "error {error:?}");
		assert_eq!(ipv6_attempts, BIND_ATTEMPTS, "the launch made {ipv6_attempts} attempts");
		assert_eq!(requests[0], SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), "the first bind must ask for the IPv4 loopback on a kernel-chosen port, requests {requests:?}");
		assert_eq!(requests[1], SocketAddr::from((Ipv6Addr::LOCALHOST, bound[0])), "the IPv6 bind must ask for the loopback on the port the IPv4 listener got, requests {requests:?}");
		// The failure of the last attempt reaches the caller unchanged, so `main`'s dialog carries the errno it reports rather than a summary of it.
		assert_eq!(error.to_string(), TAKEN_PORT, "error {error:?}");
	}

	#[test]
	fn bind_loopback_within_reports_a_failed_ipv4_bind_without_retrying() {
		let mut requests = Vec::new();
		let bind = |requested: SocketAddr| -> io::Result<TcpListener> {
			requests.push(requested);
			Err(io::Error::new(io::ErrorKind::AddrInUse, TAKEN_PORT))
		};
		let error = bind_loopback_within(bind).unwrap_err();
		assert_eq!(error.kind(), io::ErrorKind::AddrInUse, "error {error:?}");
		// The bind's own failure reaches the caller, so nothing in the message is a rewording of it.
		assert_eq!(error.to_string(), TAKEN_PORT, "error {error:?}");
		// A failed IPv4 bind ends the launch, so the one request recorded here is also the proof that no retry was attempted.
		assert_eq!(requests.len(), 1, "a failed IPv4 bind must ask once, requests {requests:?}");
		assert_eq!(requests[0], SocketAddr::from((Ipv4Addr::LOCALHOST, 0)), "the first bind must ask for the IPv4 loopback on a kernel-chosen port, requests {requests:?}");
	}
}
