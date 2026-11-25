use std::mem::{size_of, size_of_val};
use std::ptr::{copy_nonoverlapping, null, null_mut};

use windows_sys::Win32::Foundation::{GetLastError, GlobalFree, HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows_sys::Win32::System::DataExchange::{CloseClipboard, EmptyClipboard, OpenClipboard, SetClipboardData};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Memory::{GlobalAlloc, GlobalLock, GlobalUnlock, GMEM_MOVEABLE};
use windows_sys::Win32::System::Ole::CF_UNICODETEXT;
use windows_sys::Win32::UI::Shell::{ShellExecuteW, Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NOTIFYICONDATAW};
use windows_sys::Win32::UI::WindowsAndMessaging::{AppendMenuW, CreatePopupMenu, CreateWindowExW, DefWindowProcW, DestroyIcon, DestroyMenu, DestroyWindow, DispatchMessageW, GetCursorPos, GetMessageW, GetSystemMetrics, GetWindowLongPtrW, LoadIconW, LoadImageW, PostMessageW, PostQuitMessage, RegisterClassW, SetForegroundWindow, SetWindowLongPtrW, TrackPopupMenu, TranslateMessage, GWLP_USERDATA, HICON, HMENU, IDI_APPLICATION, IMAGE_ICON, MENU_ITEM_FLAGS, MF_DISABLED, MF_GRAYED, MF_STRING, MSG, SM_CXSMICON, SM_CYSMICON, SW_SHOWNORMAL, TPM_LEFTALIGN, TPM_RETURNCMD, TPM_RIGHTBUTTON, WM_APP, WM_LBUTTONUP, WM_NULL, WM_RBUTTONUP, WNDCLASSW};

use super::{to_wide_nul_terminated, warn, Failure, PRODUCT_NAME};

// The window stays a hidden top-level window: a HWND_MESSAGE window never receives the shell's tray callbacks.
const WINDOW_CLASS: &str = "folderserve.tray.window";

// szTip holds 128 units including the terminating NUL, so the copy is capped at the 127 units that fit before it.
const TIP_CAP: usize = 127;

const TRAY_ICON_ID: u32 = 1;

// The id that the resource script `folderserve.rc` gives the application icon, which the executable carries as its own file icon; the script's id and this one have to match, or the tray quietly shows the stock icon.
const APP_ICON_ID: usize = 1;

const WM_TRAY_CALLBACK: u32 = WM_APP + 1;

// The ids start at 1, because TrackPopupMenu returns 0 for "nothing selected" and 0 must never name a row; the folder row's id can never be returned, because the row is disabled.
const MENU_ID_FOLDER: usize = 1;
const MENU_ID_OPEN: usize = 2;
const MENU_ID_COPY: usize = 3;
const MENU_ID_STOP: usize = 4;

const COPY_LABEL: &str = "Copy URL";

const STOP_LABEL: &str = "Stop serving";

// A fatal startup failure exits only once the dialog is dismissed, so the folder keeps answering while it is on screen.
const EXIT_ON_DISMISS: &str = "The process exits when you dismiss this dialog, and the folder it serves stops answering then.";

// A window procedure is a bare C callback with no context parameter, so the menu's folder and URL reach it through the window's GWLP_USERDATA slot.
struct WindowState {
	folder: &'static [u16],
	url: &'static [u16],
}

// A handle this process loaded from its own module is its own to destroy; the stock fallback belongs to the system and is shared by every process, so destroying it would pull the icon out from under them.
enum TrayIcon {
	Loaded(HICON),
	Stock(HICON),
}

impl TrayIcon {
	fn handle(&self) -> HICON {
		match *self {
			TrayIcon::Loaded(handle) | TrayIcon::Stock(handle) => handle,
		}
	}

	fn destroy(self) {
		let TrayIcon::Loaded(handle) = self else {
			return;
		};
		// SAFETY: `handle` came from this process's own LoadImageW call and is destroyed exactly once, here.
		unsafe { DestroyIcon(handle) };
	}
}

fn last_error(action: &'static str) -> Failure {
	// SAFETY: GetLastError reads this thread's own error code and touches no caller memory.
	Failure::win32(action, unsafe { GetLastError() })
}

pub fn run(folder: &str, url: &str, tip: &str) -> i32 {
	// Win32 reads a menu item's text for as long as the popup is displayed, so both labels are leaked for the process lifetime and handed to the window procedure at creation.
	let state: &'static WindowState = Box::leak(Box::new(WindowState {
		folder: to_wide_nul_terminated(folder).leak(),
		url: to_wide_nul_terminated(url).leak(),
	}));
	let hwnd = match create_window(state) {
		Ok(hwnd) => hwnd,
		Err(failure) => {
			warn(&format!("{PRODUCT_NAME} could not {failure}. {EXIT_ON_DISMISS}"));
			return 1;
		}
	};
	let icon = match add_tray_icon(hwnd, tip) {
		Ok(icon) => icon,
		Err(failure) => {
			warn(&format!("{PRODUCT_NAME} could not {failure}. {EXIT_ON_DISMISS}"));
			destroy_window(hwnd);
			return 1;
		}
	};
	let exit_code = message_loop();
	// The one NIM_DELETE on every path out of the loop, a failed GetMessageW included, so no exit leaves a ghost icon.
	remove_tray_icon(hwnd);
	// The shell holds the icon handle until the NIM_DELETE above, which is why the handle is freed only after it.
	icon.destroy();
	destroy_window(hwnd);
	exit_code
}

