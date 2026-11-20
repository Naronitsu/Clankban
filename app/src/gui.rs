//! Native desktop window (Slint). The window is a thin layer over the same
//! `Store` the agent API and MCP bridge use, so every change is shared.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;
use std::time::Duration;

use chrono::{Duration as ChronoDuration, Local, SecondsFormat, Utc};
use serde_json::{json, Map, Value};
use slint::{ComponentHandle, ModelRc, SharedString, Timer, TimerMode, VecModel};

use crate::presenter::{self, Filter};
use crate::skills;
use crate::store::{ApiError, RunRequest, Store, Task};

slint::include_modules!();

const STATUSES: [(&str, &str); 4] = [
    ("backlog", "Backlog"),
    ("progress", "In progress"),
    ("review", "In review"),
    ("done", "Done"),
];

#[derive(Clone, Copy, PartialEq)]
enum Cmd {
    NewTask,
    ShowBoard,
    ShowList,
    ShowRecent,
    ShowAgent,
    FilterHigh,
    ClearFilters,
    CopyJson,
    Refresh,
    Shortcuts,
}

const COMMANDS: [(Cmd, &str, &str); 10] = [
    (Cmd::NewTask, "New task", "N"),
    (Cmd::ShowBoard, "Show board", ""),
    (Cmd::ShowList, "Show list", ""),
    (Cmd::ShowRecent, "Show recent changes", ""),
    (Cmd::ShowAgent, "Show agent runs", ""),
    (Cmd::FilterHigh, "Filter: high priority only", ""),
    (Cmd::ClearFilters, "Clear all filters", ""),
    (Cmd::CopyJson, "Copy agent JSON", ""),
    (Cmd::Refresh, "Refresh board", "F5"),
    (Cmd::Shortcuts, "Keyboard shortcuts", "?"),
];

struct State {
    store: Arc<Store>,
    tasks: Vec<Task>,
    runs: Vec<RunRequest>,
    filter: Filter,
    selected: Option<String>,
    editing: Option<String>,
    skill_values: HashMap<String, String>,
    palette_cmds: Vec<Cmd>,
    signature: String,
}

struct Ctx {
    today: String,
    now: chrono::DateTime<Utc>,
}

fn model<T: Clone + 'static>(items: Vec<T>) -> ModelRc<T> {
    ModelRc::new(VecModel::from(items))
}

fn describe(error: &ApiError) -> String {
    match error {
        ApiError::BadRequest(message) | ApiError::Conflict(message) | ApiError::Internal(message) => message.clone(),
        ApiError::NotFound => "Not found".into(),
    }
}

fn task_item(t: &Task, all: &[Task], ctx: &Ctx) -> TaskItem {
    let (done, total) = presenter::subtask_counts(t, all);
    TaskItem {
        id: t.id.as_str().into(),
        title: t.title.as_str().into(),
        description: t.description.as_str().into(),
        kind: t.kind.as_str().into(),
        kind_label: presenter::kind_label(&t.kind).into(),
        priority: t.priority.as_str().into(),
        status: t.status.as_str().into(),
        status_label: presenter::status_label(&t.status).into(),
        due: t.due.as_str().into(),
        due_label: presenter::due_label(&t.due).into(),
        overdue: presenter::is_overdue(t, &ctx.today),
        subtasks_done: done as i32,
        subtasks_total: total as i32,
        updated_label: presenter::relative_time(&t.updated, ctx.now).into(),
        parent_id: t.parent_id.clone().unwrap_or_default().into(),
    }
}

fn run_item(run: &RunRequest, ctx: &Ctx) -> RunItem {
    RunItem {
        id: run.id.to_string().into(),
        skill_label: presenter::skill_label(&run.skill).into(),
        status: run.status.as_str().into(),
        status_label: presenter::status_label(&run.status).into(),
        created_label: presenter::relative_time(&run.created, ctx.now).into(),
        result: run.result.as_str().into(),
        pending: run.status == "pending",
    }
}

fn filter_count(filter: &Filter) -> i32 {
    [&filter.priority, &filter.kind, &filter.status]
        .iter()
        .filter(|value| !value.is_empty())
        .count() as i32
}

/// Rebuilds every model from the store. Skips the work when nothing changed,
/// unless `force` is set, so the five-second poll does not reset scroll positions.
fn refresh(ui: &AppWindow, st: &mut State, force: bool) {
    match (st.store.list(), st.store.list_runs(None)) {
        (Ok(tasks), Ok(runs)) => {
            st.tasks = tasks;
            st.runs = runs;
        }
        (Err(e), _) | (_, Err(e)) => {
            ui.set_notice(format!("Could not read the board: {}", describe(&e)).into());
            return;
        }
    }
    let signature = serde_json::to_string(&(&st.tasks, &st.runs)).unwrap_or_default();
    if !force && signature == st.signature {
        return;
    }
    st.signature = signature;
    render(ui, st);
}

