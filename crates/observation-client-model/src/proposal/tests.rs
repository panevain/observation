use super::*;
use crate::editor::Editor;
use crate::history::{History, HistoryOp};
use crate::toasts::Toasts;
use observation_core::camera::{CameraPlacement, Catalog};
use observation_core::scene::AreaRef;

fn camera(id: u32, x: f32) -> CameraPlacement {
    CameraPlacement {
        id,
        label: format!("Cam {id}"),
        model_id: "m".into(),
        position: [x, 3.0, -6.0],
        yaw_deg: 180.0,
        pitch_deg: -10.0,
        roll_deg: 0.0,
        color: None,
        enabled: true,
        group: None,
    }
}

/// A document on the demo area with cameras 1 and 2.
fn doc() -> (Document, Models) {
    let models = Models::new(Catalog::default());
    let mut doc = Document::default();
    doc.area = Some(AreaRef::Demo);
    doc.add_camera(camera(0, 0.0));
    doc.add_camera(camera(0, 10.0));
    (doc, models)
}

/// A proposal that turns camera 1 and adds camera 7, made against `doc`.
fn proposal(doc: &Document, models: &Models) -> Proposal {
    let base = current_scene(doc, models).unwrap();
    let mut proposed = base.clone();
    proposed.cameras[0].yaw_deg = 90.0;
    proposed.cameras.push(camera(7, 20.0));
    Proposal {
        id: 4,
        summary: "Turn camera 1 and add one by the gate".into(),
        base_revision: 2,
        changes: ChangeSet::between(&base, &proposed).unwrap(),
    }
}

#[test]
fn an_open_proposal_follows_the_users_unrelated_edits() {
    let (mut doc, models) = doc();
    let mut p = PendingProposal::new(proposal(&doc, &models), &models);
    assert_eq!(p.lines.len(), 2);
    assert!(p.lines[1].starts_with("Add Camera 7"), "{:?}", p.lines);
    assert_eq!(p.follow(&doc, &models), None);
    assert_eq!(p.standing, Standing::Open);
    let (first, scene) = p.proposed().unwrap();
    assert_eq!(scene.cameras.len(), 3);
    // Unchanged document: nothing to do.
    assert_eq!(p.follow(&doc, &models), None);
    assert_eq!(p.proposed().unwrap().0, first);
    // The user moves camera 2, which the proposal leaves alone.
    doc.camera_mut(2).unwrap().position[0] = 12.0;
    assert_eq!(p.follow(&doc, &models), None);
    let (second, scene) = p.proposed().unwrap();
    assert_ne!(first, second, "the preview follows");
    assert_eq!(scene.cameras[1].position[0], 12.0);
}

#[test]
fn touching_an_item_it_changes_makes_it_stale_for_good() {
    let (mut doc, models) = doc();
    let mut p = PendingProposal::new(proposal(&doc, &models), &models);
    p.follow(&doc, &models);
    doc.camera_mut(1).unwrap().pitch_deg = -20.0;
    assert_eq!(p.follow(&doc, &models), Some(vec!["camera 1".to_owned()]));
    assert!(matches!(&p.standing, Standing::Stale { what } if what == &["camera 1"]));
    assert!(p.proposed().is_none());
    // Told once; putting it back does not revive it.
    doc.camera_mut(1).unwrap().pitch_deg = -10.0;
    assert_eq!(p.follow(&doc, &models), None);
    assert!(matches!(p.standing, Standing::Stale { .. }));
    let (mut d2, mut m2) = (doc, models);
    assert!(matches!(
        p.accept(&mut d2, &mut m2, 0),
        Accepted::Refused(_)
    ));
    let vm = ProposalCardVm::new(&p, Some("claude"));
    assert!(!vm.can_accept && !vm.can_reject && vm.warning);
    assert!(vm.status.unwrap().contains("camera 1"), "stale names it");
    assert_eq!(
        stale_reason(&["camera 1".into()]),
        "The user changed camera 1 before deciding"
    );
}

#[test]
fn an_id_the_user_has_taken_since_is_a_conflict() {
    let (mut doc, models) = doc();
    let mut p = PendingProposal::new(proposal(&doc, &models), &models);
    let mut mine = camera(0, -5.0);
    mine.id = 7;
    doc.cameras.push(mine);
    doc.touch();
    assert_eq!(p.follow(&doc, &models), Some(vec!["camera 7".to_owned()]));
}

