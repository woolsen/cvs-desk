//! Reading `cvs log` and drawing a file's revisions as a branch diagram.
//!
//! This is the rule behind the Graph view: the log text is read into one node per
//! revision, a revision whose number has more than two
//! dot-separated fields sits on a branch, and the first two fields say which trunk revision
//! the branch came off. The drawing gives the trunk lane 0 and each branch a lane of its own,
//! joined back to the trunk where it forked.

use crate::truncate_columns;

/// One revision of one file, as `cvs log` describes it.
#[derive(Clone, Debug, PartialEq)]
pub struct RevNode {
    pub revision: String,
    pub date: String,
    pub author: String,
    pub message: String,
    pub branch: bool,
}

/// The branch a revision belongs to: `1.2.4.1` is on branch `1.2.4`, while `1.2.6.1`
/// is a different branch. A trunk revision, having at most two fields, has no branch id.
pub fn branch_id(revision: &str) -> Option<String> {
    let fields: Vec<&str> = revision.split('.').collect();
    if fields.len() > 2 {
        Some(fields[..fields.len() - 1].join("."))
    } else {
        None
    }
}

/// The trunk revision a branch revision came off: `1.2.4.1` came off `1.2`. A trunk
/// revision has no fork.
pub fn fork_revision(revision: &str) -> Option<String> {
    let fields: Vec<&str> = revision.split('.').collect();
    if fields.len() > 2 {
        Some(fields[..2].join("."))
    } else {
        None
    }
}

/// Up to this many branch lanes are drawn. Beyond it, the extra branches share the last
/// lane instead of pushing the text columns off the window.
pub const MAX_LANES: usize = 6;

/// One branch as the drawing needs it: its revisions in display order, the trunk node it
/// forked from (when that node is in the log), and the lane it was given.
#[derive(Clone, Debug, PartialEq)]
pub struct Branch {
    pub id: String,
    pub fork: Option<usize>,
    pub revisions: Vec<usize>,
    pub lane: usize,
}

/// Lay the branches out beside the trunk. A lane is reused once its last branch has ended
/// above the next branch's first row: because [`display_order`] puts each branch directly
/// above its fork, runs for different forks never overlap. Only branches forking from the
/// same revision need separate lanes; past [`MAX_LANES`] they share the last one.
pub fn branch_layout(nodes: &[RevNode], order: &[usize]) -> Vec<Branch> {
    let mut row_of = vec![0usize; nodes.len()];
    for (row, &i) in order.iter().enumerate() {
        row_of[i] = row;
    }

    let mut branches: Vec<Branch> = Vec::new();
    for &i in order {
        let n = &nodes[i];
        if !n.branch {
            continue;
        }
        let Some(id) = branch_id(&n.revision) else {
            continue;
        };
        if let Some(b) = branches.iter_mut().find(|b| b.id == id) {
            b.revisions.push(i);
            continue;
        }
        let fork =
            fork_revision(&n.revision).and_then(|rev| nodes.iter().position(|n| n.revision == rev));
        branches.push(Branch {
            id,
            fork,
            revisions: vec![i],
            lane: 0,
        });
    }

    let mut lane_end: Vec<usize> = Vec::new();
    for b in &mut branches {
        let top = b.revisions.iter().map(|&i| row_of[i]).min().unwrap_or(0);
        let end = b
            .fork
            .map(|f| row_of[f])
            .or_else(|| b.revisions.iter().map(|&i| row_of[i]).max())
            .unwrap_or(top);
        let lane = match lane_end.iter().position(|&last| last < top) {
            Some(lane) => lane,
            None if lane_end.len() < MAX_LANES => {
                lane_end.push(end);
                lane_end.len() - 1
            }
            None => lane_end.len() - 1,
        };
        lane_end[lane] = lane_end[lane].max(end);
        b.lane = lane + 1;
    }
    branches
}