fn render(ui: &AppWindow, st: &mut State) {
    let now = Utc::now();
    let today = Local::now().format("%Y-%m-%d").to_string();
    let week = (Local::now() + ChronoDuration::days(7)).format("%Y-%m-%d").to_string();
    let ctx = Ctx { today: today.clone(), now };

    let s = presenter::stats(&st.tasks, &today, &week, now);
    ui.set_stat_active(s.active.to_string().into());
    ui.set_stat_active_sub(format!("{} high priority", s.high).into());
    ui.set_stat_due(s.due_soon.to_string().into());
    ui.set_stat_due_sub(
        if s.overdue > 0 { format!("{} overdue", s.overdue) } else { "Nothing overdue".into() }.into(),
    );
    ui.set_stat_due_warn(s.overdue > 0);
    ui.set_stat_done(s.done.to_string().into());
    ui.set_stat_done_sub(
        if s.total > 0 { format!("{}% of {}", s.done * 100 / s.total, s.total) } else { "—".into() }.into(),
    );
    ui.set_stat_recent(s.recent.to_string().into());
    ui.set_stat_recent_sub(if s.recent > 0 { "Last 24 hours" } else { "No changes today" }.into());
    ui.set_nav_board_count(s.total.to_string().into());
    ui.set_nav_recent_count(s.recent.to_string().into());

    let pending = st.runs.iter().filter(|r| r.status == "pending").count();
    let active = st.runs.iter().filter(|r| r.status == "pending" || r.status == "running").count();
    ui.set_nav_agent_count(pending.to_string().into());
    ui.set_run_count_label(format!("{active} active").into());
    ui.set_filter_count(filter_count(&st.filter));

    let visible = presenter::visible(&st.tasks, &st.filter);
    let columns: Vec<ColumnItem> = STATUSES
        .iter()
        .map(|(status, label)| {
            let items: Vec<TaskItem> = visible
                .iter()
                .filter(|t| t.status == *status)
                .map(|t| task_item(t, &st.tasks, &ctx))
                .collect();
            ColumnItem {
                status: (*status).into(),
                label: (*label).into(),
                count: items.len() as i32,
                tasks: model(items),
            }
        })
        .collect();
    ui.set_columns(model(columns));

    ui.set_list_tasks(model(visible.iter().map(|t| task_item(t, &st.tasks, &ctx)).collect()));
    let recent: Vec<TaskItem> = presenter::recent(&st.tasks, &st.filter, 50)
        .iter()
        .map(|t| task_item(t, &st.tasks, &ctx))
        .collect();
    ui.set_recent_tasks(model(recent));

    ui.set_runs(model(st.runs.iter().rev().map(|r| run_item(r, &ctx)).collect()));
    let skill_items: Vec<SkillItem> = skills::SKILLS
        .iter()
        .map(|skill| {
            let arg = skill.args.first();
            SkillItem {
                name: skill.name.into(),
                label: presenter::skill_label(skill.name).into(),
                description: skill.description.into(),
                arg_name: arg.map(|a| a.name).unwrap_or("").into(),
                arg_hint: arg.map(|a| a.description).unwrap_or("").into(),
                arg_required: arg.map(|a| a.required).unwrap_or(false),
                value: st.skill_values.get(skill.name).cloned().unwrap_or_default().into(),
            }
        })
        .collect();
    ui.set_skills(model(skill_items));

    render_drawer(ui, st, &ctx);
    if ui.get_palette_open() {
        let (items, cmds) = palette_items(&ui.get_palette_query().to_string());
        st.palette_cmds = cmds;
        ui.set_palette_items(model(items));
    }
}

