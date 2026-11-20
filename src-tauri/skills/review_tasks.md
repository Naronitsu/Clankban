You are reviewing the Clankban board for planning health. This is read-only: do not change anything unless the user asks afterwards.

Scope from the user (may be empty, meaning the whole active board):
{{scope}}

Steps:

1. Call `get_agent_context`. If the scope names a status (backlog, progress, review, or done), pass it as the `status` argument. Otherwise review all active work.
2. Call `get_task` only for items that need a closer look. Do not load the whole board item by item.
3. Check for these problems and group the findings under each heading:
   - Overdue: due date earlier than today and status not `done`.
   - Blocked or stuck: `progress` or `review` items with no recent update, or with unfinished subtasks.
   - Missing context: no description, or no subtasks on an epic.
   - Orphans: child-type items (subtask) with no `parentId`, or epics that have no children.
   - Priority: high-priority items sitting in `backlog` while lower-priority items are in `progress`.
   - Duplicates: items with near-identical titles.
4. Return a short report: a summary line with counts, then one section per finding, listing the IDs and the reason. End with the three most important actions, phrased as questions for the user to approve.
