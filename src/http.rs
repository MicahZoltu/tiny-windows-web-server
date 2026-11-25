use std::fs;
use std::io::{self, Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use crate::mime::{mime_for, TEXT_PLAIN};
use crate::served_root::ServedRoot;

// The read and write timeout for a served connection, so a peer that stalls cannot hold a handler thread.
pub(crate) const TIMEOUT: Duration = Duration::from_secs(5);

// `TIMEOUT` bounds one read or one write; this bounds the bounded part of the exchange, the request head and getting the response started, so a peer that paces its bytes under the per-call limit cannot hold a handler thread before its transfer begins.
const CONNECTION_BUDGET: Duration = Duration::from_secs(30);

// A zero socket timeout means "wait forever", so no wait derived from what is left of the budget may round down to zero.
const MIN_WAIT: Duration = Duration::from_millis(1);

const MAX_HEAD: usize = 8 * 1024;

const BODY_CHUNK: usize = 16 * 1024;

#[derive(Debug, PartialEq)]
struct Request {
	method: String,
	target: String,
}

#[derive(Debug)]
struct BadRequest;

fn set_socket_timeouts(stream: &TcpStream) -> io::Result<()> {
	stream.set_read_timeout(Some(TIMEOUT))?;
	stream.set_write_timeout(Some(TIMEOUT))
}

// The host name a request carries is never inspected, and the listener binds loopback addresses only, so the peer is a local process.
// A local peer does not make an uninspected host name safe: a page open in a browser on this machine reaches this port too.
pub(crate) fn handle_connection(stream: TcpStream, root: &ServedRoot) {
	handle_connection_within(stream, root, CONNECTION_BUDGET);
}

// The budget is a parameter so a test can watch a dribbling peer being cut off without waiting out the real one.
fn handle_connection_within(mut stream: TcpStream, root: &ServedRoot, budget: Duration) {
	let deadline = deadline_after(budget);
	if set_socket_timeouts(&stream).is_err() {
		send_text(&mut stream, &deadline, 500, "could not configure the connection\n");
		return;
	}
	let head = match read_head(&mut stream, &deadline) {
		Ok(Some(head)) => head,
		// A head that fills the read cap without its blank line is refused: parsing it would answer a fragment of a longer request.
		Ok(None) => {
			send_text(&mut stream, &deadline, 400, "the request head is too long\n");
			return;
		}
		// A failed read means the peer is gone, or the connection outlived its budget; there is nobody left to answer.
		Err(_) => return,
	};
	let Ok(head) = String::from_utf8(head) else {
		send_text(&mut stream, &deadline, 400, "the request head is not UTF-8\n");
		return;
	};
	let Some(request_line) = head.lines().next() else {
		send_text(&mut stream, &deadline, 400, "the request head is empty\n");
		return;
	};
	let Ok(request) = parse_request_line(request_line) else {
		send_text(&mut stream, &deadline, 400, "the request line is malformed\n");
		return;
	};
	if request.method != "GET" {
		send_text(&mut stream, &deadline, 405, "only GET is supported\n");
		return;
	}
	let path = match root.resolve(&request.target) {
		Ok(Some(path)) => path,
		Ok(None) => {
			send_text(&mut stream, &deadline, 404, "not found\n");
			return;
		}
		// A target that tries to leave the folder is a 400, never a 404 that hides the rejection.
		Err(_) => {
			send_text(&mut stream, &deadline, 400, "the request target is not servable\n");
			return;
		}
	};
	// `path` came from `served_root::resolve`, which canonicalised it and confirmed it stays inside the served folder, so nothing here builds a path.
	let Ok(mut file) = fs::File::open(&path) else {
		send_text(&mut stream, &deadline, 500, "the file could not be opened\n");
		return;
	};
	let Ok(content_len) = file.metadata().map(|metadata| metadata.len()) else {
		send_text(&mut stream, &deadline, 500, "the file could not be inspected\n");
		return;
	};
	let head = response_head(200, mime_for(&path), content_len);
	// The budget's last checkpoint: a spent one closes the connection before the head is written, and nothing after this write is charged to the budget.
	let Ok(left) = wait(&deadline) else {
		return;
	};
	let _ = stream.set_write_timeout(Some(left));
	// A failed write means the peer hung up; there is nothing left to tell it.
	let _ = stream.write_all(head.as_bytes());
	// The declared length is what was measured, so the body stops there: a file that grew since would otherwise overrun its own header.
	let _ = copy_body(&mut file, &mut stream, content_len);
	let _ = stream.shutdown(Shutdown::Write);
}

// The budget covers the request head and the start of the response only; every wait in those two stages is shortened to what is left of it, so they cannot add up past the budget however the peer paces its bytes.
struct Deadline {
	ends_at: Instant,
}

fn deadline_after(budget: Duration) -> Deadline {
	Deadline { ends_at: Instant::now() + budget }
}

// The wait this exchange may still spend on its bounded part, or `Err` once the budget is gone.
fn wait(deadline: &Deadline) -> io::Result<Duration> {
	let left = deadline.ends_at.saturating_duration_since(Instant::now());
	if left.is_zero() {
		return Err(io::Error::new(io::ErrorKind::TimedOut, "the connection outlived its budget"));
	}
	Ok(left.min(TIMEOUT).max(MIN_WAIT))
}

// `Ok(None)` means the head filled the cap without its terminating blank line; `Ok(Some(head))` is a complete head, or the bytes read before the peer closed the connection.
fn read_head(stream: &mut TcpStream, deadline: &Deadline) -> io::Result<Option<Vec<u8>>> {
	let mut buffer = Vec::new();
	let mut chunk = [0u8; 1024];
	while buffer.len() < MAX_HEAD {
		// Each read waits no longer than what is left of the budget, so a peer dribbling one byte per call cannot restart the clock.
		stream.set_read_timeout(Some(wait(deadline)?))?;
		let want = (MAX_HEAD - buffer.len()).min(chunk.len());
		let read = stream.read(&mut chunk[..want])?;
		if read == 0 {
			return Ok(Some(buffer));
		}
		buffer.extend_from_slice(&chunk[..read]);
		if buffer.windows(4).any(|window| window == b"\r\n\r\n".as_slice()) {
			return Ok(Some(buffer));
		}
	}
	Ok(None)
}

// The body is copied by hand rather than with `io::copy` so the measured length is what stops it; each write waits no longer than `TIMEOUT`, so a client that keeps making progress finishes a long file and a client that stops is cut off.
fn copy_body(file: &mut fs::File, stream: &mut TcpStream, content_len: u64) -> io::Result<()> {
	// The response head may have left a shorter wait on the socket, and the budget does not reach the body.
	stream.set_write_timeout(Some(TIMEOUT))?;
	let mut buffer = vec![0u8; BODY_CHUNK];
	let mut sent = 0u64;
	while sent < content_len {
		let want = (content_len - sent).min(BODY_CHUNK as u64) as usize;
		let read = file.read(&mut buffer[..want])?;
		if read == 0 {
			return Err(io::Error::new(io::ErrorKind::UnexpectedEof, "the file ended before its measured length"));
		}
		stream.write_all(&buffer[..read])?;
		sent += read as u64;
	}
	Ok(())
}

fn send_text(stream: &mut TcpStream, deadline: &Deadline, status: u16, message: &str) {
	// An error response is charged to the same budget, so a peer that spent it gets silence: the close is what tells it the request failed.
	let Ok(left) = wait(deadline) else {
		return;
	};
	let _ = stream.set_write_timeout(Some(left));
	let head = response_head(status, TEXT_PLAIN, message.len() as u64);
	let _ = stream.write_all(head.as_bytes());
	let _ = stream.write_all(message.as_bytes());
	let _ = stream.shutdown(Shutdown::Write);
}

fn response_head(status: u16, content_type: &str, content_len: u64) -> String {
	let reason = reason_for(status);
	format!("HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {content_len}\r\nConnection: close\r\n\r\n")
}

fn reason_for(status: u16) -> &'static str {
	match status {
		200 => "OK",
		400 => "Bad Request",
		404 => "Not Found",
		405 => "Method Not Allowed",
		500 => "Internal Server Error",
		_ => "",
	}
}

