//! Built-in screen locker: a full-screen purple lock screen drawn by the compositor itself
//! (no `swaylock` / `ext-session-lock` client needed). While locked no window is drawn or
//! receives input; the typed password is checked with PAM on a worker thread.

use std::{
    ffi::{c_char, c_int, c_void, CStr, CString},
    path::Path,
    sync::mpsc,
};

use fontdue::Font;
use smithay::{
    backend::{
        allocator::Fourcc,
        renderer::{
            element::{
                memory::{MemoryRenderBuffer, MemoryRenderBufferRenderElement},
                Kind,
            },
            ImportMem, Renderer,
        },
    },
    utils::{Buffer, Physical, Point, Rectangle, Size, Transform},
};

use crate::{
    bar::{blend_pixel_argb8888, fill_rect, StatusBar},
    config::{BAR_ACCENT_RGBA, BAR_LAYOUT_BG_RGBA, BAR_TEXT_DIM_RGBA, BAR_TEXT_FG_RGBA},
};

/// Lock screen backdrop: same deep obsidian purple as the desktop, fully opaque.
const LOCK_BG_RGBA: [u8; 4] = [0x11, 0x08, 0x1F, 255];
/// Error text colour (wrong password).
const LOCK_ERROR_RGBA: [u8; 4] = [0xF4, 0x3F, 0x5E, 255];

/// Key input the lock screen understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockKey {
    Char(char),
    Backspace,
    Clear,
    Submit,
}

/// Outcome of polling the background PAM check.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockPoll {
    /// No new PAM result yet.
    Pending,
    /// Password check finished and was rejected.
    Failed,
    /// Password check finished and was accepted (caller should unlock).
    Unlocked,
}

/// Inputs of a rasterised lock screen frame.
#[derive(Clone, PartialEq, Eq)]
struct LockDrawKey {
    size: Size<i32, Physical>,
    dots: usize,
    message: Option<String>,
    checking: bool,
    clock: String,
}

/// State of an active lock.
pub struct LockScreen {
    password: String,
    message: Option<String>,
    checking: bool,
    result_tx: mpsc::Sender<bool>,
    result_rx: mpsc::Receiver<bool>,
    buffer: MemoryRenderBuffer,
    drawn: Option<LockDrawKey>,
}

impl Default for LockScreen {
    fn default() -> Self {
        Self::new()
    }
}

impl LockScreen {
    pub fn new() -> Self {
        let (result_tx, result_rx) = mpsc::channel();
        Self {
            password: String::new(),
            message: None,
            checking: false,
            result_tx,
            result_rx,
            buffer: MemoryRenderBuffer::new(Fourcc::Argb8888, (1, 1), 1, Transform::Normal, None),
            drawn: None,
        }
    }

    /// Apply a key press. Submitting starts a PAM check on a worker thread.
    pub fn handle_key(&mut self, key: LockKey) {
        if self.checking {
            return;
        }
        match key {
            LockKey::Char(c) => {
                self.message = None;
                if self.password.len() < 256 {
                    self.password.push(c);
                }
            }
            LockKey::Backspace => {
                self.password.pop();
            }
            LockKey::Clear => self.password.clear(),
            LockKey::Submit => {
                if self.password.is_empty() {
                    return;
                }
                self.checking = true;
                self.message = None;
                let password = std::mem::take(&mut self.password);
                let tx = self.result_tx.clone();
                std::thread::spawn(move || {
                    let ok = authenticate_current_user(password);
                    let _ = tx.send(ok);
                });
            }
        }
    }

    /// Poll the worker thread for a finished PAM result.
    pub fn poll_result(&mut self) -> LockPoll {
        match self.result_rx.try_recv() {
            Ok(true) => LockPoll::Unlocked,
            Ok(false) => {
                self.checking = false;
                self.message = Some("Wrong password — try again".to_string());
                LockPoll::Failed
            }
            Err(_) => LockPoll::Pending,
        }
    }

    pub fn is_checking(&self) -> bool {
        self.checking
    }

    pub fn password_len(&self) -> usize {
        self.password.chars().count()
    }

    pub fn message(&self) -> Option<&str> {
        self.message.as_deref()
    }

    /// Full-screen lock element, re-rasterised only when something on it changed.
    pub fn render_element<R>(
        &mut self,
        renderer: &mut R,
        font: Option<&Font>,
        size: Size<i32, Physical>,
        scale: f64,
    ) -> Option<MemoryRenderBufferRenderElement<R>>
    where
        R: Renderer + ImportMem,
        R::TextureId: Send + Clone + 'static,
    {
        let clock = StatusBar::format_local_time();
        let key = LockDrawKey {
            size,
            dots: self.password.chars().count(),
            message: self.message.clone(),
            checking: self.checking,
            clock: clock.clone(),
        };
        if self.drawn.as_ref() != Some(&key) {
            self.redraw(font, size, scale, &clock);
            self.drawn = Some(key);
        }
        MemoryRenderBufferRenderElement::from_buffer(
            renderer,
            Point::<f64, Physical>::from((0.0, 0.0)),
            &self.buffer,
            Some(1.0),
            None,
            None,
            Kind::Unspecified,
        )
        .ok()
    }

