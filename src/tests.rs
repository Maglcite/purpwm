#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use smithay::{
        backend::input::Keycode,
        input::keyboard::{keysyms, Keysym, ModifiersState},
        reexports::wayland_server::Display,
        utils::{Logical, Physical, Point, Rectangle, Size},
    };

    use crate::{
        bar::{wifi_label_from_nmcli, BarClickAction, StatusBar, BUNDLED_FONT},
        config::{
            window_rules_settled, WindowRule, BAR_HEIGHT, GAP_INNER, GAP_OUTER, RESIZE_RAMP_SECS,
            RESIZE_SPEED_MAX, RESIZE_SPEED_MIN, RESIZE_STEP,
        },
        glide::{glide_speed, KeyGlide},
        help::{rasterize, HelpOverlay},
        input::{keysym_to_lock_key, KeyAction},
        lock::{pam_check, LockKey, LockPoll, LockScreen, CONVERSATIONS},
        notify::{parse_dbus_line, parse_dbus_notification_line, DbusLine, NotificationCenter},
        state::{
            focus_after_zoom, resize_rect, touch_physical_to_logical_coords, wifi_needs_autoconnect,
            PurpState, ResizeEdges,
        },
        wm::{
            centered_side_widths, master_stack_widths, scratchpad_geo, split_lengths,
            split_weighted, LayoutMode, Workspace,
        },
    };

    use std::time::{Duration, Instant};

    #[test]
    fn test_layout_mode_cycle() {
        let mode = LayoutMode::Tile;
        assert_eq!(mode.symbol(), "[]=");
        let mode = mode.next();
        assert_eq!(mode, LayoutMode::CenteredMaster);
        assert_eq!(mode.symbol(), "|M|");
        let mode = mode.next();
        assert_eq!(mode, LayoutMode::Grid);
        assert_eq!(mode.symbol(), "###");
        let mode = mode.next();
        assert_eq!(mode, LayoutMode::Monocle);
        assert_eq!(mode.symbol(), "[M]");
        let mode = mode.next();
        assert_eq!(mode, LayoutMode::Floating);
        assert_eq!(mode.symbol(), "><>");
        assert_eq!(mode.next(), LayoutMode::Tile);
    }

    #[test]
    fn test_status_bar_hit_test() {
        let bar = StatusBar::new(2560);
        // Tag 1 is at x=0..34
        assert_eq!(bar.hit_test(5), BarClickAction::Tag(0));
        // Tag 9 is at x=34*8..34*9
        assert_eq!(bar.hit_test(34 * 8 + 4), BarClickAction::Tag(8));
        // Layout symbol sits right after the 9 tag cells
        assert_eq!(bar.hit_test(34 * 9 + 10), BarClickAction::CycleLayout);
        // Clicking in the middle title area cycles focus
        assert_eq!(bar.hit_test(800), BarClickAction::FocusNext);

        // Changing scale must not collapse the layout-symbol hit target to zero width
        // before the next raster (startup calls set_scale before the first frame).
        let mut scaled = StatusBar::new(1600);
        scaled.set_scale(1.5);
        assert_eq!(scaled.hit_test(34 * 9 + 4), BarClickAction::CycleLayout);
    }

    #[test]
    fn test_workspace_pertag_and_gaps() {
        let mut ws = Workspace::default();
        assert_eq!(ws.selected_tags, 1 << 0);
        assert_eq!(ws.layout(), LayoutMode::Tile);
        assert!(ws.show_bar);
        assert!(BAR_HEIGHT > 0 && GAP_INNER >= 0 && GAP_OUTER >= 0);

        // Change layout on Tag 1 (`cur_tag = 0`) to Grid
        ws.set_layout(LayoutMode::Grid);
        assert_eq!(ws.layout(), LayoutMode::Grid);

        // Switch to Tag 2 (`cur_tag = 1`) and verify it still has its own independent Tile layout
        ws.selected_tags = 1 << 1;
        ws.sync_cur_tag();
        assert_eq!(ws.cur_tag, 1);
        assert_eq!(ws.layout(), LayoutMode::Tile);

        // Switch back to Tag 1 and verify Grid was preserved
        ws.selected_tags = 1 << 0;
        ws.sync_cur_tag();
        assert_eq!(ws.layout(), LayoutMode::Grid);

        // Test runtime vanity gaps adjustment
        ws.adjust_gaps(4);
        assert_eq!(ws.gap_inner, GAP_INNER + 4);
        ws.reset_gaps();
        assert_eq!(ws.gap_inner, GAP_INNER);

        let r = Rectangle::<i32, Logical>::new(Point::from((0, 0)), Size::from((2560, 1600)));
        assert_eq!(r.size.w, 2560);
    }

    #[test]
    fn test_touch_physical_to_logical_coords() {
        let out = Size::<i32, Logical>::from((1600, 1000));
        // At 1.5x scale, physical (900, 600) maps to logical (600, 400)
        let p = touch_physical_to_logical_coords(900.0, 600.0, 1.5, out);
        assert!((p.x - 600.0).abs() < 1e-6);
        assert!((p.y - 400.0).abs() < 1e-6);

        // Coordinates outside the window are clamped to valid logical bounds
        let clamped = touch_physical_to_logical_coords(-25.0, 5000.0, 1.5, out);
        assert_eq!(clamped.x, 0.0);
        assert_eq!(clamped.y, 999.0);
    }

    #[test]
    fn test_lock_screen_keys_and_pam_confdir() {
        let mods = ModifiersState::default();
        assert_eq!(
            keysym_to_lock_key(Keysym::new(keysyms::KEY_a), &mods),
            Some(LockKey::Char('a'))
        );
        assert_eq!(
            keysym_to_lock_key(Keysym::new(keysyms::KEY_BackSpace), &mods),
            Some(LockKey::Backspace)
        );
        assert_eq!(
            keysym_to_lock_key(Keysym::new(keysyms::KEY_Escape), &mods),
            Some(LockKey::Clear)
        );
        assert_eq!(
            keysym_to_lock_key(Keysym::new(keysyms::KEY_Return), &mods),
            Some(LockKey::Submit)
        );

        let mut lock = LockScreen::new();
        lock.handle_key(LockKey::Char('p'));
        lock.handle_key(LockKey::Char('u'));
        lock.handle_key(LockKey::Char('r'));
        assert_eq!(lock.password_len(), 3);
        lock.handle_key(LockKey::Backspace);
        assert_eq!(lock.password_len(), 2);
        lock.handle_key(LockKey::Clear);
        assert_eq!(lock.password_len(), 0);
        assert_eq!(lock.poll_result(), LockPoll::Pending);

        // Private PAM confdir test using pam_echo.so + pam_permit.so / pam_deny.so
        // (never touches real user credentials or pam_faillock)
        let dir = std::env::temp_dir().join(format!("purpwm-pam-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&dir);
        std::fs::write(
            dir.join("test-allow"),
            "auth optional pam_echo.so prompt\nauth required pam_permit.so\n",
        )
        .unwrap();
        std::fs::write(dir.join("test-deny"), "auth required pam_deny.so\n").unwrap();

        let before = CONVERSATIONS.load(Ordering::Relaxed);
        let dir_str = dir.to_str().unwrap();
        assert!(pam_check(
            "test-allow",
            "nobody",
            "secret".to_string(),
            Some(dir_str)
        ));
        assert!(CONVERSATIONS.load(Ordering::Relaxed) > before);
        assert!(!pam_check(
            "test-deny",
            "nobody",
            "secret".to_string(),
            Some(dir_str)
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn test_notification_center_and_dismiss() {
        let mut nc = NotificationCenter::new();
        assert!(nc.is_empty());

        let id1 = nc.push(0, "purpwm", "Hello", "First toast", 1, Some(10_000));
        let _id2 = nc.push(0, "purpwm", "Second", "Second toast", 2, Some(10_000));
        assert_eq!(nc.len(), 2);

        // Replacing an existing notification ID updates in place without growing the stack
        nc.push(id1, "purpwm", "Updated", "Replaced body", 1, Some(10_000));
        assert_eq!(nc.len(), 2);

        let geo = nc.stack_geometry(1600, true).expect("toast stack geometry");
        // Clicking inside the top card dismisses it
        let click_pt =
            Point::<f64, Logical>::from(((geo.loc.x + 20) as f64, (geo.loc.y + 20) as f64));
        assert!(nc.dismiss_at(click_pt, 1600, true));
        assert_eq!(nc.len(), 1);

        let parsed = parse_dbus_notification_line("42\t2\t5000\tnotify-send\tAlert\tBody text")
            .expect("valid dbus notification line");
        assert_eq!(parsed.id, 42);
        assert_eq!(parsed.urgency, 2);
        assert_eq!(parsed.app_name, "notify-send");
        assert_eq!(parsed.summary, "Alert");
        assert_eq!(parsed.body, "Body text");

        // Freedesktop: expire_timeout 0 means the toast never expires; a positive
        // timeout is honoured instead of being clamped to 30s.
        let persistent = parse_dbus_notification_line("9\t1\t0\tapp\tStay\tbody").unwrap();
        assert!(
            persistent.expires_at.is_none(),
            "timeout 0 expired after {:?}",
            persistent.expires_at
        );
        let minute = parse_dbus_notification_line("8\t1\t60000\tapp\tHold\tbody").unwrap();
        let left = minute
            .expires_at
            .expect("positive timeout")
            .saturating_duration_since(Instant::now());
        assert!(
            left > Duration::from_secs(50) && left < Duration::from_secs(70),
            "60s timeout became {left:?}"
        );

        // A click on the inner edge of the remaining card must count (rounding
        // x.8 up to the exclusive bound used to miss it).
        let geo = nc.stack_geometry(1600, true).expect("remaining toast");
        let edge = Point::<f64, Logical>::from((
            geo.loc.x as f64 + geo.size.w as f64 - 0.2,
            geo.loc.y as f64 + geo.size.h as f64 - 0.2,
        ));
        assert!(nc.dismiss_at(edge, 1600, true));
        assert!(nc.is_empty());
    }

    #[test]
    fn test_spotlight_socket_toggle_protocol() {
        use std::io::{BufRead, BufReader, Write};
        use std::os::unix::net::{UnixListener, UnixStream};

        let temp_dir = std::env::temp_dir().join(format!("purpwm-sock-test-{}", std::process::id()));
        let _ = std::fs::create_dir_all(&temp_dir);
        let sock_path = temp_dir.join("spotlight.sock");

        let listener = UnixListener::bind(&sock_path).expect("bind test socket");

        // Simulate purpwm fast-path send
        let mut client = UnixStream::connect(&sock_path).expect("connect test socket");
        assert!(client.write_all(b"\"Toggle\"\n").is_ok());
        assert!(client.flush().is_ok());
        drop(client);

        let (server, _) = listener.accept().expect("accept connection");
        let mut reader = BufReader::new(server);
        let mut line = String::new();
        reader.read_line(&mut line).expect("read line");
        assert_eq!(line.trim(), "\"Toggle\"");

        let _ = std::fs::remove_file(&sock_path);
        let _ = std::fs::remove_dir(&temp_dir);
    }

    #[test]
    fn test_help_overlay_toggle_and_rasterize() {
        let mut help = HelpOverlay::default();
        assert!(!help.visible);
        help.toggle();
        assert!(help.visible);
        help.toggle();
        assert!(!help.visible);

        let bar = StatusBar::new(2560);
        let out = Size::<i32, Physical>::from((2560, 1600));
        let (_buf, loc) = rasterize(bar.font(), out, 1.5);
        assert!(loc.x > 0 && loc.x < out.w / 2);
        assert!(loc.y > 0 && loc.y < out.h / 2);
    }

    #[test]
    fn test_pointer_focus_sync_and_fractional_hit_testing() {
        let display: Display<PurpState> = Display::new().expect("create wayland display");
        let mut state = PurpState::new(
            display.handle(),
            String::from("wayland-test-0"),
            Size::from((1600, 1000)),
            true,
        );

        // Subpixel coordinates near the bottom-right edge (e.g. 99.8 inside a 0..100 rect)
        // must remain inside `to_f64().contains(...)` rather than rounding outside to 100.
        let rect = Rectangle::<i32, Logical>::new(Point::from((0, 0)), Size::from((100, 100)));
        let edge_pt = Point::<f64, Logical>::from((99.8, 99.8));
        assert!(rect.to_f64().contains(edge_pt));

        // Toggling help and syncing pointer focus must cleanly clear/update pointer focus.
        state.execute_key_action(KeyAction::ToggleHelp);
        assert!(state.help.visible);
        state.process_pointer_button(0x110, smithay::backend::input::ButtonState::Pressed, 10);
        assert!(!state.help.visible);
    }

    #[test]
    fn test_cascade_keeps_oversized_windows_on_screen() {
        let display: Display<PurpState> = Display::new().expect("create wayland display");
        let state = PurpState::new(
            display.handle(),
            String::from("wayland-test-1"),
            Size::from((200, 180)),
            true,
        );
        let size = Size::from((480, 320));
        let loc = state.cascade_loc(size, None);
        assert!(loc.x >= 0, "x {}", loc.x);
        assert!(loc.y >= 0, "y {}", loc.y);
        assert!(loc.x + size.w > 200 || loc.x == 0);
        assert_eq!(loc.x, 0);
        assert_eq!(loc.y, BAR_HEIGHT);
    }

    #[test]
    fn test_layout_splits_fit_the_usable_rect() {
        let columns = split_lengths(100, 3, 6);
        assert_eq!(columns.span(), 100);
        let tight = split_lengths(20, 5, 8);
        assert_eq!(tight.span(), 20);
        assert!(tight.lengths.iter().all(|len| *len >= 0));

        let (master, stack, gap) = master_stack_widths(100, 8, 0.9);
        assert_eq!(master + gap + stack, 100);
        let (left, master, right, gap) = centered_side_widths(100, 8, 0.9);
        assert_eq!(left + gap + master + gap + right, 100);

        let pad = scratchpad_geo(Size::from((200, 180)), true);
        assert!(pad.loc.x >= 0 && pad.loc.y >= BAR_HEIGHT);
        assert!(pad.loc.x + pad.size.w <= 200);
        assert!(pad.loc.y + pad.size.h <= 180);
    }

    #[test]
    fn test_bundled_font_renders_text() {
        let font = fontdue::Font::from_bytes(BUNDLED_FONT, fontdue::FontSettings::default())
            .expect("bundled font parses");
        let (metrics, bitmap) = font.rasterize('A', 16.0);
        assert!(metrics.width > 0 && bitmap.iter().any(|&a| a > 0));
    }

    #[test]
    fn test_weighted_column_split_keeps_the_span() {
        let even = split_weighted(100, &[1.0, 1.0, 1.0], 6);
        assert_eq!(even, split_lengths(100, 3, 6));
        let uneven = split_weighted(100, &[3.0, 1.0], 4);
        assert_eq!(uneven.span(), 100);
        assert_eq!(uneven.lengths, vec![72, 24]);
    }

    #[test]
    fn test_resizing_tiled_windows_moves_the_splits() {
        let rect =
            |x, y, w, h| Rectangle::<i32, Logical>::new(Point::from((x, y)), Size::from((w, h)));
        let edges = |left, top, right, bottom| ResizeEdges {
            left,
            top,
            right,
            bottom,
        };
        // Usable area (10, 10) 980x950 with 10px gaps.
        let output = Size::from((1000, 970));
        let mut ws = Workspace {
            show_bar: false,
            gap_outer: 10,
            gap_inner: 10,
            ..Workspace::default()
        };
        // `[]=` with a master and two stacked windows on the right.
        let tiled = [
            (0, rect(10, 10, 534, 950), 1.0),
            (1, rect(554, 10, 436, 470), 1.0),
            (2, rect(554, 490, 436, 470), 1.0),
        ];
        let start = ws.mfact();

        // Dragging the master's right edge (or its screen edge) moves the master/stack divider.
        for e in [
            edges(false, false, true, false),
            edges(true, false, false, false),
        ] {
            let split = ws
                .plan_tiled_resize(output, &tiled, 0, e)
                .mfact
                .expect("master divider");
            assert!((split.start + split.per_px * 97.0 - (start + 0.1)).abs() < 1e-4);
        }
        // The stack's left edge is the same divider, and it does not touch the rows.
        let stack = ws.plan_tiled_resize(output, &tiled, 1, edges(true, false, false, false));
        assert!(stack.mfact.is_some());
        assert!(stack.rows.is_none());

        // Keyboard resize (bottom-right edges) moves the divider the way the arrow points for
        // every window: `Right` (+x) pushes it right, i.e. raises `mfact`, even from the stack
        // window whose own right edge is the screen edge.
        for idx in [0, 1, 2] {
            let split = ws
                .plan_tiled_resize(output, &tiled, idx, ResizeEdges::BOTTOM_RIGHT)
                .mfact
                .expect("keyboard divider");
            assert!(
                split.per_px > 0.0,
                "window {idx}: Right must move the divider right"
            );
        }

        // Bottom edge of the upper stack window: the row divider follows the pointer and
        // the windows keep their minimum height.
        let rows = ws
            .plan_tiled_resize(output, &tiled, 1, edges(false, false, false, true))
            .rows
            .expect("row divider");
        assert_eq!((rows.upper, rows.lower), (1, 2));
        let (upper, lower) = rows.cfacts(100);
        assert!((upper * rows.px_per_cfact - 570.0).abs() < 0.01);
        assert!((upper + lower - 2.0).abs() < 1e-4);
        let (_, lower) = rows.cfacts(10_000);
        assert!((lower * rows.px_per_cfact - 80.0).abs() < 0.01);
        // The last window in a column has no lower neighbour: use the divider above it.
        // `Down` (+y) still moves that divider down, shrinking the last window.
        let last = ws
            .plan_tiled_resize(output, &tiled, 2, edges(false, false, false, true))
            .rows
            .expect("row above");
        assert_eq!((last.upper, last.lower), (1, 2));
        let (upper, lower) = last.cfacts(40);
        assert!(upper * last.px_per_cfact > 470.0 && lower * last.px_per_cfact < 470.0);
        // A lone column window has no row divider.
        assert!(ws
            .plan_tiled_resize(output, &tiled, 0, edges(false, true, false, false))
            .rows
            .is_none());

        // `|M|` with stacks on both sides: the left divider grows the master when dragged left.
        ws.set_layout(LayoutMode::CenteredMaster);
        let centered = [
            (0, rect(10, 10, 200, 950), 1.0),
            (1, rect(220, 10, 560, 950), 1.0),
            (2, rect(790, 10, 200, 950), 1.0),
        ];
        let left = ws.plan_tiled_resize(output, &centered, 0, edges(false, false, true, false));
        assert!(left.mfact.expect("left divider").per_px < 0.0);
        let right = ws.plan_tiled_resize(output, &centered, 1, edges(false, false, true, false));
        assert!(right.mfact.expect("right divider").per_px > 0.0);

        ws.set_layout(LayoutMode::Monocle);
        let monocle = ws.plan_tiled_resize(output, &tiled, 0, edges(false, false, true, true));
        assert!(monocle.mfact.is_none() && monocle.rows.is_none());
    }

    /// Sum the motion of `glide` over 8 ms frames from `t` until it finishes or `frames` run
    /// out, checking no single frame jumps more than `max_frame_px`.
    fn run_glide(
        glide: &mut KeyGlide,
        t: &mut Instant,
        frames: usize,
        max_frame_px: i32,
    ) -> (i32, i32) {
        let mut total = (0, 0);
        for _ in 0..frames {
            *t += Duration::from_millis(8);
            let (dx, dy) = glide.advance(*t);
            assert!(
                dx.abs() <= max_frame_px && dy.abs() <= max_frame_px,
                "frame jumped ({dx}, {dy})"
            );
            total = (total.0 + dx, total.1 + dy);
            if glide.is_done() {
                break;
            }
        }
        total
    }

    #[test]
    fn test_keyboard_resize_glides_smoothly() {
        assert_eq!(glide_speed(0.0), RESIZE_SPEED_MIN);
        assert_eq!(glide_speed(RESIZE_RAMP_SECS), RESIZE_SPEED_MAX);
        assert_eq!(glide_speed(10.0), RESIZE_SPEED_MAX);
        assert!(glide_speed(0.2) < glide_speed(0.4));

        let step = RESIZE_STEP.round() as i32;
        let start = Instant::now();

        // A tap glides exactly one step in a few small frames, then stops.
        let mut t = start;
        let mut tap = KeyGlide::new(t);
        tap.press(None, (-1, 0), t);
        assert_eq!(run_glide(&mut tap, &mut t, 100, 3), (-step, 0));
        assert!(tap.is_done());

        // Releasing a key right after pressing it still finishes the step.
        let key = Keycode::new(116);
        let mut t = start;
        let mut quick = KeyGlide::new(t);
        quick.press(Some(key), (0, 1), t);
        t += Duration::from_millis(8);
        let first = quick.advance(t).1;
        quick.release(key);
        let rest = run_glide(&mut quick, &mut t, 100, 3).1;
        assert_eq!(first + rest, step);
        assert!(quick.is_done());

        // Holding keeps it moving and speeding up, never in big jumps; a repeated press of
        // the held key does not double the speed. Releasing stops it straight away.
        let mut t = start;
        let mut held = KeyGlide::new(t);
        held.press(Some(key), (1, 0), t);
        held.press(Some(key), (1, 0), t);
        let early = run_glide(&mut held, &mut t, 25, 3).0;
        let late = run_glide(&mut held, &mut t, 100, 13).0;
        assert!(!held.is_done());
        assert!(
            early > step && late > early * 3,
            "early {early}, late {late}"
        );
        held.release(key);
        t += Duration::from_millis(8);
        assert!(held.advance(t).0.abs() <= 1);
        assert!(held.is_done());

        // A stalled frame only credits a short slice of motion.
        let mut t = start;
        let mut stalled = KeyGlide::new(t);
        stalled.press(Some(key), (1, 0), t);
        t += Duration::from_secs(2);
        assert!(stalled.advance(t).0 <= (RESIZE_SPEED_MAX * 0.05).ceil() as i32);
    }

    #[test]
    fn test_window_rules_are_conjunctive() {
        let rule = WindowRule {
            app_id_match: Some("firefox"),
            title_match: Some("Open File"),
            tags: 0,
            is_floating: true,
            opacity: None,
        };
        assert!(rule.matches("firefox", "Open File"));
        assert!(!rule.matches("firefox", "Browser"));
        assert!(!rule.matches("chrome", "Open File"));
        assert!(!window_rules_settled(false, "firefox", ""));
        assert!(window_rules_settled(false, "firefox", "Browser"));
        assert!(window_rules_settled(true, "firefox", ""));
    }

    #[test]
    fn test_wifi_status_prefers_a_live_device() {
        assert!(!wifi_needs_autoconnect(
            "wifi:connected\nwifi:disconnected"
        ));
        assert!(wifi_needs_autoconnect("wifi:disconnected"));
        assert!(!wifi_needs_autoconnect("ethernet:disconnected"));
        assert!(!wifi_needs_autoconnect("wifi:connecting"));

        let (label, up) = wifi_label_from_nmcli(
            "wifi:disconnected:\nwifi:connected:Home Network\n",
        );
        assert!(up);
        assert_eq!(label, "WIFI: Home Network");
    }

    #[test]
    fn test_preferred_mode_prefers_the_internal_panel() {
        use crate::pick_preferred_mode;
        assert_eq!(
            pick_preferred_mode(&[("card0-HDMI-A-1", "1920x1080"), ("card0-eDP-1", "2560x1600")]),
            Some((2560, 1600))
        );
        assert_eq!(
            pick_preferred_mode(&[("card0-HDMI-A-1", "1920x1080")]),
            Some((1920, 1080))
        );
    }

    #[test]
    fn test_resize_and_zoom_keep_the_anchored_edge() {
        let start = Rectangle::new(Point::from((100, 100)), Size::from((200, 200)));
        let resized = resize_rect(
            start,
            40,
            0,
            ResizeEdges {
                left: true,
                top: false,
                right: false,
                bottom: false,
            },
            80,
            60,
        );
        assert_eq!(resized.loc.x, 140);
        assert_eq!(resized.size.w, 160);
        assert_eq!(resized.loc.x + resized.size.w, 300);
        assert_eq!(focus_after_zoom(0, 0, 2), 0);
        assert_eq!(focus_after_zoom(0, 2, 0), 0);
    }

    #[test]
    fn test_close_notification_removes_the_toast() {
        let mut nc = NotificationCenter::new();
        let id = nc.push(0, "app", "Hi", "Body", 1, Some(10_000));
        assert!(matches!(
            parse_dbus_line(&format!("close\t{id}")),
            Some(DbusLine::Close(closed)) if closed == id
        ));
        nc.ingest_line(DbusLine::Close(id));
        assert!(nc.tick());
        assert!(nc.is_empty());
    }
}