fn parse_request_line(line: &str) -> Result<Request, BadRequest> {
	let mut parts = line.split_whitespace();
	let method = parts.next().ok_or(BadRequest)?;
	let target = parts.next().ok_or(BadRequest)?;
	if let Some(version) = parts.next() {
		if !version.starts_with("HTTP/") {
			return Err(BadRequest);
		}
	}
	if parts.next().is_some() {
		return Err(BadRequest);
	}
	Ok(Request {
		method: method.to_string(),
		target: target.to_string(),
	})
}

#[cfg(test)]
pub(crate) mod tests {
	use std::fs;
	use std::io::{Read, Write};
	use std::net::{Ipv4Addr, SocketAddr, TcpListener, TcpStream};
	use std::thread;
	use std::time::Duration;

	use crate::mime::{TEXT_HTML, TEXT_PLAIN};
	use crate::served_root::{create_unique_temp_folder, ServedRoot};

	use super::{deadline_after, handle_connection, handle_connection_within, parse_request_line, response_head, wait, Request, MAX_HEAD, TIMEOUT};

	const IPV4_LOOPBACK: &str = "127.0.0.1";

	const REQUEST_LINES: &[(&str, &str, &str)] = &[
		("GET / HTTP/1.1", "GET", "/"),
		("GET / HTTP/1.0", "GET", "/"),
		("GET /", "GET", "/"),
		("GET /a/b%20c.txt?x=1 HTTP/1.1", "GET", "/a/b%20c.txt?x=1"),
		("POST /index.html HTTP/1.1", "POST", "/index.html"),
	];