fn render_drawer(ui: &AppWindow, st: &mut State, ctx: &Ctx) {
    let Some(id) = st.selected.clone() else {
        ui.set_drawer_open(false);
        return;
    };
    let Some(task) = st.tasks.iter().find(|t| t.id == id).cloned() else {
        st.selected = None;
        ui.set_drawer_open(false);
        return;
    };
    let mut rows: Vec<SubtaskItem> = task
        .subtasks
        .iter()
        .enumerate()
        .map(|(index, sub)| SubtaskItem {
            index: index as i32,
            title: sub.title.as_str().into(),
            done: sub.done,
            child: false,
            child_status: SharedString::default(),
        })
        .collect();
    for child in st.tasks.iter().filter(|t| t.parent_id.as_deref() == Some(task.id.as_str())) {
        rows.push(SubtaskItem {
            index: -1,
            title: format!("{} · {}", child.id, child.title).into(),
            done: child.status == "done",
            child: true,
            child_status: presenter::status_label(&child.status).into(),
        });
    }
    ui.set_drawer_task(task_item(&task, &st.tasks, ctx));
    ui.set_drawer_subtasks(model(rows));
    ui.set_drawer_status_label(presenter::status_label(&task.status).into());
    ui.set_drawer_updated(presenter::relative_time(&task.updated, ctx.now).into());
    ui.set_drawer_open(true);
}

fn open_new_form(ui: &AppWindow, st: &mut State, status: &str) {
    st.editing = None;
    ui.set_form_heading("Create a task".into());
    ui.set_form_title("".into());
    ui.set_form_description("".into());
    ui.set_form_type("task".into());
    ui.set_form_priority("medium".into());
    ui.set_form_status(if status.is_empty() { "backlog" } else { status }.into());
    ui.set_form_due("".into());
    ui.set_form_open(true);
}

fn open_edit_form(ui: &AppWindow, st: &mut State, task: &Task) {
    st.editing = Some(task.id.clone());
    ui.set_form_heading("Edit work item".into());
    ui.set_form_title(task.title.as_str().into());
    ui.set_form_description(task.description.as_str().into());
    ui.set_form_type(task.kind.as_str().into());
    ui.set_form_priority(task.priority.as_str().into());
    ui.set_form_status(task.status.as_str().into());
    ui.set_form_due(task.due.as_str().into());
    ui.set_form_open(true);
}

fn save_form(ui: &AppWindow, st: &mut State) {
    let title = ui.get_form_title().trim().to_string();
    if title.is_empty() {
        ui.set_notice("A title is required.".into());
        return;
    }
    let mut body = Map::new();
    body.insert("title".into(), json!(title));
    body.insert("description".into(), json!(ui.get_form_description().to_string()));
    body.insert("type".into(), json!(ui.get_form_type().to_string()));
    body.insert("priority".into(), json!(ui.get_form_priority().to_string()));
    body.insert("status".into(), json!(ui.get_form_status().to_string()));
    body.insert("due".into(), json!(ui.get_form_due().trim().to_string()));

    let result = match st.editing.clone() {
        Some(id) => st.store.update(&id, &body).map(|_| ()),
        None => st.store.create(&body).map(|_| ()),
    };
    match result {
        Ok(()) => {
            ui.set_form_open(false);
            ui.set_notice("".into());
            refresh(ui, st, true);
        }
        Err(e) => ui.set_notice(format!("Could not save: {}", describe(&e)).into()),
    }
}

fn toggle_subtask(ui: &AppWindow, st: &mut State, index: usize, checked: bool) {
    let Some(id) = st.selected.clone() else { return };
    let Some(task) = st.tasks.iter().find(|t| t.id == id).cloned() else { return };
    let mut subtasks = task.subtasks.clone();
    let Some(sub) = subtasks.get_mut(index) else { return };
    sub.done = checked;
    let mut body = Map::new();
    body.insert("subtasks".into(), serde_json::to_value(&subtasks).unwrap_or(Value::Null));
    if let Err(e) = st.store.update(&id, &body) {
        ui.set_notice(format!("Could not save the checklist: {}", describe(&e)).into());
    }
    refresh(ui, st, true);
}

fn delete_selected(ui: &AppWindow, st: &mut State) {
    let Some(id) = st.selected.take() else { return };
    match st.store.delete(&id) {
        Ok(()) => ui.set_notice("".into()),
        Err(e) => ui.set_notice(format!("Could not delete: {}", describe(&e)).into()),
    }
    ui.set_drawer_open(false);
    refresh(ui, st, true);
}

fn cancel_run(ui: &AppWindow, st: &mut State, id: &str) {
    let Ok(id) = id.parse::<i64>() else { return };
    if let Err(e) = st.store.update_run(id, "cancelled", Some("Cancelled from the board")) {
        ui.set_notice(format!("Could not cancel: {}", describe(&e)).into());
    }
    refresh(ui, st, true);
}

