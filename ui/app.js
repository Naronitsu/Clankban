const SEED = [
  {id:'AT-101',title:'Define project scope & success metrics',description:'Align on the first release boundary and document how we will know it is working.',type:'epic',priority:'high',status:'backlog',due:'2026-11-24',created:'2026-11-17',subtasks:[{title:'Draft the one-page brief',done:true},{title:'Review with stakeholders',done:false}]},
  {id:'AT-104',title:'Design the agent context panel',description:'Give local agents a clean, inspectable view of board state, task relationships, and recent changes.',type:'task',priority:'high',status:'progress',due:'2026-11-22',created:'2026-11-18',subtasks:[{title:'Map the JSON shape',done:true},{title:'Add copy action',done:false},{title:'Document the read contract',done:false}]},
  {id:'AT-107',title:'Build drag-and-drop board view',description:'Make work feel tangible: move cards between statuses and preserve the state locally.',type:'task',priority:'medium',status:'progress',due:'2026-11-28',created:'2026-11-18',subtasks:[]},
  {id:'AT-109',title:'Fix empty state on new workspace',description:'New workspaces should explain the next step instead of showing a blank canvas.',type:'bug',priority:'high',status:'progress',due:'2026-11-21',created:'2026-11-19',subtasks:[]},
  {id:'AT-112',title:'Write local API adapter notes',description:'Capture the endpoints an agent bridge will need to read and mutate the board.',type:'subtask',priority:'low',status:'review',due:'2026-11-30',created:'2026-11-19',subtasks:[]},
  {id:'AT-115',title:'Choose a stable task identifier format',description:'Document how identifiers are generated and how they should be referenced in agent output.',type:'task',priority:'medium',status:'review',due:'2026-12-01',created:'2026-11-19',subtasks:[]},
  {id:'AT-098',title:'Set up local persistence',description:'Keep the board useful between sessions with browser-local storage.',type:'task',priority:'medium',status:'done',due:'2026-11-18',created:'2026-11-14',subtasks:[]},
  {id:'AT-093',title:'Create visual language tokens',description:'Establish color, type, spacing, and status patterns for the workspace.',type:'task',priority:'low',status:'done',due:'2026-11-16',created:'2026-11-12',subtasks:[]}
];

const CACHE_KEY = 'clankban-tasks';
const columns = [['backlog','Backlog'],['progress','In progress'],['review','In review'],['done','Done']];
const statusLabel = Object.fromEntries(columns);
const priorityRank = {high:0, medium:1, low:2};
const DAY_MS = 864e5;
const API_BASE = window.clankban?.apiUrl || '';
const remoteMode = Boolean(API_BASE);

function readCache() {
  try {
    const value = JSON.parse(localStorage.getItem(CACHE_KEY) || 'null');
    return Array.isArray(value) ? value : null;
  } catch { return null; }
}
function persist() {
  try { localStorage.setItem(CACHE_KEY, JSON.stringify(tasks)); } catch { /* cache is optional */ }
}

let tasks = readCache() || (remoteMode ? [] : SEED);
let view = 'board';
let filters = { priority: '', type: '', status: '' };
let activeTaskId = null;
let editingId = null;
let lastSync = null;
let syncOk = !remoteMode;

const $ = selector => document.querySelector(selector);
const enc = encodeURIComponent;

async function apiRequest(path, options = {}) {
  const response = await fetch(`${API_BASE}${path}`, {
    ...options,
    headers: { 'content-type': 'application/json', ...(options.headers || {}) },
  });
  if (!response.ok) throw new Error(await response.text());
  return response.json();
}

async function loadRemoteTasks() {
  if (!remoteMode) return;
  try {
    const result = await apiRequest('/api/tasks');
    tasks = result.tasks;
    syncOk = true;
    lastSync = new Date();
    persist();
  } catch (error) {
    syncOk = false;
    console.warn('Clankban API unavailable during refresh.', error);
  }
  render();
}

