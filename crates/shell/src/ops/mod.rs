pub mod bootstrap;
pub mod clean;
pub mod doctor;
pub mod explain;
pub mod install;
pub mod remove;
pub mod repair;

pub(crate) async fn recover_interrupted(
    ctx: &crate::Context,
    tree: &mut mix_events::Tree,
    performer: &mut crate::drive::Performer,
    losing: bool,
) -> bool {
    use mix_events::v1::{Code, Step, node_started};
    use mix_events::{Ending, ROOT, Start};
    let journals = ctx.journals.as_path();
    if !ctx.interrupted(journals) {
        return true;
    }
    let node = tree
        .start(
            ROOT,
            Start::new(
                "recover",
                node_started::Kind::Step(Step {
                    verb: mix_events::v1::Verb::Recovering as i32,
                    subject: "interrupted request".to_string(),
                }),
            )
            .shielded(),
        )
        .expect("the root is open");
    let recovered = ctx
        .recover(journals, performer, &ctx.scope.shielded(), losing)
        .await;
    for (_, failure) in &recovered.failures {
        let _ = tree.warn(
            node,
            mix_core::diagnose::warning(
                Code::CleanupIncomplete,
                "could not finish an interrupted request",
                failure,
            ),
        );
    }
    for (_, failure) in &recovered.lost {
        let _ = tree.warn(
            node,
            mix_core::diagnose::warning(
                Code::CleanupIncomplete,
                "what an interrupted request changed could not be put back, so mix writes what it declares instead",
                failure,
            ),
        );
    }
    let _ = tree.finish(node, Ending::succeeded());
    recovered.failures.is_empty()
}