fn create_window(state: &'static WindowState) -> Result<HWND, Failure> {
	// SAFETY: a null module name asks for this executable's own handle, which always exists.
	let instance = unsafe { GetModuleHandleW(null()) };
	// The system reads the class name and the window name for the window's whole life, so both are leaked for the process lifetime.
	let class: &'static [u16] = to_wide_nul_terminated(WINDOW_CLASS).leak();
	let name: &'static [u16] = to_wide_nul_terminated(PRODUCT_NAME).leak();
	let window_class = WNDCLASSW {
		lpfnWndProc: Some(wndproc),
		hInstance: instance,
		lpszClassName: class.as_ptr(),
		..Default::default()
	};
	// SAFETY: `window_class` is fully initialised, its class-name pointer is a process-lifetime leak, and `run` registers the class once per process.
	let atom = unsafe { RegisterClassW(&window_class) };
	if atom == 0 {
		return Err(last_error("register its window class"));
	}
	// The state reaches the window procedure only through GWLP_USERDATA: this procedure has no WM_CREATE arm, so a null `lpParam` is never read.
	// SAFETY: the class is registered, both name pointers are process-lifetime leaks, a zero style creates the window hidden with no parent, and nothing dereferences `lpParam`.
	let hwnd = unsafe { CreateWindowExW(0, class.as_ptr(), name.as_ptr(), 0, 0, 0, 0, 0, null_mut(), null_mut(), instance, null()) };
	if hwnd.is_null() {
		return Err(last_error("create its hidden window"));
	}
	// No tray callback can arrive before this store: the NIM_ADD that starts them comes later, and until then the window procedure's empty-slot branch forwards to DefWindowProcW.
	// SetWindowLongPtrW answers with the slot's previous contents rather than a status, so a refused store is caught by reading the slot back.
	let slot = state as *const WindowState as isize;
	// SAFETY: the window was just created on this thread, and `state` lives for the rest of the process.
	let _ = unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, slot) };
	// SAFETY: the window was just created on this thread and stays live until it is destroyed here or in `run`.
	let stored = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
	// A window whose slot stayed empty would send every tray callback to DefWindowProcW and never open the menu, which is the only way to stop the folder.
	if stored != slot {
		let failure = last_error("store its window state");
		destroy_window(hwnd);
		return Err(failure);
	}
	Ok(hwnd)
}

// The identity of the single tray icon: NIM_ADD and NIM_DELETE have to agree on cbSize, window, and id.
fn icon_data(hwnd: HWND) -> NOTIFYICONDATAW {
	NOTIFYICONDATAW {
		cbSize: size_of::<NOTIFYICONDATAW>() as u32,
		hWnd: hwnd,
		uID: TRAY_ICON_ID,
		..Default::default()
	}
}