	const BAD_LINES: &[&str] = &[
		"",
		"   ",
		"GET",
		"GET / HTTP/1.1 extra",
		"GET / FTP/1.0",
		"/only-a-target",
	];

	const STATUSES: &[(u16, &str)] = &[
		(200, "OK"),
		(400, "Bad Request"),
		(404, "Not Found"),
		(405, "Method Not Allowed"),
		(500, "Internal Server Error"),
	];

	pub(crate) fn exchange(port: u16, request: Vec<u8>) -> String {
		exchange_within(SocketAddr::from((Ipv4Addr::LOCALHOST, port)), request)
	}

	// The address is a parameter so a test can reach the `::1` listener as well as the IPv4 loopback.
	pub(crate) fn exchange_within(address: SocketAddr, request: Vec<u8>) -> String {
		let mut stream = TcpStream::connect(address).unwrap();
		stream.set_read_timeout(Some(TIMEOUT)).unwrap();
		stream.write_all(&request).unwrap();
		let mut response = Vec::new();
		let _ = stream.read_to_end(&mut response);
		String::from_utf8_lossy(&response).into_owned()
	}

	#[test]
	fn parse_request_line_accepts_valid_lines() {
		for (line, method, target) in REQUEST_LINES {
			let request = parse_request_line(line).unwrap();
			let expected = Request {
				method: (*method).to_string(),
				target: (*target).to_string(),
			};
			assert_eq!(request, expected, "case {line:?}");
		}
	}

	#[test]
	fn parse_request_line_rejects_garbage() {
		for line in BAD_LINES {
			assert!(parse_request_line(line).is_err(), "case {line:?}");
		}
	}

	#[test]
	fn response_head_contains_framing() {
		let head = response_head(200, TEXT_HTML, 42);
		assert!(head.starts_with("HTTP/1.1 200 OK\r\n"), "head {head:?}");
		assert!(head.contains(&format!("\r\nContent-Type: {TEXT_HTML}\r\n")), "head {head:?}");
		assert!(head.contains("\r\nContent-Length: 42\r\n"), "head {head:?}");
		assert!(head.contains("\r\nConnection: close\r\n"), "head {head:?}");
		assert!(head.ends_with("\r\n\r\n"), "head {head:?}");
	}

	#[test]
	fn response_head_names_each_status_reason() {
		for (status, reason) in STATUSES {
			let head = response_head(*status, TEXT_PLAIN, 3);
			let line = format!("HTTP/1.1 {status} {reason}\r\n");
			assert!(head.starts_with(&line), "case {status}");
		}
	}

	// A budget shorter than `TIMEOUT` is the only case that can tell a wait shortened to what is left from a fresh `TIMEOUT`.
	#[test]
	fn deadline_shortens_every_wait_to_what_is_left() {
		let budget = Duration::from_secs(1);
		let left = wait(&deadline_after(budget)).unwrap();
		assert!(left <= budget, "wait {left:?} outlived the budget {budget:?}");
	}

	#[test]
	fn deadline_refuses_a_wait_once_the_budget_is_spent() {
		let deadline = deadline_after(Duration::ZERO);
		assert!(wait(&deadline).is_err(), "a spent budget must refuse a wait");
	}

	// Pins the fixture constant to the address the tests dial: `IPV4_LOOPBACK` must equal `Ipv4Addr::LOCALHOST`, because the socket tests bind the literal while `exchange` dials the address, so a spelling that drifted would surface as a refused connect in every one of those tests rather than here.
	#[test]
	fn loopback_spelling_parses_to_the_ipv4_address_the_bind_uses() {
		assert_eq!(IPV4_LOOPBACK.parse::<Ipv4Addr>().unwrap(), Ipv4Addr::LOCALHOST, "the fixture constant must name the IPv4 loopback");
	}

