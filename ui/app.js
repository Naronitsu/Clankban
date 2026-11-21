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
let view = remoteMode ? 'dashboard' : 'board';
let dashboardScope = 'all';   // 'all' boards, or 'workspace' for the current board
let dashboardData = null;
let sprints = [];
let customViews = [];
let activeCustomView = null;   // the id of the saved view being shown, or null
let filters = { priority: '', type: '', status: '', tag: '', sprint: '' };
let activeTaskId = null;
let editingId = null;
let lastSync = null;
let syncOk = !remoteMode;
let skills = [];
let runRequests = [];
let agentShellBuilt = false;
let selectedSkill = '';
let formSkill = '';
const agentDrafts = {};
let workspaces = [];
let projectDocs = null;
let docsRefreshedAt = null;     // when the folder was last read
let docsNeedsRender = false;    // a refresh changed the list while the filter had focus
let docsRefreshing = false;
let selectedDoc = null;
let docBody = '';
let docsSearch = '';
let workspace = (() => {
  try { return localStorage.getItem('clankban-workspace') || 'default'; } catch { return 'default'; }
})();

const $ = selector => document.querySelector(selector);
const enc = encodeURIComponent;

async function apiRequest(path, options = {}) {
  const response = await fetch(`${API_BASE}${path}`, {
    ...options,
    headers: {
      'content-type': 'application/json',
      'x-clankban-actor': 'human',
      'x-clankban-client': 'Board',
      ...(options.headers || {}),
    },
  });
  if (!response.ok) throw new Error(await response.text());
  return response.json();
}

async function loadRemoteTasks() {
  if (!remoteMode) return;
  try {
    await loadWorkspaces();
    if (!workspaces.some(w => w.id === workspace)) setWorkspace('default', false);
    const result = await apiRequest(`/api/tasks?workspace=${enc(workspace)}`);
    tasks = result.tasks;
    await Promise.all([loadRunRequests(), loadSkills(), loadBoardRequests(), loadSprints(), loadViews()]);
    if (view === 'dashboard') await loadDashboard();
    syncOk = true;
    lastSync = new Date();
    persist();
  } catch (error) {
    syncOk = false;
    console.warn('Clankban API unavailable during refresh.', error);
  }
  render();
}

// ---- workspaces ------------------------------------------------------------

async function loadWorkspaces() {
  const result = await apiRequest('/api/workspaces');
  workspaces = result.workspaces;
}

function setWorkspace(id, reload = true) {
  workspace = id;
  activeCustomView = null;
  projectDocs = null;
  selectedDoc = null;
  docBody = '';
  try { localStorage.setItem('clankban-workspace', id); } catch { /* the choice is a convenience only */ }
  if (reload) loadRemoteTasks();
}

function renderWorkspaceControls() {
  const button = $('#workspaceButton');
  if (!button) return;
  if (!remoteMode) {
    $('#workspacePickerName').textContent = 'Default';
    $('#workspaceCrumb').textContent = 'Default';
    $('#workspaceCount').textContent = 'browser copy';
    button.disabled = true;
    $('#editWorkspace').disabled = true;
    closeWorkspaceMenu();
    return;
  }
  button.disabled = false;
  $('#editWorkspace').disabled = false;
  const current = workspaces.find(w => w.id === workspace);
  $('#workspacePickerName').textContent = current ? current.name : 'Default';
  $('#workspaceCrumb').textContent = current ? current.name : 'Default';
  $('#workspaceCount').textContent = current ? String(current.tasks) : '';
  $('#workspaceMenu').innerHTML = workspaces
    .map(w => `<button class="ws-item ${w.id === workspace ? 'active' : ''}" role="option" data-workspace="${escapeHtml(w.id)}">
        <span class="ws-item-name">${escapeHtml(w.name)}</span><span class="ws-item-count">${w.tasks}</span>
      </button>`)
    .join('') + '<button class="ws-item ws-new" data-new-workspace>＋ New workspace</button>';
}

function openWorkspaceMenu() {
  renderWorkspaceControls();
  $('#workspaceMenu').hidden = false;
  $('#workspaceButton').setAttribute('aria-expanded', 'true');
}

function closeWorkspaceMenu() {
  const menu = $('#workspaceMenu');
  if (menu) menu.hidden = true;
  $('#workspaceButton')?.setAttribute('aria-expanded', 'false');
}

function openWorkspaceModal(editing = false) {
  const current = workspaces.find(w => w.id === workspace);
  $('#workspaceModalTitle').textContent = editing ? 'Rename workspace' : 'New workspace';
  $('#workspaceName').value = editing && current ? current.name : '';
  $('#workspaceModal').dataset.mode = editing ? 'edit' : 'create';
  const deletable = editing && current && current.id !== 'default' && current.tasks === 0;
  $('#deleteWorkspace').hidden = !deletable;
  $('#workspaceModalError').textContent = '';
  $('#workspaceModal').classList.add('open');
  $('#workspaceName').focus();
}

function closeWorkspaceModal() { $('#workspaceModal').classList.remove('open'); }

async function saveWorkspace(event) {
  event.preventDefault();
  const name = $('#workspaceName').value.trim();
  const editing = $('#workspaceModal').dataset.mode === 'edit';
  try {
    if (editing) {
      const result = await apiRequest(`/api/workspaces/${enc(workspace)}`, { method: 'PATCH', body: JSON.stringify({ name }) });
      workspaces = result.workspaces;
    } else {
      const created = await apiRequest('/api/workspaces', { method: 'POST', body: JSON.stringify({ name }) });
      setWorkspace(created.id, false);
    }
    closeWorkspaceModal();
    await loadRemoteTasks();
  } catch (error) {
    $('#workspaceModalError').textContent = error.message;
  }
}

async function deleteWorkspace() {
  if (!confirm('Delete this workspace? Only empty workspaces can be deleted.')) return;
  try {
    await apiRequest(`/api/workspaces/${enc(workspace)}`, { method: 'DELETE' });
    closeWorkspaceModal();
    setWorkspace('default', false);
    await loadRemoteTasks();
  } catch (error) {
    $('#workspaceModalError').textContent = error.message;
  }
}

// ---- change history --------------------------------------------------------

async function loadHistory(id) {
  const box = $('#drawerHistory');
  if (!remoteMode) {
    box.innerHTML = '<div class="history-empty">History is recorded by the desktop app.</div>';
    return;
  }
  box.innerHTML = '<div class="history-empty">Loading…</div>';
  try {
    const result = await apiRequest(`/api/tasks/${enc(id)}/history`);
    if (activeTaskId !== id) return;
    box.innerHTML = result.history.length
      ? result.history.map(h => `
        <div class="history-entry">
          <div class="history-who">
            <strong>${escapeHtml(h.actorName)}</strong>
            ${h.actorModel ? `<span class="history-model">${escapeHtml(h.actorModel)}</span>` : ''}
            <span class="history-kind ${escapeHtml(h.actorKind)}">${escapeHtml(h.actorKind)}</span>
          </div>
          <div class="history-summary">${escapeHtml(h.summary)}</div>
          <small>${formatDateTime(h.at)}</small>
        </div>`).join('')
      : '<div class="history-empty">No changes recorded yet.</div>';
  } catch (error) {
    box.innerHTML = '<div class="history-empty">Could not load history.</div>';
  }
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
  if (filters.tag && !(t.tags || []).includes(filters.tag)) return false;
  if (filters.sprint === 'none' && t.sprint) return false;
  if (filters.sprint && filters.sprint !== 'none' && String(t.sprint || '') !== filters.sprint) return false;
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
  $('#filterTag').value = filters.tag;
}
function resetFilters() {
  filters = { priority: '', type: '', status: '', tag: '', sprint: '' };
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
  $('#navAgentCount').textContent = runRequests.filter(r => r.status === 'pending').length;
}

// Cards that have been shown before do not animate again, so the five-second refresh stays still.
const shownCards = new Set();