/// Read `cvs log` output into one node per revision. The log prints a `revision 1.3` line,
/// then `date: …;  author: …;  state: …;`, then the log message **flush left** (a detail a
/// test caught), then a row of dashes.
pub fn revision_nodes(lines: &[String]) -> Vec<RevNode> {
    let mut out: Vec<RevNode> = Vec::new();
    let mut cur: Option<RevNode> = None;
    for raw in lines {
        let line = raw.trim_end();
        let t = line.trim();
        if let Some(rest) = t.strip_prefix("revision ") {
            if let Some(n) = cur.take() {
                out.push(n);
            }
            let rev = rest.trim().to_string();
            let branch = rev.split('.').count() > 2;
            cur = Some(RevNode {
                revision: rev,
                date: String::new(),
                author: String::new(),
                message: String::new(),
                branch,
            });
            continue;
        }
        if let Some(n) = cur.as_mut() {
            if t.starts_with("date:") {
                for part in t.split(';') {
                    let part = part.trim();
                    if let Some(d) = part.strip_prefix("date:") {
                        n.date = short_cvs_date(d.trim());
                    } else if let Some(a) = part.strip_prefix("author:") {
                        n.author = a.trim().to_string();
                    }
                }
                continue;
            }
            let separator = !t.is_empty() && t.chars().all(|c| c == '-' || c == '=' || c == ' ');
            if !separator && !t.is_empty() {
                if !n.message.is_empty() {
                    n.message.push(' ');
                }
                n.message.push_str(t);
            }
        }
    }
    if let Some(n) = cur.take() {
        out.push(n);
    }
    out
}

/// `2026/11/22 09:14:03` → `2026-11-22`. Leaves anything else alone.
fn short_cvs_date(d: &str) -> String {
    let d = d.split_whitespace().next().unwrap_or(d);
    d.replace('/', "-")
}

/// A short note about the revision set, for the status bar.
pub fn summary(nodes: &[RevNode]) -> String {
    let mut branches: Vec<String> = Vec::new();
    for n in nodes.iter().filter(|n| n.branch) {
        if let Some(id) = branch_id(&n.revision) {
            if !branches.contains(&id) {
                branches.push(id);
            }
        }
    }
    match branches.len() {
        0 => format!("{} revision(s), no branches", nodes.len()),
        1 => format!("{} revision(s), 1 branch", nodes.len()),
        n => format!("{} revision(s), {} branches", nodes.len(), n),
    }
}

/// The trunk's colour, from the reference graph.
pub const TRUNK_COLOR: egui::Color32 = egui::Color32::from_rgb(0x40, 0x9E, 0xFF);
/// A branch's colour, from the reference graph.
pub const BRANCH_COLOR: egui::Color32 = egui::Color32::from_rgb(0x2F, 0x9E, 0x5F);
/// The revision numbers' colour.
pub const TEXT_COLOR: egui::Color32 = egui::Color32::from_rgb(0x30, 0x90, 0xC0);
/// The row the newest trunk revision sits on.
pub const LATEST_BG: egui::Color32 = egui::Color32::from_rgb(0xDB, 0xEA, 0xFE);

/// Node and line sizes, kept compact to match the rest of the window.
const NODE_R: f32 = 3.5;
const LINE_W: f32 = 1.5;
const LANE_GAP: f32 = 16.0;
const LANE_GAP_TIGHT: f32 = 12.0;
const LEFT: f32 = 18.0;
const TEXT_GAP: f32 = 14.0;
const CORNER: f32 = 5.0;
const PAD_Y: f32 = 4.0;
const DATE_X: f32 = 70.0;
const NOTE_X: f32 = 250.0;

/// The rows in the order the graph draws them: the trunk from newest to oldest, and each
/// branch's revisions immediately above the trunk revision they forked from. Branches whose
/// fork is not in the log keep their place at the end.
pub fn display_order(nodes: &[RevNode]) -> Vec<usize> {
    struct Group {
        id: String,
        fork: String,
        revisions: Vec<usize>,
    }
    let mut groups: Vec<Group> = Vec::new();
    for (i, n) in nodes.iter().enumerate().filter(|(_, n)| n.branch) {
        let Some(id) = branch_id(&n.revision) else {
            continue;
        };
        let Some(fork) = fork_revision(&n.revision) else {
            continue;
        };
        if let Some(g) = groups.iter_mut().find(|g| g.id == id) {
            g.revisions.push(i);
        } else {
            groups.push(Group {
                id,
                fork,
                revisions: vec![i],
            });
        }
    }

    let mut out = Vec::with_capacity(nodes.len());
    for (i, n) in nodes.iter().enumerate().filter(|(_, n)| !n.branch) {
        for g in &groups {
            if g.fork == n.revision {
                out.extend(g.revisions.iter().copied());
            }
        }
        out.push(i);
    }
    for (j, n) in nodes.iter().enumerate() {
        if n.branch && !out.contains(&j) {
            out.push(j);
        }
    }
    out
}

