const http = require('http');
const fs = require('fs');
const path = require('path');
const { DatabaseSync } = require('node:sqlite');

const seedTasks = [
  ['AT-101','Define project scope & success metrics','Align on the first release boundary and document how we will know it is working.','epic','high','backlog','2026-11-24','2026-11-17',JSON.stringify([{title:'Draft the one-page brief',done:true},{title:'Review with stakeholders',done:false}])],
  ['AT-104','Design the agent context panel','Give local agents a clean, inspectable view of board state, task relationships, and recent changes.','task','high','progress','2026-11-22','2026-11-18',JSON.stringify([{title:'Map the JSON shape',done:true},{title:'Add copy action',done:false},{title:'Document the read contract',done:false}])],
  ['AT-107','Build drag-and-drop board view','Make work feel tangible: move cards between statuses and preserve the state locally.','task','medium','progress','2026-11-28','2026-11-18','[]'],
  ['AT-109','Fix empty state on new workspace','New workspaces should explain the next step instead of showing a blank canvas.','bug','high','progress','2026-11-21','2026-11-19','[]'],
  ['AT-112','Write local API adapter notes','Capture the endpoints an agent bridge will need to read and mutate the board.','subtask','low','review','2026-11-30','2026-11-19','[]'],
  ['AT-115','Choose a stable task identifier format','Document how identifiers are generated and how they should be referenced in agent output.','task','medium','review','2026-12-01','2026-11-19','[]'],
  ['AT-098','Set up local persistence','Keep the board useful between sessions with browser-local storage.','task','medium','done','2026-11-18','2026-11-14','[]'],
  ['AT-093','Create visual language tokens','Establish color, type, spacing, and status patterns for the workspace.','task','low','done','2026-11-16','2026-11-12','[]']
];

function openDatabase(dataDir) {
  fs.mkdirSync(dataDir, { recursive: true });
  const db = new DatabaseSync(path.join(dataDir, 'clankban.sqlite'));
  db.exec(`CREATE TABLE IF NOT EXISTS tasks (id TEXT PRIMARY KEY, title TEXT NOT NULL, description TEXT DEFAULT '', type TEXT NOT NULL, priority TEXT NOT NULL, status TEXT NOT NULL, due TEXT, created TEXT NOT NULL, subtasks TEXT NOT NULL DEFAULT '[]', updated TEXT NOT NULL, parent_id TEXT)`);
  const columns = db.prepare('PRAGMA table_info(tasks)').all().map(column => column.name);
  if (!columns.includes('parent_id')) db.exec('ALTER TABLE tasks ADD COLUMN parent_id TEXT');
  if (db.prepare('SELECT COUNT(*) AS count FROM tasks').get().count === 0) {
    const insert = db.prepare('INSERT INTO tasks VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)');
    const now = new Date().toISOString(); seedTasks.forEach(t => insert.run(...t, now, null));
  }
  return db;
}
function readTasks(db) { return db.prepare('SELECT * FROM tasks ORDER BY created DESC').all().map(t => ({...t, status: t.status === 'todo' ? 'backlog' : t.status, parentId: t.parent_id || (t.description.match(/(?:Parent|Epic):\s*(AT-\d+)/i)||[])[1] || null, subtasks: JSON.parse(t.subtasks)})); }
function taskFromBody(body) { return { id: body.id, title: body.title, description: body.description || '', type: body.type || 'task', priority: body.priority || 'medium', status: body.status === 'todo' ? 'backlog' : (body.status || 'backlog'), due: body.due || '', created: body.created || new Date().toISOString().slice(0,10), subtasks: body.subtasks || [], parentId: body.parentId || null }; }
function json(res, status, body) { const output = JSON.stringify(body); res.writeHead(status, {'content-type':'application/json; charset=utf-8','access-control-allow-origin':'*','access-control-allow-headers':'content-type'}); res.end(output); }
function startServer({ port = 4187, dataDir = path.join(__dirname, '.data') } = {}) {
  const db = openDatabase(dataDir);
  const server = http.createServer((req, res) => {
    if (req.method === 'OPTIONS') { res.writeHead(204, {'access-control-allow-origin':'*','access-control-allow-methods':'GET,POST,PATCH,DELETE,OPTIONS','access-control-allow-headers':'content-type'}); return res.end(); }
    const url = new URL(req.url, 'http://127.0.0.1');
    if (req.method === 'GET' && url.pathname === '/api/health') return json(res, 200, {ok:true, app:'clankban', version:'0.1.0'});
    if (req.method === 'GET' && url.pathname === '/api/tasks') return json(res, 200, {tasks:readTasks(db)});
    if (req.method === 'GET' && url.pathname === '/api/agent-context') {
      const tasks = readTasks(db); const active = tasks.filter(t => t.status !== 'done');
      return json(res, 200, {workspace:'atlas', schemaVersion:'1.0', summary:{active:active.length, blocked:0, dueSoon:active.filter(t=>t.due && t.due <= new Date(Date.now()+7*864e5).toISOString().slice(0,10)).length}, tasks:active.map(({id,title,type,priority,status,due,subtasks})=>({id,title,type,priority,status,due,subtasks}))});
    }
    const match = url.pathname.match(/^\/api\/tasks\/([^/]+)$/); const id = match && decodeURIComponent(match[1]);
    if (match && req.method === 'GET') { const task=readTasks(db).find(t=>t.id===id); return task ? json(res,200,task) : json(res,404,{error:'Task not found'}); }
    let raw=''; req.on('data', chunk=>raw+=chunk); req.on('end', ()=>{ let body={}; try{body=raw?JSON.parse(raw):{}}catch{return json(res,400,{error:'Invalid JSON'})}
      if (req.method === 'POST' && url.pathname === '/api/tasks') { const existing=readTasks(db); const next=body.id || `AT-${Math.max(0,...existing.map(t=>Number(t.id.slice(3))))+1}`; const task=taskFromBody({...body,id:next}); const now=new Date().toISOString(); db.prepare('INSERT INTO tasks VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)').run(task.id,task.title,task.description,task.type,task.priority,task.status,task.due,task.created,JSON.stringify(task.subtasks),now,task.parentId); return json(res,201,task); }
      if (match && req.method === 'PATCH') { const old=readTasks(db).find(t=>t.id===id); if(!old)return json(res,404,{error:'Task not found'}); const task=taskFromBody({...old,...body,id}); db.prepare('UPDATE tasks SET title=?,description=?,type=?,priority=?,status=?,due=?,subtasks=?,updated=?,parent_id=? WHERE id=?').run(task.title,task.description,task.type,task.priority,task.status,task.due,JSON.stringify(task.subtasks),new Date().toISOString(),task.parentId,id); return json(res,200,task); }
      if (match && req.method === 'DELETE') { db.prepare('DELETE FROM tasks WHERE id=?').run(id); return json(res,200,{deleted:id}); }
      return json(res,404,{error:'Route not found'});
    });
  });
  return new Promise(resolve=>server.listen(port,'127.0.0.1',()=>resolve({server,db,url:`http://127.0.0.1:${server.address().port}`})));
}
function closeServer(instance) { if (!instance) return Promise.resolve(); instance.db.close(); return new Promise(resolve=>instance.server.close(resolve)); }
if (require.main === module) startServer({port:Number(process.env.CLANKBAN_PORT)||4187}).then(s=>console.log(`Clankban API listening at ${s.url}`));
module.exports = { startServer, closeServer, readTasks };