fn send_skill(ui: &AppWindow, st: &mut State, index: usize) {
    let Some(skill) = skills::SKILLS.get(index) else { return };
    let value = st.skill_values.get(skill.name).map(|v| v.trim().to_string()).unwrap_or_default();
    let mut args = Map::new();
    if let Some(arg) = skill.args.first() {
        if !value.is_empty() {
            args.insert(arg.name.into(), Value::String(value));
        }
    }
    let args = Value::Object(args);
    if let Some(missing) = skills::missing_required(skill, &args) {
        ui.set_notice(format!("{missing} is required.").into());
        return;
    }
    match st.store.create_run(skill.name, &args) {
        Ok(_) => {
            st.skill_values.remove(skill.name);
            ui.set_notice("Sent to the agent. It will pick it up the next time it checks.".into());
        }
        Err(e) => ui.set_notice(format!("Could not send: {}", describe(&e)).into()),
    }
    refresh(ui, st, true);
}

fn json_payload(st: &State) -> String {
    let tasks: Vec<Value> = st
        .tasks
        .iter()
        .map(|t| {
            json!({
                "id": t.id,
                "title": t.title,
                "description": t.description,
                "type": t.kind,
                "priority": t.priority,
                "status": t.status,
                "due": t.due,
                "created": t.created,
                "subtasks": t.subtasks,
            })
        })
        .collect();
    serde_json::to_string_pretty(&json!({
        "workspace": "atlas",
        "generatedAt": Utc::now().to_rfc3339_opts(SecondsFormat::Millis, true),
        "schemaVersion": "1.0",
        "tasks": tasks,
    }))
    .unwrap_or_default()
}

fn palette_items(query: &str) -> (Vec<PaletteItem>, Vec<Cmd>) {
    let needle = query.trim().to_lowercase();
    COMMANDS
        .iter()
        .filter(|(_, label, _)| needle.is_empty() || label.to_lowercase().contains(&needle))
        .map(|(cmd, label, hint)| {
            (
                PaletteItem { label: (*label).into(), hint: (*hint).into() },
                *cmd,
            )
        })
        .unzip()
}

fn run_command(ui: &AppWindow, st: &mut State, cmd: Cmd) {
    match cmd {
        Cmd::NewTask => open_new_form(ui, st, ""),
        Cmd::ShowBoard => ui.set_view("board".into()),
        Cmd::ShowList => ui.set_view("list".into()),
        Cmd::ShowRecent => ui.set_view("recent".into()),
        Cmd::ShowAgent => ui.set_view("agent".into()),
        Cmd::FilterHigh => {
            st.filter.priority = "high".into();
            ui.set_filter_priority("high".into());
        }
        Cmd::ClearFilters => clear_filters(ui, st),
        Cmd::CopyJson => {
            ui.set_json_text(json_payload(st).into());
            ui.set_json_open(true);
        }
        Cmd::Refresh => {}
        Cmd::Shortcuts => ui.set_help_open(true),
    }
    refresh(ui, st, true);
}

fn clear_filters(ui: &AppWindow, st: &mut State) {
    st.filter.priority.clear();
    st.filter.kind.clear();
    st.filter.status.clear();
    ui.set_filter_priority("".into());
    ui.set_filter_type("".into());
    ui.set_filter_status("".into());
}

pub fn run(store: Arc<Store>, api: Result<String, String>) -> Result<(), slint::PlatformError> {
    let ui = AppWindow::new()?;
    ui.set_sort("Priority".into());
    ui.set_version_text(env!("CARGO_PKG_VERSION").into());
    match &api {
        Ok(url) => {
            ui.set_sync_text("Board ready".into());
            ui.set_sync_detail(format!("Agents connect at {url}").into());
            ui.set_sync_ok(true);
        }
        Err(message) => {
            ui.set_sync_text("Agent API not running".into());
            ui.set_sync_detail(message.as_str().into());
            ui.set_sync_ok(false);
        }
    }

    let st = Rc::new(RefCell::new(State {
        store,
        tasks: Vec::new(),
        runs: Vec::new(),
        filter: Filter { sort: "Priority".into(), ..Default::default() },
        selected: None,
        editing: None,
        skill_values: HashMap::new(),
        palette_cmds: Vec::new(),
        signature: String::new(),
    }));
    refresh(&ui, &mut st.borrow_mut(), true);
    register(&ui, &st);

    // Pick up changes made by agents through the API or MCP bridge.
    let timer = Timer::default();
    let (u, s) = (ui.as_weak(), st.clone());
    timer.start(TimerMode::Repeated, Duration::from_secs(2), move || {
        if let Some(ui) = u.upgrade() {
            refresh(&ui, &mut s.borrow_mut(), false);
        }
    });

    let result = ui.run();
    drop(timer);
    result
}