function escapeHtml(value = '') {
  return String(value).replace(/[&<>'"]/g, c => ({ '&':'&amp;', '<':'&lt;', '>':'&gt;', "'":'&#39;', '"':'&quot;' }[c]));
}
function todayStr() { return new Date().toLocaleDateString('en-CA'); }
function addDays(days) {
  const date = new Date();
  date.setDate(date.getDate() + days);
  return date.toLocaleDateString('en-CA');
}
function formatDate(value) {
  if (!value) return 'No due date';
  return new Date(`${value}T00:00:00`).toLocaleDateString(undefined, { month: 'short', day: 'numeric' });
}
function formatDateTime(iso) {
  const time = Date.parse(iso || '');
  if (!Number.isFinite(time)) return 'No change recorded yet';
  return new Date(time).toLocaleString(undefined, { month: 'short', day: 'numeric', hour: 'numeric', minute: '2-digit' });
}
function relativeTime(iso) {
  const time = Date.parse(iso || '');
  if (!Number.isFinite(time)) return '';
  const minutes = Math.round((Date.now() - time) / 60000);
  if (minutes < 1) return 'just now';
  if (minutes < 60) return `${minutes} min ago`;
  const hours = Math.round(minutes / 60);
  if (hours < 24) return `${hours} h ago`;
  return `${Math.round(hours / 24)} d ago`;
}
function typeLabel(t) { return t.type === 'bug' ? 'BUG FIX' : t.type.toUpperCase(); }
function isRecent(t) {
  const time = Date.parse(t.updated || '');
  return Number.isFinite(time) && Date.now() - time < DAY_MS;
}
function nextLocalId() {
  return `AT-${Math.max(0, ...tasks.map(t => Number(String(t.id).slice(3)) || 0)) + 1}`;
}

// ---- filtering and sorting -------------------------------------------------

function matches(t) {
  const query = $('#searchInput').value.trim().toLowerCase();
  if (query && !`${t.id} ${t.title} ${t.description || ''}`.toLowerCase().includes(query)) return false;
  if (filters.priority && t.priority !== filters.priority) return false;
  if (filters.type && t.type !== filters.type) return false;
  if (filters.status && t.status !== filters.status) return false;
  return true;
}
function visibleTasks() {
  const sort = $('#sortSelect').value;
  return tasks.filter(matches).sort((a, b) => {
    if (sort === 'priority') return (priorityRank[a.priority] ?? 9) - (priorityRank[b.priority] ?? 9);
    if (sort === 'due') return (a.due || '9999').localeCompare(b.due || '9999');
    return (b.created || '').localeCompare(a.created || '');
  });
}
function activeFilterCount() { return Object.values(filters).filter(Boolean).length; }
function syncFilterUI() {
  $('#filterPriority').value = filters.priority;
  $('#filterType').value = filters.type;
  $('#filterStatus').value = filters.status;
}
function resetFilters() {
  filters = { priority: '', type: '', status: '' };
  syncFilterUI();
}

// ---- rendering -------------------------------------------------------------

function renderStats() {
  const active = tasks.filter(t => t.status !== 'done');
  const high = active.filter(t => t.priority === 'high').length;
  const today = todayStr();
  const week = addDays(7);
  const dueSoon = active.filter(t => t.due && t.due <= week);
  const overdue = dueSoon.filter(t => t.due < today).length;
  const done = tasks.filter(t => t.status === 'done').length;
  const recent = tasks.filter(isRecent).length;

  $('#activeCount').textContent = active.length;
  $('#activeSub').textContent = `${high} high priority`;
  $('#dueCount').textContent = dueSoon.length;
  $('#dueSub').textContent = overdue ? `${overdue} overdue` : 'Nothing overdue';
  $('#dueSub').classList.toggle('warning', overdue > 0);
  $('#completedCount').textContent = done;
  $('#completedSub').textContent = tasks.length ? `${Math.round((done / tasks.length) * 100)}% of ${tasks.length}` : '—';
  $('#recentCount').textContent = recent;
  $('#recentSub').textContent = recent ? 'Last 24 hours' : 'No changes today';
  $('#navBoardCount').textContent = tasks.length;
  $('#navRecentCount').textContent = recent;
}

function card(t) {
  const subtasks = t.subtasks || [];
  const children = tasks.filter(x => x.parentId === t.id);
  const done = subtasks.filter(s => s.done).length + children.filter(x => x.status === 'done').length;
  const total = subtasks.length + children.length;
  return `<article class="task-card" data-id="${escapeHtml(t.id)}"><div class="card-top"><span class="type-badge ${t.type}">${typeLabel(t)}</span><span class="priority ${t.priority}" title="${t.priority} priority"></span></div><h3>${escapeHtml(t.title)}</h3><p>${escapeHtml(t.description)}</p><div class="card-bottom"><span class="card-id">${escapeHtml(t.id)}</span><div class="card-meta">${total ? `<span class="subtask-chip">□ ${done}/${total}</span>` : ''}<span class="${t.due && t.status !== 'done' ? 'due-soon' : ''}">${formatDate(t.due)}</span></div></div></article>`;
}

function renderBoard(list) {
  $('#boardGrid').innerHTML = columns.map(([status, label]) => {
    const items = list.filter(t => t.status === status);
    return `<div class="board-column"><div class="column-heading"><span>${label}</span><span class="count">${items.length}</span><button class="column-menu" data-status="${status}" title="Column actions" aria-label="Actions for ${label}">•••</button></div><div class="task-list">${items.map(card).join('')}</div><button class="column-add" data-status="${status}">＋ Add to ${label}</button></div>`;
  }).join('');
}

function renderList(list) {
  $('#listView').innerHTML = list.length
    ? `<table class="list-table"><thead><tr><th>ID</th><th>Title</th><th>Type</th><th>Priority</th><th>Status</th><th>Due</th></tr></thead><tbody>${list.map(t => `<tr data-id="${escapeHtml(t.id)}"><td>${escapeHtml(t.id)}</td><td class="${t.status === 'done' ? 'done-title' : ''}">${escapeHtml(t.title)}</td><td>${typeLabel(t)}</td><td>${t.priority}</td><td>${statusLabel[t.status] || t.status}</td><td>${formatDate(t.due)}</td></tr>`).join('')}</tbody></table>`
    : '<div class="empty-note">No tasks match these filters.</div>';
}

function renderRecent(list) {
  const recent = list.filter(t => Date.parse(t.updated || '')).sort((a, b) => Date.parse(b.updated) - Date.parse(a.updated)).slice(0, 50);
  $('#recentView').innerHTML = recent.length
    ? `<ul class="recent-list">${recent.map(t => `<li data-id="${escapeHtml(t.id)}"><span><strong>${escapeHtml(t.id)}</strong> ${escapeHtml(t.title)}</span><small>${statusLabel[t.status] || t.status} · ${relativeTime(t.updated)}</small></li>`).join('')}</ul>`
    : '<div class="empty-note">No changes recorded yet. Edits made from the board or an agent appear here.</div>';
}

function updateSyncStatus() {
  const dot = $('#syncDot');
  const status = $('#syncStatus');
  const detail = $('#syncDetail');
  if (!remoteMode) {
    status.textContent = 'Browser mode';
    detail.textContent = 'Saved in this browser only';
    dot.classList.remove('offline');
  } else if (syncOk) {
    status.textContent = 'Connected to local API';
    detail.textContent = lastSync ? `Synced ${lastSync.toLocaleTimeString([], { hour: 'numeric', minute: '2-digit' })}` : 'Waiting for first sync';
    dot.classList.remove('offline');
  } else {
    status.textContent = 'API offline';
    detail.textContent = 'Showing last saved data';
    dot.classList.add('offline');
  }
}

function render() {
  const list = visibleTasks();
  renderStats();
  document.querySelectorAll('[data-view]').forEach(button => button.classList.toggle('active', button.dataset.view === view));
  $('#boardGrid').hidden = view !== 'board';
  $('#listView').hidden = view !== 'list';
  $('#recentView').hidden = view !== 'recent';
  $('#viewTitle').textContent = { board: 'Product board', list: 'List', recent: 'Recent changes' }[view];
  if (view === 'board') renderBoard(list);
  if (view === 'list') renderList(list);
  if (view === 'recent') renderRecent(list);

  const count = activeFilterCount();
  $('#filterBadge').hidden = count === 0;
  $('#filterBadge').textContent = count;
  $('#filterButton').classList.toggle('active', count > 0);
  updateSyncStatus();
}

function setView(next) {
  view = next;
  render();
}

// ---- drawer ----------------------------------------------------------------

function openDrawer(id) {
  const t = tasks.find(x => x.id === id);
  if (!t) return;
  activeTaskId = id;
  const subtasks = t.subtasks || [];
  const children = tasks.filter(x => x.parentId === id);
  const embedded = subtasks.map((s, i) => ({ title: s.title, done: s.done, embedded: i }));
  const related = children.map(x => ({ title: `${x.id} · ${x.title}`, done: x.status === 'done', child: x.id }));
  const all = [...embedded, ...related];

  $('#drawerType').className = `type-badge ${t.type}`;
  $('#drawerType').textContent = typeLabel(t);
  $('#drawerId').textContent = t.id;
  $('#drawerTitle').textContent = t.title;
  $('#drawerDescription').textContent = t.description || 'No description yet.';
  $('#drawerMeta').innerHTML = `<span class="meta-pill">${t.priority} priority</span><span class="meta-pill">${statusLabel[t.status] || t.status}</span><span class="meta-pill">Due ${formatDate(t.due)}</span>`;
  $('#drawerUpdated').textContent = formatDateTime(t.updated);
  const done = all.filter(s => s.done).length;
  $('#subtaskCount').textContent = `${done} / ${all.length}`;
  $('#subtaskList').innerHTML = all.length
    ? all.map(s => s.child
        ? `<div class="subtask ${s.done ? 'done' : ''}"><span>${escapeHtml(s.title)}</span><span class="subtask-chip">${statusLabel[tasks.find(x => x.id === s.child)?.status] || ''}</span></div>`
        : `<label class="subtask ${s.done ? 'done' : ''}"><input type="checkbox" data-subtask="${s.embedded}" ${s.done ? 'checked' : ''}/> <span>${escapeHtml(s.title)}</span></label>`).join('')
    : '<div class="subtask">No subtasks yet.</div>';
  $('#taskDrawer').classList.add('open');
  $('#drawerBackdrop').classList.add('open');
  document.querySelectorAll('[data-subtask]').forEach(box => box.addEventListener('change', e => toggleSubtask(Number(e.target.dataset.subtask), e.target.checked)));
}

async function toggleSubtask(index, checked) {
  const t = tasks.find(x => x.id === activeTaskId);
  if (!t || !t.subtasks[index]) return;
  t.subtasks[index].done = checked;
  t.updated = new Date().toISOString();
  persist();
  if (remoteMode) {
    try {
      const saved = await apiRequest(`/api/tasks/${enc(t.id)}`, { method: 'PATCH', body: JSON.stringify({ subtasks: t.subtasks }) });
      Object.assign(t, saved);
      syncOk = true;
    } catch (error) {
      syncOk = false;
      alert(`Could not save the checklist change: ${error.message}`);
    }
  }
  render();
  openDrawer(t.id);
}

function closeDrawer() {
  $('#taskDrawer').classList.remove('open');
  $('#drawerBackdrop').classList.remove('open');
}

async function deleteActive() {
  if (!activeTaskId || !confirm('Delete this work item?')) return;
  try {
    if (remoteMode) await apiRequest(`/api/tasks/${enc(activeTaskId)}`, { method: 'DELETE' });
    tasks = tasks.filter(t => t.id !== activeTaskId);
    persist();
    closeDrawer();
    render();
  } catch (error) {
    alert(`Could not delete: ${error.message}`);
  }
}

// ---- create / edit modal ---------------------------------------------------

function openModal(id = null, status = 'backlog') {
  editingId = id;
  const t = id && tasks.find(x => x.id === id);
  $('#modalTitle').textContent = t ? 'Edit work item' : 'Create a task';
  const form = $('#taskForm');
  form.reset();
  if (t) {
    for (const key of ['title', 'type', 'priority', 'status', 'due', 'description']) form.elements[key].value = t[key] || '';
  } else {
    form.elements.status.value = status;
  }
  $('#modalBackdrop').classList.add('open');
  form.elements.title.focus();
}
function closeModal() {
  $('#modalBackdrop').classList.remove('open');
  editingId = null;
}

async function saveTask(event) {
  event.preventDefault();
  const data = Object.fromEntries(new FormData(event.target));
  try {
    if (editingId) {
      const t = tasks.find(x => x.id === editingId);
      const saved = remoteMode ? await apiRequest(`/api/tasks/${enc(editingId)}`, { method: 'PATCH', body: JSON.stringify(data) }) : null;
      Object.assign(t, saved || data, { updated: new Date().toISOString() });
    } else if (remoteMode) {
      tasks.push(await apiRequest('/api/tasks', { method: 'POST', body: JSON.stringify(data) }));
    } else {
      tasks.push({ ...data, id: nextLocalId(), created: todayStr(), subtasks: [], parentId: null, updated: new Date().toISOString() });
    }
    syncOk = true;
    persist();
    closeModal();
    render();
  } catch (error) {
    syncOk = false;
    alert(`Could not save: ${error.message}`);
  }
}

// ---- column menu -----------------------------------------------------------

function closeColumnMenu() { document.querySelector('#columnMenu')?.remove(); }
function openColumnMenu(button) {
  closeColumnMenu();
  const status = button.dataset.status;
  const rect = button.getBoundingClientRect();
  const menu = document.createElement('div');
  menu.className = 'column-menu-pop';
  menu.id = 'columnMenu';
  menu.style.position = 'fixed';
  menu.style.top = `${rect.bottom + 6}px`;
  menu.style.left = `${Math.max(8, rect.right - 200)}px`;
  menu.innerHTML = `<button data-act="new">＋ New task in ${statusLabel[status]}</button><button data-act="only">Show only ${statusLabel[status]}</button><button data-act="clear">Clear status filter</button>`;
  menu.addEventListener('click', e => {
    const act = e.target.dataset.act;
    closeColumnMenu();
    if (act === 'new') openModal(null, status);
    if (act === 'only') { filters.status = status; syncFilterUI(); render(); }
    if (act === 'clear') { filters.status = ''; syncFilterUI(); render(); }
  });
  document.body.appendChild(menu);
}

// ---- filter panel ----------------------------------------------------------

function toggleFilterPanel(open) {
  const panel = $('#filterPanel');
  const next = open ?? panel.hidden;
  panel.hidden = !next;
  $('#filterButton').setAttribute('aria-expanded', String(next));
}

// ---- command menu, help, JSON ----------------------------------------------

const commands = [
  { label: 'New task', hint: 'N', run: () => openModal() },
  { label: 'Search tasks', hint: '/', run: () => $('#searchInput').focus() },
  { label: 'Show board', run: () => setView('board') },
  { label: 'Show list', run: () => setView('list') },
  { label: 'Show recent changes', run: () => setView('recent') },
  { label: 'Filter: high priority only', run: () => { filters.priority = 'high'; syncFilterUI(); render(); } },
  { label: 'Clear all filters', run: () => { resetFilters(); render(); } },
  { label: 'Copy agent JSON', run: () => showJson() },
  { label: 'Refresh board', hint: 'F5', run: () => refreshNow() },
  { label: 'Keyboard shortcuts', hint: '?', run: () => openHelp() },
];
let paletteIndex = 0;
let paletteItems = [];

function openPalette() {
  $('#paletteBackdrop').classList.add('open');
  const input = $('#paletteInput');
  input.value = '';
  paletteIndex = 0;
  renderPalette();
  input.focus();
}
function closePalette() { $('#paletteBackdrop').classList.remove('open'); }
function renderPalette() {
  const query = $('#paletteInput').value.trim().toLowerCase();
  paletteItems = commands.filter(c => c.label.toLowerCase().includes(query));
  paletteIndex = Math.min(paletteIndex, Math.max(0, paletteItems.length - 1));
  $('#paletteList').innerHTML = paletteItems.length
    ? paletteItems.map((c, i) => `<li data-index="${i}" class="${i === paletteIndex ? 'active' : ''}"><span>${c.label}</span>${c.hint ? `<kbd>${c.hint}</kbd>` : ''}</li>`).join('')
    : '<li>No matching command</li>';
}
function runPaletteItem(index) {
  const command = paletteItems[index];
  if (!command) return;
  closePalette();
  command.run();
}

function openHelp() { $('#helpBackdrop').classList.add('open'); }
function closeHelp() { $('#helpBackdrop').classList.remove('open'); }

function showJson() {
  const payload = {
    workspace: 'atlas',
    generatedAt: new Date().toISOString(),
    schemaVersion: '1.0',
    tasks: tasks.map(({ id, title, description, type, priority, status, due, created, subtasks }) => ({ id, title, description, type, priority, status, due, created, subtasks })),
  };
  $('#jsonOutput').textContent = JSON.stringify(payload, null, 2);
  $('#jsonModalBackdrop').classList.add('open');
}

async function refreshNow() {
  const button = $('#refreshBoard');
  button.disabled = true;
  button.textContent = '↻';
  await loadRemoteTasks();
  button.disabled = false;
}

// ---- wiring ----------------------------------------------------------------

function isOpen(selector) { return document.querySelector(selector).classList.contains('open'); }

function closeTop() {
  if (isOpen('#paletteBackdrop')) return closePalette();
  if (isOpen('#helpBackdrop')) return closeHelp();
  if (isOpen('#jsonModalBackdrop')) return $('#jsonModalBackdrop').classList.remove('open');
  if (isOpen('#modalBackdrop')) return closeModal();
  if (isOpen('#taskDrawer')) return closeDrawer();
  if (document.querySelector('#columnMenu')) return closeColumnMenu();
  if (!$('#filterPanel').hidden) toggleFilterPanel(false);
}

document.querySelectorAll('[data-view]').forEach(button => button.addEventListener('click', () => setView(button.dataset.view)));

$('#newTask').onclick = () => openModal();
$('#closeModal').onclick = closeModal;
$('#cancelModal').onclick = closeModal;
$('#drawerBackdrop').onclick = closeDrawer;
$('#closeDrawer').onclick = closeDrawer;
$('#editTask').onclick = () => { closeDrawer(); openModal(activeTaskId); };
$('#deleteTask').onclick = deleteActive;
$('#taskForm').onsubmit = saveTask;

$('#searchInput').oninput = render;
$('#sortSelect').onchange = render;
$('#focusSearch').onclick = () => $('#searchInput').focus();
$('#filterButton').onclick = () => toggleFilterPanel();
$('#filterPriority').onchange = e => { filters.priority = e.target.value; render(); };
$('#filterType').onchange = e => { filters.type = e.target.value; render(); };
$('#filterStatus').onchange = e => { filters.status = e.target.value; render(); };
$('#clearFilters').onclick = () => { resetFilters(); render(); };

$('#copyJson').onclick = showJson;
$('#closeJson').onclick = () => $('#jsonModalBackdrop').classList.remove('open');
$('#copyJsonModal').onclick = () => navigator.clipboard.writeText($('#jsonOutput').textContent).then(() => { $('#copyJsonModal').textContent = 'Copied'; });

$('#openPalette').onclick = openPalette;
$('#paletteInput').oninput = () => { paletteIndex = 0; renderPalette(); };
$('#paletteBackdrop').onclick = e => { if (e.target.id === 'paletteBackdrop') closePalette(); };
$('#paletteList').onclick = e => { const item = e.target.closest('[data-index]'); if (item) runPaletteItem(Number(item.dataset.index)); };
$('#paletteInput').addEventListener('keydown', e => {
  if (e.key === 'ArrowDown' || e.key === 'ArrowUp') {
    e.preventDefault();
    const step = e.key === 'ArrowDown' ? 1 : -1;
    paletteIndex = (paletteIndex + step + paletteItems.length) % Math.max(1, paletteItems.length);
    renderPalette();
  } else if (e.key === 'Enter') {
    e.preventDefault();
    runPaletteItem(paletteIndex);
  } else if (e.key === 'Escape') {
    e.stopPropagation();
    closePalette();
  }
});

$('#openHelp').onclick = openHelp;
$('#closeHelp').onclick = closeHelp;
$('#helpBackdrop').onclick = e => { if (e.target.id === 'helpBackdrop') closeHelp(); };

$('#refreshBoard').onclick = refreshNow;

document.addEventListener('click', e => {
  const menuButton = e.target.closest('.column-menu');
  if (menuButton) return openColumnMenu(menuButton);
  const addButton = e.target.closest('.column-add');
  if (addButton) return openModal(null, addButton.dataset.status);
  if (!e.target.closest('#columnMenu')) closeColumnMenu();
  const row = e.target.closest('#boardGrid [data-id], #listView [data-id], #recentView [data-id]');
  if (row) openDrawer(row.dataset.id);
});

document.addEventListener('keydown', e => {
  const typing = /INPUT|TEXTAREA|SELECT/.test(e.target.tagName);
  if ((e.ctrlKey || e.metaKey) && e.key.toLowerCase() === 'k') {
    e.preventDefault();
    return openPalette();
  }
  if (e.key === 'Escape') return closeTop();
  if (typing) return;
  if (e.key === '/') { e.preventDefault(); $('#searchInput').focus(); }
  else if (e.key === 'n' || e.key === 'N') openModal();
  else if (e.key === 'F5') { e.preventDefault(); refreshNow(); }
  else if (e.key === '?') openHelp();
});

render();
loadRemoteTasks();
if (remoteMode) setInterval(loadRemoteTasks, 5000);