/// Draw the diagram as the reference does: a blue trunk with hollow nodes, each branch a
/// green lane that sits above its fork and rejoins the trunk through a rounded corner. The
/// newest trunk revision's row is tinted and tagged.
pub fn draw(ui: &mut egui::Ui, nodes: &[RevNode], height: f32, row_h: f32) {
    let want = nodes.len() as f32 * row_h + PAD_Y * 2.0;
    let view_h = want.min(height).max(row_h + PAD_Y * 2.0);

    let order = display_order(nodes);
    let mut row_of = vec![0usize; nodes.len()];
    for (row, &i) in order.iter().enumerate() {
        row_of[i] = row;
    }
    let branches = branch_layout(nodes, &order);
    let mut lane_of = vec![0usize; nodes.len()];
    for b in &branches {
        for &i in &b.revisions {
            lane_of[i] = b.lane;
        }
    }
    let lanes = branches.iter().map(|b| b.lane).max().unwrap_or(0) + 1;
    let lane_gap = if lanes > 4 { LANE_GAP_TIGHT } else { LANE_GAP };
    // The newest trunk revision is what the log aims at; a branch above it is not "latest".
    let latest = order.iter().copied().find(|&i| !nodes[i].branch);

    egui::ScrollArea::vertical()
        .max_height(view_h)
        .auto_shrink([false, false])
        .show(ui, |ui| {
            ui.set_min_height(want.min(4000.0));
            let width = ui.available_width().max(320.0);
            let (rect, _) = ui.allocate_exact_size(egui::vec2(width, want), egui::Sense::hover());
            let painter = ui.painter();
            // The graph paints its own text, so it resolves the window's scale here
            // rather than carrying sizes of its own. A revision number is monospaced and
            // the same size as the date beside it; the "latest" tag uses that size too.
            let body = egui::TextStyle::Body.resolve(ui.style());
            let version = egui::FontId::new(body.size, egui::FontFamily::Monospace);
            let x_lane = |lane: usize| rect.left() + LEFT + lane as f32 * lane_gap;
            let top = rect.top() + PAD_Y + row_h / 2.0;
            let y_of = |i: usize| top + row_of[i] as f32 * row_h;
            let trunk_ink = egui::Stroke::new(LINE_W, TRUNK_COLOR);
            let branch_ink = egui::Stroke::new(LINE_W, BRANCH_COLOR);

            // The newest trunk row is the only one tinted.
            if let Some(i) = latest {
                let y = y_of(i);
                painter.rect_filled(
                    egui::Rect::from_min_max(
                        egui::pos2(rect.left(), y - row_h / 2.0),
                        egui::pos2(rect.right(), y + row_h / 2.0),
                    ),
                    0.0,
                    LATEST_BG,
                );
            }

            // The trunk, running through the rows in display order.
            let trunk: Vec<usize> = order
                .iter()
                .copied()
                .filter(|&i| !nodes[i].branch)
                .collect();
            if let (Some(first), Some(last)) = (trunk.first(), trunk.last()) {
                painter.line_segment(
                    [
                        egui::pos2(x_lane(0), y_of(*first)),
                        egui::pos2(x_lane(0), y_of(*last)),
                    ],
                    trunk_ink,
                );
            }

            // Each branch runs down its lane and turns into the fork with a rounded corner.
            for b in &branches {
                let x = x_lane(b.lane);
                let (Some(&first), Some(&last)) = (b.revisions.first(), b.revisions.last()) else {
                    continue;
                };
                let top_y = y_of(first);
                let bottom_y = y_of(last);
                if let Some(fork) = b.fork {
                    let fork_y = y_of(fork);
                    let turn_x = x - CORNER;
                    let mut path = Vec::with_capacity(10);
                    path.push(egui::pos2(x, top_y));
                    path.push(egui::pos2(x, fork_y - CORNER));
                    for step in 0..=6 {
                        let a = std::f32::consts::FRAC_PI_2 * step as f32 / 6.0;
                        path.push(egui::pos2(
                            turn_x + CORNER * a.cos(),
                            fork_y - CORNER + CORNER * a.sin(),
                        ));
                    }
                    path.push(egui::pos2(x_lane(0), fork_y));
                    painter.add(egui::Shape::line(path, branch_ink));
                } else if bottom_y > top_y {
                    // No fork row to turn into: just connect the branch's own revisions.
                    painter
                        .line_segment([egui::pos2(x, top_y), egui::pos2(x, bottom_y)], branch_ink);
                }
            }

            let text_x = x_lane(lanes - 1) + TEXT_GAP;
            let date_x = text_x + DATE_X;
            let note_x = text_x + NOTE_X;
            let cell = (body.size * 0.5).max(1.0);
            let date_cols = (((note_x - date_x - 8.0).max(20.0)) / cell) as usize;
            let note_cols = (((rect.right() - note_x - 8.0).max(20.0)) / cell) as usize;
            for &i in &order {
                let n = &nodes[i];
                let y = y_of(i);
                let text_color = if n.branch { BRANCH_COLOR } else { TEXT_COLOR };
                let node_color = if n.branch { BRANCH_COLOR } else { TRUNK_COLOR };
                let c = egui::pos2(x_lane(lane_of[i]), y);
                painter.circle(
                    c,
                    NODE_R,
                    egui::Color32::WHITE,
                    egui::Stroke::new(LINE_W, node_color),
                );
                let rev = painter.text(
                    egui::pos2(text_x, y),
                    egui::Align2::LEFT_CENTER,
                    &n.revision,
                    version.clone(),
                    text_color,
                );
                if Some(i) == latest {
                    painter.text(
                        egui::pos2(rev.right() + 6.0, y),
                        egui::Align2::LEFT_CENTER,
                        "latest",
                        version.clone(),
                        TEXT_COLOR,
                    );
                }
                painter.text(
                    egui::pos2(date_x, y),
                    egui::Align2::LEFT_CENTER,
                    truncate_columns(&format!("{}  {}", n.date, n.author), date_cols),
                    body.clone(),
                    egui::Color32::from_gray(110),
                );
                if !n.message.is_empty() {
                    painter.text(
                        egui::pos2(note_x, y),
                        egui::Align2::LEFT_CENTER,
                        truncate_columns(&n.message, note_cols),
                        body.clone(),
                        egui::Color32::from_gray(60),
                    );
                }
            }
        });
}