// The tray asks for the shell's small-icon size, so the 16, 20, and 24 px images in the executable's resource are used instead of a downscaled 32 px one.
// An executable whose build had no resource compiler carries no icon resource at all, and this answers null for it.
fn load_app_icon() -> Option<HICON> {
	// SAFETY: both metrics are system constants and neither call takes a pointer.
	let (width, height) = unsafe { (GetSystemMetrics(SM_CXSMICON), GetSystemMetrics(SM_CYSMICON)) };
	// SAFETY: a null module name asks for this executable's own module, whose icon resource the build script links in.
	let instance = unsafe { GetModuleHandleW(null()) };
	// `windows-sys` has no MAKEINTRESOURCE macro, so the integer id is passed as the name pointer itself.
	// SAFETY: the id is a pointer literal that is never dereferenced, the module is this executable's own, and a null load name makes the two size arguments the only source of the size.
	let icon = unsafe { LoadImageW(instance, APP_ICON_ID as *const u16, IMAGE_ICON, width, height, 0) };
	(!icon.is_null()).then_some(icon)
}

fn add_tray_icon(hwnd: HWND, tip: &str) -> Result<TrayIcon, Failure> {
	let icon = match load_app_icon() {
		Some(handle) => TrayIcon::Loaded(handle),
		// An entry with no icon would report success while leaving no visible control surface, so the stock icon keeps one on a build that carried no icon resource.
		None => {
			// SAFETY: a null instance plus a system icon identifier asks for the stock icon and touches no caller memory.
			let stock = unsafe { LoadIconW(null_mut(), IDI_APPLICATION) };
			if stock.is_null() {
				return Err(last_error("load its tray icon"));
			}
			TrayIcon::Stock(stock)
		}
	};
	let mut data = icon_data(hwnd);
	data.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
	data.uCallbackMessage = WM_TRAY_CALLBACK;
	data.hIcon = icon.handle();
	let tip = to_wide_nul_terminated(tip);
	let mut length = tip.len().min(TIP_CAP);
	// A high surrogate as the last copied unit is half of a character, so the copy stops before it.
	if tip[..length].last().is_some_and(|&unit| (0xD800..0xDC00).contains(&unit)) {
		length -= 1;
	}
	data.szTip[..length].copy_from_slice(&tip[..length]);
	data.szTip[length] = 0;
	// NIM_SETVERSION is deliberately not called: classic behaviour delivers the raw mouse message in lParam, which is the well-trodden path.
	// SAFETY: `data` is fully initialised with a cbSize matching its own type, and it outlives the call.
	if unsafe { Shell_NotifyIconW(NIM_ADD, &data) } != 0 {
		return Ok(icon);
	}
	let failure = last_error("add its tray icon");
	// NIM_ADD failed, so the shell never took the handle and no NIM_DELETE will follow, which is why `destroy` runs here rather than after a delete.
	icon.destroy();
	Err(failure)
}

fn remove_tray_icon(hwnd: HWND) {
	let data = icon_data(hwnd);
	// A refused NIM_DELETE leaves a ghost icon that the shell may keep showing, so the result is not actionable at exit.
	// SAFETY: NIM_DELETE reads only cbSize, hWnd, and uID, which `icon_data` sets.
	let _ = unsafe { Shell_NotifyIconW(NIM_DELETE, &data) };
}

fn destroy_window(hwnd: HWND) {
	// SAFETY: the window was created on this thread and is destroyed once, with no message for it left pending after WM_QUIT.
	let _ = unsafe { DestroyWindow(hwnd) };
}

fn message_loop() -> i32 {
	let mut msg = MSG::default();
	loop {
		// GetMessageW returns -1 for an error, 0 for WM_QUIT, and any other nonzero value for a message, so an error must not be read as the loop's own exit code.
		// SAFETY: `msg` is a live out-parameter, and a null window filter receives this thread's messages, which includes WM_QUIT.
		let result = unsafe { GetMessageW(&mut msg, null_mut(), 0, 0) };
		// The tray owns its exit status: PostQuitMessage(0) is the only source of WM_QUIT, so success is reported here rather than read out of wParam.
		if result == 0 {
			return 0;
		}
		if result == -1 {
			let failure = last_error("read its window messages");
			warn(&format!("{PRODUCT_NAME} could not {failure}. {EXIT_ON_DISMISS}"));
			return 1;
		}
		// SAFETY: `msg` was filled by GetMessageW and is only passed by reference, so nothing is owned across the boundary.
		unsafe { TranslateMessage(&msg) };
		// SAFETY: `msg` was filled by GetMessageW and is only passed by reference, so nothing is owned across the boundary.
		unsafe { DispatchMessageW(&msg) };
	}
}

