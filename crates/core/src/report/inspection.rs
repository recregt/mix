use mix_events::v1::finding::Kind;
use mix_events::v1::{
    Category as WireCategory, Finding as WireFinding, Generation, Gid, Ids, InspectionReport,
    Interrupted as WireInterrupted, Mode, NotAMember, Paths, Unfixable as WireUnfixable,
    Unreadable,
};

use crate::declared::targets::Category;
use crate::ops::health::{Drift, Finding, HealthReport, Unfixable};

pub fn report(report: &HealthReport) -> InspectionReport {
    InspectionReport {
        target: report.name.clone(),
        category: category(report.category) as i32,
        finding: report.finding.clone().map(finding),
        drift: report.drift.as_ref().map(drift),
        unfixable: report
            .finding
            .as_ref()
            .and_then(Finding::unfixable)
            .map_or(WireUnfixable::Unspecified, unfixable) as i32,
    }
}

pub fn unfixable(reason: Unfixable) -> WireUnfixable {
    match reason {
        Unfixable::NotADirectory => WireUnfixable::NotADirectory,
        Unfixable::MissingUser => WireUnfixable::MissingUser,
        Unfixable::MissingRuntime => WireUnfixable::MissingRuntime,
        Unfixable::Unrecovered => WireUnfixable::Unrecovered,
        Unfixable::InTheWay => WireUnfixable::InTheWay,
    }
}

pub fn drift(drift: &Drift) -> mix_events::v1::Drift {
    mix_events::v1::Drift {
        path: drift.path.clone(),
        hunks: drift
            .hunks
            .iter()
            .map(|hunk| mix_events::v1::Hunk {
                found_line: hunk.found_line,
                found: hunk.found.clone(),
                expected: hunk.expected.clone(),
            })
            .collect(),
    }
}

pub fn category(category: Category) -> WireCategory {
    match category {
        Category::Filesystem => WireCategory::Filesystem,
        Category::Identity => WireCategory::Identity,
        Category::Services => WireCategory::Services,
        Category::Configuration => WireCategory::Configuration,
    }
}

fn ids((actual_uid, actual_gid): (u32, u32), (expected_uid, expected_gid): (u32, u32)) -> Ids {
    Ids {
        actual_uid,
        actual_gid,
        expected_uid,
        expected_gid,
    }
}

pub fn finding(finding: Finding) -> WireFinding {
    let kind = match finding {
        Finding::Missing => Kind::Missing(Default::default()),
        Finding::Unreadable { kind } => Kind::Unreadable(Unreadable {
            kind: mix_events::io_kind::name(kind),
        }),
        Finding::NotADirectory => Kind::NotADirectory(Default::default()),
        Finding::Mode { actual, expected } => Kind::Mode(Mode { actual, expected }),
        Finding::Owner { actual, expected } => Kind::Owner(ids(actual, expected)),
        Finding::ContentDrift => Kind::ContentDrift(Default::default()),
        Finding::GroupMissing => Kind::GroupMissing(Default::default()),
        Finding::GroupGid { actual, expected } => Kind::GroupGid(Gid { actual, expected }),
        Finding::NotAMember { group } => Kind::NotAMember(NotAMember {
            group: group.to_string(),
        }),
        Finding::NoSuchUser => Kind::NoSuchUser(Default::default()),
        Finding::UserMissing => Kind::UserMissing(Default::default()),
        Finding::UserIds { actual, expected } => Kind::UserIds(ids(actual, expected)),
        Finding::UnitMissing => Kind::UnitMissing(Default::default()),
        Finding::UnitDrift => Kind::UnitDrift(Default::default()),
        Finding::UnitInactive => Kind::UnitInactive(Default::default()),
        Finding::RuntimeMissing => Kind::RuntimeMissing(Default::default()),
        Finding::RepositoryBroken => Kind::RepositoryBroken(Default::default()),
        Finding::RepositoryLocked => Kind::RepositoryLocked(Default::default()),
        Finding::Interrupted { requests, pending } => {
            Kind::Interrupted(WireInterrupted { requests, pending })
        }
        Finding::Leftovers { paths } => Kind::Leftovers(Paths { paths }),
        Finding::GenerationDangling { generation } => {
            Kind::GenerationDangling(Generation { generation })
        }
        Finding::InTheWay { paths } => Kind::InTheWay(Paths { paths }),
    };
    WireFinding { kind: Some(kind) }
}