    fn redraw(&mut self, font: Option<&Font>, size: Size<i32, Physical>, scale: f64, clock: &str) {
        let (w, h) = (size.w.max(1), size.h.max(1));
        let px = |v: f64| (v * scale).round() as i32;
        let (date, time) = clock.rsplit_once(' ').unwrap_or(("", clock));
        let dots = "•".repeat(self.password.chars().count().min(32));
        let status = if self.checking {
            ("Checking…".to_string(), BAR_TEXT_DIM_RGBA)
        } else if let Some(msg) = &self.message {
            (msg.clone(), LOCK_ERROR_RGBA)
        } else {
            (
                "Type your password and press Enter".to_string(),
                BAR_TEXT_DIM_RGBA,
            )
        };
        let user = current_user_name().unwrap_or_default();

        let mut ctx = self.buffer.render();
        ctx.resize((w, h));
        let _ = ctx.draw(|buf| {
            fill_rect(buf, w, h, 0, 0, w, h, LOCK_BG_RGBA);
            let center = |text: &str, px_size: f32| w / 2 - measure(font, text, px_size) / 2;

            let time_px = 120.0 * scale as f32;
            let mut y = h / 2 - px(150.0);
            draw(
                buf,
                w,
                h,
                font,
                time,
                center(time, time_px),
                y,
                time_px,
                BAR_TEXT_FG_RGBA,
            );
            y += px(150.0);
            let small = 20.0 * scale as f32;
            draw(
                buf,
                w,
                h,
                font,
                date,
                center(date, small),
                y - px(10.0),
                small,
                BAR_ACCENT_RGBA,
            );

            // Password field
            let field_w = px(360.0);
            let field_h = px(48.0);
            let field_x = w / 2 - field_w / 2;
            let field_y = y + px(30.0);
            fill_rect(
                buf,
                w,
                h,
                field_x - px(2.0),
                field_y - px(2.0),
                field_w + px(4.0),
                field_h + px(4.0),
                BAR_ACCENT_RGBA,
            );
            fill_rect(
                buf,
                w,
                h,
                field_x,
                field_y,
                field_w,
                field_h,
                BAR_LAYOUT_BG_RGBA,
            );
            let dots_px = 24.0 * scale as f32;
            let dots_y = field_y + (field_h - dots_px as i32) / 2;
            draw(
                buf,
                w,
                h,
                font,
                &dots,
                center(&dots, dots_px),
                dots_y,
                dots_px,
                BAR_TEXT_FG_RGBA,
            );

            let status_y = field_y + field_h + px(24.0);
            draw(
                buf,
                w,
                h,
                font,
                &status.0,
                center(&status.0, small),
                status_y,
                small,
                status.1,
            );
            let user_line = format!("Locked · {}", user);
            draw(
                buf,
                w,
                h,
                font,
                &user_line,
                center(&user_line, small),
                status_y + px(40.0),
                small,
                BAR_TEXT_DIM_RGBA,
            );

            Result::<Vec<Rectangle<i32, Buffer>>, ()>::Ok(vec![Rectangle::from_size((w, h).into())])
        });
    }
}

pub(crate) fn measure(font: Option<&Font>, text: &str, px: f32) -> i32 {
    let Some(font) = font else {
        return (text.chars().count() as i32) * ((px * 0.6) as i32).max(8);
    };
    text.chars()
        .map(|c| font.metrics(c, px).advance_width.ceil() as i32)
        .sum()
}

/// Draw `text` with its top at `top` (glyphs rasterised on the fly; the lock screen redraws rarely).
#[allow(clippy::too_many_arguments)]
pub(crate) fn draw(
    buf: &mut [u8],
    w: i32,
    h: i32,
    font: Option<&Font>,
    text: &str,
    x: i32,
    top: i32,
    px: f32,
    rgba: [u8; 4],
) {
    let Some(font) = font else {
        return;
    };
    let baseline = top + (px * 0.8) as i32;
    let mut pen = x;
    for ch in text.chars() {
        let (m, bitmap) = font.rasterize(ch, px);
        let gx = pen + m.xmin;
        let gy = baseline - m.height as i32 - m.ymin;
        for row in 0..m.height {
            for col in 0..m.width {
                let cov = bitmap[row * m.width + col];
                if cov > 0 {
                    blend_pixel_argb8888(buf, w, h, gx + col as i32, gy + row as i32, rgba, cov);
                }
            }
        }
        pen += m.advance_width.ceil() as i32;
    }
}

// ============================================================================
// PAM
// ============================================================================

const PAM_SUCCESS: c_int = 0;
const PAM_PROMPT_ECHO_OFF: c_int = 1;
const PAM_PROMPT_ECHO_ON: c_int = 2;
const PAM_BUF_ERR: c_int = 5;
const PAM_CONV_ERR: c_int = 19;

