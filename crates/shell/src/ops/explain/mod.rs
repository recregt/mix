//! `mix explain` asks which failure codes this `mix` defines: one by name, or all of them. The
//! daemon answers with the codes and the client words them, so the answer always comes from
//! the side that produces the codes.

use mix_events::v1::{Code, ExplainRequest, ExplainResult, node_finished};
use mix_events::{Ending, Fault};

use crate::Context;
use crate::request::{Concluded, Root};

pub(crate) async fn explain(
    _ctx: &Context,
    root: &mut Root,
    request: &ExplainRequest,
) -> Concluded {
    let codes = match request.code.map(Code::try_from) {
        None => defined(),
        Some(Ok(code)) if code != Code::Unspecified => vec![code],
        Some(Ok(_) | Err(_)) => {
            return root.conclude(Ending::from(Fault::failed(
                Code::Internal,
                "the request names a code this `mix` doesn't define",
                None,
            )));
        }
    };
    root.conclude(
        Ending::succeeded().with_result(node_finished::Result::Explain(ExplainResult {
            codes: codes.into_iter().map(|code| code as i32).collect(),
        })),
    )
}

fn defined() -> Vec<Code> {
    Code::DEFINED
        .iter()
        .filter_map(|value| Code::try_from(*value).ok())
        .filter(|code| *code != Code::Unspecified)
        .collect()
}

#[cfg(test)]
mod tests {
    use mix_events::v1::command::Request;

    use super::*;
    use crate::request::ran::{Ran, ran};

    async fn asked(code: Option<i32>) -> Ran {
        ran(
            crate::Session::new(mix_exec::Scope::root()),
            Request::Explain(ExplainRequest { code }),
        )
        .await
    }

    fn answered(ran: &Ran) -> Vec<i32> {
        match ran.result() {
            Some(node_finished::Result::Explain(result)) => result.codes.clone(),
            other => panic!("expected an explain result, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn a_named_code_is_answered_with_itself() {
        assert_eq!(
            answered(&asked(Some(Code::Network as i32)).await),
            vec![Code::Network as i32]
        );
    }

    #[tokio::test]
    async fn no_code_is_answered_with_every_code_but_the_unspecified_one() {
        let all = answered(&asked(None).await);

        let mut expected: Vec<i32> = Code::DEFINED
            .iter()
            .copied()
            .filter(|code| *code != Code::Unspecified as i32)
            .collect();
        expected.sort_unstable();
        let mut all = all;
        all.sort_unstable();
        assert_eq!(all, expected);
    }

    #[tokio::test]
    async fn a_code_this_mix_does_not_define_is_refused() {
        for code in [Code::Unspecified as i32, 6, i32::MAX] {
            assert_eq!(
                asked(Some(code)).await.code(),
                Some(Code::Internal),
                "{code}"
            );
        }
    }
}