function card(t) {
  const fresh = !shownCards.has(t.id);
  shownCards.add(t.id);
  const subtasks = t.subtasks || [];
  const children = tasks.filter(x => x.parentId === t.id);
  const done = subtasks.filter(s => s.done).length + children.filter(x => x.status === 'done').length;
  const total = subtasks.length + children.length;
  return `<article class="task-card${fresh ? ' is-new' : ''}" data-id="${escapeHtml(t.id)}"><div class="card-top"><span class="type-badge ${t.type}">${typeLabel(t)}</span><span class="priority ${t.priority}" title="${t.priority} priority"></span></div><h3>${escapeHtml(t.title)}</h3><p>${escapeHtml(t.description)}</p>${tagRow(t)}<div class="card-bottom"><span class="card-id">${escapeHtml(t.id)}</span><div class="card-meta">${total ? `<span class="subtask-chip">□ ${done}/${total}</span>` : ''}<span class="${t.due && t.status !== 'done' ? 'due-soon' : ''}">${formatDate(t.due)}</span></div></div></article>`;
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

// The Recent view has two tabs: the tasks changed most recently, and the workspace's history.
// History entries link to their task. The feed is fetched only while its tab is open, and the
// view is redrawn only when the feed actually changed.
let recentTab = 'tasks';
let recentHistory = null;             // newest first, from /api/history; { error } when it failed
let recentHistoryKey = '';            // ids and titles of the last feed, to skip redraws
let recentHistoryWorkspace = null;    // the workspace the feed belongs to
let recentHistoryLoading = false;

function recentTabsMarkup() {
  const tab = (name, label) => `<button type="button" class="tab ${recentTab === name ? 'active' : ''}" role="tab" aria-selected="${recentTab === name}" data-recent-tab="${name}">${label}</button>`;
  return `<div class="recent-tabs" role="tablist">${tab('tasks', 'Tasks')}${tab('history', 'History')}</div>`;
}

function recentTasksMarkup(list) {
  const recent = list.filter(t => Date.parse(t.updated || '')).sort((a, b) => Date.parse(b.updated) - Date.parse(a.updated)).slice(0, 50);
  return recent.length
    ? `<ul class="recent-list">${recent.map(t => `<li data-id="${escapeHtml(t.id)}"><span><strong>${escapeHtml(t.id)}</strong> ${escapeHtml(t.title)}</span><small>${statusLabel[t.status] || t.status} · ${relativeTime(t.updated)}</small></li>`).join('')}</ul>`
    : '<div class="empty-note">No changes recorded yet. Edits made from the board or an agent appear here.</div>';
}

function recentHistoryMarkup() {
  if (!remoteMode) return '<div class="empty-note">History needs the desktop app and its local API.</div>';
  if (!recentHistory) return '<div class="empty-note">Loading history…</div>';
  if (recentHistory.error) return `<div class="empty-note">Could not load history: ${escapeHtml(recentHistory.error)}</div>`;
  if (!recentHistory.length) return '<div class="empty-note">No history yet. Changes made on the board or by an agent appear here.</div>';
  return `<ul class="recent-list recent-history">${recentHistory.map(h => `
    <li data-id="${escapeHtml(h.taskId)}" title="Open ${escapeHtml(h.taskId)}">
      <span class="rh-main"><strong>${escapeHtml(h.taskTitle)}</strong><small class="rh-id">${escapeHtml(h.taskId)}</small></span>
      <span class="rh-summary">${escapeHtml(h.summary || h.action)}</span>
      <small class="rh-meta">${escapeHtml(h.actorName)}${h.actorModel ? ` · ${escapeHtml(h.actorModel)}` : ''} · ${relativeTime(h.at)}</small>
    </li>`).join('')}</ul>`;
}

// Draws the view from what is already known. It never fetches, so it is safe to call on every refresh.
function drawRecent(list) {
  const body = recentTab === 'history' ? recentHistoryMarkup() : recentTasksMarkup(list);
  $('#recentView').innerHTML = `${recentTabsMarkup()}<div class="recent-body">${body}</div>`;
}

// Draws the view and, on the History tab, refreshes the feed. A changed feed redraws once.
function renderRecent(list) {
  drawRecent(list);
  if (recentTab === 'history') {
    loadRecentHistory().then(changed => { if (changed && view === 'recent' && recentTab === 'history') drawRecent(visibleTasks()); });
  }
}

// Returns true when the feed differs from what was shown before.
async function loadRecentHistory() {
  if (!remoteMode || !workspace || recentHistoryLoading) return false;
  if (recentHistoryWorkspace !== workspace) {
    recentHistoryWorkspace = workspace;
    recentHistory = null;
    recentHistoryKey = '';
  }
  recentHistoryLoading = true;
  try {
    const result = await apiRequest(`/api/history?workspace=${enc(workspace)}&limit=100`);
    const key = result.history.map(h => `${h.id}:${h.taskTitle}`).join(',');
    const changed = key !== recentHistoryKey || !Array.isArray(recentHistory);
    recentHistory = result.history;
    recentHistoryKey = key;
    return changed;
  } catch (error) {
    const changed = !recentHistory || !recentHistory.error;
    recentHistory = { error: errorText(error) };
    recentHistoryKey = '';
    return changed;
  } finally {
    recentHistoryLoading = false;
  }
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

// ---- board requests --------------------------------------------------------
// An agent that asks for a new board files a request. Nothing is created until a person
// approves it here.

let boardRequests = [];

async function loadBoardRequests() {
  // A failure here must not mark the whole board as offline, so the banner just stays hidden.
  try {
    const result = await apiRequest('/api/board-requests');
    boardRequests = result.requests;
  } catch {
    boardRequests = [];
  }
}

function renderBoardRequests() {
  const box = $('#boardRequests');
  if (!box) return;
  if (!remoteMode || !boardRequests.length) {
    box.innerHTML = '';
    return;
  }
  box.innerHTML = boardRequests.map(r => `
    <div class="board-request">
      <div>
        <strong>${escapeHtml(r.requestedBy)}</strong> asks for a new board <strong>${escapeHtml(r.name)}</strong>${r.folder ? ` for <code>${escapeHtml(r.folder)}</code>` : ''}.
        <small>Nothing is created until you approve it.</small>
      </div>
      <div class="board-request-actions">
        <button class="button primary" data-approve-board="${r.id}">Create board</button>
        <button class="button quiet" data-reject-board="${r.id}">Decline</button>
      </div>
    </div>`).join('');
}

async function decideBoardRequest(id, decision) {
  try {
    await apiRequest(`/api/board-requests/${enc(id)}/${decision}`, { method: 'POST' });
    await loadBoardRequests();
    await loadWorkspaces();
    render();
  } catch (error) {
    alert(`Could not ${decision === 'approve' ? 'create the board' : 'decline the request'}: ${errorText(error)}`);
  }
}

function render() {
  $('#board').classList.toggle('dash-mode', view === 'dashboard');
  renderBoardRequests();
  renderSprintFilter();
  renderViewPicker();
  renderWorkspaceControls();
  const list = visibleTasks();
  renderStats();
  document.querySelectorAll('[data-view]').forEach(button => button.classList.toggle('active', button.dataset.view === view));
  $('#boardGrid').hidden = view !== 'board';
  $('#listView').hidden = view !== 'list';
  $('#dashboardView').hidden = view !== 'dashboard';
  $('#recentView').hidden = view !== 'recent';
  $('#agentView').hidden = view !== 'agent';
  $('#docsView').hidden = view !== 'docs';
  $('#viewTitle').textContent = view === 'dashboard'
    ? (dashboardScope === 'workspace' ? `${currentWorkspace()?.name || 'Workspace'} dashboard` : 'Dashboard')
    : { board: 'Product board', list: 'List', recent: 'Recent changes', agent: 'Agent runs', docs: 'Project docs' }[view];
  if (view === 'board') renderBoard(list);
  if (view === 'list') renderList(list);
  if (view === 'dashboard') renderDashboard();
  if (view === 'recent') renderRecent(list);
  if (view === 'agent') renderAgent();
  if (view === 'docs') {
    renderDocs();
    // Opening the tab reads the folder again, so the list is current before anyone looks.
    docsAutoRefresh();
  }

  const count = activeFilterCount();
  $('#filterBadge').hidden = count === 0;
  $('#filterBadge').textContent = count;
  $('#filterButton').classList.toggle('active', count > 0);
  updateSyncStatus();
}

function setView(next) {
  view = next;
  activeCustomView = null;
  render();
  if (next === 'dashboard') loadDashboard();
}

// ---- drawer ----------------------------------------------------------------

// ---- saved views -----------------------------------------------------------
// A saved view is a list or board with its filters already applied, for example one sprint. The
// sidebar dropdown lists the board's views. Choosing one applies its layout and filters. The
// filters can still be changed, and "Save view" updates the view or saves a new one.

async function loadViews() {
  if (!remoteMode) {
    customViews = [];
    return;
  }
  try {
    const result = await apiRequest(`/api/views?workspace=${enc(workspace)}`);
    customViews = result.views;
  } catch {
    customViews = [];
  }
}

// The Views button shows the open view's name, or "Views". Its menu lists the board's views, and
// offers creating a view, plus updating or deleting the open one.
function renderViewPicker() {
  const button = $('#viewsButton');
  if (!button) return;
  if (activeCustomView && !customViews.some(v => v.id === activeCustomView)) activeCustomView = null;
  const current = customViews.find(v => v.id === activeCustomView);
  button.innerHTML = `<span>❖</span> ${escapeHtml(current ? current.name : 'Views')} <span class="views-caret">▾</span>`;
  if (!$('#viewsMenu').hidden) renderViewsMenu();
}

function renderViewsMenu() {
  const current = customViews.find(v => v.id === activeCustomView);
  const list = customViews.length
    ? customViews.map(v => `<button type="button" class="ws-item${v.id === activeCustomView ? ' active' : ''}" data-view-id="${v.id}"><span>${escapeHtml(v.name)}</span><small>${v.layout === 'list' ? 'List' : 'Board'}</small></button>`).join('')
    : '<div class="views-empty">No saved views yet.</div>';
  const actions = current
    ? `<div class="views-divider"></div><button type="button" class="ws-item" data-view-action="update">Update “${escapeHtml(current.name)}”</button><button type="button" class="ws-item views-danger" data-view-action="delete">Delete “${escapeHtml(current.name)}”</button>`
    : '';
  $('#viewsMenu').innerHTML = `<button type="button" class="ws-item views-create" data-view-action="create">+ Create view…</button><div class="views-divider"></div>${list}${actions}`;
}

function openViewsMenu(open) {
  const menu = $('#viewsMenu');
  if (!menu) return;
  menu.hidden = !open;
  $('#viewsButton').setAttribute('aria-expanded', String(open));
  if (open) renderViewsMenu();
}

function currentFilterSnapshot() {
  return { priority: filters.priority, type: filters.type, status: filters.status, tag: filters.tag, sprint: filters.sprint };
}

function applyCustomView(id) {
  const found = customViews.find(v => v.id === Number(id));
  if (!found) return;
  filters = {
    priority: found.filters.priority || '',
    type: found.filters.type || '',
    status: found.filters.status || '',
    tag: found.filters.tag || '',
    sprint: found.filters.sprint || '',
  };
  syncFilterUI();
  activeCustomView = found.id;
  view = found.layout === 'list' ? 'list' : 'board';
  render();
}

function runViewAction(action) {
  if (action === 'create') return openViewDialog();
  if (action === 'update') return saveActiveViewChanges();
  if (action === 'delete') return deleteActiveView();
}

function openViewDialog() {
  if (!remoteMode) {
    alert('Saved views need the desktop app and its local API.');
    return;
  }
  const form = $('#viewForm');
  form.reset();
  form.elements.layout.value = view === 'list' ? 'list' : 'board';
  updateViewSummary();
  $('#viewBackdrop').classList.add('open');
  form.elements.name.focus();
}

function closeViewDialog() {
  $('#viewBackdrop').classList.remove('open');
}

function updateViewSummary() {
  const labels = { priority: 'Priority', type: 'Type', status: 'Status', tag: 'Tag', sprint: 'Sprint' };
  const parts = Object.entries(currentFilterSnapshot())
    .filter(([, value]) => value)
    .map(([key, value]) => `${labels[key]}: ${key === 'sprint' ? (value === 'none' ? 'no sprint' : sprintName(value)) : value}`);
  $('#viewSummary').textContent = parts.length
    ? `Saves these filters: ${parts.join(', ')}.`
    : 'Saves no filters: every task on the board.';
}

async function createViewFromDialog(event) {
  event.preventDefault();
  const form = event.target;
  const name = (form.elements.name.value || '').trim();
  const layout = form.elements.layout.value;
  try {
    const result = await apiRequest('/api/views', {
      method: 'POST',
      body: JSON.stringify({ workspace, name, layout, filters: currentFilterSnapshot() }),
    });
    activeCustomView = result.view.id;
    closeViewDialog();
    await loadViews();
    render();
  } catch (error) {
    alert(`Could not create the view: ${errorText(error)}`);
  }
}

async function saveActiveViewChanges() {
  if (!activeCustomView) return;
  const current = customViews.find(v => v.id === activeCustomView);
  if (!confirm(`Update "${current?.name || 'this view'}" with the current layout and filters?`)) return;
  try {
    await apiRequest(`/api/views/${enc(activeCustomView)}`, {
      method: 'PATCH',
      body: JSON.stringify({ layout: view === 'list' ? 'list' : 'board', filters: currentFilterSnapshot() }),
    });
    await loadViews();
    render();
  } catch (error) {
    alert(`Could not update the view: ${errorText(error)}`);
  }
}

async function deleteActiveView() {
  if (!activeCustomView) return;
  const current = customViews.find(v => v.id === activeCustomView);
  if (!confirm(`Delete the view "${current?.name || ''}"? Its tasks stay where they are.`)) return;
  try {
    await apiRequest(`/api/views/${enc(activeCustomView)}`, { method: 'DELETE' });
    activeCustomView = null;
    await loadViews();
    render();
  } catch (error) {
    alert(`Could not delete the view: ${errorText(error)}`);
  }
}

$('#viewsButton').onclick = () => openViewsMenu($('#viewsMenu').hidden);
$('#viewForm').onsubmit = createViewFromDialog;
$('#closeView').onclick = closeViewDialog;
$('#cancelView').onclick = closeViewDialog;
$('#viewBackdrop').addEventListener('click', e => { if (e.target.id === 'viewBackdrop') closeViewDialog(); });


// ---- sprints ---------------------------------------------------------------
// A sprint is a time box on one board, with a start and an end. One sprint runs at a time in a
// board. Tasks join a sprint from the task form, and the board filters by sprint. The default
// length comes from the board; a length or end date can be typed for a single sprint.

async function loadSprints() {
  if (!remoteMode) {
    sprints = [];
    return;
  }
  try {
    const result = await apiRequest(`/api/sprints?workspace=${enc(workspace)}`);
    sprints = result.sprints;
  } catch {
    sprints = [];
  }
}

function sprintName(id) {
  const found = sprints.find(s => s.id === Number(id));
  return found ? found.name : 'Sprint';
}

function sprintStatusLabel(status) {
  return { planned: 'planned', active: 'running', closed: 'closed' }[status] || status;
}

function renderSprintFilter() {
  const select = $('#filterSprint');
  if (!select) return;
  select.innerHTML = `<option value="">Any</option><option value="none">No sprint</option>${sprints.map(s => `<option value="${s.id}">${escapeHtml(s.name)}</option>`).join('')}`;
  select.value = filters.sprint;
}

function renderSprintSelect() {
  const select = $('#taskSprint');
  if (!select) return;
  select.innerHTML = `<option value="">No sprint</option>${sprints.map(s => `<option value="${s.id}">${escapeHtml(s.name)} (${escapeHtml(sprintStatusLabel(s.status))})</option>`).join('')}`;
}

function sprintRow(s) {
  const actions = [];
  if (s.status === 'planned') actions.push(`<button type="button" class="button quiet" data-sprint-action="start" data-sprint-id="${s.id}">Start</button>`);
  actions.push(`<button type="button" class="button quiet danger" data-sprint-action="delete" data-sprint-id="${s.id}">Delete</button>`);
  return `<div class="sprint-row">
      <div class="sprint-row-main">
        <div><strong>${escapeHtml(s.name)}</strong> <span class="sprint-status ${escapeHtml(s.status)}">${escapeHtml(sprintStatusLabel(s.status))}</span></div>
        <div class="sprint-meta">${formatDate(s.startDate)} to ${formatDate(s.endDate)} · ${s.tasks} task${s.tasks === 1 ? '' : 's'}</div>
      </div>
      <div class="sprint-actions">${actions.join('')}</div>
    </div>`;
}

function renderSprintManager() {
  const running = sprints.find(s => s.status === 'active');
  $('#sprintRunning').innerHTML = running
    ? `<div class="sprint-running-card">
        <div class="sprint-running-main">
          <span class="sprint-status active">Running now</span>
          <strong>${escapeHtml(running.name)}</strong>
          <span class="sprint-meta">${formatDate(running.startDate)} to ${formatDate(running.endDate)} · ${running.tasks} task${running.tasks === 1 ? '' : 's'}</span>
        </div>
        <button type="button" class="button quiet" data-sprint-action="close" data-sprint-id="${running.id}">Close sprint</button>
      </div>`
    : '<div class="sprint-hint">No sprint is running. Start one from the list below.</div>';
  const others = sprints.filter(s => s !== running);
  $('#sprintList').innerHTML = others.length
    ? others.map(sprintRow).join('')
    : '<div class="empty-note">No other sprints yet. Create one below.</div>';
}

// Shows what the new sprint will cover, as the form is filled in.
function updateSprintPreview() {
  const form = $('#sprintForm');
  const preview = $('#sprintPreview');
  if (!form || !preview) return;
  const boardDefault = currentWorkspace()?.sprintDays || 14;
  const typedLength = Number(form.elements.lengthDays.value);
  const typedEnd = form.elements.endDate.value;
  const startDay = new Date(`${form.elements.startDate.value || todayStr()}T00:00:00`);
  let endDay;
  if (typedEnd) {
    endDay = new Date(`${typedEnd}T00:00:00`);
  } else {
    endDay = new Date(startDay);
    endDay.setDate(endDay.getDate() + (typedLength || boardDefault) - 1);
  }
  const days = Math.round((endDay - startDay) / 86400000) + 1;
  if (!(days >= 1) || Number.isNaN(days)) {
    preview.textContent = 'The end date must fall on or after the start.';
    preview.classList.add('bad');
    return;
  }
  preview.classList.remove('bad');
  const fmt = d => d.toLocaleDateString(undefined, { month: 'short', day: 'numeric', year: 'numeric' });
  const source = typedEnd ? 'ends on the date you chose' : typedLength ? 'your length' : `board default of ${boardDefault} days`;
  preview.textContent = `${fmt(startDay)} to ${fmt(endDay)} · ${days} day${days === 1 ? '' : 's'} (${source})`;
}

function openSprintManager() {
  if (!remoteMode) {
    alert('Sprints need the desktop app and its local API.');
    return;
  }
  renderSprintManager();
  $('#sprintDaysForm').elements.days.value = currentWorkspace()?.sprintDays || 14;
  $('#sprintForm').reset();
  $('#sprintForm').elements.startDate.value = todayStr();
  updateSprintPreview();
  $('#sprintBackdrop').classList.add('open');
}

function closeSprintManager() {
  $('#sprintBackdrop').classList.remove('open');
}

async function sprintAction(id, action) {
  try {
    if (action === 'delete') {
      if (!confirm('Delete this sprint? Its tasks stay on the board, out of the sprint.')) return;
      await apiRequest(`/api/sprints/${enc(id)}`, { method: 'DELETE' });
    } else {
      await apiRequest(`/api/sprints/${enc(id)}`, { method: 'PATCH', body: JSON.stringify({ status: action === 'start' ? 'active' : 'closed' }) });
    }
    await loadSprints();
    renderSprintManager();
    render();
  } catch (error) {
    alert(`Could not ${action} the sprint: ${errorText(error)}`);
  }
}

async function createSprintFromForm(event) {
  event.preventDefault();
  const form = event.target;
  const data = Object.fromEntries(new FormData(form));
  const body = {
    workspace,
    name: (data.name || '').trim(),
    startDate: data.startDate || undefined,
    lengthDays: data.lengthDays ? Number(data.lengthDays) : undefined,
    endDate: data.endDate || undefined,
  };
  try {
    await apiRequest('/api/sprints', { method: 'POST', body: JSON.stringify(body) });
    form.reset();
    form.elements.startDate.value = todayStr();
    updateSprintPreview();
    await loadSprints();
    renderSprintManager();
    render();
  } catch (error) {
    alert(`Could not create the sprint: ${errorText(error)}`);
  }
}

async function saveSprintDays(event) {
  event.preventDefault();
  const days = Number(event.target.elements.days.value);
  try {
    await apiRequest(`/api/workspaces/${enc(workspace)}/sprint-days`, { method: 'PUT', body: JSON.stringify({ days }) });
    await loadWorkspaces();
    updateSprintPreview();
    render();
  } catch (error) {
    alert(`Could not save the default length: ${errorText(error)}`);
  }
}

$('#openSprints').onclick = openSprintManager;
$('#closeSprints').onclick = closeSprintManager;
$('#sprintBackdrop').addEventListener('click', e => { if (e.target.id === 'sprintBackdrop') closeSprintManager(); });
$('#sprintForm').onsubmit = createSprintFromForm;
$('#sprintForm').addEventListener('input', updateSprintPreview);
$('#sprintDaysForm').onsubmit = saveSprintDays;


// ---- dashboard -------------------------------------------------------------
// The first screen. "All boards" covers every workspace; a workspace's own dashboard opens from its
// name at the top left. Numbers come from the API, and the page redraws on each refresh.

async function loadDashboard() {
  if (!remoteMode) return;
  const scope = dashboardScope === 'workspace' ? workspace : 'all';
  try {
    dashboardData = await apiRequest(`/api/dashboard?workspace=${enc(scope)}`);
  } catch (error) {
    dashboardData = { error: errorText(error) };
  }
  if (view === 'dashboard') renderDashboard();
}

function openWorkspaceDashboard(id) {
  if (id !== workspace) setWorkspace(id);
  dashboardScope = 'workspace';
  setView('dashboard');
}

// Opens a task in whichever board it belongs to, switching boards first when needed.
async function openTaskAnywhere(id, workspaceId) {
  if (workspaceId && workspaceId !== workspace) {
    setWorkspace(workspaceId, false);
    await loadRemoteTasks();
  }
  setView('board');
  openDrawer(id);
}

function dashboardBoardCard(b) {
  const details = [
    `${b.active} active`,
    `${b.highPriority} high`,
    `${b.dueSoon} due soon`,
    b.overdue ? `${b.overdue} overdue` : '',
    b.blocked ? `${b.blocked} blocked` : '',
  ].filter(Boolean).join(' · ');
  return `<button type="button" class="dash-board" data-open-board="${escapeHtml(b.id)}">
      <strong>${escapeHtml(b.name)}</strong>
      <span class="dash-board-meta">${escapeHtml(details)}</span>
      <small>${b.lastUpdated ? `Updated ${relativeTime(b.lastUpdated)}` : 'No changes yet'}</small>
    </button>`;
}

function renderDashboard() {
  const box = $('#dashboardView');
  if (!box) return;
  if (!remoteMode) {
    box.innerHTML = '<div class="empty-note">The dashboard needs the desktop app and its local API.</div>';
    return;
  }
  if (!dashboardData) {
    box.innerHTML = '<div class="empty-note">Loading the dashboard…</div>';
    return;
  }
  if (dashboardData.error) {
    box.innerHTML = `<div class="empty-note">Could not load the dashboard: ${escapeHtml(dashboardData.error)}</div>`;
    return;
  }
  const single = dashboardScope === 'workspace';
  const boards = dashboardData.workspaces || [];
  const totals = dashboardData.totals;
  const nameOf = id => (boards.find(b => b.id === id) || {}).name || id;
  const byStatus = { backlog: 0, progress: 0, review: 0, done: 0 };
  for (const b of boards) for (const key of Object.keys(byStatus)) byStatus[key] += b.byStatus[key] || 0;
  const labels = { backlog: 'Backlog', progress: 'In progress', review: 'In review', done: 'Done' };

  const stat = (label, value) => `<div class="stat-card"><div><span>${label}</span><strong>${value}</strong></div></div>`;
  const dueLabel = totals.overdue ? `Due in 7 days <em class="warning">${totals.overdue} overdue</em>` : 'Due in 7 days';
  const pills = Object.entries(byStatus).map(([key, n]) => `<span class="dash-pill">${labels[key]}<b>${n}</b></span>`).join('');

  const upcoming = dashboardData.upcoming || [];
  const upcomingRows = upcoming.length
    ? upcoming.map(u => `<button type="button" class="dash-row" data-dash-task="${escapeHtml(u.id)}" data-workspace="${escapeHtml(u.workspace)}">
        <span><strong>${escapeHtml(u.id)}</strong>${escapeHtml(u.title)}</span>
        <small>${formatDate(u.due)}${single ? '' : ` · ${escapeHtml(u.workspaceName)}`}</small>
      </button>`).join('')
    : '<div class="empty-note">Nothing due in the next 7 days.</div>';

  const recent = dashboardData.recent || [];
  const recentRows = recent.length
    ? recent.map(h => `<button type="button" class="dash-row" data-dash-task="${escapeHtml(h.taskId)}" data-workspace="${escapeHtml(h.taskWorkspace)}">
        <span><strong>${escapeHtml(h.taskId)}</strong>${escapeHtml(h.taskTitle)}${single ? '' : ` · ${escapeHtml(nameOf(h.taskWorkspace))}`}</span>
        <small>${escapeHtml(h.summary || h.action)} · ${relativeTime(h.at)}</small>
      </button>`).join('')
    : '<div class="empty-note">No changes recorded yet.</div>';

  box.innerHTML = `
    <div class="stats-row dash-stats">
      ${stat('Active work', totals.active)}
      ${stat('High priority', totals.highPriority)}
      ${stat(dueLabel, totals.dueSoon)}
      ${stat('Blocked', totals.blocked)}
    </div>
    <div class="dash-status">${pills}</div>
    ${single ? '' : `<section class="dash-section">
      <div class="section-heading"><span>Workspaces</span><span>${boards.length}</span></div>
      <div class="dash-boards">${boards.map(dashboardBoardCard).join('')}</div>
    </section>`}
    <div class="dash-columns">
      <section class="dash-section">
        <div class="section-heading"><span>Due in the next 7 days</span><span>${upcoming.length}</span></div>
        <div class="dash-list">${upcomingRows}</div>
      </section>
      <section class="dash-section">
        <div class="section-heading"><span>Recent activity</span><span>${recent.length}</span></div>
        <div class="dash-list">${recentRows}</div>
      </section>
    </div>`;
}

// ---- tags and suggested model ----------------------------------------------
// Tags are labels; "important" and "on hold" have their own toggles in the drawer.
// The suggested model is advice for whichever agent picks the task up.

function tagLabel(tag) {
  return { important: 'Important', 'on hold': 'On hold' }[tag] || tag;
}

function tagClass(tag) {
  return tag.replace(/\s+/g, '-');
}

function tagRow(t) {
  const tags = t.tags || [];
  if (!tags.length && !t.suggestedModel && !t.sprint) return '';
  return `<div class="tag-row">${t.sprint ? `<span class="tag-chip sprint-chip">${escapeHtml(sprintName(t.sprint))}</span>` : ''}${tags.map(tag => `<span class="tag-chip ${tagClass(tag)}">${escapeHtml(tagLabel(tag))}</span>`).join('')}${t.suggestedModel ? `<span class="model-suggest" title="Suggested model. Advisory only.">${escapeHtml(t.suggestedModel)}</span>` : ''}</div>`;
}

function renderDrawerTags(t) {
  const box = $('#drawerTags');
  if (!box) return;
  const tags = t.tags || [];
  const toggles = ['important', 'on hold'].map(tag => {
    const on = tags.includes(tag);
    return `<button type="button" class="tag-toggle ${tagClass(tag)}${on ? ' on' : ''}" data-toggle-tag="${escapeHtml(tag)}" aria-pressed="${on}">${on ? '✓ ' : ''}${escapeHtml(tagLabel(tag))}</button>`;
  }).join('');
  const others = tags.filter(tag => !['important', 'on hold'].includes(tag))
    .map(tag => `<span class="tag-chip">${escapeHtml(tag)}</span>`).join('');
  const sprintChip = t.sprint ? `<span class="tag-chip sprint-chip">${escapeHtml(sprintName(t.sprint))}</span>` : '';
  const model = t.suggestedModel
    ? `<span class="model-suggest" title="Suggested model. Advisory only.">Suggested model: <code>${escapeHtml(t.suggestedModel)}</code></span>`
    : '';
  box.innerHTML = `${sprintChip}${toggles}${others}${model}`;
}

async function toggleTag(id, tag) {
  const t = tasks.find(x => x.id === id);
  if (!t) return;
  const current = t.tags || [];
  const next = current.includes(tag) ? current.filter(x => x !== tag) : [...current, tag];
  try {
    const saved = remoteMode
      ? await apiRequest(`/api/tasks/${enc(id)}`, { method: 'PATCH', body: JSON.stringify({ tags: next }) })
      : null;
    Object.assign(t, saved || { tags: next }, { updated: new Date().toISOString() });
    syncOk = true;
    persist();
    render();
    renderDrawerTags(t);
    loadHistory(id);
  } catch (error) {
    alert(`Could not change the tag: ${error.message}`);
  }
}

// ---- relationships, links and comments ------------------------------------------

// Links open in the person's browser. In the desktop app that is the system browser, and the
// app window stays where it is.
function openLink(url) {
  if (tauriCore) {
    return tauriCore.invoke('open_external', { url }).catch(error => alert(`Could not open the link: ${errorText(error)}`));
  }
  window.open(url, '_blank', 'noopener,noreferrer');
}

function taskChip(id) {
  const other = tasks.find(x => x.id === id);
  const label = other ? `${other.id} · ${other.title}` : `${id} (not found)`;
  return `<button type="button" class="task-chip${other && other.status === 'done' ? ' done' : ''}" data-open-task="${escapeHtml(id)}">${escapeHtml(label)}</button>`;
}

function linkItems(urls) {
  return urls.map(url => `<a class="ext-link" href="${escapeHtml(url)}" target="_blank" rel="noopener noreferrer">${escapeHtml(url.replace(/^https?:\/\//i, ''))}</a>`).join('');
}

// Subtasks relate to their parent automatically: the parent appears under "Part of".
function renderDrawerRelations(t) {
  const box = $('#drawerRelations');
  if (!box) return;
  const blockedBy = t.blockedBy || [];
  const relatesTo = t.relatesTo || [];
  const blocks = tasks.filter(x => (x.blockedBy || []).includes(t.id)).map(x => x.id);
  const group = (label, body) => body
    ? `<div class="rel-group"><div class="rel-label">${label}</div><div class="rel-items">${body}</div></div>`
    : '';
  const pickUp = t.standalone === false
    ? `<div class="pickup-note"><strong>Can't be picked up on its own.</strong> ${escapeHtml(t.standaloneNote || 'No reason given.')}</div>`
    : '';
  const links = (t.links || []).length
    ? `<div class="rel-group"><div class="rel-label">Links</div><div class="rel-items link-items">${linkItems(t.links)}</div></div>`
    : '';
  const body = pickUp
    + group('Part of', t.parentId ? taskChip(t.parentId) : '')
    + group('Blocked by', blockedBy.map(taskChip).join(''))
    + group('Blocks', blocks.map(taskChip).join(''))
    + group('Relates to', relatesTo.map(taskChip).join(''))
    + links;
  box.innerHTML = body ? `<div class="drawer-section relations">${body}</div>` : '';
}

async function loadComments(id) {
  const list = $('#commentList');
  const form = $('#commentForm');
  if (!list || !form) return;
  if (!remoteMode) {
    list.innerHTML = '<div class="history-empty">Comments need the desktop app and its local API.</div>';
    form.hidden = true;
    return;
  }
  form.hidden = false;
  list.innerHTML = '<div class="history-empty">Loading…</div>';
  try {
    const result = await apiRequest(`/api/tasks/${enc(id)}/comments`);
    if (activeTaskId !== id) return;
    renderComments(result.comments);
  } catch (error) {
    if (activeTaskId === id) list.innerHTML = '<div class="history-empty">Could not load comments.</div>';
  }
}

function renderComments(comments) {
  $('#commentCount').textContent = comments.length ? String(comments.length) : '';
  $('#commentList').innerHTML = comments.length
    ? comments.map(c => `
      <div class="comment">
        <div class="comment-who">
          <strong>${escapeHtml(c.actorName)}</strong>
          ${c.actorModel ? `<span class="history-model">${escapeHtml(c.actorModel)}</span>` : ''}
          <span class="history-kind ${escapeHtml(c.actorKind)}">${escapeHtml(c.actorKind)}</span>
          <small>${formatDateTime(c.at)}</small>
        </div>
        <div class="comment-body">${renderMarkdown(c.body)}</div>
      </div>`).join('')
    : '<div class="history-empty">No comments yet.</div>';
}

async function postComment(event) {
  event.preventDefault();
  const form = event.target;
  const body = form.elements.body.value.trim();
  if (!body || !activeTaskId) return;
  try {
    await apiRequest(`/api/tasks/${enc(activeTaskId)}/comments`, { method: 'POST', body: JSON.stringify({ body }) });
    form.reset();
    await loadComments(activeTaskId);
  } catch (error) {
    alert(`Could not post the comment: ${errorText(error)}`);
  }
}

$('#commentForm').onsubmit = postComment;

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
  $('#drawerDescription').innerHTML = t.description ? renderMarkdown(t.description) : '<p>No description yet.</p>';
  renderDrawerRelations(t);
  loadComments(id);
  $('#drawerMeta').innerHTML = `<span class="meta-pill">${t.priority} priority</span><span class="meta-pill">${statusLabel[t.status] || t.status}</span><span class="meta-pill">Due ${formatDate(t.due)}</span>`;
  $('#drawerUpdated').textContent = formatDateTime(t.updated);
  renderDrawerTags(t);
  renderDrawerDocs(t);
  loadHistory(id);
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
  form.elements.docs.value = t ? (t.docs || []).join(', ') : '';
  form.elements.tags.value = t ? (t.tags || []).join(', ') : '';
  form.elements.blockedBy.value = t ? (t.blockedBy || []).join(', ') : '';
  form.elements.relatesTo.value = t ? (t.relatesTo || []).join(', ') : '';
  form.elements.links.value = t ? (t.links || []).join(', ') : '';
  renderSprintSelect();
  form.elements.sprint.value = t && t.sprint ? String(t.sprint) : '';
  form.elements.standaloneNote.value = t ? (t.standaloneNote || '') : '';
  form.elements.standalone.checked = t ? t.standalone !== false : true;
  form.elements.suggestedModel.value = t ? (t.suggestedModel || '') : '';
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
  data.docs = parseDocList(data.docs);
  data.tags = parseDocList(data.tags).map(tag => tag.toLowerCase());
  data.suggestedModel = (data.suggestedModel || '').trim();
  data.blockedBy = parseDocList(data.blockedBy);
  data.relatesTo = parseDocList(data.relatesTo);
  data.links = parseDocList(data.links);
  data.standalone = event.target.elements.standalone.checked;
  data.standaloneNote = (data.standaloneNote || '').trim();
  data.sprint = data.sprint ? Number(data.sprint) : null;
  try {
    if (editingId) {
      const t = tasks.find(x => x.id === editingId);
      const saved = remoteMode ? await apiRequest(`/api/tasks/${enc(editingId)}`, { method: 'PATCH', body: JSON.stringify(data) }) : null;
      Object.assign(t, saved || data, { updated: new Date().toISOString() });
    } else if (remoteMode) {
      tasks.push(await apiRequest('/api/tasks', { method: 'POST', body: JSON.stringify({ ...data, workspace }) }));
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

// ---- agent runs ------------------------------------------------------------

async function loadRunRequests() {
  const result = await apiRequest('/api/run-requests');
  runRequests = result.runs;
}

async function loadSkills() {
  if (skills.length) return;
  const result = await apiRequest('/api/skills');
  skills = result.skills;
}

function runStatusLabel(status) {
  return { pending: 'Pending', running: 'Running', done: 'Done', failed: 'Failed', cancelled: 'Cancelled' }[status] || status;
}

// The page has a skill list, one form for the chosen skill, and the request queue below.
// The form is rebuilt only when the chosen skill changes; typed text is kept per skill in
// agentDrafts, so the five-second refresh never wipes what someone is writing.

function skillTitle(name) {
  return name.replace(/_/g, ' ');
}

function buildAgentShell() {
  $('#agentView').innerHTML = `
    <p class="agent-intro">Choose a skill, fill in what it asks for, and send it. The connected agent reads the request through the MCP bridge and shows its plan before it changes anything.</p>
    <div class="agent-layout">
      <div class="skill-list" id="skillList" role="group" aria-label="Skills"></div>
      <form class="skill-form" id="skillForm" autocomplete="off"></form>
    </div>
    <section class="run-section">
      <div class="section-heading"><span>Requests</span><span id="runCount"></span></div>
      <div class="run-list" id="runList"></div>
    </section>`;
  agentShellBuilt = true;
}

function renderSkillList() {
  const list = $('#skillList');
  if (!list) return;
  list.innerHTML = skills.map(skill => `
    <button type="button" class="skill-option${skill.name === selectedSkill ? ' active' : ''}" data-skill="${escapeHtml(skill.name)}" aria-pressed="${skill.name === selectedSkill}">
      <strong>${escapeHtml(skillTitle(skill.name))}</strong>
      <small>${escapeHtml(skill.description)}</small>
    </button>`).join('');
}

function renderSkillForm(force = false) {
  const form = $('#skillForm');
  const skill = skills.find(s => s.name === selectedSkill);
  if (!form) return;
  if (!skill) {
    form.innerHTML = '';
    formSkill = '';
    return;
  }
  if (formSkill === skill.name && !force) return;
  formSkill = skill.name;
  const draft = agentDrafts[skill.name] || {};
  const fields = skill.arguments.length
    ? skill.arguments.map(arg => `
        <label class="skill-field">
          <span>${escapeHtml(arg.name)} ${arg.required ? '<em class="arg-tag required">required</em>' : '<em class="arg-tag">optional</em>'}</span>
          <textarea name="${escapeHtml(arg.name)}" rows="3" placeholder="${escapeHtml(arg.description)}">${escapeHtml(draft[arg.name] || '')}</textarea>
        </label>`).join('')
    : '<p class="no-args">This skill needs no details. The agent reads what it needs from the board.</p>';
  form.dataset.skill = skill.name;
  form.innerHTML = `
    <div class="skill-form-head">
      <h2>${escapeHtml(skillTitle(skill.name))}</h2>
      <p>${escapeHtml(skill.description)}</p>
    </div>
    ${fields}
    <div class="skill-form-footer">
      <button class="button primary" type="submit">Send to agent</button>
    </div>`;
}

function selectSkill(name) {
  selectedSkill = name;
  renderSkillList();
  renderSkillForm(true);
}

function renderRunList() {
  const list = $('#runList');
  if (!list) return;
  const active = runRequests.filter(r => r.status === 'pending' || r.status === 'running').length;
  $('#runCount').textContent = active ? `${active} active` : '';
  const newestFirst = [...runRequests].reverse();
  list.innerHTML = newestFirst.length
    ? newestFirst.map(run => `
      <div class="run-row">
        <div class="run-info">
          <span class="run-title">${escapeHtml(skillTitle(run.skill))}</span>
          <span class="run-meta">#${run.id} · ${relativeTime(run.created)}</span>
        </div>
        <div class="run-side">
          <span class="run-status ${run.status}">${runStatusLabel(run.status)}</span>
          ${run.status === 'pending' ? `<button class="button quiet cancel-run" data-id="${run.id}">Cancel</button>` : ''}
        </div>
        ${run.result ? `<p class="run-result">${escapeHtml(run.result)}</p>` : ''}
      </div>`).join('')
    : '<div class="empty-note">No requests yet. Pick a skill above to send one.</div>';
}

function renderAgent() {
  if (!remoteMode) {
    $('#agentView').innerHTML = '<div class="empty-note">Agent runs need the desktop app and its local API. Open the installed Clankban to send skills to an agent.</div>';
    agentShellBuilt = false;
    formSkill = '';
    return;
  }
  if (!agentShellBuilt) buildAgentShell();
  if (!skills.some(s => s.name === selectedSkill)) selectedSkill = skills[0]?.name || '';
  renderSkillList();
  renderSkillForm();
  renderRunList();
}

async function sendRun(form) {
  const skill = skills.find(s => s.name === form.dataset.skill);
  if (!skill) return;
  const args = {};
  for (const arg of skill.arguments) {
    const value = form.elements[arg.name]?.value.trim();
    if (value) args[arg.name] = value;
  }
  const missing = skill.arguments.find(a => a.required && !args[a.name]);
  if (missing) {
    alert(`${missing.name} is required.`);
    return;
  }
  try {
    await apiRequest('/api/run-requests', { method: 'POST', body: JSON.stringify({ skill: skill.name, arguments: args }) });
    form.querySelectorAll('textarea').forEach(t => { t.value = ''; });
    delete agentDrafts[skill.name];
    await loadRunRequests();
    render();
  } catch (error) {
    alert(`Could not send: ${error.message}`);
  }
}

async function cancelRun(id) {
  if (!confirm('Cancel this request? The agent will not run it.')) return;
  try {
    await apiRequest(`/api/run-requests/${id}`, { method: 'PATCH', body: JSON.stringify({ status: 'cancelled', result: 'Cancelled from the board' }) });
    await loadRunRequests();
    render();
  } catch (error) {
    alert(`Could not cancel: ${error.message}`);
  }
}

// ---- updates ---------------------------------------------------------------
// The desktop app checks for updates in Rust: a few seconds after launch and every six hours.
// When a newer version exists it sends an "update-available" event. In a plain browser there is
// no desktop shell, so the controls say so.

const tauriApi = window.__TAURI__;
const tauriCore = tauriApi?.core;
const tauriEvent = window.__TAURI__?.event;

function setUpdateStatus(text) { $('#updateStatus').textContent = text; }

// ---- release notes ---------------------------------------------------------
// An update brings its notes with it (latest.json, written from CHANGELOG.md by the release
// script). The installed build also carries the whole changelog. Notes are plain text:
// "### " starts a heading and "- " starts a bullet.

let currentVersion = '';
let pendingUpdate = null;

function renderNotes(markdown) {
  const out = [];
  let inList = false;
  for (const raw of String(markdown || '').split(/\r?\n/)) {
    const line = raw.trim();
    if (line.startsWith('- ')) {
      if (!inList) { out.push('<ul>'); inList = true; }
      out.push(`<li>${escapeHtml(line.slice(2))}</li>`);
      continue;
    }
    if (inList) { out.push('</ul>'); inList = false; }
    if (!line) continue;
    out.push(line.startsWith('### ')
      ? `<h4>${escapeHtml(line.slice(4))}</h4>`
      : `<p>${escapeHtml(line)}</p>`);
  }
  if (inList) out.push('</ul>');
  return out.join('');
}

// The body of one version's section in CHANGELOG.md, without its heading line.
function changelogSection(text, version) {
  const lines = String(text || '').split(/\r?\n/);
  const start = lines.findIndex(l => l === `## ${version}` || l.startsWith(`## ${version} `));
  if (start < 0) return '';
  const rest = lines.slice(start + 1);
  const next = rest.findIndex(l => l.startsWith('## '));
  return (next < 0 ? rest : rest.slice(0, next)).join('\n').trim();
}

function showNotes(title, markdown, { installable = false } = {}) {
  $('#changelogTitle').textContent = title;
  $('#changelogBody').innerHTML = renderNotes(markdown) || '<p>No release notes for this version.</p>';
  const install = $('#changelogInstall');
  install.hidden = !(installable && pendingUpdate);
  if (!install.hidden) install.textContent = `Install ${pendingUpdate.version} and restart`;
  $('#changelogBackdrop').classList.add('open');
}

function closeNotes() { $('#changelogBackdrop').classList.remove('open'); }

// After an update, the first launch of the new version explains what changed. Builds before
// this feature never saved a version, so the first launch after upgrading from them also shows
// the notes. The one cost is a welcome on a fresh install.
async function showWhatsNewAfterUpdate() {
  if (!tauriCore || !currentVersion) return;
  let seen = null;
  try { seen = localStorage.getItem('clankban-seen-version'); } catch { /* storage blocked */ }
  try { localStorage.setItem('clankban-seen-version', currentVersion); } catch { /* storage blocked */ }
  if (seen === currentVersion) return;
  try {
    const text = await tauriCore.invoke('get_changelog');
    showNotes(`What's new in ${currentVersion}`, changelogSection(text, currentVersion));
  } catch { /* the notes are a convenience; the app works without them */ }
}

async function openWhatsNew() {
  if (pendingUpdate) return showNotes(`Version ${pendingUpdate.version}`, pendingUpdate.notes, { installable: true });
  if (!tauriCore) return showNotes("What's new", 'Release notes are in the installed app.');
  try {
    const text = await tauriCore.invoke('get_changelog');
    showNotes(`What's new in ${currentVersion || 'this version'}`, changelogSection(text, currentVersion));
  } catch (error) {
    showNotes("What's new", `Could not load the notes: ${error}`);
  }
}

// ---- updates ---------------------------------------------------------------

function offerUpdate(info) {
  pendingUpdate = info;
  setUpdateStatus(`Version ${info.version} is available.`);
  $('#installUpdate').textContent = `Install ${info.version} and restart`;
  $('#installUpdate').disabled = false;
  $('#installUpdate').hidden = false;
}

async function checkForUpdates({ silent = false } = {}) {
  if (!tauriCore) {
    if (!silent) setUpdateStatus('Updates work in the installed app.');
    return;
  }
  if (!silent) setUpdateStatus('Checking…');
  try {
    const info = await tauriCore.invoke('check_for_update');
    if (info) offerUpdate(info);
    else if (!silent) setUpdateStatus('You have the latest version.');
  } catch (error) {
    if (!silent) setUpdateStatus(`Could not check: ${error}`);
  }
}

async function installUpdate() {
  if (!tauriCore) return;
  if (!confirm('Install the update now? Clankban will restart when it is done.')) return;
  $('#installUpdate').disabled = true;
  setUpdateStatus('Downloading…');
  try {
    await tauriCore.invoke('install_update');
    setUpdateStatus('Restarting…');
  } catch (error) {
    $('#installUpdate').disabled = false;
    setUpdateStatus(`Update failed: ${error}`);
  }
}

if (tauriEvent) tauriEvent.listen('update-available', event => offerUpdate(event.payload));
if (tauriCore) setTimeout(() => checkForUpdates({ silent: true }), 4000);

$('#showChangelog').onclick = openWhatsNew;
$('#closeChangelog').onclick = closeNotes;
$('#laterChangelog').onclick = closeNotes;
$('#changelogInstall').onclick = () => { closeNotes(); installUpdate(); };
$('#changelogBackdrop').addEventListener('click', e => { if (e.target.id === 'changelogBackdrop') closeNotes(); });

async function showAppVersion() {
  try {
    const version = await tauriApi?.app?.getVersion?.();
    currentVersion = version || '';
    $('#appVersion').textContent = version || 'browser build';
    showWhatsNewAfterUpdate();
  } catch {
    $('#appVersion').textContent = 'unknown';
  }
}

// ---- theme -----------------------------------------------------------------
// Colours come from the tokens in theme.css, which switch under html[data-theme="dark"].
// The first paint uses the saved choice or the Windows setting (see index.html).

function currentTheme() { return document.documentElement.dataset.theme === 'dark' ? 'dark' : 'light'; }

function updateThemeButton() {
  $('#themeToggle').textContent = currentTheme() === 'dark' ? 'Light mode' : 'Dark mode';
}

function toggleTheme() {
  const next = currentTheme() === 'dark' ? 'light' : 'dark';
  document.documentElement.dataset.theme = next;
  try { localStorage.setItem('clankban-theme', next); } catch { /* the choice is a convenience only */ }
  updateThemeButton();
}

// ---- project docs (read-only) ----------------------------------------------
// The workspace's project folder, shown read-only. A person picks the folder. The board
// never writes to it.

function errorText(error) {
  try { return JSON.parse(error.message).error || error.message; } catch { return error.message; }
}

function currentWorkspace() { return workspaces.find(w => w.id === workspace); }

function parseDocList(text = '') {
  return String(text).split(',').map(s => s.trim()).filter(Boolean);
}

async function loadProjectDocs() {
  const current = currentWorkspace();
  if (!remoteMode || !current?.projectRoot) {
    projectDocs = null;
    return;
  }
  try {
    projectDocs = await apiRequest(`/api/workspaces/${enc(workspace)}/docs`);
    docsRefreshedAt = new Date();
  } catch (error) {
    projectDocs = { error: errorText(error) };
  }
}

// What changed in the folder: a file added, removed, resized, or saved again.
function docsSignature(data) {
  return (data?.docs || []).map(d => `${d.path}|${d.bytes}|${d.modified}`).join('\n');
}

// Re-reads the folder and, if the open document changed, its text too. Nothing is drawn
// here: the caller decides when to render. Returns true when the page needs redrawing.
async function refreshProjectDocs() {
  const current = currentWorkspace();
  if (!remoteMode || !current?.projectRoot || docsRefreshing) return false;
  docsRefreshing = true;
  const previous = projectDocs;
  try {
    const fresh = await apiRequest(`/api/workspaces/${enc(workspace)}/docs`);
    docsRefreshedAt = new Date();
    let changed = !previous || previous.error || previous.root !== fresh.root || docsSignature(previous) !== docsSignature(fresh);
    projectDocs = fresh;
    if (selectedDoc) {
      const entry = fresh.docs.find(d => d.path === selectedDoc);
      const before = previous?.docs?.find(d => d.path === selectedDoc);
      if (!entry) {
        selectedDoc = null;
        docBody = '';
        changed = true;
      } else if (!before || before.modified !== entry.modified || before.bytes !== entry.bytes) {
        const result = await apiRequest(`/api/workspaces/${enc(workspace)}/docs/read?path=${enc(selectedDoc)}`);
        docBody = result.content;
        changed = true;
      }
    }
    return changed;
  } catch (error) {
    projectDocs = { error: errorText(error) };
    return true;
  } finally {
    docsRefreshing = false;
  }
}

// Background refresh while the Docs tab is open. The page is only redrawn when something changed.
async function docsAutoRefresh() {
  if ($('#docsView').hidden || !remoteMode) return;
  if (await refreshProjectDocs() || docsNeedsRender) renderDocs(false);
}

async function manualDocsRefresh() {
  const button = document.querySelector('[data-doc-refresh]');
  if (button) { button.disabled = true; button.textContent = 'Refreshing…'; }
  await refreshProjectDocs();
  renderDocs(true);
}

function docGroups(docs, query) {
  const q = query.trim().toLowerCase();
  const groups = new Map();
  for (const doc of docs.filter(d => !q || `${d.path} ${d.title}`.toLowerCase().includes(q))) {
    const slash = doc.path.lastIndexOf('/');
    const folder = slash < 0 ? '(top level)' : doc.path.slice(0, slash);
    if (!groups.has(folder)) groups.set(folder, []);
    groups.get(folder).push(doc);
  }
  return groups;
}

function inlineMarkdown(text) {
  return escapeHtml(text)
    .replace(/`([^`]+)`/g, '<code>$1</code>')
    .replace(/\*\*([^*]+)\*\*/g, '<strong>$1</strong>')
    .replace(/(^|[^*])\*([^*\s][^*]*)\*/g, '$1<em>$2</em>')
    .replace(/\[([^\]]+)\]\((https?:\/\/[^\s)]+)\)/g, (m, label, url) => `<a href="${url}" target="_blank" rel="noopener noreferrer">${label}</a>`)
    .replace(/(^|[\s(])(https?:\/\/[^\s<)]+)/g, (m, pre, url) => `${pre}<a href="${url}" target="_blank" rel="noopener noreferrer">${url}</a>`);
}

// A small Markdown renderer: headings, lists, fenced code, paragraphs, and inline code and emphasis.
function renderMarkdown(md) {
  const out = [];
  let list = null;
  let code = null;
  const closeList = () => { if (list) { out.push(`</${list}>`); list = null; } };
  for (const raw of md.split('\n')) {
    if (raw.startsWith('```')) {
      if (code === null) { closeList(); code = []; }
      else { out.push(`<pre><code>${escapeHtml(code.join('\n'))}</code></pre>`); code = null; }
      continue;
    }
    if (code !== null) { code.push(raw); continue; }
    const line = raw.trimEnd();
    const heading = line.match(/^(#{1,4})\s+(.*)$/);
    const bullet = line.match(/^\s*[-*]\s+(.*)$/);
    const numbered = line.match(/^\s*\d+\.\s+(.*)$/);
    if (heading) {
      closeList();
      const level = heading[1].length;
      out.push(`<h${level}>${inlineMarkdown(heading[2])}</h${level}>`);
    } else if (bullet || numbered) {
      const tag = numbered ? 'ol' : 'ul';
      if (list !== tag) { closeList(); out.push(`<${tag}>`); list = tag; }
      out.push(`<li>${inlineMarkdown((bullet || numbered)[1])}</li>`);
    } else if (line === '') {
      closeList();
    } else {
      closeList();
      out.push(`<p>${inlineMarkdown(line)}</p>`);
    }
  }
  if (code !== null) out.push(`<pre><code>${escapeHtml(code.join('\n'))}</code></pre>`);
  closeList();
  return out.join('\n');
}

async function openDoc(path) {
  selectedDoc = path;
  docBody = 'Loading…';
  renderDocs(true);
  try {
    const result = await apiRequest(`/api/workspaces/${enc(workspace)}/docs/read?path=${enc(path)}`);
    if (selectedDoc !== path) return;
    docBody = result.content;
  } catch (error) {
    if (selectedDoc !== path) return;
    docBody = `Could not open ${path}: ${errorText(error)}`;
  }
  renderDocs(true);
}

function renderSuggestion() {
  const box = $('#docsSuggestion');
  if (!box) return;
  const current = currentWorkspace();
  if (!remoteMode || !current?.suggestedRoot || current.suggestedRoot === current.projectRoot) {
    box.innerHTML = '';
    return;
  }
  box.innerHTML = `<div class="suggest-banner">
      <div>
        <strong>${escapeHtml(current.suggestedBy || 'An agent')}</strong> suggests this project folder
        <div class="suggest-path"><code>${escapeHtml(current.suggestedRoot)}</code></div>
      </div>
      <div class="suggest-actions">
        <button class="button primary" data-accept-suggestion>Use this folder</button>
        <button class="button quiet" data-dismiss-suggestion>Dismiss</button>
      </div>
    </div>`;
}

async function dismissSuggestion() {
  try {
    const result = await apiRequest(`/api/workspaces/${enc(workspace)}/suggestion`, { method: 'DELETE' });
    workspaces = result.workspaces;
    render();
  } catch (error) {
    alert(`Could not dismiss the suggestion: ${errorText(error)}`);
  }
}

// HTML pages in the project folder were written by agents, so they are never run as part of
// Clankban. They are shown in a sandboxed frame: sandbox="" allows no scripts, no forms, and no
// access to the app. A content policy inside the frame also blocks outside requests.
function isHtmlDoc(path) {
  return /\.html?$/i.test(path || '');
}

function renderHtmlPage(html) {
  const policy = '<meta http-equiv="Content-Security-Policy" content="default-src \'none\'; img-src data:; style-src \'unsafe-inline\'; font-src data:">';
  const page = /<head[^>]*>/i.test(html) ? html.replace(/<head[^>]*>/i, match => match + policy) : policy + html;
  return `<iframe class="doc-frame" sandbox="" title="Project page" srcdoc="${escapeHtml(page)}"></iframe>`;
}

function renderDocs(force = false) {
  renderSuggestion();
  const box = $('#docsBody');
  if (!box) return;
  const scroll = docsScrollPositions(box);
  // Keep typing in the filter undisturbed by background refreshes. Remember that the list changed.
  if (!force && document.activeElement?.id === 'docsSearch') { docsNeedsRender = true; return; }
  if (!remoteMode) {
    box.innerHTML = '<div class="empty-note">Project docs need the desktop app. Open the installed Clankban to read a project folder.</div>';
    return;
  }
  const current = currentWorkspace();
  if (!current?.projectRoot) {
    box.innerHTML = `<div class="docs-empty">
        <h2>Connect a project folder</h2>
        <p>Pick the folder of the project this workspace is for. Clankban reads its Markdown files and never writes to them.</p>
        <button class="button primary" data-doc-folder>Choose folder…</button>
      </div>`;
    return;
  }
  if (!projectDocs) {
    box.innerHTML = '<div class="empty-note">Reading the project folder…</div>';
    loadProjectDocs().then(() => renderDocs(true));
    return;
  }
  if (projectDocs.error) {
    box.innerHTML = `<div class="docs-empty">
        <h2>Folder unavailable</h2>
        <p>${escapeHtml(projectDocs.error)}</p>
        <button class="button quiet" data-doc-folder>Choose another folder…</button>
      </div>`;
    return;
  }
  const groups = [...docGroups(projectDocs.docs, docsSearch)];
  const list = groups.map(([folder, items]) => `
      <div class="doc-group">
        <div class="doc-folder">${escapeHtml(folder)}</div>
        ${items.map(d => `<button class="doc-item ${d.path === selectedDoc ? 'active' : ''}" data-doc="${escapeHtml(d.path)}">
            <span class="doc-title">${escapeHtml(d.title)}</span>
            <span class="doc-path">${escapeHtml(d.path.split('/').pop())}</span>
          </button>`).join('')}
      </div>`).join('') || '<div class="empty-note">No Markdown files match.</div>';
  box.innerHTML = `
    <div class="docs-header">
      <div class="docs-root"><span class="docs-label">Project folder</span><code>${escapeHtml(projectDocs.root)}</code></div>
      <div class="docs-actions">
        ${docsRefreshedAt ? `<span class="docs-updated">Updated ${docsRefreshedAt.toLocaleTimeString([], { hour: '2-digit', minute: '2-digit', second: '2-digit' })}</span>` : ''}
        <button class="button quiet" data-doc-refresh>Refresh</button>
        <button class="button quiet" data-doc-folder>Change…</button>
        <button class="button quiet" data-doc-clear>Disconnect</button>
      </div>
    </div>
    <div class="docs-layout">
      <aside class="docs-list">
        <input id="docsSearch" class="docs-search" placeholder="Filter ${projectDocs.docs.length} documents" value="${escapeHtml(docsSearch)}" />
        ${list}
      </aside>
      <article class="docs-reader">
        ${selectedDoc
          ? `<div class="docs-reader-path">${escapeHtml(selectedDoc)}</div>${isHtmlDoc(selectedDoc) ? renderHtmlPage(docBody) : renderMarkdown(docBody)}`
          : '<div class="empty-note">Pick a document to read it.</div>'}
      </article>
    </div>`;
  restoreDocsScroll(box, scroll);
  docsNeedsRender = false;
}

// Scroll positions of the page, the document list, and the reader, so a refresh keeps the view.
function docsScrollPositions(box) {
  return {
    page: document.querySelector('.main-content')?.scrollTop ?? 0,
    list: box.querySelector('.docs-list')?.scrollTop ?? 0,
    reader: box.querySelector('.docs-reader')?.scrollTop ?? 0,
  };
}

function restoreDocsScroll(box, pos) {
  const page = document.querySelector('.main-content');
  if (page) page.scrollTop = pos.page;
  const list = box.querySelector('.docs-list');
  if (list) list.scrollTop = pos.list;
  const reader = box.querySelector('.docs-reader');
  if (reader) reader.scrollTop = pos.reader;
}

function renderDrawerDocs(task) {
  const box = $('#drawerDocs');
  if (!box) return;
  const docs = task.docs || [];
  box.innerHTML = docs.length
    ? docs.map(p => `<button class="doc-chip" data-open-doc="${escapeHtml(p)}">${escapeHtml(p)}</button>`).join('')
    : '<span class="history-empty">No linked documents. Add some in the edit form.</span>';
}

async function openLinkedDoc(path) {
  closeDrawer();
  selectedDoc = null;
  setView('docs');
  await openDoc(path);
}

async function setProjectFolder(root) {
  try {
    const result = await apiRequest(`/api/workspaces/${enc(workspace)}/project`, {
      method: 'PUT',
      body: JSON.stringify({ root }),
    });
    workspaces = result.workspaces;
    projectDocs = null;
    selectedDoc = null;
    docBody = '';
    render();
  } catch (error) {
    alert(`Could not set the project folder: ${errorText(error)}`);
  }
}

async function chooseProjectFolder() {
  if (!tauriCore) return;
  let path = null;
  try {
    path = await tauriCore.invoke('pick_project_folder');
  } catch (error) {
    alert(`Could not open the folder picker: ${errorText(error)}`);
    return;
  }
  if (path) await setProjectFolder(path);
}

// ---- wiring ----------------------------------------------------------------

function isOpen(selector) { return document.querySelector(selector).classList.contains('open'); }

function closeTop() {
  if (isOpen('#workspaceModal')) return closeWorkspaceModal();
  if (isOpen('#paletteBackdrop')) return closePalette();
  if (isOpen('#helpBackdrop')) return closeHelp();
  if (isOpen('#changelogBackdrop')) return closeNotes();
  if (isOpen('#sprintBackdrop')) return closeSprintManager();
  if (isOpen('#viewBackdrop')) return closeViewDialog();
  if (isOpen('#jsonModalBackdrop')) return $('#jsonModalBackdrop').classList.remove('open');
  if (isOpen('#modalBackdrop')) return closeModal();
  if (isOpen('#taskDrawer')) return closeDrawer();
  if (document.querySelector('#columnMenu')) return closeColumnMenu();
  if (!$('#filterPanel').hidden) toggleFilterPanel(false);
}

document.querySelectorAll('[data-view]').forEach(button => button.addEventListener('click', () => {
  // The sidebar's Dashboard covers every board. A board's own dashboard opens from its name.
  if (button.dataset.view === 'dashboard') dashboardScope = 'all';
  setView(button.dataset.view);
}));
$('#workspaceCrumb').onclick = () => openWorkspaceDashboard(workspace);

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
$('#filterTag').onchange = e => { filters.tag = e.target.value; render(); };
$('#filterSprint').onchange = e => { filters.sprint = e.target.value; render(); };
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
  const doc = e.target.closest('[data-doc]');
  if (doc) return openDoc(doc.dataset.doc);
  const chip = e.target.closest('[data-open-doc]');
  if (chip) return openLinkedDoc(chip.dataset.openDoc);
  const accept = e.target.closest('[data-accept-suggestion]');
  if (accept) {
    const current = currentWorkspace();
    if (current?.suggestedRoot) return setProjectFolder(current.suggestedRoot);
  }
  if (e.target.closest('[data-dismiss-suggestion]')) return dismissSuggestion();
  if (e.target.closest('[data-doc-refresh]')) return manualDocsRefresh();
  if (e.target.closest('[data-doc-folder]')) return chooseProjectFolder();
  if (e.target.closest('[data-doc-clear]') && confirm('Disconnect the project folder? Its files stay where they are.')) {
    return setProjectFolder(null);
  }
});
document.addEventListener('input', e => {
  if (e.target.id === 'docsSearch') {
    docsSearch = e.target.value;
    renderDocs(true);
  }
});
$('#workspaceButton').onclick = () => ($('#workspaceMenu').hidden ? openWorkspaceMenu() : closeWorkspaceMenu());
$('#workspaceMenu').onclick = e => {
  if (e.target.closest('[data-new-workspace]')) {
    closeWorkspaceMenu();
    openWorkspaceModal(false);
    return;
  }
  const item = e.target.closest('[data-workspace]');
  if (item) {
    closeWorkspaceMenu();
    setWorkspace(item.dataset.workspace);
  }
};
document.addEventListener('click', e => {
  if (!e.target.closest('.ws-picker')) closeWorkspaceMenu();
});
$('#editWorkspace').onclick = () => openWorkspaceModal(true);
$('#workspaceForm').onsubmit = saveWorkspace;
$('#closeWorkspaceModal').onclick = closeWorkspaceModal;
$('#cancelWorkspace').onclick = closeWorkspaceModal;
$('#deleteWorkspace').onclick = deleteWorkspace;

document.addEventListener('submit', e => {
  if (e.target.matches('#skillForm')) {
    e.preventDefault();
    sendRun(e.target);
  }
});

document.addEventListener('input', e => {
  const form = e.target.closest?.('#skillForm');
  if (!form || !form.dataset.skill || !e.target.name) return;
  const draft = agentDrafts[form.dataset.skill] || (agentDrafts[form.dataset.skill] = {});
  draft[e.target.name] = e.target.value;
});

document.addEventListener('click', e => {
  const viewItem = e.target.closest('[data-view-id]');
  if (viewItem && viewItem.closest('#viewsMenu')) {
    openViewsMenu(false);
    return applyCustomView(viewItem.dataset.viewId);
  }
  const viewAction = e.target.closest('[data-view-action]');
  if (viewAction && viewAction.closest('#viewsMenu')) {
    openViewsMenu(false);
    return runViewAction(viewAction.dataset.viewAction);
  }
  if (!e.target.closest('#viewsPicker')) openViewsMenu(false);
  const sprintButton = e.target.closest('[data-sprint-action]');
  if (sprintButton) return sprintAction(sprintButton.dataset.sprintId, sprintButton.dataset.sprintAction);
  const dashTask = e.target.closest('[data-dash-task]');
  if (dashTask) return openTaskAnywhere(dashTask.dataset.dashTask, dashTask.dataset.workspace);
  const dashBoard = e.target.closest('[data-open-board]');
  if (dashBoard) return openWorkspaceDashboard(dashBoard.dataset.openBoard);
  const externalLink = e.target.closest('a[href]');
  if (externalLink && /^https?:\/\//i.test(externalLink.getAttribute('href'))) {
    e.preventDefault();
    return openLink(externalLink.getAttribute('href'));
  }
  const openTask = e.target.closest('[data-open-task]');
  if (openTask) return openDrawer(openTask.dataset.openTask);
  const recentTabButton = e.target.closest('[data-recent-tab]');
  if (recentTabButton) { recentTab = recentTabButton.dataset.recentTab; return render(); }
  const approveBoard = e.target.closest('[data-approve-board]');
  if (approveBoard) return decideBoardRequest(approveBoard.dataset.approveBoard, 'approve');
  const rejectBoard = e.target.closest('[data-reject-board]');
  if (rejectBoard) return decideBoardRequest(rejectBoard.dataset.rejectBoard, 'reject');
  const tagToggle = e.target.closest('[data-toggle-tag]');
  if (tagToggle) return toggleTag(activeTaskId, tagToggle.dataset.toggleTag);
  const skillOption = e.target.closest('.skill-option');
  if (skillOption) return selectSkill(skillOption.dataset.skill);
  const cancelButton = e.target.closest('.cancel-run');
  if (cancelButton) return cancelRun(Number(cancelButton.dataset.id));
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

$('#checkUpdates').onclick = () => checkForUpdates();
$('#themeToggle').onclick = toggleTheme;
updateThemeButton();
$('#installUpdate').onclick = installUpdate;

render();
loadRemoteTasks();
if (remoteMode) setInterval(loadRemoteTasks, 5000);
// Project docs refresh every 8 seconds while the Docs tab is open. The folder scan is cheap
// and the page only redraws when a file changed.
if (remoteMode) setInterval(docsAutoRefresh, 8000);
showAppVersion();
