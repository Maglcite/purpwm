#[cfg(test)]
mod tests {
    use smithay::utils::{Point, Rectangle, Size};

    use crate::{
        bar::{BarClickAction, StatusBar},
        config::{BAR_HEIGHT, GAP_INNER, GAP_OUTER},
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

        let r = Rectangle::<i32, smithay::utils::Logical>::new(
            Point::from((0, 0)),
            Size::from((2560, 1600)),
        );
        assert_eq!(r.size.w, 2560);
    }
}
