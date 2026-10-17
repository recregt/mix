#[allow(clippy::disallowed_methods)]
pub fn enrolled() -> Vec<String> {
    members(&std::fs::read_to_string("/etc/group").unwrap_or_default())
}

fn members(group_file: &str) -> Vec<String> {
    group_file
        .lines()
        .filter_map(|line| {
            let mut fields = line.split(':');
            let name = fields.next()?;
            let members = fields.nth(2)?;
            (name == mix_events::MIX_USERS_GROUP).then_some(members)
        })
        .flat_map(|members| members.split(',').filter(|member| !member.is_empty()))
        .map(str::to_string)
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_members_of_the_group_are_listed_and_no_other_group() {
        let file = "root:x:0:\nmix-users:x:990:alice,bob\nstaff:x:50:carol\n";

        assert_eq!(members(file), ["alice", "bob"]);
    }

    #[test]
    fn an_empty_or_missing_group_has_no_members() {
        assert_eq!(members("mix-users:x:990:\n"), Vec::<String>::new());
        assert_eq!(members(""), Vec::<String>::new());
    }
}
