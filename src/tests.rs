#[cfg(test)]
mod tests {
    use std::sync::atomic::Ordering;

    use smithay::{
        input::keyboard::{keysyms, Keysym, ModifiersState},
        utils::{Logical, Point, Rectangle, Size},
    };

    use crate::{
        bar::{BarClickAction, StatusBar},
        config::{BAR_HEIGHT, GAP_INNER, GAP_OUTER},
        input::keysym_to_lock_key,
        lock::{pam_check, LockKey, LockPoll, LockScreen, CONVERSATIONS},
        notify::{parse_dbus_notification_line, NotificationCenter},
        state::touch_physical_to_logical_coords,
        wm::{LayoutMode, Workspace},
    };

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
    }
}