#[repr(C)]
struct PamMessage {
    msg_style: c_int,
    msg: *const c_char,
}

#[repr(C)]
struct PamResponse {
    resp: *mut c_char,
    resp_retcode: c_int,
}

#[repr(C)]
struct PamConv {
    conv: extern "C" fn(c_int, *mut *const PamMessage, *mut *mut PamResponse, *mut c_void) -> c_int,
    appdata_ptr: *mut c_void,
}

#[link(name = "pam")]
extern "C" {
    fn pam_start(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        pamh: *mut *mut c_void,
    ) -> c_int;
    fn pam_start_confdir(
        service: *const c_char,
        user: *const c_char,
        conv: *const PamConv,
        confdir: *const c_char,
        pamh: *mut *mut c_void,
    ) -> c_int;
    fn pam_authenticate(pamh: *mut c_void, flags: c_int) -> c_int;
    fn pam_end(pamh: *mut c_void, status: c_int) -> c_int;
}

/// Answers every password prompt with the password passed through `appdata`.
extern "C" fn pam_conversation(
    num_msg: c_int,
    msg: *mut *const PamMessage,
    resp: *mut *mut PamResponse,
    appdata: *mut c_void,
) -> c_int {
    if num_msg <= 0 || msg.is_null() || resp.is_null() || appdata.is_null() {
        return PAM_CONV_ERR;
    }
    let password = unsafe { &*(appdata as *const CString) };
    // PAM frees the array and the strings, so they must come from malloc.
    let replies = unsafe { libc::calloc(num_msg as usize, std::mem::size_of::<PamResponse>()) }
        as *mut PamResponse;
    if replies.is_null() {
        return PAM_BUF_ERR;
    }
    for i in 0..num_msg as usize {
        let message = unsafe { &**msg.add(i) };
        if matches!(message.msg_style, PAM_PROMPT_ECHO_OFF | PAM_PROMPT_ECHO_ON) {
            unsafe { (*replies.add(i)).resp = libc::strdup(password.as_ptr()) };
        }
        CONVERSATIONS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    unsafe { *resp = replies };
    PAM_SUCCESS
}

/// Number of PAM prompts answered (lets tests check the conversation is wired up).
pub(crate) static CONVERSATIONS: std::sync::atomic::AtomicUsize =
    std::sync::atomic::AtomicUsize::new(0);

/// Run the PAM `auth` stack of `service` for `user`. `confdir` overrides `/etc/pam.d` (tests).
pub(crate) fn pam_check(
    service: &str,
    user: &str,
    password: String,
    confdir: Option<&str>,
) -> bool {
    let (Ok(service), Ok(user), Ok(password)) = (
        CString::new(service),
        CString::new(user),
        CString::new(password),
    ) else {
        return false;
    };
    let conv = PamConv {
        conv: pam_conversation,
        appdata_ptr: &password as *const CString as *mut c_void,
    };
    let mut handle: *mut c_void = std::ptr::null_mut();
    let started = unsafe {
        match confdir.map(CString::new) {
            Some(Ok(dir)) => pam_start_confdir(
                service.as_ptr(),
                user.as_ptr(),
                &conv,
                dir.as_ptr(),
                &mut handle,
            ),
            Some(Err(_)) => return false,
            None => pam_start(service.as_ptr(), user.as_ptr(), &conv, &mut handle),
        }
    };
    if started != PAM_SUCCESS || handle.is_null() {
        return false;
    }
    let result = unsafe { pam_authenticate(handle, 0) };
    unsafe { pam_end(handle, result) };
    // Wipe our copy of the password.
    let mut bytes = password.into_bytes_with_nul();
    for b in &mut bytes {
        unsafe { std::ptr::write_volatile(b, 0) };
    }
    result == PAM_SUCCESS
}

/// Check `password` for the user running purpwm. Uses `/etc/pam.d/purpwm` when installed
/// (see `install-session.sh`), otherwise `/etc/pam.d/vlock` or the `login` stack.
fn authenticate_current_user(password: String) -> bool {
    let Some(user) = current_user_name() else {
        return false;
    };
    let confdir_env = std::env::var("PURPWM_PAM_CONFDIR")
        .ok()
        .filter(|s| !s.is_empty());
    let service_env = std::env::var("PURPWM_PAM_SERVICE")
        .ok()
        .filter(|s| !s.is_empty());
    let service = if let Some(ref s) = service_env {
        s.as_str()
    } else if Path::new("/etc/pam.d/purpwm").exists() {
        "purpwm"
    } else if Path::new("/etc/pam.d/vlock").exists() {
        "vlock"
    } else {
        "login"
    };
    pam_check(service, &user, password, confdir_env.as_deref())
}

fn current_user_name() -> Option<String> {
    unsafe {
        let pw = libc::getpwuid(libc::getuid());
        if pw.is_null() || (*pw).pw_name.is_null() {
            return None;
        }
        CStr::from_ptr((*pw).pw_name)
            .to_str()
            .ok()
            .map(String::from)
    }
}
