//! Frame times with a big tree: `cargo test -p treetabs-ui --release perf -- --ignored --nocapture`
use super::App;
use crate::{Poll, Transport};
use eframe::egui;
use treetabs_core::protocol::{ClientMsg, ServerMsg};
use treetabs_core::{Node, NodeKind, Tree};

struct Fake(Option<Tree>);

impl Transport for Fake {
    fn send(&mut self, _: &ClientMsg) -> Result<(), String> {
        Ok(())
    }
    fn poll(&mut self) -> Poll {
        let msgs = self
            .0
            .take()
            .map(|t| {
                vec![ServerMsg::Snapshot {
                    seq: 1,
                    tree: Box::new(t),
                }]
            })
            .unwrap_or_default();
        Poll { msgs, lost: false }
    }
    fn reconnect(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn connected(&self) -> bool {
        true
    }
    fn describe(&self) -> String {
        "fake".into()
    }
}

/// `windows` windows of `tabs` tabs, every 5th tab with 3 children.
pub(super) fn big_tree(windows: usize, tabs: usize) -> Tree {
    let mut t = Tree::default();
    for w in 0..windows {
        let mut win = Node::new(NodeKind::SavedWindow);
        for i in 0..tabs {
            let mut tab = Node::new(NodeKind::SavedTab);
            tab.title = Some(format!("Page {w}-{i} about something"));
            tab.url = Some(format!("https://example{}.com/{w}/{i}", i % 50));
            if i % 5 == 0 {
                for c in 0..3 {
                    let mut child = Node::new(NodeKind::SavedTab);
                    child.title = Some(format!("Child {c}"));
                    child.url = Some(format!("https://example.org/{w}/{i}/{c}"));
                    tab.children.push(child);
                }
            }
            win.children.push(tab);
        }
        t.root.children.push(win);
    }
    t
}

fn frames(
    app: &mut App,
    ctx: &egui::Context,
    n: usize,
    input: impl Fn(usize) -> egui::RawInput,
) -> std::time::Duration {
    let start = std::time::Instant::now();
    for i in 0..n {
        let mut out = ctx.run_ui(input(i), |ui| app.show(ui));
        out.textures_delta.clear();
    }
    start.elapsed() / u32::try_from(n).expect("a few hundred frames")
}

#[test]
#[ignore = "benchmark: run with --release and --nocapture"]
fn big_tree_frame_time() {
    let tree = big_tree(100, 300); // 100 windows, 30,000 tabs + 18,000 children
    let n = tree.root.count();
    let ctx = egui::Context::default();
    let mut app = App::new(Box::new(Fake(Some(tree))), None);
    let screen = |_| egui::RawInput {
        screen_rect: Some(egui::Rect::from_min_size(
            egui::Pos2::ZERO,
            egui::vec2(1200.0, 800.0),
        )),
        ..Default::default()
    };
    frames(&mut app, &ctx, 3, screen);
    let idle = frames(&mut app, &ctx, 20, screen);
    let scroll = frames(&mut app, &ctx, 20, |_| egui::RawInput {
        events: vec![egui::Event::MouseWheel {
            unit: egui::MouseWheelUnit::Point,
            delta: egui::vec2(0.0, -400.0),
            phase: egui::TouchPhase::Move,
            modifiers: Default::default(),
        }],
        ..screen(0)
    });
    app.search = "something 7".into();
    let search_first = frames(&mut app, &ctx, 1, screen);
    let search = frames(&mut app, &ctx, 20, screen);
    println!(
        "{n} nodes: idle {idle:?}/frame, scrolling {scroll:?}/frame, search first {search_first:?}, then {search:?}/frame"
    );
}
