You are importing work items into the Clankban board through the `clankban` MCP tools.

Source material from the user:
{{source}}

Follow this order. Do not skip step 2.

1. Read the board first. Call `get_agent_context` and note existing IDs, titles, and parents, so you do not create duplicates.
2. Build an inventory before writing anything. For each item, show: proposed title, type (epic, task, subtask, or bug), priority (high, medium, or low), status (use `backlog` unless the source says otherwise), the parent it belongs under, and any checklist items. Flag items that look like duplicates of existing board work. Then stop and ask the user to approve the inventory.
3. Only after explicit approval, create the items with `create_task`:
   - Create epics first, then tasks and subtasks, so each child can reference its parent.
   - Always set `parentId` explicitly for child items. Do not rely on description text.
   - Use `backlog`, never `todo`.
   - Put checklist items in `subtasks`. Put context and acceptance notes in `description`.
   - Keep the original source reference in the description, for example "Imported from <source name> item 12".
4. Report back with a table of the IDs the board assigned, and the parent of each child. Do not guess IDs before the tool returns them.

Never delete or overwrite existing items as part of an import. If an import would change an existing item, list it under "needs decision" and ask.