// SAFETY: the system calls this with the window handle and message parameters it packed itself, and the procedure dereferences no parameter it did not store itself.
unsafe extern "system" fn wndproc(hwnd: HWND, msg: u32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
	match msg {
		WM_TRAY_CALLBACK => {
			// Classic tray mode carries the raw mouse message in lParam and the icon id in wParam; the cursor coordinates are packed into wParam only under NOTIFYICON_VERSION_4, which is why the menu anchors on GetCursorPos.
			let mouse = lparam as u32;
			// SAFETY: the slot holds the process-lifetime pointer stored when the window was created, or nothing.
			let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) };
			if state == 0 {
				// SAFETY: DefWindowProcW is the default handler for this window and no parameter is dereferenced here.
				unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
			} else {
				if mouse == WM_LBUTTONUP || mouse == WM_RBUTTONUP {
					// SAFETY: `state` is the pointer stored at creation, and it is never freed.
					let state = unsafe { &*(state as *const WindowState) };
					show_menu(hwnd, state);
				}
				0
			}
		}
		_ => {
			// SAFETY: DefWindowProcW is the default handler for this window and no parameter is dereferenced here.
			unsafe { DefWindowProcW(hwnd, msg, wparam, lparam) }
		}
	}
}

fn append(menu: HMENU, flags: MENU_ITEM_FLAGS, id: usize, text: &[u16]) -> bool {
	// SAFETY: `menu` is a live popup menu this thread owns and `text` is a NUL-terminated buffer that outlives it.
	unsafe { AppendMenuW(menu, flags, id, text.as_ptr()) != 0 }
}

fn show_menu(hwnd: HWND, state: &WindowState) {
	let mut point = POINT::default();
	// SAFETY: `point` is a live out-parameter and the call only reads the cursor position.
	if unsafe { GetCursorPos(&mut point) } == 0 {
		return;
	}
	// SAFETY: CreatePopupMenu returns a menu owned by this thread, or null, which is checked before use.
	let menu = unsafe { CreatePopupMenu() };
	if menu.is_null() {
		return;
	}
	// The two fixed labels are built here so their buffers outlive DestroyMenu, which every path below reaches.
	let copy = to_wide_nul_terminated(COPY_LABEL);
	let stop = to_wide_nul_terminated(STOP_LABEL);
	let built = append(menu, MF_STRING | MF_DISABLED | MF_GRAYED, MENU_ID_FOLDER, state.folder)
		&& append(menu, MF_STRING, MENU_ID_OPEN, state.url)
		&& append(menu, MF_STRING, MENU_ID_COPY, &copy)
		&& append(menu, MF_STRING, MENU_ID_STOP, &stop);
	// A menu missing a row would state less than what is served, so the popup is dropped instead of shown.
	if !built {
		// SAFETY: `menu` is owned by this thread and has not been displayed.
		unsafe { DestroyMenu(menu) };
		return;
	}
	// Without the foreground dance the popup survives the first click outside it.
	// SAFETY: `hwnd` is this thread's message window and the call only moves the foreground window.
	let _ = unsafe { SetForegroundWindow(hwnd) };
	// SAFETY: the menu, its anchor point, and the window are valid, and TPM_RETURNCMD makes the call return the chosen id instead of posting WM_COMMAND.
	let selected = unsafe { TrackPopupMenu(menu, TPM_LEFTALIGN | TPM_RETURNCMD | TPM_RIGHTBUTTON, point.x, point.y, 0, hwnd, null()) };
	// The mandatory follow-up to TrackPopupMenu: the queued WM_NULL lets the shell drop the menu's modal state.
	// SAFETY: `hwnd` is live and PostMessageW only queues a message.
	let _ = unsafe { PostMessageW(hwnd, WM_NULL, 0, 0) };
	// SAFETY: the menu is no longer displayed and this thread owns it.
	unsafe { DestroyMenu(menu) };
	// A dismissed menu answers 0, which names no row, so it leaves the folder serving.
	match selected as usize {
		MENU_ID_OPEN => open_in_browser(state.url),
		MENU_ID_COPY => copy_url(hwnd, state.url),
		// Stopping this folder is ending its process: `run` deletes the icon once the loop returns, and the OS closes the listening sockets.
		MENU_ID_STOP => {
			// SAFETY: PostQuitMessage only posts to this thread's queue and frees nothing.
			unsafe { PostQuitMessage(0) }
		}
		_ => {}
	}
}