#[test]
fn accepting_applies_everything_as_one_undo_step_and_logs_it() {
    let (mut doc, mut models) = doc();
    let mut history = History::default();
    let mut toasts = Toasts::default();
    let mut editor = Editor::default();
    history.observe(&doc, &models, false, &mut toasts);
    let mut p = PendingProposal::new(proposal(&doc, &models), &models);
    p.follow(&doc, &models);
    // An unrelated edit, recorded as its own step.
    doc.camera_mut(2).unwrap().label = "Porch".into();
    history.observe(&doc, &models, false, &mut toasts);

    assert_eq!(
        p.accept(&mut doc, &mut models, 1_793_475_782),
        Accepted::Applied
    );
    assert_eq!(p.standing, Standing::Accepted);
    assert_eq!(doc.cameras.len(), 3);
    assert_eq!(doc.camera(1).unwrap().yaw_deg, 90.0);
    assert_eq!(
        doc.camera(2).unwrap().label,
        "Porch",
        "the user's edit stays"
    );
    assert_eq!(doc.agent_log.len(), 1);
    assert_eq!(
        doc.agent_log[0].summary,
        "Turn camera 1 and add one by the gate"
    );
    assert_eq!(doc.agent_log[0].accepted_unix, 1_793_475_782);
    // A camera added later does not reuse the proposal's id.
    assert_eq!(doc.add_camera(camera(0, 30.0)), 8);
    doc.remove_camera(8);

    history.observe(&doc, &models, false, &mut toasts);
    assert_eq!(
        history.undo_label().as_deref(),
        Some("Agent: Turn camera 1 and add one by the gate")
    );
    let toast = toasts.iter().last().unwrap();
    assert!(
        toast.text.starts_with("Applied the agent's proposal"),
        "{}",
        toast.text
    );
    history.step(
        HistoryOp::Undo,
        &mut doc,
        &mut models,
        &mut editor,
        &mut toasts,
    );
    assert_eq!(doc.cameras.len(), 2, "one undo reverts the whole proposal");
    assert!(doc.agent_log.is_empty(), "and its log entry");
    assert_eq!(doc.camera(2).unwrap().label, "Porch");
    let vm = ProposalCardVm::new(&p, None);
    assert_eq!(vm.title, "Proposal from the agent");
    assert!(!vm.can_accept);
}

#[test]
fn a_proposal_that_would_leave_an_invalid_scene_cannot_be_accepted() {
    let (doc, mut models) = doc();
    let base = current_scene(&doc, &models).unwrap();
    let mut proposed = base.clone();
    proposed.params.subject_height_m = 50.0;
    let mut p = PendingProposal::new(
        Proposal {
            id: 1,
            summary: "Tall people".into(),
            base_revision: 0,
            changes: ChangeSet::between(&base, &proposed).unwrap(),
        },
        &models,
    );
    p.follow(&doc, &models);
    assert!(matches!(&p.standing, Standing::Unusable { why } if why.contains("subject height")));
    let mut d = doc;
    assert!(matches!(
        p.accept(&mut d, &mut models, 0),
        Accepted::Refused(_)
    ));
    let vm = ProposalCardVm::new(&p, None);
    assert!(!vm.can_accept && vm.can_reject);
    // A malformed set is unusable from the start.
    let p = PendingProposal::new(
        Proposal {
            id: 2,
            summary: "Nothing".into(),
            base_revision: 0,
            changes: ChangeSet::default(),
        },
        &models,
    );
    assert!(matches!(p.standing, Standing::Unusable { .. }));
}

#[test]
fn rejecting_keeps_the_reason_and_closing_ends_an_open_one() {
    let (doc, models) = doc();
    let mut p = PendingProposal::new(proposal(&doc, &models), &models);
    p.reason = "  too close to the neighbours  ".into();
    assert_eq!(p.reject().as_deref(), Some("too close to the neighbours"));
    assert_eq!(p.standing, Standing::Rejected);
    p.close("withdrawn");
    assert_eq!(p.standing, Standing::Rejected, "a decision stays");
    let mut q = PendingProposal::new(proposal(&doc, &models), &models);
    q.close("The agent withdrew this proposal.");
    assert_eq!(
        ProposalCardVm::new(&q, None).status.as_deref(),
        Some("The agent withdrew this proposal.")
    );
}

#[test]
fn the_figures_compare_the_street_seen_before_and_after() {
    let stats = |seen: f32| CoverageStats {
        street_m2: 1000.0,
        street_seen_m2: seen,
        ..CoverageStats::default()
    };
    let day = (DoriLevel::Recognise, Lighting::Day);
    let vm = FiguresVm::new((day, &stats(312.0)), None);
    assert_eq!(vm.lead, "Street seen at Recognise by day");
    assert_eq!(vm.before, "312 m² (31%)");
    assert_eq!((vm.after, vm.change), (None, None), "computing");
    let vm = FiguresVm::new((day, &stats(312.0)), Some((day, &stats(365.0))));
    assert_eq!(vm.after.as_deref(), Some("365 m² (36%)"));
    assert_eq!(vm.change.as_deref(), Some("+53 m²"));
    let night = (DoriLevel::Recognise, Lighting::Night);
    let vm = FiguresVm::new((day, &stats(312.0)), Some((night, &stats(100.0))));
    assert_eq!(
        vm.after.as_deref(),
        Some("100 m² (10%) at Recognise at night")
    );
    assert_eq!(vm.change.as_deref(), Some("\u{2212}212 m²"));
}
