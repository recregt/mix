use std::borrow::Cow;

use crate::declared::identity::{InvokingUser, MIX_USERS_GROUP};
use crate::effect::{Action, Fact, Failure, Query};
use crate::ops::removal::RemoveHome;
use crate::run::{StepSpec, Title};
use mix_events::v1::Verb;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Settings {
    pub user: InvokingUser,
}

struct RemoveMembership {
    user: InvokingUser,
}

impl StepSpec for RemoveMembership {
    fn key(&self) -> Cow<'static, str> {
        format!("remove-membership:{}", self.user.name).into()
    }

    fn title(&self) -> Title {
        Title::new(
            Verb::Removing,
            format!("{} from {MIX_USERS_GROUP}", self.user.name),
        )
    }

    fn queries(&self) -> Vec<Query> {
        vec![Query::Group(MIX_USERS_GROUP.to_string())]
    }

    fn actions(&self, facts: &[Fact]) -> Result<Vec<Action>, Failure> {
        let Fact::Group(Some(group)) = &facts[0] else {
            return Ok(Vec::new());
        };
        if group.members.contains(&self.user.name) {
            Ok(vec![Action::RemoveMember {
                group: MIX_USERS_GROUP.to_string(),
                user: self.user.name.clone(),
            }])
        } else {
            Ok(Vec::new())
        }
    }
}

pub fn steps(settings: &Settings) -> Vec<Box<dyn StepSpec>> {
    vec![
        Box::new(RemoveHome::new(settings.user.clone())),
        Box::new(RemoveMembership {
            user: settings.user.clone(),
        }),
    ]
}

#[cfg(test)]
mod tests;