fn open_in_browser(url: &[u16]) {
	// SAFETY: `url` is a NUL-terminated process-lifetime buffer, a null operation name asks for the URL's default verb, and null parameters add no arguments and keep the current directory.
	let launched = unsafe { ShellExecuteW(null_mut(), null(), url.as_ptr(), null(), null(), SW_SHOWNORMAL) };
	// ShellExecuteW answers a value above 32 on success and an error code at or below it, so 32 or less says only that the shell could not start the address.
	if launched as isize <= 32 {
		warn(&format!("{PRODUCT_NAME} could not open its address in the default browser. The folder is still served, so choose \"{COPY_LABEL}\" and paste the address into a browser."));
	}
}

fn copy_url(hwnd: HWND, url: &[u16]) {
	// SAFETY: `hwnd` is this thread's message window and the call only reads the handle as the clipboard's owner.
	if unsafe { OpenClipboard(hwnd) } == 0 {
		let failure = last_error("open the clipboard");
		warn(&format!("{PRODUCT_NAME} could not {failure}. The folder is still served."));
		return;
	}
	let handed_over = hand_over(url);
	// SAFETY: this thread opened the clipboard above and closes it exactly once here, whatever the handover did.
	let _ = unsafe { CloseClipboard() };
	if let Err(failure) = handed_over {
		warn(&format!("{PRODUCT_NAME} could not {failure}. The folder is still served."));
	}
}

// The clipboard takes ownership of a movable block handed to SetClipboardData, so the block is freed here only when that handover fails.
fn hand_over(url: &[u16]) -> Result<(), Failure> {
	// SAFETY: EmptyClipboard only touches the clipboard this thread opened and takes no arguments.
	if unsafe { EmptyClipboard() } == 0 {
		return Err(last_error("empty the clipboard"));
	}
	// GMEM_MOVEABLE is what the clipboard accepts ownership of, and the size is the whole buffer including its terminating NUL.
	// SAFETY: the size is a byte count and GlobalAlloc reads nothing else.
	let block = unsafe { GlobalAlloc(GMEM_MOVEABLE, size_of_val(url)) };
	if block.is_null() {
		return Err(last_error("allocate the clipboard memory"));
	}
	// SAFETY: `block` is a live movable block of `size_of_val(url)` bytes that nothing else holds a pointer into yet.
	let target = unsafe { GlobalLock(block) };
	if target.is_null() {
		let failure = last_error("lock the clipboard memory");
		// SAFETY: `block` is a live movable block this thread owns and the clipboard never received it.
		let _ = unsafe { GlobalFree(block) };
		return Err(failure);
	}
	let target = target.cast::<u16>();
	// SAFETY: `target` addresses `url.len()` writable units of the locked block, `url` holds exactly that many, and the two allocations do not overlap.
	unsafe { copy_nonoverlapping(url.as_ptr(), target, url.len()) };
	// GlobalUnlock answers zero both for a released lock and for a failure, so its result is not actionable here.
	// SAFETY: `block` is the block just locked above, which is unlocked exactly once here.
	unsafe { GlobalUnlock(block) };
	// SAFETY: `block` is still this thread's movable block, and a successful handover makes it the clipboard's to own and free.
	let handed = unsafe { SetClipboardData(CF_UNICODETEXT as u32, block) };
	if !handed.is_null() {
		return Ok(());
	}
	let failure = last_error("hand the address to the clipboard");
	// SAFETY: the handover failed, so `block` is still owned here and is freed exactly once.
	let _ = unsafe { GlobalFree(block) };
	Err(failure)
}