	// The listener is the caller's so a test can serve several requests on one bound port.
	fn serve_on(listener: &TcpListener, request: Vec<u8>, root: &ServedRoot) -> String {
		let port = listener.local_addr().unwrap().port();
		let client = thread::spawn(move || exchange(port, request));
		let (stream, _) = listener.accept().unwrap();
		handle_connection(stream, root);
		client.join().unwrap()
	}

	fn respond_to(request: Vec<u8>, root: &ServedRoot) -> String {
		serve_on(&TcpListener::bind((IPV4_LOOPBACK, 0)).unwrap(), request, root)
	}

	// A head whose first line is a whole request but which never ends: parsing it would answer a file the peer may not have asked for.
	fn head_that_fills_the_cap() -> Vec<u8> {
		let mut head = b"GET /index.html HTTP/1.1\r\nX-Padding: ".to_vec();
		head.resize(MAX_HEAD, b'a');
		head
	}

	#[test]
	fn handle_connection_rejects_capped_head() {
		let folder = create_unique_temp_folder("http");
		let root = ServedRoot::new(folder.clone()).unwrap();
		let response = respond_to(head_that_fills_the_cap(), &root);
		assert!(response.starts_with("HTTP/1.1 400"), "response {response:?}");
		// The body names the cap, so a 400 for a malformed line or a refused target cannot pass for this one.
		assert!(response.ends_with("the request head is too long\n"), "response {response:?}");
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn handle_connection_rejects_non_get_with_405() {
		let folder = create_unique_temp_folder("http");
		let root = ServedRoot::new(folder.clone()).unwrap();
		let response = respond_to(b"POST /index.html HTTP/1.1\r\n\r\n".to_vec(), &root);
		assert!(response.starts_with("HTTP/1.1 405"), "response {response:?}");
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn handle_connection_serves_the_index_of_the_folder() {
		let folder = create_unique_temp_folder("http");
		fs::write(folder.join("index.html"), "<title>served</title>\n").unwrap();
		fs::write(folder.join("style.css"), "body { color: red }\n").unwrap();
		let root = ServedRoot::new(folder.clone()).unwrap();
		let response = respond_to(b"GET / HTTP/1.1\r\n\r\n".to_vec(), &root);
		assert!(response.starts_with("HTTP/1.1 200 OK"), "response {response:?}");
		assert!(response.contains("<title>served</title>"), "{response:?}");
		// The folder serves its stylesheet with the table's type, so this assertion covers the `mime_for` call over a real socket.
		let stylesheet = respond_to(b"GET /style.css HTTP/1.1\r\n\r\n".to_vec(), &root);
		assert!(stylesheet.contains("\r\nContent-Type: text/css; charset=utf-8\r\n"), "response {stylesheet:?}");
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn handle_connection_answers_404_for_a_missing_file() {
		let folder = create_unique_temp_folder("http");
		let root = ServedRoot::new(folder.clone()).unwrap();
		for target in ["/missing.txt", "/missing/"] {
			let response = respond_to(format!("GET {target} HTTP/1.1\r\n\r\n").into_bytes(), &root);
			assert!(response.starts_with("HTTP/1.1 404"), "case {target}: {response:?}");
		}
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn handle_connection_answers_400_for_a_traversing_target() {
		let folder = create_unique_temp_folder("http");
		let root = ServedRoot::new(folder.clone()).unwrap();
		let response = respond_to(b"GET /../secret.txt HTTP/1.1\r\n\r\n".to_vec(), &root);
		assert!(response.starts_with("HTTP/1.1 400"), "response {response:?}");
		// The body names the refused target, so the 400 cannot be the head cap or a malformed request line instead.
		assert!(response.ends_with("the request target is not servable\n"), "response {response:?}");
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn handle_connection_serves_whatever_host_the_request_names() {
		let folder = create_unique_temp_folder("http");
		fs::write(folder.join("index.html"), "<title>served</title>\n").unwrap();
		let root = ServedRoot::new(folder.clone()).unwrap();
		// One listener serves every request in the table, so the loop cannot pass on separately bound ports.
		let listener = TcpListener::bind((IPV4_LOOPBACK, 0)).unwrap();
		let port = listener.local_addr().unwrap().port();
		let other_port = u32::from(port) + 1;
		let hosts = [
			("another name", format!("attacker.example:{port}")),
			("an alternate name", format!("folder-server.localhost:{port}")),
			("another address", format!("127.0.0.2:{port}")),
			("another port", format!("LocalHost:{other_port}")),
			("the IPv6 loopback", format!("[::1]:{port}")),
			("the bare loopback address", String::from("127.0.0.1")),
			("the bare advertised name", String::from("localhost")),
			("an empty value", String::new()),
			("two values joined by a comma", format!("localhost:{port}, localhost:{port}")),
		];
		for (case, host) in hosts {
			let request = format!("GET / HTTP/1.1\r\nHost: {host}\r\n\r\n").into_bytes();
			let response = serve_on(&listener, request, &root);
			assert!(response.starts_with("HTTP/1.1 200 OK"), "case {case}: host {host:?}: {response:?}");
			assert!(response.contains("<title>served</title>"), "case {case}: host {host:?}: {response:?}");
		}
		fs::remove_dir_all(&folder).unwrap();
	}

	#[test]
	fn handle_connection_serves_a_request_naming_no_host_or_two() {
		let folder = create_unique_temp_folder("http");
		fs::write(folder.join("index.html"), "<title>served</title>\n").unwrap();
		let root = ServedRoot::new(folder.clone()).unwrap();
		let no_host = respond_to(b"GET / HTTP/1.1\r\n\r\n".to_vec(), &root);
		assert!(no_host.starts_with("HTTP/1.1 200 OK"), "no Host header: {no_host:?}");
		assert!(no_host.contains("<title>served</title>"), "no Host header: {no_host:?}");
		let two_hosts = respond_to(b"GET / HTTP/1.1\r\nHost: attacker.example\r\nHost: 127.0.0.1:1\r\n\r\n".to_vec(), &root);
		assert!(two_hosts.starts_with("HTTP/1.1 200 OK"), "two Host headers: {two_hosts:?}");
		assert!(two_hosts.contains("<title>served</title>"), "two Host headers: {two_hosts:?}");
		fs::remove_dir_all(&folder).unwrap();
	}

	// A peer that sends a head without its blank line and then stops is cut off by the budget: the read that follows waits out what is left of it and fails, so no response is written.
	#[test]
	fn handle_connection_within_cuts_off_a_peer_that_never_finishes_its_head() {
		let folder = create_unique_temp_folder("http");
		let root = ServedRoot::new(folder.clone()).unwrap();
		let listener = TcpListener::bind((IPV4_LOOPBACK, 0)).unwrap();
		let port = listener.local_addr().unwrap().port();
		let client = thread::spawn(move || {
			let mut stream = TcpStream::connect((IPV4_LOOPBACK, port)).unwrap();
			stream.set_read_timeout(Some(TIMEOUT)).unwrap();
			// The peer may be gone once the budget is spent, and a refused write is not what this test measures.
			let _ = stream.write_all(b"GET / HTTP/1.1\r\n");
			let mut response = Vec::new();
			let _ = stream.read_to_end(&mut response);
			response
		});
		let (stream, _) = listener.accept().unwrap();
		handle_connection_within(stream, &root, Duration::from_millis(50));
		let response = client.join().unwrap();
		assert!(response.is_empty(), "a peer that never finished its head got a response: {:?}", String::from_utf8_lossy(&response));
		fs::remove_dir_all(&folder).unwrap();
	}

	// The body overflows the loopback socket buffers, so its writes block on the client and every byte still has to arrive.
	#[test]
	fn handle_connection_delivers_a_body_that_overflows_the_socket_buffers() {
		const BODY_LEN: usize = 32 * 1024 * 1024;
		let folder = create_unique_temp_folder("http");
		fs::write(folder.join("big.bin"), vec![b'x'; BODY_LEN]).unwrap();
		let root = ServedRoot::new(folder.clone()).unwrap();
		let response = respond_to(b"GET /big.bin HTTP/1.1\r\n\r\n".to_vec(), &root);
		let response = response.as_bytes();
		let body_start = response.windows(4).position(|window| window == b"\r\n\r\n").map(|end| end + 4);
		let body = body_start.map_or(&[][..], |start| &response[start..]);
		assert!(response.starts_with(b"HTTP/1.1 200 OK"), "response starts {:?}", String::from_utf8_lossy(&response[..response.len().min(32)]));
		assert_eq!(body.len(), BODY_LEN, "the transfer stopped after {} of {BODY_LEN} bytes", body.len());
		assert!(body.iter().all(|byte| *byte == b'x'), "the body is not the file's bytes");
		fs::remove_dir_all(&folder).unwrap();
	}
}