fn register(ui: &AppWindow, st: &Rc<RefCell<State>>) {
    macro_rules! on {
        ($setter:ident, |$ui:ident, $state:ident $(, $arg:ident)*| $body:block) => {{
            let (u, s) = (ui.as_weak(), st.clone());
            ui.$setter(move |$($arg),*| {
                if let Some($ui) = u.upgrade() {
                    let mut guard = s.borrow_mut();
                    let $state: &mut State = &mut guard;
                    $body
                }
            });
        }};
    }

    on!(on_set_view, |ui, st, view| {
        ui.set_view(view);
        refresh(&ui, st, true);
    });
    on!(on_search_changed, |ui, st, text| {
        st.filter.search = text.to_string();
        refresh(&ui, st, true);
    });
    on!(on_sort_changed, |ui, st, sort| {
        st.filter.sort = sort.to_string();
        refresh(&ui, st, true);
    });
    on!(on_filter_changed, |ui, st, priority, kind, status| {
        st.filter.priority = priority.to_string();
        st.filter.kind = kind.to_string();
        st.filter.status = status.to_string();
        refresh(&ui, st, true);
    });
    on!(on_filter_cleared, |ui, st| {
        clear_filters(&ui, st);
        refresh(&ui, st, true);
    });
    on!(on_refresh, |ui, st| {
        refresh(&ui, st, true);
    });
    on!(on_show_only_status, |ui, st, status| {
        st.filter.status = status.to_string();
        ui.set_filter_status(status);
        refresh(&ui, st, true);
    });
    on!(on_new_task, |ui, st, status| {
        open_new_form(&ui, st, status.as_str());
        refresh(&ui, st, true);
    });
    on!(on_open_task, |ui, st, id| {
        st.selected = Some(id.to_string());
        refresh(&ui, st, true);
    });
    on!(on_edit_task, |ui, st| {
        let task = st.selected.clone().and_then(|id| st.tasks.iter().find(|t| t.id == id).cloned());
        if let Some(task) = task {
            ui.set_drawer_open(false);
            open_edit_form(&ui, st, &task);
        }
    });
    on!(on_toggle_subtask, |ui, st, index, checked| {
        if index >= 0 {
            toggle_subtask(&ui, st, index as usize, checked);
        }
    });
    on!(on_save_form, |ui, st| {
        save_form(&ui, st);
    });
    on!(on_palette_query_changed, |ui, st, query| {
        let (items, cmds) = palette_items(query.as_str());
        st.palette_cmds = cmds;
        ui.set_palette_items(model(items));
    });
    on!(on_palette_run, |ui, st, index| {
        ui.set_palette_open(false);
        if let Some(cmd) = st.palette_cmds.get(index as usize).copied() {
            run_command(&ui, st, cmd);
        }
    });
    on!(on_show_json, |ui, st| {
        ui.set_json_text(json_payload(st).into());
        ui.set_json_open(true);
    });
    on!(on_copy_json, |ui, st| {
        let text = json_payload(st);
        match arboard::Clipboard::new().and_then(|mut board| board.set_text(text)) {
            Ok(()) => ui.set_notice("Copied the agent JSON.".into()),
            Err(e) => ui.set_notice(format!("Could not copy: {e}").into()),
        }
    });
    on!(on_skill_edited, |ui, st, index, text| {
        if let Some(skill) = skills::SKILLS.get(index as usize) {
            st.skill_values.insert(skill.name.to_string(), text.to_string());
        }
        let _ = ui;
    });
    on!(on_send_run, |ui, st, index| {
        if index >= 0 {
            send_skill(&ui, st, index as usize);
        }
    });
    on!(on_cancel_run, |ui, st, id| {
        ui.set_confirm_text("Cancel this request? The agent will not run it.".into());
        ui.set_confirm_action(format!("cancel-run:{id}").into());
        ui.set_confirm_open(true);
        let _ = st;
    });
    on!(on_confirm_accepted, |ui, st, action| {
        let action = action.to_string();
        if action == "delete" {
            delete_selected(&ui, st);
        } else if let Some(id) = action.strip_prefix("cancel-run:") {
            cancel_run(&ui, st, id);
        } else if action == "install-update" {
            crate::updater::install(&ui);
        }
    });
    on!(on_check_updates, |ui, st| {
        crate::updater::check(&ui);
        let _ = st;
    });
    on!(on_install_update, |ui, st| {
        ui.set_confirm_text("Install the update now? Clankban will close and restart.".into());
        ui.set_confirm_action("install-update".into());
        ui.set_confirm_open(true);
        let _ = st;
    });
}
