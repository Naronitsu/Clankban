//! Pure presentation logic: filtering, sorting, labels, and summary stats.
//! Kept free of UI types so it can be unit tested.

use chrono::{DateTime, NaiveDate, Utc};

use crate::store::Task;

#[derive(Debug, Clone, Default)]
pub struct Filter {
    pub search: String,
    pub priority: String,
    pub kind: String,
    pub status: String,
    pub sort: String,
}

#[derive(Debug, PartialEq)]
pub struct Stats {
    pub active: usize,
    pub high: usize,
    pub due_soon: usize,
    pub overdue: usize,
    pub done: usize,
    pub total: usize,
    pub recent: usize,
}

pub fn status_label(status: &str) -> String {
    match status {
        "backlog" => "Backlog".into(),
        "progress" => "In progress".into(),
        "review" => "In review".into(),
        "done" => "Done".into(),
        other => other.into(),
    }
}

pub fn kind_label(kind: &str) -> String {
    if kind == "bug" {
        "BUG FIX".into()
    } else {
        kind.to_uppercase()
    }
}

pub fn skill_label(name: &str) -> String {
    name.replace('_', " ")
}

pub fn due_label(due: &str) -> String {
    if due.is_empty() {
        return "No due date".into();
    }
    match NaiveDate::parse_from_str(due, "%Y-%m-%d") {
        Ok(date) => date.format("%b %-d").to_string(),
        Err(_) => due.into(),
    }
}

pub fn is_overdue(task: &Task, today: &str) -> bool {
    !task.due.is_empty() && task.due.as_str() < today && task.status != "done"
}

/// "just now", "12 min ago", "3 h ago", "2 d ago". Empty when the time is unknown.
pub fn relative_time(iso: &str, now: DateTime<Utc>) -> String {
    let Ok(when) = DateTime::parse_from_rfc3339(iso) else {
        return String::new();
    };
    let minutes = (now - when.with_timezone(&Utc)).num_minutes();
    if minutes < 1 {
        "just now".into()
    } else if minutes < 60 {
        format!("{minutes} min ago")
    } else if minutes < 60 * 24 {
        format!("{} h ago", minutes / 60)
    } else {
        format!("{} d ago", minutes / (60 * 24))
    }
}

/// Subtask checklist counts, including child tasks linked by parent id.
pub fn subtask_counts(task: &Task, all: &[Task]) -> (usize, usize) {
    let children: Vec<&Task> = all
        .iter()
        .filter(|t| t.parent_id.as_deref() == Some(task.id.as_str()))
        .collect();
    let done = task.subtasks.iter().filter(|s| s.done).count()
        + children.iter().filter(|c| c.status == "done").count();
    let total = task.subtasks.len() + children.len();
    (done, total)
}

pub fn matches(task: &Task, filter: &Filter) -> bool {
    let query = filter.search.trim().to_lowercase();
    if !query.is_empty() {
        let haystack = format!("{} {} {}", task.id, task.title, task.description).to_lowercase();
        if !haystack.contains(&query) {
            return false;
        }
    }
    (filter.priority.is_empty() || task.priority == filter.priority)
        && (filter.kind.is_empty() || task.kind == filter.kind)
        && (filter.status.is_empty() || task.status == filter.status)
}

/// Tasks that pass the filter, in the order chosen by the sort control.
pub fn visible<'a>(tasks: &'a [Task], filter: &Filter) -> Vec<&'a Task> {
    let mut list: Vec<&Task> = tasks.iter().filter(|t| matches(t, filter)).collect();
    match filter.sort.as_str() {
        "Due date" => list.sort_by(|a, b| {
            let (x, y) = (if a.due.is_empty() { "9999" } else { &a.due }, if b.due.is_empty() { "9999" } else { &b.due });
            x.cmp(y)
        }),
        "Newest" => list.sort_by(|a, b| b.created.cmp(&a.created)),
        _ => list.sort_by_key(|t| priority_rank(&t.priority)),
    }
    list
}

fn priority_rank(priority: &str) -> u8 {
    match priority {
        "high" => 0,
        "medium" => 1,
        "low" => 2,
        _ => 9,
    }
}

/// Recently changed tasks, newest change first.
pub fn recent(tasks: &[Task], filter: &Filter, limit: usize) -> Vec<Task> {
    let mut list: Vec<Task> = visible(tasks, filter)
        .into_iter()
        .filter(|t| DateTime::parse_from_rfc3339(&t.updated).is_ok())
        .cloned()
        .collect();
    list.sort_by(|a, b| b.updated.cmp(&a.updated));
    list.truncate(limit);
    list
}

