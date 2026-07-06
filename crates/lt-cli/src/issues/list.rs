use std::io::Write;

use anyhow::{Result, anyhow};
use chrono::Utc;
use lt_runtime::Runtime;
use lt_types::issues::{AssigneeFilter, IssueFilter, IssueSort, IssuesQuery, IssuesVariables};
use lt_types::viewer::ViewerQuery;

use super::IssueArgs;
use super::display::print_table;

/// Lower `args` into the typed variables shared by the cached and `--live`
/// reads, resolving `--assignee=me` against the persisted viewer identity
/// (`lt sync` populates it); `--live` shares this local resolution rather
/// than deferring to a server-side filter.
fn lower(args: &IssueArgs, runtime: &Runtime) -> Result<IssuesVariables> {
    let mut filter = args.literal_filter()?;
    if let Some(AssigneeFilter::Contains(value)) = &filter.assignee
        && value.eq_ignore_ascii_case("me")
    {
        let name = runtime
            .load::<ViewerQuery>(&())?
            .ok_or_else(|| anyhow!("`--assignee me` needs a synced viewer; run `lt sync` first"))?
            .user
            .name;
        filter.assignee = Some(AssigneeFilter::Exact(name));
    }
    let filter = (filter != IssueFilter::default()).then_some(filter);
    let sort = Some(IssueSort {
        field: args.sort.clone(),
        direction: args.sort_direction(),
    });
    Ok(IssuesVariables {
        filter,
        sort,
        first: Some(i32::try_from(args.limit.min(250)).unwrap_or(250)),
        after: None,
    })
}

pub fn run(out: &mut dyn Write, args: &IssueArgs, runtime: &Runtime) -> Result<()> {
    // --live: refresh the replica from upstream, then read the same cached path.
    if args.live {
        let vars = lower(args, runtime)?;
        runtime.refresh::<IssuesQuery>(vars.clone())?;
        let page = runtime.load::<IssuesQuery>(&vars)?;
        print_table(out, &page.nodes, "")?;
        if page.page_info.has_next_page {
            writeln!(out, "\n+more issues")?;
        }
        return Ok(());
    }

    match runtime.last_synced_at() {
        None => {
            writeln!(out, "No local cache yet -- run `lt sync` first.")?;
        }
        Some(ts) => {
            let vars = lower(args, runtime)?;
            let age_secs = Utc::now().signed_duration_since(ts).num_seconds();
            let page = runtime.load::<IssuesQuery>(&vars)?;
            let note = format!("(cached, age {age_secs}s)");
            print_table(out, &page.nodes, &note)?;
        }
    }

    Ok(())
}

#[cfg(all(test, feature = "sim"))]
mod tests {
    use super::*;

    fn args_with_limit(limit: u32) -> IssueArgs {
        IssueArgs {
            team: None,
            assignee: None,
            no_assignee: false,
            state: None,
            priority: None,
            created_after: None,
            created_before: None,
            updated_after: None,
            updated_before: None,
            sort: lt_runtime::query::SortField::Updated,
            asc: false,
            title: None,
            limit,
            live: false,
        }
    }

    #[test]
    fn cached_list_reads_seeded_issues_from_the_runtime() {
        let runtime = Runtime::new(
            lt_runtime::db::Database::memory().unwrap(),
            Box::new(lt_runtime::HttpTransportSource),
            Box::new(|_| {}),
        );
        runtime.seed_sim(0, 5).unwrap();

        let mut out = Vec::new();
        run(&mut out, &args_with_limit(50), &runtime).unwrap();

        let text = String::from_utf8(out).unwrap();
        assert!(text.contains("(cached, age"));
    }
}