/// A legend naming the colours, built from the `P_Graph*Color` setting names.
pub fn legend() -> Vec<(&'static str, egui::Color32)> {
    vec![
        ("revision", TRUNK_COLOR),
        ("branch", BRANCH_COLOR),
        ("text", egui::Color32::from_gray(110)),
    ]
}

/// Keep this module honest about what it does with a real log: the states it can show.
#[cfg(test)]
mod tests {
    use super::*;

    /// The row height the window passes in; the drawing tests use the same value.
    const ROW_H: f32 = 22.0;

    fn lines(text: &str) -> Vec<String> {
        text.lines().map(|l| l.to_string()).collect()
    }

    fn node(revision: &str, branch: bool) -> RevNode {
        RevNode {
            revision: revision.into(),
            date: String::new(),
            author: String::new(),
            message: String::new(),
            branch,
        }
    }

    #[test]
    fn reads_each_revision_from_a_cvs_log() {
        let log = lines(
            "RCS file: /var/lib/cvs/sampleproj/main.c,v\n\
             revision 1.2\n\
             date: 2026/11/19 09:41:13;  author: bob;  state: Exp;  lines: +1 -0\n\
             Tag the vendor import\n\
             ----------------------------\n\
             revision 1.1\n\
             date: 2026/11/18 08:15:30;  author: ann;  state: Exp;\n\
             Initial revision\n",
        );
        let nodes = revision_nodes(&log);
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[0].revision, "1.2");
        assert_eq!(nodes[0].author, "bob");
        assert_eq!(nodes[0].date, "2026-11-19");
        assert_eq!(nodes[0].message, "Tag the vendor import");
        assert_eq!(nodes[1].message, "Initial revision");
        assert!(!nodes[0].branch);
    }

    #[test]
    fn marks_a_branch_revision() {
        let log = lines("revision 1.2.4.1\nno message\n");
        let nodes = revision_nodes(&log);
        assert!(nodes[0].branch);
    }

    #[test]
    fn a_branch_revision_hangs_off_its_trunk_revision() {
        assert_eq!(branch_id("1.2.4.1").as_deref(), Some("1.2.4"));
        assert_eq!(
            branch_id("1.2.4.7").as_deref(),
            Some("1.2.4"),
            "same branch"
        );
        assert_eq!(
            branch_id("1.2.6.1").as_deref(),
            Some("1.2.6"),
            "a different branch off the same trunk revision"
        );
        assert_eq!(branch_id("1.3.2.1").as_deref(), Some("1.3.2"));
        assert_eq!(
            branch_id("1.2").as_deref(),
            None,
            "a trunk revision has no id"
        );
        assert_eq!(branch_id("1.10").as_deref(), None);

        assert_eq!(fork_revision("1.2.4.1").as_deref(), Some("1.2"));
        assert_eq!(
            fork_revision("1.2.4.7").as_deref(),
            Some("1.2"),
            "same branch"
        );
        assert_eq!(
            fork_revision("1.3.2.1").as_deref(),
            Some("1.3"),
            "a second fork"
        );
        assert_eq!(
            fork_revision("1.2").as_deref(),
            None,
            "a trunk revision has no fork"
        );
        assert_eq!(fork_revision("1.10").as_deref(), None);
    }

    #[test]
    fn branches_above_different_forks_can_share_a_lane() {
        let nodes = vec![
            node("1.1", false),
            node("1.2", false),
            node("1.2.4.1", true),
            node("1.2.4.2", true),
            node("1.3", false),
            node("1.3.2.1", true),
        ];
        let order = display_order(&nodes);
        let branches = branch_layout(&nodes, &order);
        assert_eq!(
            branches.iter().map(|b| b.id.as_str()).collect::<Vec<_>>(),
            vec!["1.2.4", "1.3.2"]
        );
        assert_eq!(
            branches.iter().map(|b| b.lane).collect::<Vec<_>>(),
            vec![1, 1],
            "two runs that do not overlap may share a lane"
        );
    }

    #[test]
    fn branches_above_the_same_fork_get_separate_lanes() {
        let nodes = vec![
            node("1.1", false),
            node("1.2", false),
            node("1.2.4.1", true),
            node("1.2.6.1", true),
        ];
        let order = display_order(&nodes);
        let branches = branch_layout(&nodes, &order);
        assert_eq!(
            branches.iter().map(|b| b.lane).collect::<Vec<_>>(),
            vec![1, 2]
        );
    }

    #[test]
    fn a_log_with_no_branches_has_no_branch_lanes() {
        let nodes = vec![node("1.1", false)];
        let order = display_order(&nodes);
        assert!(branch_layout(&nodes, &order).is_empty());
        assert_eq!(order, vec![0]);
    }

    #[test]
    fn the_lanes_stop_at_the_cap() {
        let mut nodes = vec![node("1.2", false), node("1.1", false)];
        for i in 0..(MAX_LANES + 2) {
            nodes.push(node(&format!("1.2.{}.1", 4 + i * 2), true));
        }
        let order = display_order(&nodes);
        let branches = branch_layout(&nodes, &order);
        assert_eq!(branches.len(), MAX_LANES + 2);
        assert_eq!(
            branches.iter().map(|b| b.lane).max(),
            Some(MAX_LANES),
            "extra branches share the last lane instead of widening the gutter"
        );
        assert!(branches.iter().skip(MAX_LANES).all(|b| b.lane == MAX_LANES));
    }

    #[test]
    fn empty_log_gives_no_nodes() {
        assert!(revision_nodes(&lines("")).is_empty());
    }

    #[test]
    fn the_summary_counts_branches() {
        let nodes = vec![node("1.1", false), node("1.2.4.1", true)];
        assert_eq!(summary(&nodes), "2 revision(s), 1 branch");
        assert_eq!(summary(&[node("1.1", false)]), "1 revision(s), no branches");
        let vendors = vec![node("1.2.4.1", true), node("1.2.6.1", true)];
        assert_eq!(summary(&vendors), "2 revision(s), 2 branches");
    }

    fn drawn(nodes: &[RevNode]) -> egui::FullOutput {
        let ctx = egui::Context::default();
        let input = egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1000.0, 800.0),
            )),
            ..Default::default()
        };
        let mut output = ctx.run_ui(input, |ui| {
            egui::CentralPanel::default().show(ui, |ui| draw(ui, nodes, 600.0, ROW_H));
        });
        // The test only reads the shapes; the font atlas delta belongs to an integration.
        output.textures_delta.clear();
        output
    }

    enum Drawn {
        Circle(egui::epaint::CircleShape),
        Path(egui::epaint::PathShape),
        Text(String, egui::Rect),
        Rect(egui::epaint::RectShape),
    }

    fn drawn_shapes(output: &egui::FullOutput) -> Vec<Drawn> {
        fn walk(shape: &egui::Shape, out: &mut Vec<Drawn>) {
            match shape {
                egui::Shape::Circle(c) => out.push(Drawn::Circle(*c)),
                egui::Shape::Path(p) => out.push(Drawn::Path(p.clone())),
                egui::Shape::Text(t) => out.push(Drawn::Text(
                    t.galley.text().to_owned(),
                    egui::Rect::from_min_size(t.pos, t.galley.size()),
                )),
                egui::Shape::Rect(r) => out.push(Drawn::Rect(r.clone())),
                egui::Shape::Vec(inner) => inner.iter().for_each(|s| walk(s, out)),
                _ => {}
            }
        }
        let mut out = Vec::new();
        for clipped in &output.shapes {
            walk(&clipped.shape, &mut out);
        }
        out
    }

    fn circles(shapes: &[Drawn]) -> Vec<egui::epaint::CircleShape> {
        shapes
            .iter()
            .filter_map(|s| match s {
                Drawn::Circle(c) => Some(*c),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn branches_are_drawn_above_the_revision_they_forked_from() {
        let nodes = vec![
            node("1.4", false),
            node("1.3", false),
            node("1.2", false),
            node("1.1", false),
            node("1.3.2.1", true),
        ];
        assert_eq!(display_order(&nodes), vec![0, 4, 1, 2, 3]);

        let nodes = vec![
            node("1.5", false),
            node("1.4", false),
            node("1.3", false),
            node("1.2", false),
            node("1.2.4.2", true),
            node("1.2.4.1", true),
            node("1.3.2.1", true),
            node("1.1", false),
        ];
        assert_eq!(
            display_order(&nodes),
            vec![0, 1, 6, 2, 4, 5, 3, 7],
            "each branch's revisions stay together, just above their fork"
        );
    }

    #[test]
    fn a_branch_without_its_fork_stays_at_the_end() {
        let nodes = vec![
            node("1.2", false),
            node("1.9.2.1", true),
            node("1.1", false),
        ];
        assert_eq!(display_order(&nodes), vec![0, 2, 1]);
    }

    #[test]
    fn every_revision_is_a_hollow_node() {
        let nodes = vec![
            node("1.4", false),
            node("1.3", false),
            node("1.2", false),
            node("1.1", false),
            node("1.3.2.1", true),
        ];
        let output = drawn(&nodes);
        let circles = circles(&drawn_shapes(&output));
        assert_eq!(circles.len(), nodes.len());
        for c in &circles {
            assert_eq!(c.fill, egui::Color32::WHITE, "nodes are hollow");
            assert!(
                c.stroke.color == TRUNK_COLOR || c.stroke.color == BRANCH_COLOR,
                "a node carries its lane's colour"
            );
        }
    }

    #[test]
    fn a_branch_is_above_its_fork_and_joined_by_a_rounded_lane() {
        let nodes = vec![
            node("1.4", false),
            node("1.3", false),
            node("1.2", false),
            node("1.1", false),
            node("1.3.2.1", true),
        ];
        let output = drawn(&nodes);
        let shapes = drawn_shapes(&output);
        let circles = circles(&shapes);
        let branch = circles
            .iter()
            .find(|c| c.stroke.color == BRANCH_COLOR)
            .expect("the branch revision has a green node");
        let trunk: Vec<&egui::epaint::CircleShape> = circles
            .iter()
            .filter(|c| c.stroke.color == TRUNK_COLOR)
            .collect();
        let fork = trunk
            .iter()
            .find(|c| c.center.y > branch.center.y)
            .expect("the fork row is below the branch");
        assert!(
            (fork.center.y - branch.center.y - 22.0).abs() < 0.01,
            "the branch sits directly above its fork"
        );
        let lane = shapes
            .iter()
            .find_map(|s| match s {
                Drawn::Path(p)
                    if p.stroke.color == egui::epaint::ColorMode::Solid(BRANCH_COLOR) =>
                {
                    Some(p)
                }
                _ => None,
            })
            .expect("the branch has a green lane");
        assert!(
            (lane.points.first().unwrap().x - branch.center.x).abs() < 0.01
                && (lane.points.first().unwrap().y - branch.center.y).abs() < 0.01,
            "the lane starts at the branch node"
        );
        assert!(
            lane.points
                .iter()
                .any(|p| (p.x - fork.center.x).abs() < 0.01 && (p.y - fork.center.y).abs() < 0.01),
            "the lane turns into the trunk at the fork"
        );
        let corner = lane
            .points
            .iter()
            .any(|p| p.x > branch.center.x - 5.01 && p.x < branch.center.x - 4.99);
        assert!(corner, "the turn is rounded rather than a sharp corner");
    }

    #[test]
    fn the_newest_trunk_revision_is_tinted_and_tagged() {
        let nodes = vec![
            node("1.4", false),
            node("1.3", false),
            node("1.2", false),
            node("1.1", false),
            node("1.3.2.1", true),
        ];
        let output = drawn(&nodes);
        let shapes = drawn_shapes(&output);
        let head = circles(&shapes)
            .into_iter()
            .filter(|c| c.stroke.color == TRUNK_COLOR)
            .min_by(|a, b| a.center.y.partial_cmp(&b.center.y).unwrap())
            .expect("the trunk has a head row");
        let tint = shapes
            .iter()
            .find_map(|s| match s {
                Drawn::Rect(r) if r.fill == LATEST_BG => Some(r.rect),
                _ => None,
            })
            .expect("the head row is tinted");
        assert!((tint.center().y - head.center.y).abs() < 0.01);
        let tag = shapes
            .iter()
            .find_map(|s| match s {
                Drawn::Text(t, rect) if t == "latest" => Some(*rect),
                _ => None,
            })
            .expect("the head row is tagged");
        assert!((tag.center().y - head.center.y).abs() < 1.0);
    }

    #[test]
    fn the_nodes_and_gutter_stay_compact() {
        let nodes = vec![
            node("1.4", false),
            node("1.3", false),
            node("1.2", false),
            node("1.1", false),
            node("1.3.2.1", true),
        ];
        let output = drawn(&nodes);
        let shapes = drawn_shapes(&output);
        for c in circles(&shapes) {
            assert!(c.radius <= 4.0, "nodes stay small: {}", c.radius);
        }
        let head = shapes
            .iter()
            .find_map(|s| match s {
                Drawn::Text(t, rect) if t == "1.4" => Some(*rect),
                _ => None,
            })
            .expect("the head revision is on screen");
        assert!(
            head.left() < 70.0,
            "the revision text starts close to the gutter: {}",
            head.left()
        );
    }
}