pub fn stats(tasks: &[Task], today: &str, week: &str, now: DateTime<Utc>) -> Stats {
    let active: Vec<&Task> = tasks.iter().filter(|t| t.status != "done").collect();
    let due_soon: Vec<&&Task> = active.iter().filter(|t| !t.due.is_empty() && t.due.as_str() <= week).collect();
    let recent = tasks
        .iter()
        .filter(|t| {
            DateTime::parse_from_rfc3339(&t.updated)
                .map(|when| (now - when.with_timezone(&Utc)).num_hours() < 24)
                .unwrap_or(false)
        })
        .count();
    Stats {
        active: active.len(),
        high: active.iter().filter(|t| t.priority == "high").count(),
        due_soon: due_soon.len(),
        overdue: due_soon.iter().filter(|t| t.due.as_str() < today).count(),
        done: tasks.iter().filter(|t| t.status == "done").count(),
        total: tasks.len(),
        recent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::Subtask;
    use chrono::TimeZone;

    fn task(id: &str, status: &str, priority: &str, due: &str) -> Task {
        Task {
            id: id.into(),
            title: format!("Title {id}"),
            description: String::new(),
            kind: "task".into(),
            priority: priority.into(),
            status: status.into(),
            due: due.into(),
            created: "2026-11-01".into(),
            updated: "2026-11-20T10:00:00.000Z".into(),
            subtasks: vec![],
            parent_id: None,
        }
    }

    #[test]
    fn labels() {
        assert_eq!(status_label("progress"), "In progress");
        assert_eq!(kind_label("bug"), "BUG FIX");
        assert_eq!(kind_label("epic"), "EPIC");
        assert_eq!(due_label(""), "No due date");
        assert_eq!(due_label("2026-11-24"), "Nov 24");
        assert_eq!(skill_label("review_tasks"), "review tasks");
    }

    #[test]
    fn filter_and_sort_by_priority() {
        let tasks = vec![task("AT-1", "backlog", "low", ""), task("AT-2", "backlog", "high", "")];
        let filter = Filter { sort: "Priority".into(), ..Default::default() };
        let ids: Vec<&str> = visible(&tasks, &filter).iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, ["AT-2", "AT-1"]);
    }

    #[test]
    fn search_matches_id_and_status_filter_applies() {
        let tasks = vec![task("AT-1", "backlog", "low", ""), task("AT-2", "done", "low", "")];
        let filter = Filter { search: "at-2".into(), ..Default::default() };
        assert_eq!(visible(&tasks, &filter).len(), 1);
        let filter = Filter { status: "done".into(), ..Default::default() };
        assert_eq!(visible(&tasks, &filter)[0].id, "AT-2");
    }

    #[test]
    fn stats_count_overdue_and_done() {
        let tasks = vec![
            task("AT-1", "progress", "high", "2026-11-18"),
            task("AT-2", "backlog", "low", "2026-11-25"),
            task("AT-3", "done", "low", "2026-11-01"),
        ];
        let now = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
        let s = stats(&tasks, "2026-11-20", "2026-11-27", now);
        assert_eq!(s.active, 2);
        assert_eq!(s.high, 1);
        assert_eq!(s.due_soon, 2);
        assert_eq!(s.overdue, 1);
        assert_eq!(s.done, 1);
        assert!(is_overdue(&tasks[0], "2026-11-20"));
        assert!(!is_overdue(&tasks[2], "2026-11-20"));
    }

    #[test]
    fn relative_time_buckets() {
        let now = Utc.with_ymd_and_hms(2026, 11, 20, 12, 0, 0).unwrap();
        assert_eq!(relative_time("2026-11-20T11:59:30Z", now), "just now");
        assert_eq!(relative_time("2026-11-20T11:30:00Z", now), "30 min ago");
        assert_eq!(relative_time("2026-11-20T09:00:00Z", now), "3 h ago");
        assert_eq!(relative_time("2026-11-17T12:00:00Z", now), "3 d ago");
        assert_eq!(relative_time("not a date", now), "");
    }

    #[test]
    fn subtask_counts_include_children() {
        let mut parent = task("AT-1", "progress", "high", "");
        parent.subtasks = vec![Subtask { title: "a".into(), done: true }, Subtask { title: "b".into(), done: false }];
        let mut child = task("AT-2", "done", "low", "");
        child.parent_id = Some("AT-1".into());
        assert_eq!(subtask_counts(&parent, &[parent.clone(), child]), (2, 3));
    }
}
